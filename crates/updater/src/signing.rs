// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only
//! The operating system's signature on an executable, and whether an
//! update's matches the launcher's (milestone 0.5.0, S4).
//!
//! # Two signatures, two questions
//!
//! The manifest's Ed25519 signature (`docs/distribution.md` §2) answers "did
//! the release key sign these bytes", and it is the trust the updater rests
//! on. The operating system's signature — a Developer ID on macOS, an
//! Authenticode certificate on Windows (§8a) — answers "does the player's
//! machine trust this program". A release that lost it would still verify
//! perfectly here and then greet the player with a warning on its first
//! run; a release signed by somebody else's certificate after the release
//! key was stolen would too. So the launcher asks the system who signed the
//! launcher itself, and applies an update only when the system says every
//! executable in it was signed by that same identity.
//!
//! # What "the same identity" is
//!
//! On macOS the team identifier, which a Developer ID carries and which
//! survives the certificate being renewed. On Windows the signing
//! certificate's subject, for the same reason: Azure Trusted Signing issues
//! a fresh short-lived certificate for every signing with the same subject
//! each time, so a thumbprint would match nothing. Linux has no signing
//! scheme the launcher reads. A launcher that is itself unsigned — a
//! developer's build, a release made before the certificates existed — has
//! no identity to hold an update to, and applies it on the manifest's
//! signature alone, as every launcher did before this.
//!
//! # How it is read
//!
//! By asking the system's own tool, never by parsing a signature here:
//! `codesign` on macOS, `Get-AuthenticodeSignature` through PowerShell on
//! Windows, which the launcher already uses for its alerts. Both are on
//! every machine the launcher runs on, both verify the signature exactly as
//! the system would, and neither puts a parser of a stranger's bytes in this
//! crate (charter rule 14).
use std::path::{Path, PathBuf};

/// Who the operating system says signed an executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signer {
    /// A signature the system verified, by this identity.
    Identity(String),
    /// No signature, or one that names nobody: an ad-hoc macOS signature,
    /// which the linker gives every local build on Apple silicon.
    None,
    /// A signature the system rejected, in its words.
    Invalid(String),
    /// Nothing to read: a platform with no signing scheme, or a tool that
    /// could not be run.
    Unreadable,
}

impl std::fmt::Display for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Identity(id) => write!(f, "by {id}"),
            Self::None => f.write_str("no"),
            Self::Invalid(why) => write!(f, "no: the system rejects its signature ({why})"),
            Self::Unreadable => f.write_str("not readable on this system"),
        }
    }
}

/// What the system says about the program at `path`.
#[must_use]
pub fn signer_of(path: &Path) -> Signer {
    platform::signer_of(path)
}

/// Whether an update whose executables the system describes as
/// `executables` may be applied by a launcher the system describes as
/// `launcher`: yes when the launcher has no identity to hold it to, and
/// otherwise only when every executable carries that identity.
///
/// # Errors
///
/// The reason, naming the executable, when it may not.
pub fn check(launcher: &Signer, executables: &[(PathBuf, Signer)]) -> Result<(), String> {
    let Signer::Identity(expected) = launcher else {
        return Ok(());
    };
    for (path, signer) in executables {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match signer {
            Signer::Identity(found) if found == expected => {}
            Signer::Identity(found) => {
                return Err(format!(
                    "`{name}` is signed by {found} and this launcher by {expected}; an update \
                     is applied only when the two agree"
                ));
            }
            Signer::None => {
                return Err(format!(
                    "`{name}` is unsigned and this launcher is signed by {expected}; a signed \
                     launcher applies only signed updates"
                ));
            }
            Signer::Invalid(why) => {
                return Err(format!(
                    "`{name}` carries a signature the system rejects: {why}"
                ));
            }
            Signer::Unreadable => {
                return Err(format!(
                    "`{name}`'s signature could not be read, and this launcher is signed by \
                     {expected}"
                ));
            }
        }
    }
    Ok(())
}

/// The executables an update carries at the top of its `current`: every
/// file with an executable bit (`.exe` on Windows), and the client and the
/// server by name whatever their mode, since those two are what is run.
/// Sorted, so a refusal names the same file every time.
#[must_use]
pub fn executables_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().into_owned();
            let by_name = name == format!("client{EXE}") || name == format!("server{EXE}");
            (by_name || is_executable(&path, &metadata)).then_some(path)
        })
        .collect();
    found.sort();
    found
}

#[cfg(windows)]
const EXE: &str = ".exe";
#[cfg(not(windows))]
const EXE: &str = "";

#[cfg(unix)]
fn is_executable(_: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(path: &Path, _: &std::fs::Metadata) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

/// What `codesign -dv --verbose=2` and `codesign --verify --strict` said,
/// read as a signer.
#[cfg(any(target_os = "macos", test))]
fn parse_codesign(details: &str, verified: Result<(), String>) -> Signer {
    if details.contains("not signed at all") {
        return Signer::None;
    }
    let team = details
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set");
    match (team, verified) {
        (None, _) => Signer::None,
        (Some(team), Ok(())) => Signer::Identity(team.to_owned()),
        (Some(_), Err(why)) => Signer::Invalid(why),
    }
}

/// What `Get-AuthenticodeSignature` said, as `Status|Subject`, read as a
/// signer.
#[cfg(any(windows, test))]
fn parse_authenticode(line: &str) -> Signer {
    let line = line.trim();
    let (status, subject) = line.split_once('|').unwrap_or((line, ""));
    match (status.trim(), subject.trim()) {
        ("Valid", subject) if !subject.is_empty() => Signer::Identity(subject.to_owned()),
        ("Valid" | "NotSigned", _) => Signer::None,
        ("", _) => Signer::Unreadable,
        (status, _) => Signer::Invalid(status.to_owned()),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Signer, parse_codesign};
    use std::path::Path;
    use std::process::Command;

    pub fn signer_of(path: &Path) -> Signer {
        let Ok(details) = Command::new("codesign")
            .args(["-dv", "--verbose=2"])
            .arg(path)
            .output()
        else {
            return Signer::Unreadable;
        };
        let details = String::from_utf8_lossy(&details.stderr).into_owned();
        let verified = match Command::new("codesign")
            .args(["--verify", "--strict"])
            .arg(path)
            .output()
        {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("rejected")
                .to_owned()),
            Err(_) => return Signer::Unreadable,
        };
        parse_codesign(&details, verified)
    }
}

#[cfg(windows)]
mod platform {
    use super::{Signer, parse_authenticode};
    use std::path::Path;
    use std::process::Command;

    pub fn signer_of(path: &Path) -> Signer {
        // A single-quoted PowerShell literal: only the quote itself needs
        // doubling.
        let literal = path.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$s = Get-AuthenticodeSignature -LiteralPath '{literal}'; \
             Write-Output ($s.Status.ToString() + '|' + \
             $(if ($s.SignerCertificate) {{ $s.SignerCertificate.Subject }} else {{ '' }}))"
        );
        let Ok(out) = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(&script)
            .output()
        else {
            return Signer::Unreadable;
        };
        if !out.status.success() {
            return Signer::Unreadable;
        }
        parse_authenticode(&String::from_utf8_lossy(&out.stdout))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::Signer;
    use std::path::Path;

    pub fn signer_of(_: &Path) -> Signer {
        Signer::Unreadable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str) -> Signer {
        Signer::Identity(id.to_owned())
    }

    #[test]
    fn a_launcher_without_an_identity_holds_an_update_to_nothing() {
        let update = vec![(PathBuf::from("client"), Signer::None)];
        assert!(check(&Signer::None, &update).is_ok());
        assert!(check(&Signer::Unreadable, &update).is_ok());
        assert!(check(&Signer::Invalid("x".into()), &update).is_ok());
    }

    #[test]
    fn a_signed_launcher_applies_only_what_its_own_identity_signed() {
        let launcher = team("ABCDE12345");
        let same = vec![
            (PathBuf::from("client"), team("ABCDE12345")),
            (PathBuf::from("server"), team("ABCDE12345")),
        ];
        assert!(check(&launcher, &same).is_ok());
        for (what, bad) in [
            ("another identity", team("ZZZZZ00000")),
            ("unsigned", Signer::None),
            ("rejected", Signer::Invalid("modified".into())),
            ("unreadable", Signer::Unreadable),
        ] {
            let mixed = vec![
                (PathBuf::from("client"), team("ABCDE12345")),
                (PathBuf::from("server"), bad),
            ];
            let err = check(&launcher, &mixed).expect_err(what);
            assert!(err.contains("`server`"), "{what}: names the file: {err}");
        }
    }

    #[test]
    fn codesign_output_is_read_as_the_system_meant_it() {
        assert_eq!(
            parse_codesign(
                "client: code object is not signed at all\n",
                Err("no".into())
            ),
            Signer::None
        );
        let adhoc =
            "Executable=/x/client\nIdentifier=client\nSignature=adhoc\nTeamIdentifier=not set\n";
        assert_eq!(parse_codesign(adhoc, Ok(())), Signer::None);
        let signed = "Executable=/x/client\nAuthority=Developer ID Application: Iridesium (ABCDE12345)\nTeamIdentifier=ABCDE12345\n";
        assert_eq!(parse_codesign(signed, Ok(())), team("ABCDE12345"));
        assert_eq!(
            parse_codesign(
                signed,
                Err("invalid signature (code or signature have been modified)".into())
            ),
            Signer::Invalid("invalid signature (code or signature have been modified)".into())
        );
    }

    #[test]
    fn authenticode_output_is_read_as_the_system_meant_it() {
        assert_eq!(
            parse_authenticode("Valid|CN=Iridesium, O=Iridesium, L=Somewhere\r\n"),
            team("CN=Iridesium, O=Iridesium, L=Somewhere")
        );
        assert_eq!(parse_authenticode("NotSigned|\n"), Signer::None);
        assert_eq!(
            parse_authenticode("HashMismatch|CN=Iridesium"),
            Signer::Invalid("HashMismatch".into())
        );
        assert_eq!(
            parse_authenticode("NotTrusted|CN=Somebody"),
            Signer::Invalid("NotTrusted".into())
        );
        assert_eq!(parse_authenticode(""), Signer::Unreadable);
    }

    #[test]
    fn the_executables_of_an_update_are_found_by_mode_and_by_name() {
        let dir = std::env::temp_dir().join("tiamat-signing-executables");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(format!("client{EXE}")), b"c").expect("client");
        std::fs::write(dir.join("notes.txt"), b"n").expect("notes");
        std::fs::create_dir_all(dir.join("game")).expect("game");
        let names: Vec<String> = executables_in(&dir)
            .into_iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![format!("client{EXE}")],
            "the client by name, a text file never"
        );
    }

    #[test]
    fn this_test_binary_is_not_described_as_signed_by_an_identity() {
        // A build from a working copy is unsigned (or ad-hoc signed, on Apple
        // silicon), and the system's tool says so without an error; this is
        // the one place the real probe runs in CI, on all three platforms.
        let exe = std::env::current_exe().expect("this binary");
        let signer = signer_of(&exe);
        assert!(
            !matches!(signer, Signer::Identity(_)),
            "a test binary signed by an identity: {signer:?}"
        );
    }
}
