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
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::Connection;

/// The schema this build reads and writes.
pub const SCHEMA_VERSION: i64 = 1;

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
    /// [`DbError::Sql`] if the closure's SQL failed, [`DbError::Poisoned`] if a
    /// previous holder panicked.
    pub fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DbError> {
        let mut connection = self.connection.lock().map_err(|_| DbError::Poisoned)?;
        work(&mut connection).map_err(DbError::Sql)
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
    if version < 1 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(SCHEMA_V1)?;
        transaction.pragma_update(None, "user_version", 1)?;
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

    /// A previous holder of the connection panicked, so its state is unknown.
    #[error("a previous request panicked while holding the database connection")]
    Poisoned,
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
            "limits",
            "meta",
            "names",
            "recoveries",
            "reserved",
            "revocations",
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
    fn a_poisoned_connection_is_an_error_not_a_panic() {
        let db = std::sync::Arc::new(Db::in_memory().expect("open"));
        let poisoned = std::sync::Arc::clone(&db);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.connection.lock().expect("lock");
            panic!("deliberate, to poison the mutex");
        })
        .join();
        assert!(matches!(db.with(|_| Ok(())), Err(DbError::Poisoned)));
    }
}
