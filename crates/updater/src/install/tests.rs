// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Applying an update, and every way of refusing to.
//!
//! These build a whole install directory in a scratch folder — a `current`
//! with a file in it, a staged update with a real signed manifest and a real
//! archive — and then check what survives. The recurring assertion is that a
//! REFUSED update leaves the installed version exactly as it was: an updater
//! that fails safe is the difference between a bad release and a brick.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use tiamot_core::release::{Artifact, Manifest, SCHEMA, to_hex};

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-install-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// An archive holding `current/` with one file saying which version it is.
fn release_archive(version: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let body = format!("version {version}");
    for (name, bytes) in [
        (
            format!("tiamot-{version}-{TARGET}/current/client{EXE}"),
            body.as_bytes(),
        ),
        (
            format!("tiamot-{version}-{TARGET}/current/marker"),
            body.as_bytes(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o755);
        header.set_path(&name).expect("path");
        header.set_cksum();
        builder
            .append(&header, std::io::Cursor::new(bytes))
            .expect("append");
    }
    let tar = builder.into_inner().expect("finish");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).expect("compress");
    encoder.finish().expect("finish")
}

/// Stages an update in `root`: the archive, a manifest describing it, and a
/// signature over that manifest.
fn stage(root: &Path, signer: &SigningKey, version: &str, edit: impl FnOnce(&mut Manifest)) {
    let archive = release_archive(version);
    let staged = root.join("staged");
    std::fs::create_dir_all(&staged).expect("staged");
    std::fs::write(staged.join("update.archive"), &archive).expect("archive");

    let mut manifest = Manifest {
        schema: SCHEMA,
        channel: "test".to_owned(),
        version: version.to_owned(),
        commit: None,
        released: "2026-09-22T00:00:00Z".to_owned(),
        protocol: 70,
        key: to_hex(signer.verifying_key().as_bytes()),
        next_key: None,
        notes: None,
        artifacts: vec![Artifact {
            target: TARGET.to_owned(),
            name: format!("tiamot-{version}-{TARGET}.tar.gz"),
            size: archive.len() as u64,
            hash: blake3::hash(&archive).to_hex().to_string(),
            urls: vec!["https://example.invalid/tiamot.tar.gz".to_owned()],
        }],
    };
    edit(&mut manifest);
    let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest");
    std::fs::write(staged.join("update.json"), &bytes).expect("write manifest");
    std::fs::write(staged.join("update.sig"), signer.sign(&bytes).to_bytes()).expect("sign");
}

/// An install with `version` in `current`.
fn installed(name: &str, version: &str) -> PathBuf {
    let root = scratch(name);
    let current = root.join("current");
    std::fs::create_dir_all(&current).expect("current");
    std::fs::write(
        current.join(format!("client{EXE}")),
        format!("version {version}"),
    )
    .expect("client");
    std::fs::write(current.join("marker"), format!("version {version}")).expect("marker");
    std::fs::write(
        root.join("install.json"),
        serde_json::to_vec(&State {
            version: version.to_owned(),
            channel: "test".to_owned(),
            next_key: None,
            failed_starts: 0,
        })
        .expect("state"),
    )
    .expect("install.json");
    root
}

/// What `current/marker` says, which is how a test tells which version is in
/// place without running anything.
fn installed_version(root: &Path) -> String {
    std::fs::read_to_string(root.join("current/marker")).unwrap_or_default()
}

#[test]
fn a_signed_newer_update_is_applied_and_the_old_one_is_kept() {
    let signer = key(1);
    let root = installed("applied", "0.1.0");
    stage(&root, &signer, "0.2.0", |_| {});

    let install = Install::open(&root).expect("open");
    let applied = install
        .apply_staged(Some(&to_hex(signer.verifying_key().as_bytes())))
        .expect("a signed, newer update applies");

    assert_eq!(applied.as_deref(), Some("0.2.0"));
    assert_eq!(installed_version(&root), "version 0.2.0");
    assert_eq!(
        std::fs::read_to_string(root.join("previous/marker")).expect("kept"),
        "version 0.1.0",
        "the version it replaced is kept for a rollback"
    );
    assert!(
        !root.join("staged").exists(),
        "a staged update that was applied is not still waiting"
    );

    // And the state now says what is installed, so the next update knows what
    // it is replacing.
    let state = Install::open(&root).expect("reopen").state;
    assert_eq!(state.version, "0.2.0");
    assert_eq!(state.failed_starts, 0);
}

#[test]
fn an_update_this_build_cannot_verify_changes_nothing() {
    // Each of these is a different lie, and the assertion after every one is
    // the same: the installed version is untouched.
    let signer = key(1);
    let trusted = to_hex(signer.verifying_key().as_bytes());

    // Signed by somebody else.
    let root = installed("impostor", "0.1.0");
    stage(&root, &key(2), "0.2.0", |_| {});
    let err = Install::open(&root)
        .expect("open")
        .apply_staged(Some(&trusted))
        .expect_err("another key's signature");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.1.0");

    // Signed properly, then edited.
    let root = installed("edited", "0.1.0");
    stage(&root, &signer, "0.2.0", |_| {});
    let path = root.join("staged/update.json");
    let text = std::fs::read_to_string(&path).expect("read");
    std::fs::write(&path, text.replace("0.2.0", "0.3.0")).expect("edit");
    let err = Install::open(&root)
        .expect("open")
        .apply_staged(Some(&trusted))
        .expect_err("an edited manifest");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.1.0");

    // The manifest is honest and the archive is not the one it describes.
    let root = installed("swapped", "0.1.0");
    stage(&root, &signer, "0.2.0", |_| {});
    std::fs::write(root.join("staged/update.archive"), release_archive("6.6.6"))
        .expect("swap the archive");
    let err = Install::open(&root)
        .expect("open")
        .apply_staged(Some(&trusted))
        .expect_err("a swapped archive");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.1.0");

    // Older than what is installed: a captured manifest replayed.
    let root = installed("downgrade", "0.2.0");
    stage(&root, &signer, "0.1.0", |_| {});
    let err = Install::open(&root)
        .expect("open")
        .apply_staged(Some(&trusted))
        .expect_err("a downgrade");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.2.0");

    // And a build with no key at all applies nothing, which is what a
    // developer's copy should do.
    let root = installed("keyless", "0.1.0");
    stage(&root, &signer, "0.2.0", |_| {});
    let err = Install::open(&root)
        .expect("open")
        .apply_staged(None)
        .expect_err("a build with no release key");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.1.0");
}

#[test]
fn a_release_without_a_build_for_this_machine_is_refused() {
    let signer = key(1);
    let root = installed("other-target", "0.1.0");
    stage(&root, &signer, "0.2.0", |manifest| {
        manifest.artifacts[0].target = "sparc64-unknown-none".to_owned();
    });

    let err = Install::open(&root)
        .expect("open")
        .apply_staged(Some(&to_hex(signer.verifying_key().as_bytes())))
        .expect_err("no build for this target");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.1.0");
}

#[test]
fn the_key_rotates_only_to_the_successor_the_last_manifest_named() {
    let old = key(1);
    let new = key(3);
    let trusted = to_hex(old.verifying_key().as_bytes());
    let committed = blake3::hash(new.verifying_key().as_bytes())
        .to_hex()
        .to_string();

    // An install that has been told which key comes next.
    let root = installed("rotation", "0.1.0");
    std::fs::write(
        root.join("install.json"),
        serde_json::to_vec(&State {
            version: "0.1.0".to_owned(),
            channel: "test".to_owned(),
            next_key: Some(committed.clone()),
            failed_starts: 0,
        })
        .expect("state"),
    )
    .expect("install.json");
    stage(&root, &new, "0.2.0", |_| {});
    Install::open(&root)
        .expect("open")
        .apply_staged(Some(&trusted))
        .expect("the committed successor signs the next release");
    assert_eq!(installed_version(&root), "version 0.2.0");

    // A thief with the OLD key signs a manifest naming their own successor.
    // The install only accepts the hash it was already holding.
    let root = installed("thief", "0.1.0");
    std::fs::write(
        root.join("install.json"),
        serde_json::to_vec(&State {
            version: "0.1.0".to_owned(),
            channel: "test".to_owned(),
            next_key: Some(committed),
            failed_starts: 0,
        })
        .expect("state"),
    )
    .expect("install.json");
    stage(&root, &key(4), "0.2.0", |_| {});
    let err = Install::open(&root)
        .expect("open")
        .apply_staged(Some(&trusted))
        .expect_err("a key nobody committed to");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.1.0");
}

#[test]
fn a_rollback_puts_the_previous_version_back() {
    let signer = key(1);
    let root = installed("rollback", "0.1.0");
    stage(&root, &signer, "0.2.0", |_| {});
    Install::open(&root)
        .expect("open")
        .apply_staged(Some(&to_hex(signer.verifying_key().as_bytes())))
        .expect("applies");
    assert_eq!(installed_version(&root), "version 0.2.0");

    Install::open(&root)
        .expect("open")
        .rollback()
        .expect("rolls back");
    assert_eq!(installed_version(&root), "version 0.1.0");
    assert!(
        !root.join("previous").exists(),
        "the rolled-back version is not still offered as a rollback"
    );

    // And a second rollback has nothing to go back to, which is a message
    // rather than a lost install.
    let err = Install::open(&root)
        .expect("open")
        .rollback()
        .expect_err("nothing kept");
    assert!(matches!(err, InstallError::Refused(_)), "{err}");
    assert_eq!(installed_version(&root), "version 0.1.0");
}

#[test]
fn a_staged_update_that_was_refused_is_discarded_rather_than_retried_for_ever() {
    let root = installed("discard", "0.1.0");
    stage(&root, &key(2), "0.2.0", |_| {});
    let install = Install::open(&root).expect("open");
    assert!(
        install
            .apply_staged(Some(&to_hex(key(1).verifying_key().as_bytes())))
            .is_err()
    );

    install.discard_staged();
    assert!(!root.join("staged").exists());
    assert!(!root.join("incoming").exists());
    // Nothing was staged the second time, so there is nothing to apply and no
    // error — the install simply runs what it has.
    assert_eq!(
        Install::open(&root)
            .expect("open")
            .apply_staged(Some(&to_hex(key(1).verifying_key().as_bytes())))
            .expect("nothing staged"),
        None
    );
}

#[test]
fn an_empty_directory_is_an_install_with_nothing_in_it_rather_than_an_error() {
    // A fresh unpack has no `install.json`, and the honest answer to "what is
    // installed" is "nothing yet" rather than a refusal to start.
    let root = scratch("fresh");
    let install = Install::open(&root).expect("a fresh directory opens");
    assert_eq!(install.state.version, "");
    assert_eq!(install.apply_staged(None).expect("nothing staged"), None);
    assert!(matches!(
        install.launch(&[]).expect_err("nothing to run"),
        InstallError::NothingToRun
    ));
}
