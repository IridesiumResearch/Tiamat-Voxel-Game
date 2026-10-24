// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The state every request reads: database, keys, mail, clock.

use std::net::SocketAddr;
use std::sync::Arc;

use tiamat_core::identity::registry::RotationRecord;

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

        let stored: Vec<Vec<u8>> = db.with(|connection| {
            let mut statement = connection.prepare("SELECT record FROM rotations ORDER BY seq")?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect()
        })?;
        let mut rotations = Vec::with_capacity(stored.len());
        for (index, bytes) in stored.iter().enumerate() {
            rotations.push(
                RotationRecord::from_bytes(bytes)
                    .map_err(|source| KeyError::BadRotation { index, source })?,
            );
        }

        // The design says the service sweeps its own limit rows. Startup is the
        // one moment the database is not answering requests, so it happens here
        // rather than on a timer nobody watches.
        db.with(|connection| crate::limits::sweep(connection, clock.now()))?;

        let keys = RegistryKeys::open(config.key_seed, config.next_key_hash, rotations)?;
        Ok(Self {
            config,
            db,
            keys,
            mailer,
            clock,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::keys::generate;
    use crate::mail::SinkMailer;
    use ed25519_dalek::SigningKey;
    use tiamat_core::identity::keyset::commit_to;
    use tiamat_core::identity::registry::{ROTATION_VERSION, Signature64};

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
        }
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
