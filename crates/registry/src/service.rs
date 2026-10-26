// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The state every request reads: database, keys, mail, clock.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::clock::Clock;
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::keys::{KeyError, RegistryKeys};
use crate::mail::Mailer;

/// A started registry: everything a handler needs, and nothing it does not.
pub struct Service {
    config: Config,
    db: Db,
    keys: RegistryKeys,
    mailer: Arc<dyn Mailer>,
    clock: Arc<dyn Clock>,
    /// When the limit and revocation sweep last ran, Unix seconds.
    last_sweep: AtomicI64,
}

impl Service {
    /// Opens the database, replays the key chain, and is ready to answer.
    ///
    /// # Errors
    ///
    /// [`StartError`] when the data directory cannot be created, the database
    /// cannot be opened or migrated, or the environment's key does not match
    /// the chain the database holds.
    pub fn open(
        config: Config,
        mailer: Arc<dyn Mailer>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StartError> {
        let db = match &config.data_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir).map_err(|source| StartError::DataDir {
                    path: dir.display().to_string(),
                    source,
                })?;
                Db::open(dir.join("registry.sqlite"))?
            }
            // In memory, which is what a test wants and what no deployment may
            // ask for: `Config::from_env` has no default data directory.
            None => Db::in_memory()?,
        };

        // The design says the service sweeps its own limit rows. It happens
        // here, and then at most hourly from `advance`, so a service that runs
        // for months does not keep every counter it ever wrote.
        let started = clock.now();
        sweep(&db, started)?;

        // Today's backup, if it is missing — the day's first request would
        // otherwise be the only trigger, and a redeploy right after midnight
        // should not lose a day of protection.
        if let Some(dir) = &config.data_dir
            && let Err(error) = crate::backup::backup_once(&db, dir, clock.now())
        {
            tracing::warn!(error = %error, "the startup backup failed");
        }

        // The host's reserved list is registry policy, not engine content: the
        // default mods' ids arrive through `REGISTRY_RESERVED` and land in the
        // table the claim path already consults.
        if !config.reserved.is_empty() {
            let reserved = &config.reserved;
            db.with(|connection| {
                for name in reserved {
                    connection.execute(
                        "INSERT OR IGNORE INTO reserved (canonical) VALUES (?1)",
                        [name.as_str()],
                    )?;
                }
                Ok(())
            })?;
            tracing::info!(count = config.reserved.len(), "reserved names seeded");
        }

        let keys = crate::keys::from_database(&db, config.key_seed, config.next_key_hash)?;
        Ok(Self {
            config,
            db,
            keys,
            mailer,
            clock,
            last_sweep: AtomicI64::new(started),
        })
    }

    /// The configuration this service was started with.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The database.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The signing key and the chain behind it.
    #[must_use]
    pub fn keys(&self) -> &RegistryKeys {
        &self.keys
    }

    /// Where the two mails go.
    #[must_use]
    pub fn mailer(&self) -> &dyn Mailer {
        self.mailer.as_ref()
    }

    /// The current time, from the injected clock.
    #[must_use]
    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    /// Completes any recovery notice whose fourteen days are up, and once an
    /// hour sweeps spent limit rows and revocations that protect nobody.
    ///
    /// Called by the handlers that read or change a name, so a notice ends
    /// even if nobody visits the site at the minute it expires.
    ///
    /// # Errors
    ///
    /// [`DbError`] when the database fails.
    pub fn advance(&self) -> Result<usize, DbError> {
        let now = self.now();
        let moved = self
            .db
            .with(|connection| crate::recover::advance(connection, now))?;
        let last = self.last_sweep.load(Ordering::Relaxed);
        // `compare_exchange` so two requests arriving together sweep once.
        if now.saturating_sub(last) >= crate::limits::SWEEP_INTERVAL
            && self
                .last_sweep
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            sweep(&self.db, now)?;
        }
        // The day's backup rides the same call: a failed backup is a log line,
        // never a failed request — the request is about a name, not a file.
        if let Some(dir) = &self.config.data_dir
            && let Err(error) = crate::backup::backup_once(&self.db, dir, now)
        {
            tracing::warn!(error = %error, "the daily backup failed");
        }
        Ok(moved)
    }
}

/// Deletes spent limit rows and revocations whose certificates have expired.
fn sweep(db: &Db, now: i64) -> Result<(), DbError> {
    db.with(|connection| {
        crate::limits::sweep(connection, now)?;
        crate::names::prune_revocations(connection, now)?;
        Ok(())
    })
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service")
            .field("config", &self.config)
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

/// Why a registry could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The data directory could not be made.
    #[error("could not create the registry data directory at `{path}`")]
    DataDir {
        /// Where it was asked for.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },

    /// The database could not be opened or migrated.
    #[error(transparent)]
    Db(#[from] DbError),

    /// The environment's key does not match the chain the database holds.
    #[error(transparent)]
    Keys(#[from] KeyError),

    /// The port could not be bound.
    #[error("could not listen on {addr}")]
    Listen {
        /// The address asked for.
        addr: SocketAddr,
        /// What the operating system said.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The listener thread could not be spawned.
    #[error("could not start the registry's HTTP thread")]
    Thread(#[source] std::io::Error),

    /// The listener reported an address that is not a TCP socket address.
    ///
    /// Unreachable through [`crate::serve`], which binds TCP; a variant rather
    /// than a panic because `tiny_http`'s address type has a Unix-socket case
    /// on Unix targets and core's rule is that nothing here panics on it.
    #[error("the registry's listener is not bound to a TCP address")]
    NotTcp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::keys::generate;
    use crate::mail::SinkMailer;
    use ed25519_dalek::SigningKey;
    use tiamat_core::identity::keyset::commit_to;
    use tiamat_core::identity::registry::{ROTATION_VERSION, RotationRecord, Signature64};

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/registry-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    fn config(
        data_dir: Option<std::path::PathBuf>,
        seed: [u8; 32],
        next: Option<[u8; 32]>,
    ) -> Config {
        Config {
            data_dir,
            bind: "127.0.0.1:0".parse().expect("bind"),
            key_seed: seed,
            next_key_hash: next,
            base_url: "http://127.0.0.1".to_owned(),
            mail_from: None,
            smtp_url: None,
            trust_forwarded_for: false,
            work_bits: 4,
            reserved: Vec::new(),
        }
    }

    #[test]
    fn the_hosts_reserved_names_reach_the_table() {
        let mut settings = config(None, [7; 32], None);
        settings.reserved = vec!["alice".to_owned()];
        let service = Service::open(
            settings,
            Arc::new(SinkMailer::default()),
            Arc::new(SystemClock),
        )
        .expect("start");
        let reserved: i64 = service
            .db()
            .with(|connection| {
                connection.query_row("SELECT COUNT(*) FROM reserved", [], |row| row.get(0))
            })
            .expect("count");
        assert_eq!(reserved, 1);
    }

    #[test]
    fn a_service_starts_in_memory() {
        let service = Service::open(
            config(None, [7; 32], None),
            Arc::new(SinkMailer::default()),
            Arc::new(SystemClock),
        )
        .expect("start");
        assert_eq!(
            service.keys().public_key(),
            SigningKey::from_bytes(&[7; 32]).verifying_key()
        );
        assert!(service.now() > 1_700_000_000);
    }

    #[test]
    fn the_clock_it_is_given_is_the_clock_it_reads() {
        #[derive(Debug)]
        struct Fixed(i64);
        impl Clock for Fixed {
            fn now(&self) -> i64 {
                self.0
            }
        }
        let service = Service::open(
            config(None, [7; 32], None),
            Arc::new(SinkMailer::default()),
            Arc::new(Fixed(1_000)),
        )
        .expect("start");
        assert_eq!(service.now(), 1_000);
    }

    #[test]
    fn advance_sweeps_spent_limits_at_most_hourly() {
        #[derive(Debug)]
        struct Moving(std::sync::atomic::AtomicI64);
        impl Clock for Moving {
            fn now(&self) -> i64 {
                self.0.load(Ordering::Relaxed)
            }
        }
        const START: i64 = 1_800_000_000;
        let clock = Arc::new(Moving(AtomicI64::new(START)));
        let service = Service::open(
            config(None, [7; 32], None),
            Arc::new(SinkMailer::default()),
            clock.clone(),
        )
        .expect("start");
        // A day's address count from a month ago, written after startup.
        let stale =
            crate::limits::window_start(START - 30 * crate::limits::DAY, crate::limits::DAY);
        let rows = || {
            service
                .db()
                .with(|connection| {
                    connection.query_row("SELECT COUNT(*) FROM limits", [], |row| {
                        row.get::<_, i64>(0)
                    })
                })
                .expect("count")
        };
        service
            .db()
            .with(|connection| {
                crate::limits::take(connection, "ip:203.0.113.1", stale, 2)?;
                Ok(())
            })
            .expect("seed");

        // Not an hour since the startup sweep: kept.
        clock.0.store(START + 60, Ordering::Relaxed);
        service.advance().expect("advance");
        assert_eq!(rows(), 1);

        // An hour on, the next request sweeps it.
        clock
            .0
            .store(START + crate::limits::SWEEP_INTERVAL, Ordering::Relaxed);
        service.advance().expect("advance");
        assert_eq!(rows(), 0);
    }

    #[test]
    fn the_published_rotations_are_replayed_from_the_database() {
        let dir = test_dir("rotation-replay");
        let old = SigningKey::from_bytes(&[1; 32]);
        let new = SigningKey::from_bytes(&[2; 32]);
        let next = SigningKey::from_bytes(&[3; 32]);
        let committed = commit_to(&next.verifying_key());

        let mut record = RotationRecord {
            version: ROTATION_VERSION,
            retired: [0; 32],
            successor: new.verifying_key().to_bytes(),
            next_key_hash: Some(committed),
            rotated_at: 1_000,
            signature: Signature64([0; 64]),
        };
        record.sign(&old);
        let encoded = postcard::to_allocvec(&record).expect("encode");

        {
            let db = Db::open(dir.join("registry.sqlite")).expect("db");
            db.with(|connection| {
                connection.execute(
                    "INSERT INTO rotations (seq, record) VALUES (0, ?1)",
                    [encoded],
                )?;
                Ok(())
            })
            .expect("write rotation");
        }

        let service = Service::open(
            config(Some(dir), [2; 32], Some(committed)),
            Arc::new(SinkMailer::default()),
            Arc::new(SystemClock),
        )
        .expect("start");
        assert_eq!(service.keys().public_key(), new.verifying_key());
        assert_eq!(service.keys().rotations().len(), 1);
    }

    #[test]
    fn the_environment_must_agree_with_the_plain_environment_config() {
        // The in-memory service has no rotations, so a key and no commitment is
        // the whole story; this pins that a generated key is usable as one.
        let generated = generate().expect("entropy");
        let service = Service::open(
            config(None, generated.seed, Some(generated.next_commitment)),
            Arc::new(SinkMailer::default()),
            Arc::new(SystemClock),
        )
        .expect("start");
        assert_eq!(service.keys().public_key(), generated.public);
        assert_eq!(
            service.keys().next_commitment(),
            Some(generated.next_commitment)
        );
    }
}
