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
//! previous version intact, and every migration is written so that running it
//! a second time changes nothing.

use std::net::IpAddr;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};

/// The schema this build reads and writes.
pub const SCHEMA_VERSION: i64 = 5;

/// One migration's work.
enum Step {
    /// A statement batch, idempotent by `IF NOT EXISTS`.
    Sql(&'static str),
    /// Work SQL alone cannot make idempotent (adding a column, rewriting rows),
    /// in Rust that checks before it changes.
    Code(fn(&Connection) -> rusqlite::Result<()>),
}

/// Every migration, oldest first, with the version it brings the database to.
///
/// A migration is applied in order when the stored version is below its
/// target. The list is append-only: changing an entry that has shipped would
/// migrate a database that already ran it into a different shape.
const MIGRATIONS: &[(i64, Step)] = &[
    (1, Step::Sql(SCHEMA_V1)),
    (2, Step::Sql(SCHEMA_V2)),
    (3, Step::Sql(SCHEMA_V3)),
    (4, Step::Code(schema_v4)),
    (5, Step::Sql(SCHEMA_V5)),
];

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

/// Schema 4: retention (§8, review B9).
///
/// - `audit.ip`: the client address moves out of `why`, where claims and the
///   mail flows wrote it as free text, into a nullable column the retention
///   sweep clears seven days after the row. The admin views that group claims
///   by network read it from here.
/// - `recoveries.ended_at`: when a recovery was moved or cancelled, so its
///   record can be deleted ninety days after it ended.
/// - The mail quota's per-address counters are re-keyed from the raw address to
///   its hash (`mailquota::address_scope`), so no address sits in `limits`.
///
/// Idempotent by construction: each column is added only when the table lacks
/// it, an address is moved only from a `why` that still parses as one, and a
/// counter is re-keyed only while its key still holds an `@`.
fn schema_v4(connection: &Connection) -> rusqlite::Result<()> {
    add_column(connection, "audit", "ip", "TEXT")?;
    add_column(connection, "recoveries", "ended_at", "INTEGER")?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS audit_ip ON audit(when_at) WHERE ip IS NOT NULL;",
    )?;

    let rows: Vec<(i64, String, String)> = {
        let mut statement =
            connection.prepare("SELECT id, what, why FROM audit WHERE ip IS NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, what, why) in rows {
        let Ok(ip) = why.trim().parse::<IpAddr>() else {
            continue;
        };
        // The word the current code writes for the same row.
        let reason = if what.starts_with("claim ") {
            "claim"
        } else {
            "player request"
        };
        connection.execute(
            "UPDATE audit SET ip = ?1, why = ?2 WHERE id = ?3",
            rusqlite::params![ip.to_string(), reason, id],
        )?;
    }

    connection.execute(
        "UPDATE recoveries SET ended_at = notice_until
         WHERE state != 'pending' AND ended_at IS NULL",
        [],
    )?;

    let counters: Vec<(String, i64, i64)> = {
        let mut statement = connection.prepare(
            "SELECT scope, window, count FROM limits
             WHERE scope LIKE 'mail:%' AND instr(scope, '@') > 0",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (scope, window, count) in counters {
        let address = scope.trim_start_matches("mail:");
        connection.execute(
            "INSERT INTO limits (scope, window, count) VALUES (?1, ?2, ?3)
             ON CONFLICT(scope, window) DO UPDATE SET count = count + excluded.count",
            rusqlite::params![crate::mailquota::address_scope(address), window, count],
        )?;
        connection.execute(
            "DELETE FROM limits WHERE scope = ?1 AND window = ?2",
            rusqlite::params![scope, window],
        )?;
    }
    Ok(())
}

/// Schema 5: what the registry knows about an identity's keys (review A3).
///
/// The registry keeps no key store — a request proves its key from the root —
/// so without a memory a root key its owner rotated away from would keep its
/// authority for ever. `crate::retired` is the module that reads and writes
/// these, and says why each is shaped as it is.
///
/// - `retired_keys`: every key a presented rotation retired, per identity,
///   with the successor it handed over to (a second, different rotation of the
///   same key is refused), `retired_at` the signed time of the first request
///   that carried the rotation, and `learned_at` this service's clock then.
/// - `root_commitments`: the first successor commitment an identity's root
///   declared; another is refused, which is what keeps a thief who holds the
///   root from rotating it to a key of their own.
///
/// Kept as long as the identity has any other row the service keeps, and
/// swept with its last (`retention`). Tables, so `IF NOT EXISTS` makes the
/// migration idempotent.
const SCHEMA_V5: &str = r"
CREATE TABLE IF NOT EXISTS retired_keys (
    uuid       BLOB NOT NULL,
    key        BLOB NOT NULL,
    successor  BLOB NOT NULL,
    retired_at INTEGER NOT NULL,
    learned_at INTEGER NOT NULL,
    PRIMARY KEY (uuid, key)
);

CREATE TABLE IF NOT EXISTS root_commitments (
    uuid       BLOB PRIMARY KEY,
    commitment BLOB NOT NULL,
    learned_at INTEGER NOT NULL
);
";

/// Adds a column unless the table already has it.
///
/// `ALTER TABLE ADD COLUMN` is the one schema change SQLite cannot spell `IF
/// NOT EXISTS`, so the check is `pragma_table_info`. The names are this
/// module's own constants, never input.
fn add_column(
    connection: &Connection,
    table: &str,
    column: &str,
    kind: &str,
) -> rusqlite::Result<()> {
    let present: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |row| row.get(0),
    )?;
    if present == 0 {
        connection.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind};"))?;
    }
    Ok(())
}

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
        for (target, step) in MIGRATIONS {
            if version < *target {
                match step {
                    Step::Sql(sql) => transaction.execute_batch(sql)?,
                    Step::Code(work) => work(&transaction)?,
                }
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
            "retired_keys",
            "revoked_holders",
            "root_commitments",
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

    /// A database exactly as a schema-3 build left it, with the rows schema 4
    /// moves: claim and mail-flow addresses in `why`, a raw address in a mail
    /// counter, and a cancelled recovery with no end time.
    fn schema_three_database(path: &Path) {
        let connection = Connection::open(path).expect("raw open");
        connection
            .execute_batch(&format!("{SCHEMA_V1}{SCHEMA_V2}{SCHEMA_V3}"))
            .expect("schema 3");
        connection
            .pragma_update(None, "user_version", 3)
            .expect("stamp");
        connection
            .execute_batch(
                "INSERT INTO audit (who, what, when_at, why) VALUES
                     ('aa', 'claim alice', 100, '203.0.113.5'),
                     ('bb', 'ask to recover alice', 101, '2001:db8::1'),
                     ('cc', 'renew alice', 102, 'renewal'),
                     ('operator', 'operator revoke --subnet 203.0.113.0/24', 103, 'burst');
                 INSERT INTO limits (scope, window, count) VALUES
                     ('mail:alice@example.com', 3600, 2),
                     ('mailnet:net:203.0.113.0/24', 3600, 2),
                     ('ip:203.0.113.5', 0, 1);
                 INSERT INTO recoveries
                     (canonical, from_uuid, to_uuid, confirmed_at, notice_until, state)
                 VALUES ('alice', X'01', X'02', 10, 20, 'cancelled'),
                        ('bob', X'01', X'02', 10, 30, 'pending');",
            )
            .expect("legacy rows");
    }

    #[test]
    fn a_schema_three_database_moves_its_addresses_out_of_the_free_text() {
        let dir = test_dir("migrate-3-to-4");
        let path = dir.join("registry.sqlite");
        schema_three_database(&path);
        let db = Db::open(&path).expect("migrate forward");
        assert_eq!(version(&db), SCHEMA_VERSION);

        let audit: Vec<(String, Option<String>)> = db
            .with(|connection| {
                let mut statement = connection.prepare("SELECT why, ip FROM audit ORDER BY id")?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect()
            })
            .expect("audit");
        assert_eq!(
            audit,
            vec![
                ("claim".to_owned(), Some("203.0.113.5".to_owned())),
                ("player request".to_owned(), Some("2001:db8::1".to_owned())),
                ("renewal".to_owned(), None),
                ("burst".to_owned(), None),
            ]
        );

        let scopes: Vec<String> = db
            .with(|connection| {
                let mut statement =
                    connection.prepare("SELECT scope FROM limits ORDER BY scope")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            })
            .expect("limits");
        assert!(
            scopes.iter().all(|scope| !scope.contains('@')),
            "{scopes:?}"
        );
        assert!(scopes.contains(&crate::mailquota::address_scope("alice@example.com")));
        assert!(scopes.contains(&"mailnet:net:203.0.113.0/24".to_owned()));

        let ended: Vec<Option<i64>> = db
            .with(|connection| {
                let mut statement =
                    connection.prepare("SELECT ended_at FROM recoveries ORDER BY canonical")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            })
            .expect("recoveries");
        assert_eq!(ended, vec![Some(20), None]);
    }

    #[test]
    fn schema_four_runs_twice_to_the_same_result() {
        let dir = test_dir("migrate-4-twice");
        let path = dir.join("registry.sqlite");
        schema_three_database(&path);
        let snapshot = |db: &Db| -> Vec<String> {
            db.with(|connection| {
                let mut statement = connection.prepare(
                    "SELECT 'audit|' || id || '|' || why || '|' || COALESCE(ip, '-') FROM audit
                     UNION ALL SELECT 'limits|' || scope || '|' || window || '|' || count FROM limits
                     UNION ALL SELECT 'recoveries|' || canonical || '|' || COALESCE(ended_at, '-')
                         FROM recoveries
                     ORDER BY 1",
                )?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            })
            .expect("snapshot")
        };
        let db = Db::open(&path).expect("migrate");
        let once = snapshot(&db);
        db.with(|connection| schema_v4(connection)).expect("again");
        assert_eq!(snapshot(&db), once);
    }

    #[test]
    fn a_schema_four_database_gains_the_memory_of_retired_keys_and_keeps_its_rows() {
        let dir = test_dir("migrate-4-to-5");
        let path = dir.join("registry.sqlite");
        {
            let connection = Connection::open(&path).expect("raw open");
            connection
                .execute_batch(&format!("{SCHEMA_V1}{SCHEMA_V2}{SCHEMA_V3}"))
                .expect("schema 3");
            schema_v4(&connection).expect("schema 4");
            connection
                .pragma_update(None, "user_version", 4)
                .expect("stamp");
            connection
                .execute(
                    "INSERT INTO names (canonical, display, uuid, claimed, renewed)
                     VALUES ('alice', 'Alice', X'01', 1, 1)",
                    [],
                )
                .expect("a schema-4 row");
        }
        let db = Db::open(&path).expect("migrate forward");
        assert_eq!(version(&db), SCHEMA_VERSION);
        let (tables, names): (i64, i64) = db
            .with(|connection| {
                let tables = connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
                     AND name IN ('retired_keys', 'root_commitments')",
                    [],
                    |row| row.get(0),
                )?;
                let names =
                    connection.query_row("SELECT COUNT(*) FROM names", [], |row| row.get(0))?;
                Ok((tables, names))
            })
            .expect("tables");
        assert_eq!(tables, 2);
        assert_eq!(names, 1, "the migration lost a row");

        // Run twice, it changes nothing: the rows written after the first run
        // are still there and no table is recreated.
        db.with(|connection| {
            connection.execute(
                "INSERT INTO retired_keys (uuid, key, successor, retired_at, learned_at)
                 VALUES (X'01', X'02', X'03', 10, 11)",
                [],
            )?;
            connection.execute_batch(SCHEMA_V5)?;
            Ok(())
        })
        .expect("again");
        let kept: i64 = db
            .with(|connection| {
                connection.query_row("SELECT COUNT(*) FROM retired_keys", [], |row| row.get(0))
            })
            .expect("count");
        assert_eq!(kept, 1);
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
