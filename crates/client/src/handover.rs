// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! An installed client started on its own, and the launcher's failure count.
//!
//! [`docs/distribution.md`](../../../docs/distribution.md) §5. The launcher
//! applies updates, so a client that an installed player starts **directly**
//! (a Dock tile pinned to `current/client`, a shortcut made by hand, a
//! double-click in the folder) would skip the update and, from a working
//! directory that is not the install, find no mods. Such a client therefore
//! hands over: it starts the launcher with the same arguments and gets out of
//! the way. On Unix that is `exec`, so the process (and on macOS its Dock
//! tile) stays the same; on Windows it is a spawn and an exit.
//!
//! # No loop
//!
//! Two environment variables make a loop impossible whatever order the two
//! programs run in. The launcher sets [`LAUNCHED`] on the client it starts, and
//! a client carrying it never hands over. A client sets [`HANDED_OVER`] on the
//! launcher it starts, and a client that finds it set never hands over either,
//! which covers a launcher that runs a client without setting [`LAUNCHED`].
//! If the launcher cannot be found, or cannot be started, the client carries
//! on as it always did and says why.
//!
//! # The failure count
//!
//! On macOS the launcher `exec`s the client, so there is no "after the game
//! exits" for it to clear `failed_starts` in `install.json` in. The client does
//! it instead, once it has put a frame on screen ([`report_started`]).

use std::path::{Path, PathBuf};

/// Set by the launcher on the client it starts.
pub const LAUNCHED: &str = "TIAMAT_LAUNCHED";

/// Set by a client on the launcher it hands over to.
pub const HANDED_OVER: &str = "TIAMAT_HANDED_OVER";

/// The platforms whose layouts differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// macOS: the launcher also lives inside `Tiamat.app`.
    Macos,
    /// Windows: executables end in `.exe`.
    Windows,
    /// Anything else.
    Other,
}

impl Os {
    /// The platform this build is for.
    #[must_use]
    pub const fn this() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// What a client should do about the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Hand over to the launcher at this path.
    HandOver(PathBuf),
    /// Carry on, for this reason.
    Stay(Stay),
}

/// Why a client carries on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stay {
    /// Not an installed build: a working copy never hands over.
    NotInstalled,
    /// The launcher started this client.
    StartedByLauncher,
    /// This process already is the far side of a hand-over.
    AlreadyHandedOver,
    /// No launcher exists where an install keeps it.
    NoLauncher,
}

/// Where an install's launcher may be, in the order to look.
///
/// `tiamat` (`tiamat.exe` on Windows) beside `current/`; on macOS the copy
/// inside `Tiamat.app` comes first, since that is the only one a macOS archive
/// carries.
#[must_use]
pub fn launcher_candidates(root: &Path, os: Os) -> Vec<PathBuf> {
    let name = if os == Os::Windows {
        "tiamat.exe"
    } else {
        "tiamat"
    };
    let mut found = Vec::new();
    if os == Os::Macos {
        found.push(root.join("Tiamat.app/Contents/MacOS").join(name));
    }
    found.push(root.join(name));
    found
}

/// What the process can see, for [`decide`].
#[derive(Debug, Clone, Copy)]
pub struct Situation<'a> {
    /// The install root, `None` for a working copy.
    pub root: Option<&'a Path>,
    /// [`LAUNCHED`] is set.
    pub launched: bool,
    /// [`HANDED_OVER`] is set.
    pub handed_over: bool,
    /// The platform.
    pub os: Os,
}

/// Decides, as a pure function of what the process can see.
#[must_use]
pub fn decide(seen: Situation<'_>, exists: impl Fn(&Path) -> bool) -> Decision {
    let Some(root) = seen.root else {
        return Decision::Stay(Stay::NotInstalled);
    };
    if seen.launched {
        return Decision::Stay(Stay::StartedByLauncher);
    }
    if seen.handed_over {
        return Decision::Stay(Stay::AlreadyHandedOver);
    }
    launcher_candidates(root, seen.os)
        .into_iter()
        .find(|path| exists(path))
        .map_or(Decision::Stay(Stay::NoLauncher), Decision::HandOver)
}

/// Hands over to the launcher if this is an installed client started directly.
///
/// Returns only when the client should carry on; a successful hand-over does
/// not return (Unix `exec`) or exits the process (Windows). Reasons to stay
/// are logged when they are worth a player's attention.
pub fn hand_over_if_installed() {
    let root = crate::update::install_root();
    let env_set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let decision = decide(
        Situation {
            root: root.as_deref(),
            launched: env_set(LAUNCHED),
            handed_over: env_set(HANDED_OVER),
            os: Os::this(),
        },
        Path::is_file,
    );
    let launcher = match decision {
        Decision::HandOver(path) => path,
        Decision::Stay(Stay::NoLauncher) => {
            tracing::warn!("no launcher beside this install; starting without it");
            return;
        }
        Decision::Stay(_) => return,
    };
    let mut command = std::process::Command::new(&launcher);
    // After `--`, so the launcher passes every argument on to the game and
    // none is taken for one of its own (`--help`, `--status`, ...).
    command
        .arg("--")
        .args(std::env::args_os().skip(1))
        .env(HANDED_OVER, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only returns on failure.
        let err = command.exec();
        tracing::warn!(launcher = %launcher.display(), %err,
                       "could not hand over to the launcher; starting without it");
    }
    #[cfg(not(unix))]
    match command.spawn() {
        Ok(_) => std::process::exit(0),
        Err(err) => tracing::warn!(launcher = %launcher.display(), %err,
                                   "could not hand over to the launcher; starting without it"),
    }
}

/// `install.json` with `failed_starts` set to 0, if that changes anything.
///
/// Read as generic JSON and changed in that one field, so whatever else the
/// launcher keeps there survives. `None` when the bytes are not a JSON object,
/// or when the field is already 0 or absent (nothing to write).
#[must_use]
pub fn cleared_failed_starts(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object_mut()?;
    match object.get("failed_starts") {
        None => return None,
        Some(count) if count.as_u64() == Some(0) => return None,
        Some(_) => {}
    }
    object.insert("failed_starts".to_owned(), serde_json::Value::from(0_u32));
    serde_json::to_vec_pretty(&value).ok()
}

/// Clears `failed_starts` in `<root>/install.json`, atomically.
///
/// Written beside and renamed over, so a crash cannot leave a half-written
/// file. Does nothing if the file is missing or unreadable.
///
/// # Errors
///
/// The temporary file could not be written or renamed into place.
pub fn clear_failed_starts(root: &Path) -> std::io::Result<()> {
    let path = root.join("install.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(());
    };
    let Some(updated) = cleared_failed_starts(&bytes) else {
        return Ok(());
    };
    let temporary = root.join("install.json.started");
    std::fs::write(&temporary, updated)?;
    std::fs::rename(&temporary, &path)
}

/// Tells the launcher's bookkeeping that this start got going.
///
/// Call after the first frame is presented. Acts once per process, and only
/// for an installed client the launcher started; anywhere else it does
/// nothing.
pub fn report_started() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(root) = crate::update::install_root() else {
            return;
        };
        if std::env::var_os(LAUNCHED).is_none_or(|value| value.is_empty()) {
            return;
        }
        if let Err(err) = clear_failed_starts(&root) {
            tracing::warn!(%err, "could not record a clean start in install.json");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sit(root: Option<&Path>, launched: bool, handed_over: bool, os: Os) -> Situation<'_> {
        Situation {
            root,
            launched,
            handed_over,
            os,
        }
    }

    fn all(_: &Path) -> bool {
        true
    }

    #[test]
    fn a_working_copy_never_hands_over() {
        assert_eq!(
            decide(sit(None, false, false, Os::Macos), all),
            Decision::Stay(Stay::NotInstalled)
        );
    }

    #[test]
    fn a_client_the_launcher_started_stays() {
        let root = Path::new("/i");
        assert_eq!(
            decide(sit(Some(root), true, false, Os::Other), all),
            Decision::Stay(Stay::StartedByLauncher)
        );
        assert_eq!(
            decide(sit(Some(root), false, true, Os::Other), all),
            Decision::Stay(Stay::AlreadyHandedOver)
        );
        assert_eq!(
            decide(sit(Some(root), true, true, Os::Other), all),
            Decision::Stay(Stay::StartedByLauncher)
        );
    }

    #[test]
    fn a_directly_started_install_hands_over_to_the_launcher() {
        let root = Path::new("/i");
        assert_eq!(
            decide(sit(Some(root), false, false, Os::Other), all),
            Decision::HandOver(PathBuf::from("/i/tiamat"))
        );
        assert_eq!(
            decide(sit(Some(root), false, false, Os::Windows), all),
            Decision::HandOver(PathBuf::from("/i/tiamat.exe"))
        );
    }

    #[test]
    fn on_macos_the_bundle_comes_first_then_the_plain_launcher() {
        let root = Path::new("/i");
        let inside = PathBuf::from("/i/Tiamat.app/Contents/MacOS/tiamat");
        assert_eq!(
            decide(sit(Some(root), false, false, Os::Macos), all),
            Decision::HandOver(inside)
        );
        assert_eq!(
            decide(sit(Some(root), false, false, Os::Macos), |p| p
                == Path::new("/i/tiamat")),
            Decision::HandOver(PathBuf::from("/i/tiamat"))
        );
    }

    #[test]
    fn no_launcher_means_carry_on() {
        assert_eq!(
            decide(sit(Some(Path::new("/i")), false, false, Os::Macos), |_| {
                false
            }),
            Decision::Stay(Stay::NoLauncher)
        );
    }

    #[test]
    fn clearing_changes_only_the_failure_count() {
        let before = br#"{"version":"0.2.1","channel":"test","next_key":"b3:x","failed_starts":1,"extra":{"a":[1,2]}}"#;
        let after = cleared_failed_starts(before).expect("a count to clear");
        let value: serde_json::Value = serde_json::from_slice(&after).expect("json");
        assert_eq!(
            value,
            serde_json::json!({"version":"0.2.1","channel":"test","next_key":"b3:x",
                               "failed_starts":0,"extra":{"a":[1,2]}})
        );
    }

    #[test]
    fn nothing_is_written_when_there_is_nothing_to_clear() {
        assert!(cleared_failed_starts(br#"{"failed_starts":0}"#).is_none());
        assert!(cleared_failed_starts(br#"{"version":"1"}"#).is_none());
        assert!(cleared_failed_starts(b"not json").is_none());
        assert!(cleared_failed_starts(b"[1,2]").is_none());
    }

    #[test]
    fn the_file_is_rewritten_in_place_and_a_missing_one_is_fine() {
        let dir = std::env::temp_dir().join("tiamat-client-handover");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        clear_failed_starts(&dir).expect("a missing file is fine");
        assert!(!dir.join("install.json").exists());

        std::fs::write(
            dir.join("install.json"),
            br#"{"failed_starts":2,"version":"9"}"#,
        )
        .expect("write");
        clear_failed_starts(&dir).expect("cleared");
        let text = std::fs::read_to_string(dir.join("install.json")).expect("read");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["failed_starts"], 0);
        assert_eq!(value["version"], "9");
        assert!(!dir.join("install.json.started").exists());

        std::fs::write(dir.join("install.json"), b"garbage").expect("write");
        clear_failed_starts(&dir).expect("an unreadable file is left alone");
        assert_eq!(
            std::fs::read(dir.join("install.json")).expect("read"),
            b"garbage"
        );
    }
}
