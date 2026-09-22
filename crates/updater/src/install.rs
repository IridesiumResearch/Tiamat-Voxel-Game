// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The install directory, and what may be done to it.
//!
//! [`docs/distribution.md`](../../../docs/distribution.md) §5 is the layout:
//!
//! ```text
//! <install>/
//!   tiamot            this launcher
//!   current/          the game
//!   staged/           an update the client downloaded and verified
//!   previous/         what `current` was, kept for one rollback
//!   install.json      what is installed, and the key to expect next
//! ```
//!
//! **Applying an update is a rename, not a copy.** Three renames on one
//! filesystem, so an interruption leaves either the old tree or the new one
//! and never half of each. A copy would leave a half-written `current` if the
//! power went out, which is the state nothing can recover from without a fresh
//! download.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tiamot_core::release::Manifest;

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The filesystem refused.
    #[error("cannot {what}: {source}")]
    Io {
        /// What was being attempted.
        what: String,
        /// Why it failed.
        source: std::io::Error,
    },
    /// The staged update is not one this build will apply.
    #[error("{0}")]
    Refused(String),
    /// The archive would not unpack.
    #[error("the update could not be unpacked: {0}")]
    Archive(#[from] crate::archive::ArchiveError),
    /// There is no game to start.
    #[error("there is no game in `current` to start")]
    NothingToRun,
}

/// What is installed here.
///
/// Written after every successful apply, read before every one. Small and
/// boring on purpose: a file the updater cannot read is a file that stops it
/// updating, so it holds the least that will do.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// The version in `current`.
    #[serde(default)]
    pub version: String,
    /// The channel it came from.
    #[serde(default)]
    pub channel: String,
    /// The hash of the key expected to sign the NEXT manifest, if the last
    /// one named a successor. Charter rule 13's pre-rotation.
    #[serde(default)]
    pub next_key: Option<String>,
    /// How many times the game has been started without reaching the point
    /// where it says it is running.
    ///
    /// Two in a row is a broken update, and the launcher rolls back.
    #[serde(default)]
    pub failed_starts: u32,
}

/// One install directory.
pub struct Install {
    root: PathBuf,
    state: State,
}

/// How many failed starts in a row before the previous version goes back.
///
/// **Two, not one.** A single failure is a crash, a full disk, a driver that
/// needs a reboot — rolling back on one would undo a good update because
/// somebody's machine hiccupped. Two in a row with no successful run between
/// them is the update.
const FAILURES_BEFORE_ROLLBACK: u32 = 2;

impl Install {
    /// Reads the install at `root`.
    ///
    /// # Errors
    ///
    /// [`InstallError::Io`] if the directory cannot be read at all. A missing
    /// or unreadable `install.json` is NOT an error: a fresh unpack has none,
    /// and the honest answer is "nothing is installed yet".
    pub fn open(root: &Path) -> Result<Self, InstallError> {
        let state = std::fs::read(root.join("install.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            root: root.to_path_buf(),
            state,
        })
    }

    /// Says what is here, for `--status`.
    pub fn report(&self) {
        let version = if self.state.version.is_empty() {
            "unknown"
        } else {
            &self.state.version
        };
        println!("installed   {version}");
        if !self.state.channel.is_empty() {
            println!("channel     {}", self.state.channel);
        }
        println!(
            "trusts key  {}",
            if RELEASE_KEY_PRESENT {
                "yes"
            } else {
                "no (a build that applies no updates)"
            }
        );
        println!(
            "staged      {}",
            if self.staged().join("update.json").exists() {
                "an update is waiting"
            } else {
                "nothing"
            }
        );
        println!(
            "previous    {}",
            if self.previous().exists() {
                "kept, for a rollback"
            } else {
                "none"
            }
        );
        if self.state.failed_starts > 0 {
            println!("failures    {}", self.state.failed_starts);
        }
    }

    /// Applies the staged update, if there is one, returning its version.
    ///
    /// The whole sequence, in the order that makes each step safe:
    ///
    /// 1. read the staged manifest and its signature;
    /// 2. check the signature against the key compiled into this binary,
    ///    allowing only the successor the last manifest committed to;
    /// 3. refuse anything not newer than what is installed;
    /// 4. check the archive against the hash in the signed manifest;
    /// 5. unpack it into a fresh directory, with every cap in `archive`;
    /// 6. rename `current` to `previous` and the new tree to `current`;
    /// 7. write the new state, and only then remove the staged files.
    ///
    /// # Errors
    ///
    /// [`InstallError::Refused`] when a check fails — and the installed
    /// version is left exactly as it was.
    pub fn apply_staged(&self, trusted_key: Option<&str>) -> Result<Option<String>, InstallError> {
        let staged = self.staged();
        let manifest_path = staged.join("update.json");
        if !manifest_path.exists() {
            return Ok(None);
        }

        let Some(trusted_key) = trusted_key else {
            return Err(InstallError::Refused(
                "this build trusts no release key, so it applies no updates. That is \
                 what a build made outside the release workflow should do."
                    .to_owned(),
            ));
        };
        let trusted = decode_key(trusted_key)
            .ok_or_else(|| InstallError::Refused("this build's release key is not a key".into()))?;

        let bytes = read(&manifest_path)?;
        let signature = read(&staged.join("update.sig"))?;
        let manifest =
            Manifest::verify(&bytes, &signature, &trusted, self.state.next_key.as_deref())
                .map_err(|err| InstallError::Refused(err.to_string()))?;

        if !self.state.version.is_empty() {
            manifest
                .newer_than(&self.state.version)
                .map_err(|err| InstallError::Refused(err.to_string()))?;
        }

        let artifact = manifest.artifact(TARGET).ok_or_else(|| {
            InstallError::Refused(format!("the release has no build for {TARGET}"))
        })?;
        let archive = read(&staged.join("update.archive"))?;
        if !artifact.matches(&archive) {
            return Err(InstallError::Refused(
                "the staged download is not the file the signed manifest describes".to_owned(),
            ));
        }

        // Unpacked beside the rest rather than over it: until the rename, an
        // interrupted update has touched nothing anybody is using.
        let incoming = self.root.join("incoming");
        let _ = std::fs::remove_dir_all(&incoming);
        std::fs::create_dir_all(&incoming).map_err(|source| InstallError::Io {
            what: "make room for the update".to_owned(),
            source,
        })?;
        crate::archive::unpack_tar_gz(&archive, &incoming)?;

        // An archive holds `<name>/current/…`; the tree to install is that
        // inner `current`, wherever it is.
        let unpacked = find_current(&incoming)
            .ok_or_else(|| InstallError::Refused("the update holds no `current`".to_owned()))?;

        let previous = self.previous();
        let current = self.current();
        let _ = std::fs::remove_dir_all(&previous);
        if current.exists() {
            rename(&current, &previous, "keep the old version")?;
        }
        if let Err(err) = rename(&unpacked, &current, "put the new version in place") {
            // Put it back rather than leaving nothing runnable, which is the
            // one outcome worse than a failed update.
            let _ = std::fs::rename(&previous, &current);
            return Err(err);
        }

        let state = State {
            version: manifest.version.clone(),
            channel: manifest.channel.clone(),
            // A manifest that names no successor leaves the one we hold: the
            // key has not rotated, and forgetting it would refuse the next
            // rotation.
            next_key: manifest
                .next_key
                .clone()
                .or_else(|| self.state.next_key.clone()),
            failed_starts: 0,
        };
        self.write_state(&state)?;
        let _ = std::fs::remove_dir_all(&staged);
        let _ = std::fs::remove_dir_all(&incoming);
        Ok(Some(manifest.version))
    }

    /// Throws away a staged update that could not be applied.
    ///
    /// **So the same broken download is not retried on every start.** The
    /// client will fetch it again if the manifest still offers it, and if it
    /// is broken at the source that is a message worth seeing twice rather
    /// than a loop nobody can leave.
    pub fn discard_staged(&self) {
        let _ = std::fs::remove_dir_all(self.staged());
        let _ = std::fs::remove_dir_all(self.root.join("incoming"));
    }

    /// Puts the previous version back.
    ///
    /// # Errors
    ///
    /// [`InstallError::Refused`] when there is nothing kept to go back to.
    pub fn rollback(&self) -> Result<(), InstallError> {
        let previous = self.previous();
        if !previous.exists() {
            return Err(InstallError::Refused(
                "there is no previous version kept here".to_owned(),
            ));
        }
        let current = self.current();
        let broken = self.root.join("broken");
        let _ = std::fs::remove_dir_all(&broken);
        if current.exists() {
            rename(&current, &broken, "set the broken version aside")?;
        }
        rename(&previous, &current, "put the previous version back")?;
        let _ = std::fs::remove_dir_all(&broken);
        // The version that is back is the one before the recorded one, and
        // nothing here knows what that was — so the state says it does not
        // know rather than claiming the version that was just removed.
        self.write_state(&State {
            version: String::new(),
            channel: self.state.channel.clone(),
            next_key: self.state.next_key.clone(),
            failed_starts: 0,
        })
    }

    /// Starts the game, and watches whether it got going.
    ///
    /// # Errors
    ///
    /// [`InstallError::NothingToRun`] when `current` holds no client.
    pub fn launch(&self, args: &[String]) -> Result<std::process::ExitCode, InstallError> {
        let client = self.current().join(format!("client{EXE}"));
        if !client.exists() {
            return Err(InstallError::NothingToRun);
        }

        // Counted BEFORE the game runs and cleared after it exits cleanly, so
        // a start that never reaches an exit — a crash, a hang the player
        // kills — is counted. A counter written after the fact would miss
        // exactly the failure it exists for.
        let mut attempt = self.state.clone();
        attempt.failed_starts = attempt.failed_starts.saturating_add(1);
        self.write_state(&attempt)?;

        let status = std::process::Command::new(&client)
            // The game's own working directory is where its mods are.
            .current_dir(self.current())
            .args(args)
            .status()
            .map_err(|source| InstallError::Io {
                what: format!("start {}", client.display()),
                source,
            })?;

        if status.success() {
            let mut cleared = self.state.clone();
            cleared.failed_starts = 0;
            self.write_state(&cleared)?;
            return Ok(std::process::ExitCode::SUCCESS);
        }

        eprintln!("tiamot: the game exited with {status}");
        if attempt.failed_starts >= FAILURES_BEFORE_ROLLBACK && self.previous().exists() {
            eprintln!(
                "tiamot: that is {} failed starts in a row; putting the previous version back.",
                attempt.failed_starts
            );
            self.rollback()?;
            eprintln!("tiamot: rolled back. Start it again to play the version before the update.");
        }
        Ok(std::process::ExitCode::FAILURE)
    }

    fn current(&self) -> PathBuf {
        self.root.join("current")
    }

    fn staged(&self) -> PathBuf {
        self.root.join("staged")
    }

    fn previous(&self) -> PathBuf {
        self.root.join("previous")
    }

    fn write_state(&self, state: &State) -> Result<(), InstallError> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|err| InstallError::Io {
            what: "describe what is installed".to_owned(),
            source: std::io::Error::other(err),
        })?;
        // Written beside and renamed over: a half-written `install.json` is an
        // install that cannot say what it is.
        let temporary = self.root.join("install.json.new");
        std::fs::write(&temporary, &bytes).map_err(|source| InstallError::Io {
            what: "write install.json".to_owned(),
            source,
        })?;
        rename(
            &temporary,
            &self.root.join("install.json"),
            "record the version",
        )
    }
}

/// Whether this build carries a release key, for `--status`.
const RELEASE_KEY_PRESENT: bool = option_env!("TIAMOT_RELEASE_KEY").is_some();

/// The target triple this binary was built for, as the manifest names it.
const TARGET: &str = env!("TIAMOT_TARGET");

/// What an executable is called here.
#[cfg(windows)]
const EXE: &str = ".exe";
/// What an executable is called here.
#[cfg(not(windows))]
const EXE: &str = "";

/// The `current` directory inside an unpacked archive.
///
/// The archive holds `tiamot-0.2.0-<target>/current/…`, so this looks one
/// level down as well as at the top — rather than hard-coding the name, which
/// carries the version in it.
fn find_current(root: &Path) -> Option<PathBuf> {
    let direct = root.join("current");
    if direct.is_dir() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let nested = entry.path().join("current");
        if nested.is_dir() {
            return Some(nested);
        }
    }
    None
}

fn read(path: &Path) -> Result<Vec<u8>, InstallError> {
    std::fs::read(path).map_err(|source| InstallError::Io {
        what: format!("read {}", path.display()),
        source,
    })
}

fn rename(from: &Path, to: &Path, what: &str) -> Result<(), InstallError> {
    std::fs::rename(from, to).map_err(|source| InstallError::Io {
        what: what.to_owned(),
        source,
    })
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

#[cfg(test)]
mod tests;
