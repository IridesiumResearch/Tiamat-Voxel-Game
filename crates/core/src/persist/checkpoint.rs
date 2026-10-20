// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only
//! The write-ahead log folded back off the tick: a thread of its own, on a
//! connection of its own.
//!
//! # Why a thread
//!
//! `SQLite`'s automatic checkpoint runs inside whichever commit crosses its
//! threshold, on the thread that made the commit — for a world, the tick. It
//! copies up to a thousand pages into the database file and syncs them, and
//! on a disk that is busy with anything else that is a long time: 127 ms
//! inside a summary save on the reference machine (2026-10-16) and 131 ms
//! inside a chunk-save batch on the nightly runner (run 82, 2026-10-20), each
//! at a WAL of 3.9–4.1 MiB, which is the thousand pages exactly. Every other
//! tick of those soaks was under 7 ms.
//!
//! So [`schema::apply_pragmas`] turns the automatic checkpoint off, and this
//! thread does the folding instead: a **passive** checkpoint every
//! [`INTERVAL`], which copies what it can without waiting for the writer or
//! any reader and gives up on the rest until next time. `SQLite` lets a
//! checkpoint run beside a writer in WAL mode — that is the mode's point —
//! so the tick's commits go on while this thread syncs, and the only lock
//! they can meet is the brief one on the WAL index, inside the tick's
//! `busy_timeout`.
//!
//! The one at close is still the owner's ([`WorldDb::close`] runs a passive
//! checkpoint after this thread has been stopped), and the truncating
//! checkpoint a save-freeze needs is still the freeze's, with the writers
//! halted first — this thread holds no transaction between its passes, so it
//! never stands in a truncate's way.
//!
//! # What it never does
//!
//! It never writes a row, never reads one, and never waits: a pass that finds
//! the WAL busy does less and says so in the debug log. If `SQLite` refuses
//! the checkpoint outright the pass is logged at `warn` and the next interval
//! tries again; the tick is unaffected either way, because the WAL simply
//! grows until a pass succeeds, and `journal_size_limit` trims the file back
//! once one does.
//!
//! [`WorldDb::close`]: super::WorldDb::close

use std::path::Path;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::Connection;

use super::{WorldError, schema};

/// How often the thread folds the log back.
///
/// Fifty players digging commit a few hundred kilobytes a second; at this
/// interval a pass copies a few hundred pages at most, which is tens of
/// milliseconds on a slow disk — on a thread that has nothing else to do.
pub const INTERVAL: Duration = Duration::from_secs(5);

/// What one passive checkpoint did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// Whether a reader or writer kept part of the log from being copied.
    pub busy: bool,
    /// Frames in the log when the pass ran.
    pub wal_frames: u64,
    /// Frames of it that are now in the database file (including those an
    /// earlier pass already copied).
    pub checkpointed: u64,
}

/// Runs one passive checkpoint on `conn` and says what it did.
///
/// # Errors
///
/// Any SQL failure. A pass that could not copy everything is not an error;
/// `busy` says so.
pub fn checkpoint_passive(conn: &Connection) -> rusqlite::Result<Pass> {
    conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
        // `SQLite` answers -1 for a database that is not in WAL mode.
        let count = |i: usize| -> rusqlite::Result<u64> {
            Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
        };
        Ok(Pass {
            busy: row.get::<_, i64>(0)? != 0,
            wal_frames: count(1)?,
            checkpointed: count(2)?,
        })
    })
}

/// The thread that folds a world's log back while the world is open.
///
/// Stopped and joined by [`Checkpointer::stop`], or on drop.
#[derive(Debug)]
pub struct Checkpointer {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Checkpointer {
    /// Opens a connection of its own to the world at `path` and starts
    /// folding its log back every `interval`.
    ///
    /// # Errors
    ///
    /// [`WorldError::Sql`] if the connection cannot be opened or its pragmas
    /// applied, [`WorldError::Thread`] if the thread cannot be spawned.
    pub fn spawn(path: &Path, interval: Duration) -> Result<Self, WorldError> {
        let conn = Connection::open(path)?;
        // The same pragmas as the owner's connection: the automatic checkpoint
        // off here too, and the same patience with a busy log.
        schema::apply_pragmas(&conn)?;
        let (stop, wake) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("checkpoint".to_owned())
            .spawn(move || Self::run(&conn, &wake, interval))
            .map_err(WorldError::Thread)?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    fn run(conn: &Connection, wake: &mpsc::Receiver<()>, interval: Duration) {
        loop {
            match wake.recv_timeout(interval) {
                // A stop, or the owner gone without one: either way, out.
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            match checkpoint_passive(conn) {
                Ok(pass) if pass.checkpointed > 0 => tracing::debug!(
                    frames = pass.checkpointed,
                    of = pass.wal_frames,
                    busy = pass.busy,
                    "WAL folded back"
                ),
                Ok(_) => {}
                Err(err) => tracing::warn!(%err, "WAL checkpoint failed; retried next interval"),
            }
        }
    }

    /// Stops the thread and waits for it.
    ///
    /// After this the owner's connection is the only one on the file, which
    /// is what a closing flush or a save-freeze's truncate wants.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() {
            // A receiver already gone means the thread already ended.
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            // A panicked checkpoint thread has nothing left to report; the
            // owner's own flush at close is what persists the world.
            let _ = thread.join();
        }
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::WorldDb;
    use super::*;
    use crate::material::Registry;

    /// A file-backed world of its own per test, because the whole point is
    /// the log file beside it.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("tiamat-checkpoint");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(format!("{}-{name}.tiamat", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.clone().into_os_string();
            file.push(suffix);
            let _ = std::fs::remove_file(file);
        }
        path
    }

    /// Writes past the thousand pages the automatic checkpoint used to fire at.
    fn grow_the_log(db: &WorldDb, rows: usize) {
        let value = "x".repeat(100 * 1024);
        for i in 0..rows {
            WorldDb::write_meta_str(&db.conn, &format!("checkpoint-test-{i}"), &value)
                .expect("write");
        }
    }

    #[test]
    fn the_automatic_checkpoint_is_off_and_the_log_grows_past_the_old_threshold() {
        let path = scratch("off");
        let mut registry = Registry::default();
        let db = WorldDb::open(&path, &mut registry).expect("open");
        let auto: i64 = db
            .conn
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
            .expect("pragma");
        assert_eq!(
            auto, 0,
            "the tick's connection must never checkpoint on its own"
        );

        // Sixty rows of a hundred kibibytes: a thousand pages is four
        // mebibytes, so with the automatic checkpoint on the file would have
        // been reused from its start by now; off, it keeps growing.
        grow_the_log(&db, 60);
        let wal = db.wal_bytes().expect("a log beside the file");
        assert!(
            wal > 5 * 1024 * 1024,
            "the log should have grown past the old threshold; it is {wal} bytes"
        );
    }

    #[test]
    fn one_passive_pass_from_another_connection_folds_everything_back() {
        let path = scratch("pass");
        let mut registry = Registry::default();
        let db = WorldDb::open(&path, &mut registry).expect("open");
        WorldDb::write_meta_i64(&db.conn, "checkpoint-test-marker", 42).expect("write");
        grow_the_log(&db, 60);

        let other = rusqlite::Connection::open(&path).expect("second connection");
        schema::apply_pragmas(&other).expect("pragmas");
        let pass = checkpoint_passive(&other).expect("checkpoint");
        assert!(!pass.busy, "nothing holds the log: {pass:?}");
        assert!(
            pass.wal_frames > 1000,
            "the log should hold the writes: {pass:?}"
        );
        assert_eq!(
            pass.checkpointed, pass.wal_frames,
            "a passive pass with no reader in the way copies every frame: {pass:?}"
        );

        // And the owner's connection still reads what it wrote, through the
        // database file now rather than the log.
        let back = WorldDb::read_meta_i64(&db.conn, "checkpoint-test-marker").expect("read");
        assert_eq!(back, Some(42));
    }

    #[test]
    fn the_thread_keeps_the_log_folded_while_the_owner_writes() {
        let path = scratch("thread");
        let mut registry = Registry::default();
        let db = WorldDb::open(&path, &mut registry).expect("open");
        let thread = db
            .checkpointer(Duration::from_millis(20))
            .expect("spawn")
            .expect("a file-backed world gets a thread");
        grow_the_log(&db, 30);
        std::thread::sleep(Duration::from_millis(250));
        grow_the_log(&db, 30);
        std::thread::sleep(Duration::from_millis(250));
        thread.stop();

        // Whatever the thread left, one more pass finds nothing in the way
        // and the count of copied frames meets the count in the log.
        let other = rusqlite::Connection::open(&path).expect("second connection");
        schema::apply_pragmas(&other).expect("pragmas");
        let pass = checkpoint_passive(&other).expect("checkpoint");
        assert!(!pass.busy, "{pass:?}");
        assert_eq!(pass.checkpointed, pass.wal_frames, "{pass:?}");
        // The thread's passes folded most of it before this one did.
        assert!(pass.wal_frames > 0, "{pass:?}");
    }

    #[test]
    fn an_in_memory_world_has_no_log_and_no_thread() {
        let mut registry = Registry::default();
        let db = WorldDb::open_in_memory(&mut registry).expect("open");
        assert!(db.checkpointer(INTERVAL).expect("no error").is_none());
    }
}
