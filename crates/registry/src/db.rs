// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The registry's SQLite database: one file, WAL, forward migrations only.
//!
//! # Why one connection behind a mutex
//!
//! This service answers a handful of requests per player per ninety days. A
//! connection pool would be more moving parts than the load justifies, and the
//! mutex also gives every handler a consistent view for free. `busy_timeout` is
//! two seconds — the design's request budget — so a handler that waits on a
//! lock returns an error rather than hanging a thread for ever.
//!
//! # Migrations move forward and never back
//!
//! `PRAGMA user_version` is the schema version. A newer database is **refused**
//! rather than guessed at (the same rule as the world database): a service that
//! read a layout it does not know would write rows the newer build cannot read.
//! Every migration runs in one transaction, so a crash mid-migration leaves the
//! previous version intact.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};

/// The schema this build reads and writes.
pub const SCHEMA_VERSION: i64 = 3;

/// Every migration, oldest first, with the version it brings the database to.
///
/// A migration is applied in order when the stored version is below its
/// target. The list is append-only: changing an entry that has shipped would
/// migrate a database that already ran it into a different shape.
const MIGRATIONS: &[(i64, &str)] = &[(1, SCHEMA_V1), (2, SCHEMA_V2), (3, SCHEMA_V3)];

/// The schema, in one statement batch.
///
/// A fresh database gets all of it; an existing one at version 1 gets nothing.
/// Comments here are for the next person changing the layout, because a
/// migration is the one change in this crate that cannot be undone by a
/// redeploy.
const SCHEMA_V1: &str = r"
-- A tiny key/value table for the counters that are not about one name:
-- `next_serial` is the certificate serial, monotonic per registry key, which a
-- revocation names.
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
);

-- One row per binding. A released binding keeps its row: the thirty-day hold
-- belongs to the UUID that released it, and a name's history is what an
-- operator is asked to look at (admin audit). 'released' and 'hold_until' are
-- NULL while the binding stands.
CREATE TABLE IF NOT EXISTS names (
    canonical  TEXT PRIMARY KEY,
    display    TEXT NOT NULL,
    uuid       BLOB NOT NULL,
    claimed    INTEGER NOT NULL,
    renewed    INTEGER NOT NULL,
    released   INTEGER,
    hold_until INTEGER,
    renewals   INTEGER NOT NULL DEFAULT 0
);

-- One active name per identity (§3). Released rows are outside the index, so a
-- player who released a name may claim another without waiting out the hold.
CREATE UNIQUE INDEX IF NOT EXISTS names_one_active
    ON names(uuid) WHERE released IS NULL;

-- A recovery address. Only ever written once the link in the mail is followed;
-- until then nothing but a hashed token exists (see `tokens`).
CREATE TABLE IF NOT EXISTS emails (
    uuid        BLOB PRIMARY KEY,
    address     TEXT NOT NULL,
    verified_at INTEGER NOT NULL
);

-- One-shot links: email verification and recovery confirmation. The token's
-- hash is the primary key, so what is stored cannot be replayed from a database
-- dump. `address` is the pending address before it is verified; `canonical` is
-- the name a recovery is about.
CREATE TABLE IF NOT EXISTS tokens (
    hash      BLOB PRIMARY KEY,
    purpose   TEXT NOT NULL,
    uuid      BLOB NOT NULL,
    canonical TEXT,
    address   TEXT,
    expires   INTEGER NOT NULL,
    created   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS tokens_expiry ON tokens(expires);

-- A recovery in flight. `state` is one of `pending`, `moved`, `cancelled`;
-- `notice_until` is the end of the fourteen-day public notice.
CREATE TABLE IF NOT EXISTS recoveries (
    canonical    TEXT PRIMARY KEY,
    from_uuid    BLOB NOT NULL,
    to_uuid      BLOB NOT NULL,
    confirmed_at INTEGER NOT NULL,
    notice_until INTEGER NOT NULL,
    state        TEXT NOT NULL
);

-- The operator's reserved list. The engine's own (administrative names and the
-- default mods' ids) is a constant in core; this table is for what changes
-- without a release.
CREATE TABLE IF NOT EXISTS reserved (
    canonical TEXT PRIMARY KEY
);

-- Revoked certificate serials. Keyed by registry key as well as serial: a new
-- registry key starts its numbering, and an old serial must not silence a new
-- certificate.
CREATE TABLE IF NOT EXISTS revocations (
    registry_key BLOB NOT NULL,
    serial       INTEGER NOT NULL,
    when_at      INTEGER NOT NULL,
    reason       TEXT NOT NULL,
    PRIMARY KEY (registry_key, serial)
);

-- Who did what, when, and why. Deliberately manual actions (a revoke, a rename)
-- are the operations a player can complain about, so they are recorded for good.
CREATE TABLE IF NOT EXISTS audit (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    who     TEXT NOT NULL,
    what    TEXT NOT NULL,
    when_at INTEGER NOT NULL,
    why     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS audit_when ON audit(when_at);

-- Rate-limit counters, by scope ('ip:…', 'net:…', 'uuid:…') and window start.
-- Rows older than seven days are deleted by the service itself (§7).
CREATE TABLE IF NOT EXISTS limits (
    scope  TEXT NOT NULL,
    window INTEGER NOT NULL,
    count  INTEGER NOT NULL,
    PRIMARY KEY (scope, window)
);
CREATE INDEX IF NOT EXISTS limits_window ON limits(window);

-- Published rotations, in order. Replayed at startup so a restarted service
-- keeps its chain of trust rather than pinning whatever key the environment
-- happens to hold today.
CREATE TABLE IF NOT EXISTS rotations (
    seq    INTEGER PRIMARY KEY,
    record BLOB NOT NULL
);
";

/// Schema 2: what the registry has issued.
///
/// A recovery moves a name to a new identity and must revoke every certificate
/// the old identity was given (§4). The registry does not keep certificates —
/// they are the client's — so this table keeps the one thing a revocation needs:
/// the serial and the registry key that signed it. It is append-only in
/// practice; a row is never updated.
const SCHEMA_V2: &str = r"
CREATE TABLE IF NOT EXISTS issued (
    serial       INTEGER PRIMARY KEY,
    registry_key BLOB NOT NULL,
    uuid         BLOB NOT NULL,
    canonical    TEXT NOT NULL,
    issued       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS issued_uuid ON issued(uuid);
";

/// Schema 3: who the operator revoked a name from.
///
/// An operator's revocation releases the binding with the ordinary hold, and a
/// released name is its old holder's to reclaim at once (that is what makes a
/// rename a release plus a claim). Without a mark, the squatter a revocation
/// was aimed at would simply claim the name back. A table rather than a column
/// on `names` because `CREATE TABLE IF NOT EXISTS` is idempotent and `ALTER
/// TABLE ADD COLUMN` is not, and a migration that can run twice is one that
/// cannot half-apply.
const SCHEMA_V3: &str = r"
CREATE TABLE IF NOT EXISTS revoked_holders (
    canonical  TEXT NOT NULL,
    uuid       BLOB NOT NULL,
    revoked_at INTEGER NOT NULL,
    PRIMARY KEY (canonical, uuid)
);
";

/// Begins a transaction that takes SQLite's write lock **at `BEGIN`**.
///
/// rusqlite's default is a deferred transaction, which reads first and asks for
/// the write lock at its first write. In WAL mode, if another connection (the
/// admin CLI, run beside the service) committed in between, that upgrade fails
/// with `SQLITE_BUSY_SNAPSHOT` at once — `busy_timeout` cannot help, because
/// waiting would not make the stale snapshot fresh. Every path here that reads
/// and then writes therefore begins `IMMEDIATE`, which waits for the lock up
/// front and then sees the latest data.
///
/// # Errors
///
/// Any SQL failure, including `SQLITE_BUSY` once `busy_timeout` has passed.
pub fn write_transaction(connection: &mut Connection) -> rusqlite::Result<Transaction<'_>> {
    connection.transaction_with_behavior(TransactionBehavior::Immediate)
}

/// The registry's database handle.
pub struct Db {
    connection: Mutex<Connection>,
}

impl Db {
    /// Opens (or creates) the database at `path` and migrates it forward.
    ///
    /// # Errors
    ///
    /// [`DbError`] when the file cannot be opened, is from a newer build, or a
    /// migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        let connection = Connection::open(path).map_err(|source| DbError::Open {
            path: path.display().to_string(),
            source,
        })?;
        Self::setup(connection)
    }

    /// An in-memory database, for tests.
    ///
    /// # Errors
    ///
    /// [`DbError`] if SQLite refuses to open one, which is unreachable in
    /// practice but is still a `Result` rather than a panic.
    pub fn in_memory() -> Result<Self, DbError> {
        Self::setup(Connection::open_in_memory()?)
    }

    /// Runs the pragmas every connection needs and applies migrations.
    fn setup(mut connection: Connection) -> Result<Self, DbError> {
        // Two seconds: the request budget from the design. A handler waiting
        // longer than that on a write lock should return 503, not hold a thread.
        connection.busy_timeout(Duration::from_secs(2))?;
        // An in-memory database answers `memory` here and there is nothing to
        // do; a file database answers `wal`.
        let _mode: String =
            connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Runs `work` with the one connection.
    ///
    /// The closure returns `rusqlite::Result` rather than a domain error so the
    /// database layer stays free of the service's vocabulary: domain outcomes
    /// (`taken`, `unknown`) are values the closure returns, and only SQL
    /// failures become [`DbError`].
    ///
    /// # Errors
    ///
    /// [`DbError::Sql`] if the closure's SQL failed.
    pub fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DbError> {
        let mut connection = self.lock();
        work(&mut connection).map_err(DbError::Sql)
    }

    /// Runs `work` with the one connection, for a closure with its own error.
    ///
    /// The flows in `recover` fail for more reasons than SQL (a token's
    /// randomness, a mail transport), so they carry a wider error; this lets
    /// them use the same lock without the database layer knowing their
    /// vocabulary.
    ///
    /// # Errors
    ///
    /// Whatever the closure returns.
    pub fn try_with<T, E: From<DbError>>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut connection = self.lock();
        work(&mut connection)
    }

    /// The connection, recovered if a previous holder panicked.
    ///
    /// **Recovering is safe here, and refusing is not.** A handler that panics
    /// mid-transaction unwinds through the `Transaction`'s drop, which rolls it
    /// back (the workspace does not set `panic = "abort"`), so the connection
    /// is in the state the last commit left it. Treating the poison as fatal
    /// instead turned one bad request into a registry that answers 500 to every
    /// request until it is restarted.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.connection.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("a request panicked holding the database; recovering the connection");
            self.connection.clear_poison();
            poisoned.into_inner()
        })
    }

    /// Whether the connection is held right now.
    ///
    /// For tests that prove slow work (a mail send) happens **outside** the
    /// lock: a transport that asks this from inside `send` must see `false`.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        matches!(
            self.connection.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Db(..)")
    }
}

/// Applies every migration the database is behind on.
fn migrate(connection: &mut Connection) -> Result<(), DbError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(DbError::TooNew { found: version });
    }
    if version < SCHEMA_VERSION {
        // One transaction for the whole chain: a crash halfway through leaves
        // the version and the tables where they were, not half-migrated.
        let transaction = write_transaction(connection)?;
        for (target, sql) in MIGRATIONS {
            if version < *target {
                transaction.execute_batch(sql)?;
            }
        }
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
    }
    Ok(())
}

/// What went wrong with the database.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The file could not be opened.
    #[error("could not open the registry database at `{path}`")]
    Open {
        /// Where it was.
        path: String,
        /// Why.
        #[source]
        source: rusqlite::Error,
    },

    /// A database written by a newer build.
    #[error(
        "the registry database is schema {found}, and this build reads {SCHEMA_VERSION}. \
         Downgrading is not supported; run the newer build instead"
    )]
    TooNew {
        /// What the file claims.
        found: i64,
    },

    /// A statement failed.
    #[error("the registry database reported: {0}")]
    Sql(#[from] rusqlite::Error),

    /// A backup could not be written or the old ones could not be pruned.
    #[error("a backup could not be written at `{path}`")]
    Backup {
        /// Where it was going, or what could not be removed.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory under the workspace target, because this environment's
    /// sandbox denies writes under the system temp directory. CI is fine with
    /// either; using the target directory keeps the suite runnable everywhere.
    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/registry-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    fn version(db: &Db) -> i64 {
        db.with(|connection| connection.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .expect("version")
    }

    #[test]
    fn a_fresh_database_is_at_the_current_schema() {
        let db = Db::in_memory().expect("open");
        assert_eq!(version(&db), SCHEMA_VERSION);
    }

    #[test]
    fn every_table_the_service_needs_exists() {
        let db = Db::in_memory().expect("open");
        let tables: Vec<String> = db
            .with(|connection| {
                let mut statement = connection
                    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            })
            .expect("tables");
        for expected in [
            "audit",
            "emails",
            "issued",
            "limits",
            "meta",
            "names",
            "recoveries",
            "reserved",
            "revocations",
            "revoked_holders",
            "rotations",
            "tokens",
        ] {
            assert!(
                tables.iter().any(|table| table == expected),
                "{expected} is missing from {tables:?}"
            );
        }
    }

    #[test]
    fn a_schema_one_database_gains_the_issued_table() {
        let dir = test_dir("migrate-1-to-2");
        let path = dir.join("registry.sqlite");
        {
            let db = Db::open(&path).expect("open");
            // Turn it into what a schema-1 build would have written: no
            // `issued` table, and the version that says so.
            db.with(|connection| {
                connection.execute("DROP TABLE issued", [])?;
                connection.pragma_update(None, "user_version", 1)?;
                Ok(())
            })
            .expect("downgrade");
        }
        {
            let db = Db::open(&path).expect("migrate forward");
            assert_eq!(version(&db), SCHEMA_VERSION);
            let count: i64 = db
                .with(|connection| {
                    connection.query_row(
                        "SELECT COUNT(*) FROM sqlite_master
                         WHERE type = 'table' AND name = 'issued'",
                        [],
                        |row| row.get(0),
                    )
                })
                .expect("issued");
            assert_eq!(count, 1, "the migration did not create `issued`");
        }
    }

    #[test]
    fn a_file_database_persists_and_reopens_without_remigrating() {
        let dir = test_dir("persist");
        let path = dir.join("registry.sqlite");
        {
            let db = Db::open(&path).expect("open");
            db.with(|connection| {
                connection.execute("INSERT INTO meta (key, value) VALUES ('hello', X'01')", [])?;
                Ok(())
            })
            .expect("write");
        }
        {
            let db = Db::open(&path).expect("reopen");
            assert_eq!(version(&db), SCHEMA_VERSION);
            let rows: i64 = db
                .with(|connection| {
                    connection.query_row(
                        "SELECT COUNT(*) FROM meta WHERE key = 'hello'",
                        [],
                        |row| row.get(0),
                    )
                })
                .expect("read");
            assert_eq!(rows, 1, "the row written before the reopen is gone");
        }
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let dir = test_dir("newer");
        let path = dir.join("registry.sqlite");
        {
            let connection = Connection::open(&path).expect("raw open");
            connection
                .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .expect("stamp");
        }
        let error = Db::open(&path).expect_err("a newer database must be refused");
        assert!(
            matches!(error, DbError::TooNew { found } if found == SCHEMA_VERSION + 1),
            "{error}"
        );
    }

    #[test]
    fn a_poisoned_connection_is_recovered_with_its_transaction_rolled_back() {
        let db = std::sync::Arc::new(Db::in_memory().expect("open"));
        let poisoned = std::sync::Arc::clone(&db);
        let _ = std::thread::spawn(move || {
            let _ = poisoned.with(|connection| -> rusqlite::Result<()> {
                let transaction = write_transaction(connection)?;
                transaction.execute("INSERT INTO meta (key, value) VALUES ('half', X'01')", [])?;
                panic!("deliberate, to poison the mutex mid-transaction");
            });
        })
        .join();
        assert!(db.connection.is_poisoned());

        // The next request is answered, and sees no trace of the half-written
        // transaction the panic unwound through.
        let rows: i64 = db
            .with(|connection| {
                connection.query_row("SELECT COUNT(*) FROM meta WHERE key = 'half'", [], |row| {
                    row.get(0)
                })
            })
            .expect("a poisoned connection is recovered, not refused");
        assert_eq!(rows, 0);
        assert!(!db.connection.is_poisoned());
    }

    #[test]
    fn a_write_transaction_takes_the_write_lock_at_begin() {
        let dir = test_dir("immediate");
        let path = dir.join("registry.sqlite");
        drop(Db::open(&path).expect("create"));
        let mut first = Connection::open(&path).expect("first");
        let second = Connection::open(&path).expect("second");
        second.busy_timeout(Duration::ZERO).expect("no wait");

        // Held before any statement: a deferred transaction would hold nothing
        // yet, and the other connection's write would slip in between this
        // transaction's read and its write.
        let transaction = write_transaction(&mut first).expect("begin");
        let refused = second.execute("INSERT INTO meta (key, value) VALUES ('other', X'01')", []);
        assert!(
            matches!(
                refused,
                Err(rusqlite::Error::SqliteFailure(ref error, _))
                    if error.code == rusqlite::ErrorCode::DatabaseBusy
            ),
            "{refused:?}"
        );
        transaction.commit().expect("commit");
        second
            .execute("INSERT INTO meta (key, value) VALUES ('other', X'01')", [])
            .expect("free once the first commits");
    }

    #[test]
    fn a_schema_two_database_gains_the_revoked_holders_table() {
        let dir = test_dir("migrate-2-to-3");
        let path = dir.join("registry.sqlite");
        {
            let db = Db::open(&path).expect("open");
            db.with(|connection| {
                connection.execute("DROP TABLE revoked_holders", [])?;
                connection.pragma_update(None, "user_version", 2)?;
                Ok(())
            })
            .expect("downgrade");
        }
        let db = Db::open(&path).expect("migrate forward");
        assert_eq!(version(&db), SCHEMA_VERSION);
        let count: i64 = db
            .with(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'revoked_holders'",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("revoked_holders");
        assert_eq!(count, 1);
    }
}
