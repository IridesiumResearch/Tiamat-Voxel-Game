// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The account this machine's identity holds: a name certificate, and the
//! registry rotation records a server may need to trust it.
//!
//! Stored beside the identity key (`account.bin`), because the account is about
//! that identity and nothing else: a phrase restored on another machine brings
//! the identity, and its first renewal brings the account with it; a wiped disk
//! loses both together. A certificate for a different identity is refused at
//! load and could never be sent.
//!
//! # Bytes on disk are checked like bytes on the wire
//!
//! The file is the player's own, but it is still bytes that a crash can
//! truncate, a backup can mangle, or another program can edit. Every cap the
//! protocol checks is checked here **before allocation** ([`MAX_CARRIED_ROTATIONS`],
//! [`MAX_ROTATION_BYTES`], [`MAX_CERTIFICATE_BYTES`]), the file itself is read
//! under [`MAX_ACCOUNT_BYTES`], and both kinds of record are parsed by
//! `core::identity::registry` before they are kept. A file that does not hold up
//! is an error the caller reports and ignores; it is never a panic and never a
//! reason the game cannot start.
//!
//! # Why the rotations are here
//!
//! Design §5 lets a server pinned before a registry rotation keep verifying
//! certificates signed by the successor, provided the client brings the signed
//! records that prove the handover. The client fetches them with each renewal
//! from `GET /v1/keys` and sends its most recent four on every challenge answer;
//! this is where they wait between renewals.

use std::path::{Path, PathBuf};

use tiamat_core::identity::Identity;
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::{
    MAX_CARRIED_ROTATIONS, MAX_CERTIFICATE_BYTES, MAX_ROTATION_BYTES, NameCertificate,
    RegistryError, RotationRecord,
};

/// The account file's name, beside `identity.key`.
pub const ACCOUNT_FILE: &str = "account.bin";

/// The file format this client writes and reads.
///
/// Bumping it is a migration, not a format detail: a file from a newer client
/// is refused rather than guessed at.
const VERSION: u8 = 1;

/// The largest account file read, checked before reading.
///
/// The contents are capped tightly; this is the envelope that also bounds a
/// file somebody has filled with noise.
pub const MAX_ACCOUNT_BYTES: u64 = 8 * 1024;

/// Something went wrong loading or saving the account.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// The file could not be read or written.
    #[error("the account file at `{path}`")]
    Io {
        /// Which file.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The file was read but is not an account this client can use.
    #[error("the account file at `{path}` is unusable: {detail}")]
    Invalid {
        /// Which file.
        path: PathBuf,
        /// What was wrong with it, in words the log can carry.
        detail: String,
    },
}

/// The certificate and rotation records as they travel on a challenge answer.
///
/// Built only by [`Account::present`], so every instance satisfies the
/// protocol's caps: one certificate of at most [`MAX_CERTIFICATE_BYTES`], and at
/// most [`MAX_CARRIED_ROTATIONS`] records of at most [`MAX_ROTATION_BYTES`] each.
/// The fields are private for exactly that reason — a caller cannot assemble one
/// out of unchecked bytes and hand it to the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presented {
    certificate: Option<Vec<u8>>,
    rotations: Vec<Vec<u8>>,
}

impl Presented {
    /// No account: the ordinary join of a player who has not claimed a name.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            certificate: None,
            rotations: Vec::new(),
        }
    }

    /// The certificate's wire bytes, if any.
    #[must_use]
    pub fn certificate(&self) -> Option<&[u8]> {
        self.certificate.as_deref()
    }

    /// The rotation records, oldest first.
    #[must_use]
    pub fn rotations(&self) -> &[Vec<u8>] {
        &self.rotations
    }

    /// Whether there is nothing to present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.certificate.is_none() && self.rotations.is_empty()
    }
}

/// What this machine's identity holds at the registry.
///
/// [`Default`] is an identity that has never claimed a name; that is a
/// legitimate account, not a missing one, and the join it makes is the one
/// players made before the registry existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    /// The registry's signed statement, if this identity holds a name.
    certificate: Option<NameCertificate>,
    /// The registry's rotation records, oldest first, newest last.
    ///
    /// Capped at [`MAX_CARRIED_ROTATIONS`]; setting more keeps the newest, which
    /// are the ones a server behind a rotation lacks.
    rotations: Vec<RotationRecord>,
}

impl Account {
    /// Reads the account beside an identity, or an empty one when there is no
    /// file yet.
    ///
    /// # Errors
    ///
    /// [`AccountError`] if the file exists but cannot be read, is larger than
    /// [`MAX_ACCOUNT_BYTES`], is from another version, contains a certificate or
    /// record the registry could not have issued, or names an identity other
    /// than `identity`'s.
    pub fn load(path: &Path, identity: &Identity) -> Result<Self, AccountError> {
        let size = match std::fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(AccountError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if size > MAX_ACCOUNT_BYTES {
            return Err(AccountError::Invalid {
                path: path.to_path_buf(),
                detail: format!("{size} bytes is over the {MAX_ACCOUNT_BYTES}-byte cap"),
            });
        }
        let bytes = std::fs::read(path).map_err(|source| AccountError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let wire: Wire = postcard::from_bytes(&bytes).map_err(|err| AccountError::Invalid {
            path: path.to_path_buf(),
            detail: format!("it does not decode: {err}"),
        })?;
        if wire.version != VERSION {
            return Err(AccountError::Invalid {
                path: path.to_path_buf(),
                detail: format!(
                    "format version {} is not this client's {VERSION}",
                    wire.version
                ),
            });
        }
        if wire.rotations.len() > MAX_CARRIED_ROTATIONS {
            return Err(AccountError::Invalid {
                path: path.to_path_buf(),
                detail: format!(
                    "{} rotation records is over the cap of {MAX_CARRIED_ROTATIONS}",
                    wire.rotations.len()
                ),
            });
        }

        let expected = identity.uuid_as_root();
        let certificate = match wire.certificate {
            None => None,
            Some(bytes) => {
                if bytes.len() > MAX_CERTIFICATE_BYTES {
                    return Err(AccountError::Invalid {
                        path: path.to_path_buf(),
                        detail: format!(
                            "the certificate is {} bytes, over the {MAX_CERTIFICATE_BYTES}-byte cap",
                            bytes.len()
                        ),
                    });
                }
                let certificate =
                    NameCertificate::from_bytes(&bytes).map_err(|err| AccountError::Invalid {
                        path: path.to_path_buf(),
                        detail: format!("the certificate does not parse: {err}"),
                    })?;
                if certificate.uuid != *expected.as_bytes() {
                    return Err(AccountError::Invalid {
                        path: path.to_path_buf(),
                        detail: "the certificate names a different identity".to_owned(),
                    });
                }
                Some(certificate)
            }
        };

        let mut rotations = Vec::with_capacity(wire.rotations.len());
        for bytes in wire.rotations {
            if bytes.len() > MAX_ROTATION_BYTES {
                return Err(AccountError::Invalid {
                    path: path.to_path_buf(),
                    detail: format!(
                        "a rotation record is {} bytes, over the {MAX_ROTATION_BYTES}-byte cap",
                        bytes.len()
                    ),
                });
            }
            let record =
                RotationRecord::from_bytes(&bytes).map_err(|err| AccountError::Invalid {
                    path: path.to_path_buf(),
                    detail: format!("a rotation record does not parse: {err}"),
                })?;
            rotations.push(record);
        }

        Ok(Self {
            certificate,
            rotations,
        })
    }

    /// Writes the account beside the identity.
    ///
    /// Written to a temporary file and then renamed into place, so a crash
    /// *while the bytes are being written* cannot leave a half-written account.
    /// On Windows the rename cannot replace an existing file, so the old one is
    /// removed immediately before it; a crash in that instant loses the file
    /// rather than corrupting it, and the next renewal writes it again.
    ///
    /// # Errors
    ///
    /// [`AccountError::Io`] if the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), AccountError> {
        let wire = Wire {
            version: VERSION,
            certificate: match &self.certificate {
                Some(certificate) => {
                    Some(
                        certificate
                            .to_bytes()
                            .map_err(|err| AccountError::Invalid {
                                path: path.to_path_buf(),
                                detail: format!("the certificate would not encode: {err}"),
                            })?,
                    )
                }
                None => None,
            },
            rotations: self
                .rotations
                .iter()
                .map(RotationRecord::to_bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| AccountError::Invalid {
                    path: path.to_path_buf(),
                    detail: format!("a rotation record would not encode: {err}"),
                })?,
        };
        let bytes = postcard::to_allocvec(&wire).map_err(|err| AccountError::Invalid {
            path: path.to_path_buf(),
            detail: format!("the account would not encode: {err}"),
        })?;
        let tmp = path.with_extension("bin.tmp");
        std::fs::write(&tmp, &bytes).map_err(|source| AccountError::Io {
            path: tmp.clone(),
            source,
        })?;
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        std::fs::rename(&tmp, path).map_err(|source| AccountError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The name this identity holds, or `None`.
    #[must_use]
    pub fn name(&self) -> Option<&NameCertificate> {
        self.certificate.as_ref()
    }

    /// The registry's rotation records, oldest first.
    #[must_use]
    pub fn rotations(&self) -> &[RotationRecord] {
        &self.rotations
    }

    /// The account as a challenge answer carries it.
    ///
    /// # Errors
    ///
    /// [`RegistryError`] if a stored record cannot be re-encoded, which cannot
    /// happen for one that parsed; it is a `Result` because core does not panic
    /// on data.
    pub fn present(&self) -> Result<Presented, RegistryError> {
        Ok(Presented {
            certificate: match &self.certificate {
                Some(certificate) => Some(certificate.to_bytes()?),
                None => None,
            },
            rotations: self
                .rotations
                .iter()
                .map(RotationRecord::to_bytes)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    /// Keeps a certificate this identity has been issued.
    ///
    /// The UUID is checked here, not only by the server: an account file that
    /// could hold somebody else's certificate would be one a client could be
    /// tricked into sending.
    ///
    /// # Errors
    ///
    /// [`RegistryError::WrongIdentity`] if `certificate` names another UUID.
    pub fn set_certificate(
        &mut self,
        identity: &Identity,
        certificate: NameCertificate,
    ) -> Result<(), RegistryError> {
        if certificate.uuid != *identity.uuid_as_root().as_bytes() {
            return Err(RegistryError::WrongIdentity);
        }
        self.certificate = Some(certificate);
        Ok(())
    }

    /// Replaces the rotation records, keeping the newest
    /// [`MAX_CARRIED_ROTATIONS`] when given more.
    pub fn set_rotations(&mut self, mut rotations: Vec<RotationRecord>) {
        if rotations.len() > MAX_CARRIED_ROTATIONS {
            rotations.drain(..rotations.len() - MAX_CARRIED_ROTATIONS);
        }
        self.rotations = rotations;
    }

    /// The UUID this account's certificate names, if any.
    #[must_use]
    pub fn uuid(&self) -> Option<PlayerUuid> {
        self.certificate
            .as_ref()
            .map(|certificate| PlayerUuid::from_bytes(certificate.uuid))
    }
}

/// What goes on disk. Kept separate from [`Account`] so the file's shape is one
/// place and the in-memory invariants another.
#[derive(serde::Serialize, serde::Deserialize)]
struct Wire {
    version: u8,
    certificate: Option<Vec<u8>>,
    rotations: Vec<Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tiamat_core::identity::registry::Name;

    fn identity() -> Identity {
        Identity::generate().expect("an identity")
    }

    fn name(canonical: &str, display: &str) -> Name {
        Name::from_parts(canonical, display).expect("a valid name")
    }

    /// A certificate the test registry issued to `identity`.
    fn issued(identity: &Identity, canonical: &str, display: &str) -> NameCertificate {
        let registry = SigningKey::from_bytes(&[0x71; 32]);
        NameCertificate::issue(
            &registry,
            &name(canonical, display),
            identity.uuid_as_root(),
            1_760_000_000,
            7,
        )
    }

    fn account_with(identity: &Identity, canonical: &str, display: &str) -> Account {
        let mut account = Account::default();
        account
            .set_certificate(identity, issued(identity, canonical, display))
            .expect("its own identity");
        account
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("tiamat-client-account")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn an_account_round_trips_beside_its_identity() {
        let identity = identity();
        let mut account = account_with(&identity, "alice", "Alice");
        let first = RotationRecord {
            version: tiamat_core::identity::registry::ROTATION_VERSION,
            retired: [0x21; 32],
            successor: [0x22; 32],
            rotated_at: 1,
            next_key_hash: None,
            signature: tiamat_core::identity::registry::Signature64([0; 64]),
        };
        account.set_rotations(vec![first.clone()]);
        let path = scratch("round-trip").join(ACCOUNT_FILE);
        account.save(&path).expect("save");

        let loaded = Account::load(&path, &identity).expect("load");
        assert_eq!(loaded, account);
        assert_eq!(
            loaded.present().expect("present").certificate(),
            account.present().expect("present").certificate(),
            "the wire bytes are the certificate's own"
        );
        assert_eq!(loaded.present().expect("present").rotations().len(), 1);
    }

    #[test]
    fn no_account_file_is_an_empty_account_not_a_failure() {
        let identity = identity();
        let path = scratch("absent").join(ACCOUNT_FILE);
        let account = Account::load(&path, &identity).expect("a missing file is not an error");
        assert_eq!(account, Account::default());
        assert!(account.present().expect("present").is_empty());
    }

    #[test]
    fn an_account_for_another_identity_is_refused() {
        let mine = identity();
        let theirs = identity();
        let account = account_with(&theirs, "alice", "Alice");
        let path = scratch("other-identity").join(ACCOUNT_FILE);
        account.save(&path).expect("save");
        // Saving wrote a certificate for `theirs`; `mine` must not adopt it.
        let err = Account::load(&path, &mine).expect_err("another identity's certificate");
        assert!(
            matches!(err, AccountError::Invalid { ref detail, .. } if detail.contains("different identity")),
            "got {err:?}"
        );
    }

    #[test]
    fn a_corrupt_or_oversized_file_is_an_error_not_a_panic() {
        let identity = identity();
        let dir = scratch("corrupt");

        let junk = dir.join("junk.bin");
        std::fs::write(&junk, b"this is not postcard").expect("write");
        assert!(Account::load(&junk, &identity).is_err());

        let huge = dir.join("huge.bin");
        std::fs::write(
            &huge,
            vec![0u8; usize::try_from(MAX_ACCOUNT_BYTES).unwrap() + 1],
        )
        .expect("write");
        assert!(Account::load(&huge, &identity).is_err());

        // A valid envelope with a certificate that is not one.
        let bad = dir.join("bad-cert.bin");
        let wire = Wire {
            version: VERSION,
            certificate: Some(vec![0xAA; 8]),
            rotations: Vec::new(),
        };
        std::fs::write(&bad, postcard::to_allocvec(&wire).expect("encode")).expect("write");
        assert!(Account::load(&bad, &identity).is_err());
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_guessed() {
        let identity = identity();
        let dir = scratch("version");
        let path = dir.join(ACCOUNT_FILE);
        let wire = Wire {
            version: VERSION + 1,
            certificate: None,
            rotations: Vec::new(),
        };
        std::fs::write(&path, postcard::to_allocvec(&wire).expect("encode")).expect("write");
        assert!(
            Account::load(&path, &identity).is_err(),
            "a file this client does not understand must not be read as one it does"
        );
    }

    #[test]
    fn more_records_than_the_wire_allows_keeps_the_newest() {
        let identity = identity();
        let mut account = account_with(&identity, "alice", "Alice");
        let records: Vec<RotationRecord> = (0..6u8)
            .map(|seed| RotationRecord {
                version: tiamat_core::identity::registry::ROTATION_VERSION,
                retired: [seed; 32],
                successor: [seed.wrapping_add(1); 32],
                rotated_at: i64::from(seed),
                next_key_hash: None,
                signature: tiamat_core::identity::registry::Signature64([0; 64]),
            })
            .collect();
        account.set_rotations(records);
        assert_eq!(account.rotations().len(), MAX_CARRIED_ROTATIONS);
        assert_eq!(
            account.rotations()[0].rotated_at,
            2,
            "the oldest were dropped, not the newest"
        );

        let presented = account.present().expect("present");
        assert_eq!(presented.rotations().len(), MAX_CARRIED_ROTATIONS);
    }

    #[test]
    fn a_certificate_for_somebody_else_cannot_be_set() {
        let mine = identity();
        let other = identity();
        let certificate = issued(&other, "alice", "Alice");
        let mut account = Account::default();
        assert_eq!(
            account.set_certificate(&mine, certificate),
            Err(RegistryError::WrongIdentity)
        );
        assert!(account.name().is_none());
    }
}
