// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Fuzzes the release manifest parser.
//!
//! **Charter rule 14: a parser ships with its fuzz target.** This one reads
//! what an installed copy fetches from the update host — a host the client
//! trusts no further than the Ed25519 key compiled in, and the manifest is
//! parsed BEFORE its signature is checked, because the key it names is part
//! of what is checked. So until the signature holds, these bytes are anybody's:
//! a host that was taken over, a proxy in the way, a typo in a URL.
//!
//! The property is that parsing is total and its caps hold: every input is a
//! manifest or a `ReleaseError`, never a panic, and a manifest that parsed is
//! inside every limit the parser promises — so nothing downstream, from the
//! download to the unpack, is handed a number the caps were meant to keep out.
//! Then the comparisons a client makes with it, which must not panic on any
//! version string the parser let through.
//!
//! Run: `cargo +nightly fuzz run manifest_decode`
#![no_main]

use libfuzzer_sys::fuzz_target;
use tiamat_core::release::{
    MAX_ARTIFACT_BYTES, MAX_ARTIFACTS, MAX_MANIFEST_BYTES, MAX_URLS, Manifest, SCHEMA,
    signing_bytes,
};

fuzz_target!(|data: &[u8]| {
    // What is signed is a function of the bytes alone, and must never panic
    // on a file that is not a manifest at all.
    let _ = signing_bytes(data);

    let Ok(manifest) = Manifest::parse(data) else {
        return;
    };

    assert!(
        data.len() <= MAX_MANIFEST_BYTES,
        "a manifest over the size cap parsed"
    );
    assert_eq!(manifest.schema, SCHEMA, "a manifest of another schema parsed");
    assert!(
        manifest.artifacts.len() <= MAX_ARTIFACTS,
        "more artifacts than the cap"
    );
    for artifact in &manifest.artifacts {
        assert!(
            !artifact.urls.is_empty() && artifact.urls.len() <= MAX_URLS,
            "an artifact with no URL, or more than the cap"
        );
        assert!(
            artifact.size <= MAX_ARTIFACT_BYTES,
            "an artifact bigger than the cap"
        );
        // A hash that is not 64 hex characters can never match a download, so
        // the parser must have refused it rather than let a client fetch a
        // gigabyte to find out.
        assert!(
            artifact.hash.len() == 64 && artifact.hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "a hash that is not 64 hex characters parsed"
        );
        // And the check itself is total on any bytes.
        let _ = artifact.matches(b"");
        let _ = artifact.matches(data);
    }

    // The comparisons a client makes, on whatever version string got through.
    let _ = manifest.newer_than("0.0.0");
    let _ = manifest.newer_than(&manifest.version);
    let _ = manifest.newer_than("not a version");
    let _ = manifest.artifact("x86_64-unknown-linux-gnu");
});
