// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Staging an update, and refusing to.
//!
//! The network half is exercised by hand against a real release (see the
//! commit that added this); what is tested here is everything that decides
//! whether bytes reach the disk — because `staged/` is read by the launcher,
//! and anything written there is a thing that will be checked again and then
//! applied.
//!
//! These build a manifest signed by a throwaway key. The build's own key is
//! compiled in from `release-key.pub`, so a signature made here is the wrong
//! one BY CONSTRUCTION — which makes "the wrong key is refused" the easy test
//! and the honest one for `stage`.

use std::path::PathBuf;

use ed25519_dalek::{Signer, SigningKey};
use tiamot_core::release::{Artifact, Manifest, SCHEMA, to_hex};

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-client-update").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A manifest describing `archive`, signed by `signer`.
fn signed(
    signer: &SigningKey,
    archive: &[u8],
    edit: impl FnOnce(&mut Manifest),
) -> (Vec<u8>, Vec<u8>) {
    let mut manifest = Manifest {
        schema: SCHEMA,
        channel: "test".to_owned(),
        version: "99.0.0".to_owned(),
        commit: None,
        released: "2026-09-22T00:00:00Z".to_owned(),
        protocol: 70,
        key: to_hex(signer.verifying_key().as_bytes()),
        next_key: None,
        notes: None,
        artifacts: vec![Artifact {
            target: TARGET.to_owned(),
            name: format!("tiamot-99.0.0-{TARGET}.tar.gz"),
            size: archive.len() as u64,
            hash: blake3::hash(archive).to_hex().to_string(),
            urls: vec!["https://example.invalid/tiamot.tar.gz".to_owned()],
        }],
    };
    edit(&mut manifest);
    let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest");
    let signature = signer.sign(&bytes).to_bytes().to_vec();
    (bytes, signature)
}

#[test]
fn a_manifest_signed_by_the_wrong_key_stages_nothing() {
    // The build trusts `release-key.pub`; this is signed by a throwaway. No
    // files may appear, because `staged/` is what the launcher acts on.
    let install = scratch("wrong-key");
    let archive = b"a release".to_vec();
    let (bytes, signature) = signed(&SigningKey::from_bytes(&[7; 32]), &archive, |_| {});

    let err = stage(&install, &bytes, &signature, &archive).expect_err("the wrong key");
    assert!(matches!(err, UpdateError::Refused(_)), "{err}");
    assert!(
        !install.join("staged").exists(),
        "a refused update left files for the launcher to find"
    );
}

#[test]
fn a_manifest_that_is_not_json_is_refused_without_touching_the_disk() {
    let install = scratch("rubbish");
    for bytes in [&b""[..], &b"{"[..], &[0xFF, 0x00, 0xFE][..]] {
        let err = stage(&install, bytes, &[0; 64], b"x").expect_err("rubbish");
        assert!(matches!(err, UpdateError::Refused(_)), "{err}");
    }
    assert!(!install.join("staged").exists());
}

#[test]
fn an_install_root_is_only_a_directory_called_current() {
    // `<install>/current/client` is the layout, so the root is two steps up.
    // A binary anywhere else — `cargo run`, a working copy, somebody's
    // Downloads folder — is not an install, and staging into it would put an
    // `update.archive` somewhere nothing will ever apply it from.
    //
    // This build is a test binary in `target/`, so it must say so.
    assert!(
        install_root().is_none(),
        "a test binary in target/ reported itself as an installed build"
    );
}

#[test]
fn a_build_with_no_manifest_url_never_offers_an_update() {
    // A working copy has no URL and no install root, and must not check: a
    // developer's build asking a website about itself on every run is both a
    // surprise and a way to get rate-limited.
    let updates = Updates::new();
    assert!(!updates.configured());
    assert_eq!(updates.status(), Status::Idle);

    // And asking anyway does nothing rather than spawning a thread that
    // fails: the front screen would otherwise show an error for a check
    // nobody asked for.
    updates.check();
    assert_eq!(updates.status(), Status::Idle);
    updates.download();
    assert_eq!(updates.status(), Status::Idle);
}

#[test]
fn the_status_is_what_the_front_screen_needs_and_nothing_more() {
    // A small guard against the enum growing a variant the screen cannot
    // draw: every state a player can be in has a sentence for it.
    let sentence = |status: &Status| match status {
        Status::Idle | Status::Checking | Status::UpToDate => true,
        Status::Available { version, size } => !version.is_empty() && *size > 0,
        Status::Downloading { done, total } => done <= total,
        Status::Staged { version } => !version.is_empty(),
        Status::Failed { what } => !what.is_empty(),
    };
    assert!(sentence(&Status::Idle));
    assert!(sentence(&Status::Available {
        version: "0.2.0".to_owned(),
        size: 1,
    }));
    assert!(sentence(&Status::Downloading { done: 1, total: 2 }));
    assert!(sentence(&Status::Staged {
        version: "0.2.0".to_owned()
    }));
    assert!(sentence(&Status::Failed {
        what: "no".to_owned()
    }));
}

#[test]
#[ignore = "reaches the real internet; run by hand with --ignored --nocapture"]
fn the_fetch_layer_reaches_a_real_https_server_and_holds_its_cap() {
    // The one thing the tests above cannot prove: that the TLS stack, the
    // roots and the reader actually work against a server nobody here
    // controls. Not a gate — a build machine with no network would fail it
    // for a reason that has nothing to do with the code.
    //
    // The URL is a file that has existed for years and is a few kilobytes.
    const URL: &str = "https://raw.githubusercontent.com/rust-lang/rust/master/LICENSE-MIT";

    let body = get(URL, 64 * 1024, |_| {}).expect("a real https server answers");
    println!("fetched {} bytes", body.len());
    println!(
        "starts: {:?}",
        String::from_utf8_lossy(&body[..40.min(body.len())])
    );
    assert!(
        String::from_utf8_lossy(&body).contains("Permission is hereby granted"),
        "that is not the MIT licence"
    );

    // And the cap is what stops a server that keeps talking: the same file,
    // with room for a hundred bytes.
    let err = get(URL, 100, |_| {}).expect_err("the cap must hold");
    assert!(matches!(err, UpdateError::Refused(_)), "{err}");

    // Plain http never leaves the building at all.
    let err = get("http://example.invalid/x", 100, |_| {}).expect_err("http");
    assert!(matches!(err, UpdateError::Refused(_)), "{err}");
}
