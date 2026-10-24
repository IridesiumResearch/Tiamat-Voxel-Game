// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The registry's signing key, its pre-committed successor, and the chain of
//! rotation records that connects them.
//!
//! The service holds one secret (`REGISTRY_KEY`) and one hash
//! (`REGISTRY_NEXT_KEY_HASH`), and stores the rotations it has published. At
//! startup the published records are **replayed** onto a fresh
//! `RegistryTrust`, so a restarted service keeps the chain rather than pinning
//! whatever key the environment happens to hold that day. If the environment's
//! key is not where the chain ends, the service refuses to start: signing with
//! the wrong key would issue certificates every server already in the field
//! rejects, and finding that out at the first login is worse than at boot.

use ed25519_dalek::{SigningKey, VerifyingKey};
use tiamat_core::identity::keyset::commit_to;
use tiamat_core::identity::registry::{
    Name, NameCertificate, RegistryError, RegistryTrust, RotationRecord,
};
use tiamat_core::identity::{PlayerUuid, public_key_from_bytes};

/// A freshly generated key and the successor it commits to.
///
/// The seeds are secrets. [`Debug`](std::fmt::Debug) redacts them, because the
/// first thing anyone does with an unexpected value is print it.
#[derive(Clone)]
pub struct GeneratedKey {
    /// The 32-byte Ed25519 seed: `REGISTRY_KEY`.
    pub seed: [u8; 32],
    /// The public key the seed produces.
    pub public: VerifyingKey,
    /// The successor's seed. It belongs in a file offline, never in the
    /// environment until the day it becomes the current key.
    pub successor_seed: [u8; 32],
    /// The successor's public key.
    pub successor_public: VerifyingKey,
    /// `BLAKE3` of `successor_public`: the value for
    /// `REGISTRY_NEXT_KEY_HASH`.
    pub next_commitment: [u8; 32],
}

/// A successor key made for a current key the operator already holds.
///
/// This is what a rotation needs: `admin key new` is only for the first key,
/// because it also makes a fresh current key. A rotation keeps the successor
/// that was made ninety days ago, makes its own successor with this, and
/// publishes the handover.
#[derive(Clone)]
pub struct GeneratedSuccessor {
    /// The successor's seed, for the offline file.
    pub seed: [u8; 32],
    /// The successor's public key.
    pub public: VerifyingKey,
    /// `BLAKE3` of `public`: the value for `REGISTRY_NEXT_KEY_HASH`.
    pub commitment: [u8; 32],
}

impl std::fmt::Debug for GeneratedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedKey")
            .field("seed", &"<redacted>")
            .field("public", &self.public)
            .field("successor_seed", &"<redacted>")
            .field("successor_public", &self.successor_public)
            .field("next_commitment", &self.next_commitment)
            .finish()
    }
}

impl std::fmt::Debug for GeneratedSuccessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedSuccessor")
            .field("seed", &"<redacted>")
            .field("public", &self.public)
            .field("commitment", &self.commitment)
            .finish()
    }
}

/// Makes a registry key, a successor, and the commitment between them.
///
/// # Errors
///
/// [`KeyError::Entropy`] if the system randomness source fails. There is no
/// fallback: a predictable registry key signs names for anyone.
pub fn generate() -> Result<GeneratedKey, KeyError> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|_| KeyError::Entropy)?;
    let signing = SigningKey::from_bytes(&seed);
    let successor = generate_successor()?;
    Ok(GeneratedKey {
        seed,
        public: signing.verifying_key(),
        successor_seed: successor.seed,
        successor_public: successor.public,
        next_commitment: successor.commitment,
    })
}

/// Makes a successor for a current key: a seed, its public key, and the
/// commitment the current key should make to it.
///
/// # Errors
///
/// [`KeyError::Entropy`] if the system randomness source fails.
pub fn generate_successor() -> Result<GeneratedSuccessor, KeyError> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|_| KeyError::Entropy)?;
    let signing = SigningKey::from_bytes(&seed);
    let public = signing.verifying_key();
    Ok(GeneratedSuccessor {
        seed,
        public,
        commitment: commit_to(&public),
    })
}

/// The service's signing key after replaying every published rotation.
pub struct RegistryKeys {
    signing: SigningKey,
    next: Option<[u8; 32]>,
    rotations: Vec<RotationRecord>,
    trust: RegistryTrust,
}

impl RegistryKeys {
    /// Replays `rotations` onto the key in the environment.
    ///
    /// The chain is rebuilt from its **first** record, not from the environment:
    /// after a rotation the environment holds the new key, and starting the
    /// trust set there would make the first record's retired key a stranger.
    /// The first record names the retired key and the successor it handed over
    /// to, which is exactly enough to reconstruct what the old key had
    /// committed to. Every later record must link, and the last successor must
    /// be the environment's key — otherwise this service would sign with a key
    /// that is not the one servers were told to trust.
    ///
    /// # Errors
    ///
    /// [`KeyError::RotationRejected`] when a published record does not verify
    /// against the chain, [`KeyError::NotCurrent`] when the environment's key is
    /// not where the chain ends, and [`KeyError::NextMismatch`] when
    /// `REGISTRY_NEXT_KEY_HASH` disagrees with the last record's commitment.
    pub fn open(
        seed: [u8; 32],
        next: Option<[u8; 32]>,
        rotations: Vec<RotationRecord>,
    ) -> Result<Self, KeyError> {
        let signing = SigningKey::from_bytes(&seed);
        let mut trust = match rotations.first() {
            None => RegistryTrust::new(signing.verifying_key(), next),
            Some(first) => {
                let retired =
                    public_key_from_bytes(&first.retired).map_err(|_| KeyError::BadRotation {
                        index: 0,
                        source: RegistryError::BadPublicKey,
                    })?;
                let successor =
                    public_key_from_bytes(&first.successor).map_err(|_| KeyError::BadRotation {
                        index: 0,
                        source: RegistryError::BadPublicKey,
                    })?;
                RegistryTrust::new(retired, Some(commit_to(&successor)))
            }
        };
        for (index, record) in rotations.iter().enumerate() {
            trust
                .accept_rotation(record)
                .map_err(|source| KeyError::RotationRejected { index, source })?;
        }
        if trust.current() != &signing.verifying_key() {
            return Err(KeyError::NotCurrent {
                count: rotations.len(),
            });
        }
        if trust.next_commitment() != next {
            return Err(KeyError::NextMismatch);
        }
        Ok(Self {
            signing,
            next,
            rotations,
            trust,
        })
    }

    /// The public half of the key this service signs with.
    #[must_use]
    pub fn public_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// `BLAKE3` of the committed successor, if there is one.
    #[must_use]
    pub fn next_commitment(&self) -> Option<[u8; 32]> {
        self.next
    }

    /// The rotations this service has published, oldest first.
    #[must_use]
    pub fn rotations(&self) -> &[RotationRecord] {
        &self.rotations
    }

    /// The trust set, for verifying a record the service did not issue.
    #[must_use]
    pub fn trust(&self) -> &RegistryTrust {
        &self.trust
    }

    /// Issues a certificate for a binding the service has just written.
    #[must_use]
    pub fn issue(
        &self,
        name: &Name,
        uuid: PlayerUuid,
        issued: i64,
        serial: u64,
    ) -> NameCertificate {
        NameCertificate::issue(&self.signing, name, uuid, issued, serial)
    }

    /// Signs a rotation record to `successor`, if the commitment already held
    /// allows it.
    ///
    /// The secret never leaves this type: `admin rotate` asks for a record and
    /// gets one, rather than holding the signing key and re-implementing the
    /// commitment rule.
    ///
    /// # Errors
    ///
    /// [`KeyError::NoCommittedSuccessor`] when the current key committed to
    /// nothing, and [`KeyError::SuccessorMismatch`] when `successor` does not
    /// hash to the commitment.
    pub fn sign_rotation(
        &self,
        successor: &VerifyingKey,
        next_key_hash: Option<[u8; 32]>,
        at: i64,
    ) -> Result<RotationRecord, KeyError> {
        let Some(committed) = self.next else {
            return Err(KeyError::NoCommittedSuccessor);
        };
        if commit_to(successor) != committed {
            return Err(KeyError::SuccessorMismatch);
        }
        let mut record = RotationRecord {
            version: tiamat_core::identity::registry::ROTATION_VERSION,
            retired: self.public_key().to_bytes(),
            successor: successor.to_bytes(),
            next_key_hash,
            rotated_at: at,
            signature: tiamat_core::identity::registry::Signature64([0; 64]),
        };
        record.sign(&self.signing);
        Ok(record)
    }

    /// Signs a revocation list with the current key.
    #[must_use]
    pub fn sign_revocations(
        &self,
        issued: i64,
        entries: Vec<tiamat_core::identity::registry::Revocation>,
    ) -> tiamat_core::identity::registry::RevocationList {
        let mut list = tiamat_core::identity::registry::RevocationList {
            version: tiamat_core::identity::registry::REVOCATION_VERSION,
            registry_key: self.public_key().to_bytes(),
            issued,
            entries,
            signature: tiamat_core::identity::registry::Signature64([0; 64]),
        };
        list.sign(&self.signing);
        list
    }
}

/// Loads the key chain from the database, replaying the published rotations.
///
/// Shared by the service and the operator commands so a rotation written by one
/// is read by the other exactly the same way.
///
/// # Errors
///
/// [`KeyError::Database`] when the rows cannot be read, and every error
/// [`RegistryKeys::open`] returns.
pub fn from_database(
    db: &crate::db::Db,
    seed: [u8; 32],
    next: Option<[u8; 32]>,
) -> Result<RegistryKeys, KeyError> {
    let stored: Vec<Vec<u8>> = db
        .with(|connection| {
            let mut statement = connection.prepare("SELECT record FROM rotations ORDER BY seq")?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect()
        })
        .map_err(|error| KeyError::Database(error.to_string()))?;
    let mut rotations = Vec::with_capacity(stored.len());
    for (index, bytes) in stored.iter().enumerate() {
        rotations.push(
            RotationRecord::from_bytes(bytes)
                .map_err(|source| KeyError::BadRotation { index, source })?,
        );
    }
    RegistryKeys::open(seed, next, rotations)
}

impl std::fmt::Debug for RegistryKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryKeys")
            .field("public", &self.public_key())
            .field("next", &self.next)
            .field("rotations", &self.rotations.len())
            .finish()
    }
}

/// What went wrong with the registry's key material.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// The system randomness source failed.
    #[error("could not obtain secure randomness for a registry key")]
    Entropy,

    /// A stored rotation record does not parse.
    #[error("the stored rotation record at position {index} is unreadable")]
    BadRotation {
        /// Its position in the chain.
        index: usize,
        /// Why.
        #[source]
        source: RegistryError,
    },

    /// A stored rotation record does not prove what it claims.
    #[error("the stored rotation record at position {index} does not verify")]
    RotationRejected {
        /// Its position in the chain.
        index: usize,
        /// What the trust set objected to.
        #[source]
        source: RegistryError,
    },

    /// The environment's key is behind the published chain.
    #[error(
        "REGISTRY_KEY is not the current registry key after {count} rotation(s); the environment \
         has not been updated to the successor"
    )]
    NotCurrent {
        /// How many rotations were replayed.
        count: usize,
    },

    /// The environment's commitment disagrees with the chain.
    #[error(
        "REGISTRY_NEXT_KEY_HASH does not match the commitment the published rotations carry; \
         set the newest commitment, not the one from before the rotation"
    )]
    NextMismatch,

    /// The database could not be read.
    #[error("the registry database could not be read: {0}")]
    Database(String),

    /// There is no successor to rotate to.
    #[error(
        "this registry key has no committed successor, so nothing can be rotated to; \
         set REGISTRY_NEXT_KEY_HASH from `admin key next` first"
    )]
    NoCommittedSuccessor,

    /// The proposed successor is not the committed one.
    #[error("the proposed successor does not hash to REGISTRY_NEXT_KEY_HASH")]
    SuccessorMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamat_core::identity::registry::{
        CERTIFICATE_VERSION, ROTATION_VERSION, RegistryError, Signature64, verify_certificate,
    };

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn rotation(
        retiring: &SigningKey,
        successor: &SigningKey,
        next: Option<[u8; 32]>,
        at: i64,
    ) -> RotationRecord {
        let mut record = RotationRecord {
            version: ROTATION_VERSION,
            retired: [0; 32],
            successor: successor.verifying_key().to_bytes(),
            next_key_hash: next,
            rotated_at: at,
            signature: Signature64([0; 64]),
        };
        record.sign(retiring);
        record
    }

    #[test]
    fn a_key_with_no_rotations_is_its_own_current_key() {
        let keys = RegistryKeys::open([1; 32], None, Vec::new()).expect("open");
        assert_eq!(keys.public_key(), key(1).verifying_key());
        assert!(keys.rotations().is_empty());
        assert_eq!(keys.next_commitment(), None);
    }

    #[test]
    fn a_rotation_chain_replays_onto_the_environment_key() {
        let first = key(1);
        let second = key(2);
        let third = key(3);
        let committed = commit_to(&third.verifying_key());
        let record = rotation(&first, &second, Some(committed), 100);

        let keys = RegistryKeys::open([2; 32], Some(committed), vec![record]).expect("open");
        assert_eq!(keys.public_key(), second.verifying_key());
        assert_eq!(keys.next_commitment(), Some(committed));
        assert_eq!(keys.rotations().len(), 1);
        assert_eq!(keys.trust().current(), &second.verifying_key());
    }

    #[test]
    fn the_environment_key_must_be_where_the_chain_ends() {
        let first = key(1);
        let second = key(2);
        let record = rotation(&first, &second, None, 100);
        let error = RegistryKeys::open([1; 32], None, vec![record]).expect_err("old key");
        assert!(
            matches!(error, KeyError::NotCurrent { count: 1 }),
            "{error}"
        );
    }

    #[test]
    fn the_committed_successor_must_match_the_chain() {
        let first = key(1);
        let second = key(2);
        let record = rotation(&first, &second, None, 100);
        let error = RegistryKeys::open([2; 32], Some([0xAA; 32]), vec![record])
            .expect_err("wrong commitment");
        assert!(matches!(error, KeyError::NextMismatch), "{error}");
    }

    #[test]
    fn a_later_rotation_to_an_uncommitted_key_is_refused() {
        let first = key(1);
        let second = key(2);
        let third = key(3);
        let fourth = key(4);
        // The first rotation commits the second key to `third`; the second
        // record then proposes `fourth`, which nothing committed to.
        let first_record = rotation(
            &first,
            &second,
            Some(commit_to(&third.verifying_key())),
            100,
        );
        let second_record = rotation(&second, &fourth, None, 200);
        let error = RegistryKeys::open([4; 32], None, vec![first_record, second_record])
            .expect_err("the second record was not committed to");
        assert!(
            matches!(
                error,
                KeyError::RotationRejected {
                    index: 1,
                    source: RegistryError::SuccessorMismatch
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_generated_key_commits_to_the_successor_it_made() {
        let generated = generate().expect("entropy");
        let current = SigningKey::from_bytes(&generated.seed).verifying_key();
        let successor = SigningKey::from_bytes(&generated.successor_seed).verifying_key();
        assert_eq!(current, generated.public);
        assert_eq!(successor, generated.successor_public);
        // The environment's `REGISTRY_NEXT_KEY_HASH` is this, and it is the
        // hash of the successor whose seed the operator now stores offline.
        assert_eq!(commit_to(&successor), generated.next_commitment);
        assert_ne!(generated.seed, generated.successor_seed);
    }

    #[test]
    fn a_rotation_successor_is_a_key_of_its_own() {
        let successor = generate_successor().expect("entropy");
        assert_eq!(
            SigningKey::from_bytes(&successor.seed).verifying_key(),
            successor.public
        );
        assert_eq!(commit_to(&successor.public), successor.commitment);
    }

    #[test]
    fn an_issued_certificate_verifies_offline() {
        let keys = RegistryKeys::open([7; 32], None, Vec::new()).expect("open");
        let name = Name::from_parts("alice", "Alice").expect("name");
        let uuid = PlayerUuid::from_root_key(&key(9).verifying_key());
        let certificate = keys.issue(&name, uuid, 1_800_000_000, 1);
        assert_eq!(certificate.version, CERTIFICATE_VERSION);
        assert_eq!(certificate.uuid, *uuid.as_bytes());

        let wire = certificate.to_bytes().expect("encode");
        let verified = verify_certificate(&wire, &uuid, 1_800_000_000, keys.trust(), None)
            .expect("a certificate the registry just signed");
        assert_eq!(verified.canonical(), "alice");
        assert_eq!(verified.display(), "Alice");
    }
}
