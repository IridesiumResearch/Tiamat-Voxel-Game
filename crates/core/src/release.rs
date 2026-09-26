// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a release says about itself, and how a client decides to believe it.
//!
//! [`docs/distribution.md`](../../../docs/distribution.md) is authoritative;
//! §2 is the trust model and §3 the manifest. This module is the part every
//! side shares: the tool that signs a manifest, the client that checks one
//! before downloading, and the updater that checks it again before applying.
//! **One implementation, because two would eventually disagree about what is
//! authentic**, and the one that disagreed quietly would be the one that
//! installed something.
//!
//! # What this module trusts
//!
//! Nothing it was given. The bytes arrive from a web server over a connection
//! whose certificate says nothing about who wrote the file behind it, so:
//!
//! - the signature is checked against a key compiled into the binary, before
//!   the JSON is looked at as anything but bytes;
//! - every size is capped before anything is allocated (charter rule 14);
//! - a version older than the one installed is refused, so a captured old
//!   manifest cannot be replayed to walk somebody backwards;
//! - the key may only change to one whose hash the PREVIOUS manifest already
//!   committed to, which is charter rule 13's pre-rotation used for code
//!   rather than for players.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The largest manifest that will be read at all.
///
/// A manifest is a few hundred bytes with four artefacts in it. Sixty-four
/// kilobytes is room for a long note and a dozen mirrors, and it is checked
/// before the bytes are parsed rather than after.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// The most artefacts one release may describe.
pub const MAX_ARTIFACTS: usize = 16;

/// The most places one artefact may be fetched from.
pub const MAX_URLS: usize = 4;

/// The largest artefact that will be downloaded, in bytes.
///
/// The client is tens of megabytes with its mods; a gigabyte is far past any
/// honest release and is the cap that stops a signed-but-wrong manifest from
/// asking a tester to download the sea.
pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

/// The schema this build understands.
pub const SCHEMA: u32 = 1;

/// What went wrong with a manifest.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReleaseError {
    /// Bigger than [`MAX_MANIFEST_BYTES`] before it was even parsed.
    #[error("the manifest is {len} bytes, over the {MAX_MANIFEST_BYTES}-byte limit")]
    TooLarge {
        /// How big it was.
        len: usize,
    },
    /// Not JSON, or not the shape of a manifest.
    #[error("the manifest could not be read: {0}")]
    Malformed(String),
    /// The signature is not this key's signature over these bytes.
    #[error("the manifest is not signed by a key this build trusts")]
    NotSigned,
    /// The key that signed it is neither the trusted one nor the successor
    /// the last manifest committed to.
    #[error("the manifest was signed by an unexpected key")]
    UnexpectedKey,
    /// A schema from the future, which this build cannot promise to read
    /// correctly.
    #[error("the manifest is schema {found}, and this build reads {SCHEMA}")]
    Schema {
        /// What it claimed.
        found: u32,
    },
    /// Something in it broke a documented limit.
    #[error("{what}")]
    Unusable {
        /// What is wrong, in terms the reader can act on.
        what: String,
    },
    /// Older than what is installed.
    #[error("the manifest offers {offered}, which is not newer than the installed {installed}")]
    NotNewer {
        /// What the manifest offers.
        offered: String,
        /// What is installed now.
        installed: String,
    },
}

/// One downloadable build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The Rust target triple it was built for.
    pub target: String,
    /// The archive's file name.
    pub name: String,
    /// Its size in bytes, checked before the download starts and again after.
    pub size: u64,
    /// Its BLAKE3 hash, as 64 hex characters.
    ///
    /// **The only thing that decides whether the bytes are right.** The URLs
    /// below are untrusted; this is not.
    pub hash: String,
    /// Where to fetch it, tried in order.
    pub urls: Vec<String>,
}

impl Artifact {
    /// Whether these bytes are the artefact this describes.
    #[must_use]
    pub fn matches(&self, bytes: &[u8]) -> bool {
        if bytes.len() as u64 != self.size {
            return false;
        }
        let actual = blake3::hash(bytes);
        // Constant-time is not the concern — a hash comparison here decides
        // whether to unpack, and an attacker who can grind it can simply send
        // the real file. Case is, because a hex string may arrive either way.
        actual.to_hex().as_str().eq_ignore_ascii_case(&self.hash)
    }
}

/// What a release says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The schema version. [`SCHEMA`] is what this build reads.
    pub schema: u32,
    /// Which channel this is the head of: `test`, `stable`.
    pub channel: String,
    /// The release's version, as `major.minor.patch`.
    pub version: String,
    /// The commit the binaries were built from.
    ///
    /// What makes the GPL's corresponding-source offer exact rather than
    /// approximate — see `docs/distribution.md` §7.
    pub commit: Option<String>,
    /// When it was released, RFC 3339.
    pub released: String,
    /// The protocol version these binaries speak.
    ///
    /// A client refuses a server whose protocol differs, so this is what lets
    /// a download page say which server build a version talks to.
    pub protocol: u32,
    /// The public key that signed this manifest, as 64 hex characters.
    pub key: String,
    /// The BLAKE3 hash of the key that will sign the NEXT manifest.
    ///
    /// Pre-rotation, from charter rule 13: a stolen signing key cannot hand
    /// itself a successor, because the successor was committed to before the
    /// theft. `None` means the key is not rotating, and a client that has one
    /// stored keeps it.
    pub next_key: Option<String>,
    /// Where a human can read about this release.
    pub notes: Option<String>,
    /// The builds, one per target.
    pub artifacts: Vec<Artifact>,
}

impl Manifest {
    /// Reads a manifest from bytes, checking every cap before trusting it.
    ///
    /// **This does not check the signature** — see [`Manifest::verify`], which
    /// is what a client calls. This exists for the tool that writes manifests
    /// and for reading one back in a test.
    ///
    /// # Errors
    ///
    /// [`ReleaseError`] when it is too large, not JSON, a schema this build
    /// does not read, or breaks one of the documented limits.
    pub fn parse(bytes: &[u8]) -> Result<Self, ReleaseError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ReleaseError::TooLarge { len: bytes.len() });
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|err| ReleaseError::Malformed(err.to_string()))?;
        if manifest.schema != SCHEMA {
            return Err(ReleaseError::Schema {
                found: manifest.schema,
            });
        }
        manifest.check()?;
        Ok(manifest)
    }

    /// Reads a manifest whose signature is this key's, or refuses it.
    ///
    /// `trusted` is the key compiled into the binary. `expected_next` is the
    /// hash the last accepted manifest committed to, if there was one: a key
    /// that is not `trusted` is accepted only when it hashes to that.
    ///
    /// # Errors
    ///
    /// [`ReleaseError::NotSigned`] when the signature is not this key's over
    /// these exact bytes, [`ReleaseError::UnexpectedKey`] when the key is one
    /// nobody committed to, and anything [`Manifest::parse`] returns.
    pub fn verify(
        bytes: &[u8],
        signature: &[u8],
        trusted: &VerifyingKey,
        expected_next: Option<&str>,
    ) -> Result<Self, ReleaseError> {
        // **Parsed first, but nothing is acted on until the signature holds.**
        // The key that signed it is named inside it, so there is no way to
        // check the signature without reading the bytes — and every cap in
        // `parse` runs before this does, so the reading is bounded.
        let manifest = Self::parse(bytes)?;

        let key = hex_to_array::<32>(&manifest.key)
            .ok_or_else(|| ReleaseError::Malformed("the key is not 32 hex bytes".to_owned()))?;
        let key = VerifyingKey::from_bytes(&key).map_err(|_| ReleaseError::UnexpectedKey)?;

        // The trusted key, or the successor the previous manifest named — and
        // nothing else. A manifest that simply asserts a new key is a manifest
        // an attacker with a signing key of their own could write.
        if key != *trusted {
            let committed = expected_next.is_some_and(|hash| {
                blake3::hash(key.as_bytes())
                    .to_hex()
                    .as_str()
                    .eq_ignore_ascii_case(hash)
            });
            if !committed {
                return Err(ReleaseError::UnexpectedKey);
            }
        }

        let signature: [u8; 64] = signature.try_into().map_err(|_| ReleaseError::NotSigned)?;
        key.verify_strict(bytes, &Signature::from_bytes(&signature))
            .map_err(|_| ReleaseError::NotSigned)?;

        Ok(manifest)
    }

    /// The build for one target triple, if this release has one.
    #[must_use]
    pub fn artifact(&self, target: &str) -> Option<&Artifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.target == target)
    }

    /// Whether this release is newer than what is installed.
    ///
    /// # Errors
    ///
    /// [`ReleaseError::NotNewer`] when it is the same or older, which is the
    /// downgrade rule: a captured manifest must not walk anybody backwards.
    pub fn newer_than(&self, installed: &str) -> Result<(), ReleaseError> {
        if compare(&self.version, installed) == std::cmp::Ordering::Greater {
            return Ok(());
        }
        Err(ReleaseError::NotNewer {
            offered: self.version.clone(),
            installed: installed.to_owned(),
        })
    }

    /// Every documented limit, checked.
    fn check(&self) -> Result<(), ReleaseError> {
        let unusable = |what: String| Err(ReleaseError::Unusable { what });
        if self.artifacts.len() > MAX_ARTIFACTS {
            return unusable(format!(
                "the manifest lists {} builds, over the limit of {MAX_ARTIFACTS}",
                self.artifacts.len()
            ));
        }
        if parts(&self.version).is_none() {
            return unusable(format!("`{}` is not a version", self.version));
        }
        if hex_to_array::<32>(&self.key).is_none() {
            return unusable("the signing key is not 32 hex bytes".to_owned());
        }
        if let Some(next) = &self.next_key
            && hex_to_array::<32>(next).is_none()
        {
            return unusable("the next key's hash is not 32 hex bytes".to_owned());
        }
        for artifact in &self.artifacts {
            if artifact.size > MAX_ARTIFACT_BYTES {
                return unusable(format!(
                    "`{}` is {} bytes, over the {MAX_ARTIFACT_BYTES}-byte limit",
                    artifact.name, artifact.size
                ));
            }
            if artifact.size == 0 {
                return unusable(format!("`{}` is empty", artifact.name));
            }
            if hex_to_array::<32>(&artifact.hash).is_none() {
                return unusable(format!("`{}` has no BLAKE3 hash", artifact.name));
            }
            if artifact.urls.is_empty() || artifact.urls.len() > MAX_URLS {
                return unusable(format!(
                    "`{}` names {} places to fetch it from; 1 to {MAX_URLS} are allowed",
                    artifact.name,
                    artifact.urls.len()
                ));
            }
            // **Only https.** A manifest is signed and an artefact is hashed,
            // so plain http would not let anybody substitute the bytes — but
            // it would let anybody watch, and a tester's download is not
            // anybody else's business.
            if let Some(bad) = artifact
                .urls
                .iter()
                .find(|url| !url.starts_with("https://"))
            {
                return unusable(format!("`{bad}` is not an https URL"));
            }
            // A name that is a path is a name that escapes the directory it is
            // written into.
            if artifact.name.is_empty()
                || artifact.name.contains('/')
                || artifact.name.contains('\\')
                || artifact.name.contains("..")
            {
                return unusable(format!("`{}` is not a plain file name", artifact.name));
            }
        }
        Ok(())
    }
}

/// The bytes a signature is made over.
///
/// **The file exactly as it is served**, not a re-serialisation of it: a
/// signature over "what we would have written" is a signature over a document
/// nobody downloaded, and the two differ the first time a field is reordered
/// or a float is printed differently.
#[must_use]
pub fn signing_bytes(manifest_file: &[u8]) -> &[u8] {
    manifest_file
}

/// Compares two `major.minor.patch` versions.
///
/// Written here rather than pulled from a crate: it is fifteen lines, the
/// versions this compares are our own, and anything it cannot read compares as
/// less — which fails towards "do not update" rather than towards "update to
/// something I could not read".
#[must_use]
pub fn compare(left: &str, right: &str) -> std::cmp::Ordering {
    match (parts(left), parts(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// `1.2.3` as `(1, 2, 3)`, or nothing.
fn parts(version: &str) -> Option<(u32, u32, u32)> {
    let mut fields = version.split('.');
    let major = fields.next()?.parse().ok()?;
    let minor = fields.next()?.parse().ok()?;
    let patch = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Hex to exactly `N` bytes, or nothing.
///
/// **Hex digits only.** `from_str_radix` takes a leading `+`, so `+f` read as
/// a byte and a hash of thirty-two `+f`s was a "hash" no download could ever
/// match — found by the manifest fuzz target on its first run. A client would
/// have fetched the whole archive to refuse it.
fn hex_to_array<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != N * 2 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0_u8; N];
    for (index, byte) in out.iter_mut().enumerate() {
        let pair = hex.get(index * 2..index * 2 + 2)?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// Bytes as lower-case hex, for writing a manifest.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        // Writing to a String cannot fail; the result is discarded for that
        // reason rather than unwrapped (charter's no-unwrap rule).
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests;
