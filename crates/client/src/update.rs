// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Checking for a new version, and fetching one.
//!
//! [`docs/distribution.md`](../../../docs/distribution.md) §1: **the client
//! downloads and the launcher applies.** This half has the window, so it is
//! the half that can show a hundred megabytes arriving; by the time the
//! launcher runs, the bytes are on disk and the work is three renames.
//!
//! # What this trusts
//!
//! The same as the launcher, and for the same reasons: the signature is
//! checked against a key compiled in from `release-key.pub`, never against the
//! certificate the web server presented. HTTPS here buys privacy, not
//! authenticity — a mirror is allowed to be somebody else's machine.
//!
//! Everything is capped before it is allocated (charter rule 14): the manifest
//! at [`tiamat_core::release::MAX_MANIFEST_BYTES`], the archive at the size
//! the signed manifest declares, and the whole exchange behind timeouts so a
//! server that accepts a connection and then says nothing cannot hold the
//! front screen for ever.
//!
//! # What it does NOT do
//!
//! Apply anything. Nothing here writes outside `staged/`, and the update that
//! lands there is applied by the launcher at the next start, after it has
//! checked the signature and the hash over again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tiamat_core::release::Manifest;

/// The public key releases are signed with, from `release-key.pub`.
///
/// `None` in a build that found no key, which then never offers an update —
/// the same rule the launcher follows.
const RELEASE_KEY: Option<&str> = option_env!("TIAMAT_RELEASE_KEY");

/// How long any single request may take before it is given up on.
///
/// **Generous for a download and short for a check.** The manifest is a few
/// hundred bytes and should arrive at once; an archive is a hundred megabytes
/// on whatever connection a tester has.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What the front screen is showing about updates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    /// Nothing asked yet, or this build does not look for updates.
    #[default]
    Idle,
    /// Asking.
    Checking,
    /// Asked, and this is the newest there is.
    UpToDate,
    /// There is a newer one, and how big it is.
    Available {
        /// The version on offer.
        version: String,
        /// The download's size in bytes.
        size: u64,
    },
    /// Fetching it.
    Downloading {
        /// Bytes so far.
        done: u64,
        /// Bytes in total, from the signed manifest.
        total: u64,
    },
    /// Fetched, verified and staged. The launcher applies it at the next start.
    Staged {
        /// The version that will be applied.
        version: String,
    },
    /// Something went wrong, in words a player can act on.
    Failed {
        /// What happened.
        what: String,
    },
}

/// Why an update could not be staged.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// This build has no manifest URL or no release key.
    #[error("this build does not check for updates")]
    NotConfigured,
    /// The download or the connection failed.
    #[error("{0}")]
    Fetch(String),
    /// The manifest was refused — signature, version, caps.
    #[error("{0}")]
    Refused(String),
    /// Writing the staged files failed.
    #[error("cannot write the update to {path}: {source}")]
    Io {
        /// Where.
        path: String,
        /// Why.
        source: std::io::Error,
    },
}

/// The update check, as the app holds it.
pub struct Updates {
    status: Arc<Mutex<Status>>,
    /// Whether a check has been asked for at all this session.
    ///
    /// **Here rather than on the screen that draws it**: the front screen
    /// draws sixty times a second and should not have to remember, and a
    /// second check would be a second round trip for the same answer.
    asked: std::sync::atomic::AtomicBool,
    /// Where `staged/` goes: the install root, which is the directory ABOVE
    /// the one this binary is in. `None` in a working copy, which is what
    /// stops a developer's build from staging updates into `target/`.
    install: Option<PathBuf>,
}

impl Default for Updates {
    fn default() -> Self {
        Self::new()
    }
}

impl Updates {
    /// Reads where this build is installed, and whether it checks at all.
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::Idle)),
            asked: std::sync::atomic::AtomicBool::new(false),
            install: install_root(),
        }
    }

    /// What to show.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    /// Whether this build looks for updates at all.
    #[must_use]
    pub fn configured(&self) -> bool {
        tiamat_core::build::MANIFEST_URL.is_some()
            && RELEASE_KEY.is_some()
            && self.install.is_some()
    }

    /// Asks the manifest whether there is a newer version, on a worker.
    ///
    /// **Never on the frame loop.** A web server that accepts a connection and
    /// then says nothing would otherwise freeze the front screen until the
    /// timeout, which is the kind of hang players report as a crash.
    pub fn check(&self) {
        if !self.configured() || self.asked.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let status = Arc::clone(&self.status);
        set(&status, Status::Checking);
        std::thread::spawn(move || {
            let outcome = fetch_manifest().and_then(|(bytes, signature)| {
                let manifest = verify(&bytes, &signature)?;
                Ok(manifest)
            });
            match outcome {
                Ok(manifest) => {
                    let installed = tiamat_core::build::VERSION;
                    if manifest.newer_than(installed).is_err() {
                        set(&status, Status::UpToDate);
                        return;
                    }
                    let size = manifest
                        .artifact(TARGET)
                        .map_or(0, |artifact| artifact.size);
                    if size == 0 {
                        set(
                            &status,
                            Status::Failed {
                                what: format!(
                                    "version {} has no build for this machine",
                                    manifest.version
                                ),
                            },
                        );
                        return;
                    }
                    set(
                        &status,
                        Status::Available {
                            version: manifest.version,
                            size,
                        },
                    );
                }
                Err(err) => set(
                    &status,
                    Status::Failed {
                        what: err.to_string(),
                    },
                ),
            }
        });
    }

    /// Fetches the update and stages it, on a worker, reporting progress.
    pub fn download(&self) {
        let Some(install) = self.install.clone() else {
            return;
        };
        if !matches!(self.status(), Status::Available { .. }) {
            return;
        }
        let status = Arc::clone(&self.status);
        std::thread::spawn(move || {
            let outcome = (|| -> Result<String, UpdateError> {
                let (bytes, signature) = fetch_manifest()?;
                let manifest = verify(&bytes, &signature)?;
                let artifact = manifest
                    .artifact(TARGET)
                    .ok_or_else(|| UpdateError::Refused("no build for this machine".to_owned()))?;
                set(
                    &status,
                    Status::Downloading {
                        done: 0,
                        total: artifact.size,
                    },
                );
                let archive = fetch_artifact(artifact, |done| {
                    set(
                        &status,
                        Status::Downloading {
                            done,
                            total: artifact.size,
                        },
                    );
                })?;
                stage(&install, &bytes, &signature, &archive)?;
                Ok(manifest.version)
            })();
            match outcome {
                Ok(version) => set(&status, Status::Staged { version }),
                Err(err) => set(
                    &status,
                    Status::Failed {
                        what: err.to_string(),
                    },
                ),
            }
        });
    }
}

/// Writes a verified update into `<install>/staged/`.
///
/// **Checked here as well as by the launcher**, because a client that wrote
/// whatever it downloaded and left the checking to somebody else would be a
/// client that could be talked into filling a disk. The archive's hash is
/// checked against the signed manifest before a byte reaches `staged/`.
///
/// # Errors
///
/// [`UpdateError::Refused`] when the manifest or the archive does not hold up,
/// and [`UpdateError::Io`] when the files cannot be written.
pub fn stage(
    install: &Path,
    manifest_bytes: &[u8],
    signature: &[u8],
    archive: &[u8],
) -> Result<(), UpdateError> {
    let manifest = verify(manifest_bytes, signature)?;
    let artifact = manifest
        .artifact(TARGET)
        .ok_or_else(|| UpdateError::Refused("no build for this machine".to_owned()))?;
    if !artifact.matches(archive) {
        return Err(UpdateError::Refused(
            "the download is not the file the signed manifest describes".to_owned(),
        ));
    }

    let staged = install.join("staged");
    // Written fresh every time: a half-finished download from last week must
    // not be mistaken for this one.
    let _ = std::fs::remove_dir_all(&staged);
    let io = |path: &Path| {
        let path = path.display().to_string();
        move |source| UpdateError::Io {
            path: path.clone(),
            source,
        }
    };
    std::fs::create_dir_all(&staged).map_err(io(&staged))?;
    // **The archive first and the manifest last.** The launcher decides there
    // is something to apply by the presence of `update.json`, so writing it
    // last means an interrupted download leaves nothing that looks ready.
    let archive_path = staged.join("update.archive");
    std::fs::write(&archive_path, archive).map_err(io(&archive_path))?;
    let signature_path = staged.join("update.sig");
    std::fs::write(&signature_path, signature).map_err(io(&signature_path))?;
    let manifest_path = staged.join("update.json");
    std::fs::write(&manifest_path, manifest_bytes).map_err(io(&manifest_path))?;
    Ok(())
}

/// Checks a manifest's signature against this build's key.
fn verify(bytes: &[u8], signature: &[u8]) -> Result<Manifest, UpdateError> {
    let key = RELEASE_KEY
        .and_then(decode_key)
        .ok_or(UpdateError::NotConfigured)?;
    // The successor a previous manifest committed to, if this install has
    // been told one. Read from the launcher's own state file so both halves
    // agree about which key comes next.
    let expected_next = install_root()
        .and_then(|root| std::fs::read(root.join("install.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|state| {
            state
                .get("next_key")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    Manifest::verify(bytes, signature, &key, expected_next.as_deref())
        .map_err(|err| UpdateError::Refused(err.to_string()))
}

/// The manifest and its detached signature.
fn fetch_manifest() -> Result<(Vec<u8>, Vec<u8>), UpdateError> {
    let url = tiamat_core::build::MANIFEST_URL.ok_or(UpdateError::NotConfigured)?;
    let manifest = get(url, tiamat_core::release::MAX_MANIFEST_BYTES as u64, |_| {})?;
    // The signature sits beside the manifest, which is one fewer thing to
    // configure and one fewer thing to get wrong.
    let signature = get(&format!("{url}.sig"), 64, |_| {})?;
    Ok((manifest, signature))
}

/// The archive, from the first URL that yields it.
fn fetch_artifact(
    artifact: &tiamat_core::release::Artifact,
    mut progress: impl FnMut(u64),
) -> Result<Vec<u8>, UpdateError> {
    let mut last = None;
    for url in &artifact.urls {
        match get(url, artifact.size, &mut progress) {
            Ok(bytes) => return Ok(bytes),
            // **Every URL is untrusted, so a failure is just a failure.** The
            // next one is tried, and the hash decides whichever answers.
            Err(err) => last = Some(err),
        }
    }
    Err(last
        .unwrap_or_else(|| UpdateError::Fetch("the release names nowhere to fetch it from".into())))
}

/// One HTTPS GET, capped at `limit` bytes.
fn get(url: &str, limit: u64, mut progress: impl FnMut(u64)) -> Result<Vec<u8>, UpdateError> {
    use std::io::Read as _;

    if !url.starts_with("https://") {
        return Err(UpdateError::Refused(format!("`{url}` is not an https URL")));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(CHECK_TIMEOUT))
        .user_agent(format!("Tiamat/{}", tiamat_core::build::VERSION))
        .build()
        .into();
    let response = agent
        .get(url)
        .call()
        .map_err(|err| UpdateError::Fetch(format!("could not reach {url}: {err}")))?;

    // **Read to the cap, not to what the server said.** A `Content-Length` is
    // a claim; this reads at most one byte more than allowed and refuses if
    // there is one, so a server promising 40 MB and sending 40 GB is a
    // refusal rather than a memory exhaustion.
    let mut body = response.into_body().into_reader().take(limit + 1);
    let mut bytes = Vec::with_capacity(usize::try_from(limit.min(8 * 1024 * 1024)).unwrap_or(0));
    // On the heap: a 64 KiB frame is more than some platforms give a thread,
    // and this runs on a worker.
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = body
            .read(&mut buffer)
            .map_err(|err| UpdateError::Fetch(format!("the download stopped: {err}")))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() as u64 > limit {
            return Err(UpdateError::Refused(format!(
                "{url} sent more than the {limit} bytes it was supposed to"
            )));
        }
        progress(bytes.len() as u64);
    }
    Ok(bytes)
}

fn set(status: &Mutex<Status>, next: Status) {
    if let Ok(mut status) = status.lock() {
        *status = next;
    }
}

fn decode_key(hex: &str) -> Option<ed25519_dalek::VerifyingKey> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    ed25519_dalek::VerifyingKey::from_bytes(&bytes).ok()
}

/// The target triple this build is for, as the manifest names it.
const TARGET: &str = env!("TIAMAT_TARGET");

/// The install root: the directory above the one holding this binary.
///
/// The layout is `<install>/current/client`, so the root is two steps up from
/// the executable. A build that is not in a `current` directory is not an
/// installed one — a working copy, a `cargo run` — and gets `None`, which is
/// what stops a developer's build from staging updates into `target/`.
fn install_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    if dir.file_name()? != "current" {
        return None;
    }
    Some(dir.parent()?.to_path_buf())
}

#[cfg(test)]
mod tests;
