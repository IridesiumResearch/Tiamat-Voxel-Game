// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a manifest must refuse.
//!
//! These are the tests for the code that decides which bytes get executed on
//! somebody else's machine, so most of them are about refusing rather than
//! about accepting.

use ed25519_dalek::{Signer, SigningKey};

use super::*;

/// A key pair from a fixed seed, so a failure is the same failure twice.
fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// A manifest as it would be served, and its signature.
fn signed(signer: &SigningKey, edit: impl FnOnce(&mut Manifest)) -> (Vec<u8>, Vec<u8>) {
    let mut manifest = Manifest {
        schema: SCHEMA,
        channel: "test".to_owned(),
        version: "0.2.0".to_owned(),
        commit: Some("8929ca1".to_owned()),
        released: "2026-09-21T10:00:00Z".to_owned(),
        protocol: 70,
        key: to_hex(signer.verifying_key().as_bytes()),
        next_key: None,
        notes: None,
        artifacts: vec![Artifact {
            target: "x86_64-unknown-linux-gnu".to_owned(),
            name: "tiamot-0.2.0-x86_64-unknown-linux-gnu.tar.gz".to_owned(),
            size: 9,
            hash: blake3::hash(b"a release").to_hex().to_string(),
            urls: vec!["https://example.invalid/tiamot.tar.gz".to_owned()],
        }],
    };
    edit(&mut manifest);
    let bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serialises");
    let signature = signer.sign(signing_bytes(&bytes)).to_bytes().to_vec();
    (bytes, signature)
}

#[test]
fn a_manifest_this_key_signed_is_read_and_one_it_did_not_is_refused() {
    let signer = key(1);
    let trusted = signer.verifying_key();
    let (bytes, signature) = signed(&signer, |_| {});

    let manifest = Manifest::verify(&bytes, &signature, &trusted, None).expect("a signed manifest");
    assert_eq!(manifest.version, "0.2.0");
    assert_eq!(manifest.protocol, 70);

    // **A byte changed after signing.** This is the whole point of the
    // mechanism: the mirror, the CDN and the certificate are all untrusted,
    // and the only thing that decides is the signature over these exact bytes.
    let mut tampered = bytes.clone();
    let spot = tampered
        .windows(5)
        .position(|window| window == b"0.2.0")
        .expect("the version is in there");
    tampered[spot + 2] = b'9';
    assert_eq!(
        Manifest::verify(&tampered, &signature, &trusted, None),
        Err(ReleaseError::NotSigned)
    );

    // A perfectly good manifest, signed by somebody else's key.
    let impostor = key(2);
    let (bytes, signature) = signed(&impostor, |_| {});
    assert_eq!(
        Manifest::verify(&bytes, &signature, &trusted, None),
        Err(ReleaseError::UnexpectedKey)
    );
}

#[test]
fn the_key_may_only_become_the_successor_the_last_manifest_named() {
    // Charter rule 13's pre-rotation, used for the thing that ships code: a
    // stolen signing key cannot hand itself a successor, because the successor
    // was committed to before the theft.
    let old = key(1);
    let new = key(3);
    let trusted = old.verifying_key();
    let committed = blake3::hash(new.verifying_key().as_bytes())
        .to_hex()
        .to_string();

    // Signed by the new key, and the client has been told to expect it.
    let (bytes, signature) = signed(&new, |_| {});
    let manifest = Manifest::verify(&bytes, &signature, &trusted, Some(&committed))
        .expect("the committed successor is accepted");
    assert_eq!(manifest.version, "0.2.0");

    // The same manifest, from a client that was never told about a successor.
    assert_eq!(
        Manifest::verify(&bytes, &signature, &trusted, None),
        Err(ReleaseError::UnexpectedKey)
    );

    // And a third key, which is what a thief who has the old key would try:
    // they can sign a manifest naming their own successor, but this client
    // only accepts the hash it already held.
    let thief = key(4);
    let (bytes, signature) = signed(&thief, |_| {});
    assert_eq!(
        Manifest::verify(&bytes, &signature, &trusted, Some(&committed)),
        Err(ReleaseError::UnexpectedKey)
    );
}

#[test]
fn a_manifest_that_is_not_newer_is_refused() {
    // The downgrade rule. A captured old manifest is still perfectly signed,
    // which is exactly why the version has to be checked as well.
    let signer = key(1);
    let (bytes, signature) = signed(&signer, |manifest| manifest.version = "0.1.0".to_owned());
    let manifest = Manifest::verify(&bytes, &signature, &signer.verifying_key(), None)
        .expect("it is signed, and that is not the question");

    assert_eq!(
        manifest.newer_than("0.2.0"),
        Err(ReleaseError::NotNewer {
            offered: "0.1.0".to_owned(),
            installed: "0.2.0".to_owned(),
        })
    );
    assert_eq!(
        manifest.newer_than("0.1.0"),
        Err(ReleaseError::NotNewer {
            offered: "0.1.0".to_owned(),
            installed: "0.1.0".to_owned(),
        }),
        "the same version is not an update"
    );
    assert!(manifest.newer_than("0.0.9").is_ok());
}

#[test]
fn every_documented_limit_is_checked_before_the_manifest_is_used() {
    let signer = key(1);
    let refused = |edit: fn(&mut Manifest)| {
        let (bytes, signature) = signed(&signer, edit);
        Manifest::verify(&bytes, &signature, &signer.verifying_key(), None)
            .expect_err("this manifest should have been refused")
    };

    // A file name that is a path escapes the directory it is written into.
    assert!(matches!(
        refused(|m| m.artifacts[0].name = "../../etc/cron.d/tiamot".to_owned()),
        ReleaseError::Unusable { .. }
    ));
    // Plain http would not let anybody swap the bytes — the hash decides — but
    // it would let anybody watch.
    assert!(matches!(
        refused(|m| m.artifacts[0].urls = vec!["http://example.invalid/x".to_owned()]),
        ReleaseError::Unusable { .. }
    ));
    assert!(matches!(
        refused(|m| m.artifacts[0].urls = Vec::new()),
        ReleaseError::Unusable { .. }
    ));
    // An artefact bigger than any honest release.
    assert!(matches!(
        refused(|m| m.artifacts[0].size = MAX_ARTIFACT_BYTES + 1),
        ReleaseError::Unusable { .. }
    ));
    assert!(matches!(
        refused(|m| m.artifacts[0].hash = "not a hash".to_owned()),
        ReleaseError::Unusable { .. }
    ));
    assert!(matches!(
        refused(|m| m.version = "tomorrow".to_owned()),
        ReleaseError::Unusable { .. }
    ));
    // A schema from the future: this build cannot promise to read it right,
    // so it says so rather than reading it wrong.
    assert!(matches!(
        refused(|m| m.schema = SCHEMA + 1),
        ReleaseError::Schema { .. }
    ));
}

#[test]
fn a_manifest_too_large_to_be_one_is_not_parsed_at_all() {
    // Charter rule 14: the cap comes before the allocation, not after.
    let huge = vec![b' '; MAX_MANIFEST_BYTES + 1];
    assert_eq!(
        Manifest::parse(&huge),
        Err(ReleaseError::TooLarge {
            len: MAX_MANIFEST_BYTES + 1
        })
    );

    // And rubbish is a message rather than a panic.
    assert!(matches!(
        Manifest::parse(b"{"),
        Err(ReleaseError::Malformed(_))
    ));
    assert!(matches!(
        Manifest::parse(&[0xFF, 0xFE, 0x00]),
        Err(ReleaseError::Malformed(_))
    ));
}

#[test]
fn an_artifact_is_the_bytes_its_hash_says_it_is() {
    let signer = key(1);
    let (bytes, signature) = signed(&signer, |_| {});
    let manifest =
        Manifest::verify(&bytes, &signature, &signer.verifying_key(), None).expect("signed");
    let artifact = manifest
        .artifact("x86_64-unknown-linux-gnu")
        .expect("the target is in there");

    assert!(artifact.matches(b"a release"));
    // One byte different, same length.
    assert!(!artifact.matches(b"a releasf"));
    // The right hash and the wrong length cannot both be true, and the length
    // is checked first because it is free.
    assert!(!artifact.matches(b"a release with more after it"));
    assert!(manifest.artifact("aarch64-apple-darwin").is_none());
}

#[test]
fn versions_compare_by_number_and_nonsense_loses() {
    use std::cmp::Ordering;
    assert_eq!(compare("0.2.0", "0.1.9"), Ordering::Greater);
    assert_eq!(
        compare("0.10.0", "0.9.0"),
        Ordering::Greater,
        "not a string compare"
    );
    assert_eq!(compare("1.0.0", "1.0.0"), Ordering::Equal);
    assert_eq!(compare("0.1.0", "0.1.1"), Ordering::Less);
    // Anything unreadable loses, so an update to something we cannot parse
    // never happens.
    assert_eq!(compare("nightly", "0.1.0"), Ordering::Less);
    assert_eq!(compare("0.1.0", "nightly"), Ordering::Greater);
    assert_eq!(compare("1.2.3.4", "1.2.3"), Ordering::Less);
}
