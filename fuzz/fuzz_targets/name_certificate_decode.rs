// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Fuzzes the name-registry parsers, and what runs on what they let through.
//!
//! **Charter rule 14: a parser ships with its fuzz target.** Every one of these
//! records arrives from a peer a server does not trust: a client's `Hello`
//! carries a certificate, and the registry service reads claims, releases,
//! email, recovery and cancellation requests, rotation records and revocation
//! lists straight off HTTP. So until a signature holds, these bytes are
//! anybody's — a captured connection, a proxy in the way, an operator pasting a
//! record from somewhere.
//!
//! The property is that every parse is **total and capped**: any input is a
//! value or a `RegistryError`, never a panic; a certificate that parsed holds
//! the name rules and a sane, forward lifetime; a revocation list is inside its
//! entry cap; and the signing layout of a parsed record is a function that
//! cannot panic on any field the parser let through.
//!
//! Then the **verifiers**, because a parse that succeeds hands its fields to
//! them: the server's real decode-then-verify path (`verify_certificate`) at
//! the certificate's own issue time and at both extremes of `i64`, a rotation
//! offered to a trust built from the record's own keys, the same bytes read as
//! the list of rotation records a challenge answer carries and walked from a
//! pin at the first record's retiring key (`RegistryTrust::advanced_by`, the
//! server's join path), a revocation list
//! checked against its own signer, a key-set proof walked from its own root to
//! its own last key — as parsed, and again with its links relabelled as each
//! kind (add-key, rotation, root commitment), so the rotation rules are reached
//! on any bytes — and every request verified at **its own timestamp** — so
//! the clock check passes and the signature, ownership and proof checks behind
//! it are reached, rather than every run stopping at a stale timestamp.
//!
//! What a verified proof says about the keys (`KeyLineage`) is held to the
//! promises the registry's memory of retired keys builds on: it is rooted at
//! the UUID and ends at the signing key, and no key a rotation in the chain
//! retired is ever counted as one with authority.
//!
//! Run: `cargo +nightly fuzz run name_certificate_decode`

#![no_main]

use libfuzzer_sys::fuzz_target;
use tiamat_core::identity::keyset::commit_to;
use tiamat_core::identity::registry::{
    CancelRecoveryRequest, ClaimRequest, EmailRequest, KeyLineage, KeySetProof, LinkKind,
    MAX_CARRIED_ROTATIONS, MAX_CERTIFICATE_BYTES, MAX_CERTIFICATE_LIFETIME_SECONDS,
    MAX_EMAIL_BYTES, MAX_KEY_SET_LINKS, MAX_KEY_SET_PROOF_BYTES, MAX_NAME_CHARS,
    MAX_PROVEN_REQUEST_BYTES, MAX_REVOCATIONS, MIN_NAME_CHARS, Name, NameCertificate,
    REVOCATION_VERSION, RecoveryRequest, RegistryError, RegistryTrust, ReleaseRequest,
    RevocationList, RotationRecord, Signature64, check_canonical, check_email_address,
    verify_certificate, verify_key_set_proof,
};
use tiamat_core::identity::{PlayerUuid, public_key_from_bytes};

/// The clocks every verifier is asked at, beside a record's own time.
const EXTREMES: [i64; 2] = [i64::MIN, i64::MAX];

fuzz_target!(|data: &[u8]| {
    certificate(data);
    revocation_list(data);
    rotation(data);
    rotation_chain(data);
    key_set_proof(data);
    claim(data);
    release(data);
    email(data);
    recovery(data);
    cancellation(data);
    email_address(data);
});

/// The certificate parser, then the server's own join check on the same bytes.
fn certificate(data: &[u8]) {
    let Ok(certificate) = NameCertificate::from_bytes(data) else {
        return;
    };
    assert!(
        data.len() <= MAX_CERTIFICATE_BYTES,
        "a certificate over the size cap parsed"
    );
    assert!(
        Name::from_parts(&certificate.name, &certificate.display).is_ok(),
        "a certificate whose name breaks the rules parsed"
    );
    let length = certificate.name.chars().count();
    assert!(
        (MIN_NAME_CHARS..=MAX_NAME_CHARS).contains(&length),
        "a name of {length} characters parsed"
    );
    assert!(
        certificate.expires > certificate.issued,
        "a certificate that expires before it was issued parsed"
    );
    assert!(
        certificate.expires - certificate.issued <= MAX_CERTIFICATE_LIFETIME_SECONDS,
        "a certificate with an absurd lifetime parsed"
    );
    // The signed layout and the wire form are functions of the parsed value;
    // neither may panic on any field the parser allowed.
    let _ = certificate.signing_payload();
    let _ = certificate.to_bytes();

    // Trust the key the certificate names, so verification runs past the trust
    // check to the clock, the signature and the revocation list.
    let Ok(registry_key) = public_key_from_bytes(&certificate.registry_key) else {
        return;
    };
    let trust = RegistryTrust::new(registry_key, None);
    let holder = PlayerUuid::from_bytes(certificate.uuid);
    let empty = RevocationList {
        version: REVOCATION_VERSION,
        registry_key: certificate.registry_key,
        issued: 0,
        entries: Vec::new(),
        signature: Signature64([0; 64]),
    };
    for now in [certificate.issued, EXTREMES[0], EXTREMES[1]] {
        for revocations in [None, Some(&empty)] {
            if let Ok(name) = verify_certificate(data, &holder, now, &trust, revocations) {
                assert_eq!(
                    name.canonical(),
                    certificate.name,
                    "a verified certificate named a different name than it carried"
                );
            }
        }
    }
    // Presented by anyone else, it is refused before anything is checked.
    let mut other = certificate.uuid;
    other[0] ^= 1;
    assert_eq!(
        verify_certificate(
            data,
            &PlayerUuid::from_bytes(other),
            certificate.issued,
            &trust,
            None
        ),
        Err(RegistryError::WrongIdentity),
        "a certificate verified for an identity it does not name"
    );
}

/// The revocation-list parser, then its signature against its own signer.
fn revocation_list(data: &[u8]) {
    let Ok(list) = RevocationList::from_bytes(data) else {
        return;
    };
    assert!(
        list.entries.len() <= MAX_REVOCATIONS,
        "a revocation list over the entry cap parsed"
    );
    let _ = list.signing_payload();
    let _ = list.is_revoked(&[0; 32], 0);
    if let Ok(signer) = public_key_from_bytes(&list.registry_key) {
        let _ = list.verify(&RegistryTrust::new(signer, None));
    }
}

/// The rotation-record parser, then the record offered to a trust that holds
/// its retiring key and the commitment to its own successor — the one trust
/// that could accept it, so every check in `accept_rotation` is reached.
fn rotation(data: &[u8]) {
    let Ok(record) = RotationRecord::from_bytes(data) else {
        return;
    };
    let _ = record.signing_payload();
    let Ok(retiring) = public_key_from_bytes(&record.retired) else {
        return;
    };
    let commitment = public_key_from_bytes(&record.successor)
        .ok()
        .map(|successor| commit_to(&successor));
    let mut trust = RegistryTrust::new(retiring, commitment);
    if trust.accept_rotation(&record).is_ok() {
        assert_eq!(
            trust.current().to_bytes(),
            record.successor,
            "an accepted rotation did not move to its successor"
        );
        assert_eq!(trust.retired().len(), 1);
        assert_eq!(trust.next_commitment(), record.next_key_hash);
    } else {
        assert_eq!(
            trust.current(),
            &retiring,
            "a refused rotation moved the trust"
        );
    }
}

/// The bytes as the rotation records a challenge answer carries — each a
/// little-endian `u16` length and that many bytes, up to one past the cap so the
/// cap itself is reached — walked from a pin at the first record's retiring key
/// committed to its successor, the one pin that lets the walk reach
/// `accept_rotation`.
///
/// The property is the security one: whatever the chain, trust only ever flows
/// from the pin. An accepted chain retired at most as many keys as it carried,
/// and the first key it retired is the pin's.
fn rotation_chain(data: &[u8]) {
    let mut records: Vec<&[u8]> = Vec::new();
    let mut rest = data;
    while records.len() <= MAX_CARRIED_ROTATIONS {
        let [low, high, tail @ ..] = rest else {
            break;
        };
        let len = usize::from(u16::from_le_bytes([*low, *high])).min(tail.len());
        let (record, after) = tail.split_at(len);
        records.push(record);
        rest = after;
    }
    let pin = records
        .first()
        .and_then(|first| RotationRecord::from_bytes(first).ok())
        .and_then(|first| {
            let retiring = public_key_from_bytes(&first.retired).ok()?;
            let commitment = public_key_from_bytes(&first.successor)
                .ok()
                .map(|successor| commit_to(&successor));
            Some(RegistryTrust::new(retiring, commitment))
        });
    let Some(pin) = pin else {
        return;
    };
    for now in [EXTREMES[0], 0, EXTREMES[1]] {
        match pin.advanced_by(&records, now) {
            Ok(advanced) => {
                assert!(
                    records.len() <= MAX_CARRIED_ROTATIONS,
                    "a chain over the cap was accepted"
                );
                assert!(
                    advanced.retired().len() <= records.len(),
                    "a chain retired more keys than it carried"
                );
                if let Some(first) = advanced.retired().first() {
                    assert_eq!(
                        &first.key,
                        pin.current(),
                        "an accepted chain did not start at the pin"
                    );
                }
            }
            Err(RegistryError::TooManyRotations { found }) => {
                assert!(
                    found > MAX_CARRIED_ROTATIONS,
                    "a chain under the cap refused as over it"
                );
            }
            Err(_) => {}
        }
    }
}

/// The key-set proof parser, then the proof walked from its own root to its
/// own last key: as parsed, and relabelled so every link is each kind in turn,
/// and alternating — the rotation and root-commitment rules check commitments
/// and placement before any signature, so they are reached on any bytes.
fn key_set_proof(data: &[u8]) {
    let Ok(proof) = KeySetProof::from_bytes(data) else {
        return;
    };
    assert!(
        !proof.chain.is_empty(),
        "a key-set proof with no links parsed"
    );
    assert!(
        proof.chain.len() <= MAX_KEY_SET_LINKS,
        "a key-set proof over the link cap parsed"
    );
    assert!(
        data.len() <= MAX_KEY_SET_PROOF_BYTES,
        "a key-set proof over the size cap parsed"
    );
    // The link's kind rides in the commitment's tag; the mapping is exact, so
    // what parsed is what it encodes to.
    let encoded = proof.to_bytes().expect("a parsed proof encodes");
    assert_eq!(
        KeySetProof::from_bytes(&encoded).as_ref(),
        Ok(&proof),
        "a proof did not survive its own encoding"
    );
    let Ok(root) = public_key_from_bytes(&proof.root) else {
        return;
    };
    let Some(last) = proof
        .chain
        .last()
        .and_then(|link| public_key_from_bytes(&link.key).ok())
    else {
        return;
    };
    let uuid = PlayerUuid::from_root_key(&root);
    let kinds = [
        LinkKind::AddKey,
        LinkKind::Rotation,
        LinkKind::RootCommitment,
    ];
    let mut variants = vec![proof.clone()];
    for kind in kinds {
        let mut relabelled = proof.clone();
        for link in &mut relabelled.chain {
            link.kind = kind;
        }
        variants.push(relabelled);
    }
    let mut alternating = proof.clone();
    for (index, link) in alternating.chain.iter_mut().enumerate() {
        link.kind = kinds[index % kinds.len()];
    }
    variants.push(alternating);
    for variant in &variants {
        if let Ok(lineage) = verify_key_set_proof(variant, &uuid, &last) {
            lineage_holds(&lineage, &uuid, &last.to_bytes(), Some(variant));
        }
    }
}

/// What every verified lineage promises the registry's memory of keys.
///
/// `signing_key` is raw bytes so this target names no type outside core.
fn lineage_holds(
    lineage: &KeyLineage,
    uuid: &PlayerUuid,
    signing_key: &[u8; 32],
    proof: Option<&KeySetProof>,
) {
    assert_eq!(&lineage.uuid(), uuid, "a lineage rooted elsewhere verified");
    assert_eq!(
        &lineage.signing_key().to_bytes(),
        signing_key,
        "a lineage that does not end at the signing key verified"
    );
    assert_eq!(
        lineage
            .authorities()
            .last()
            .map(|key| key.to_bytes())
            .as_ref(),
        Some(signing_key),
        "the signing key is not the last authority"
    );
    for retired in lineage.retired() {
        assert!(
            !lineage.authorities().contains(&retired.key),
            "a key a rotation retired was counted as an authority"
        );
        assert_ne!(retired.key, retired.successor);
    }
    match proof {
        Some(proof) => {
            assert!(
                lineage.retired().len() < proof.chain.len() + 1,
                "more keys retired than links"
            );
            if lineage.root_commitment().is_some() {
                assert_eq!(
                    proof.chain.first().map(|link| link.kind),
                    Some(LinkKind::RootCommitment),
                    "a root commitment from anywhere but the first link"
                );
            }
        }
        None => {
            let root = public_key_from_bytes(signing_key).expect("a verified key parses");
            assert_eq!(lineage, &KeyLineage::of_root(root));
        }
    }
}

/// The claim parser, then both verifiers at the claim's own time and the
/// extremes.
fn claim(data: &[u8]) {
    let Ok(claim) = ClaimRequest::from_bytes(data) else {
        return;
    };
    assert!(data.len() <= MAX_PROVEN_REQUEST_BYTES);
    assert!(
        Name::from_parts(&claim.name, &claim.display).is_ok(),
        "a claim whose name breaks the rules parsed"
    );
    let _ = claim.signing_payload();
    let _ = claim.work_bits();
    if let Ok(verified) = claim.verify_claim(claim.timestamp, 0) {
        assert_eq!(verified.uuid.as_bytes(), &claim.uuid);
        lineage_holds(
            &verified.lineage,
            &verified.uuid,
            &verified.public_key.to_bytes(),
            claim.key_set.as_ref(),
        );
    }
    let _ = claim.verify_renewal(claim.timestamp);
    for now in EXTREMES {
        let _ = claim.verify_claim(now, 0);
        let _ = claim.verify_renewal(now);
    }
}

/// The release parser, then its verifier.
fn release(data: &[u8]) {
    let Ok(release) = ReleaseRequest::from_bytes(data) else {
        return;
    };
    assert!(data.len() <= MAX_PROVEN_REQUEST_BYTES);
    assert!(check_canonical(&release.name).is_ok());
    let _ = release.signing_payload();
    for now in [release.timestamp, EXTREMES[0], EXTREMES[1]] {
        if let Ok(verified) = release.verify(now) {
            assert_eq!(verified.uuid.as_bytes(), &release.uuid);
            lineage_holds(
                &verified.lineage,
                &verified.uuid,
                &verified.public_key.to_bytes(),
                release.key_set.as_ref(),
            );
        }
    }
}

/// The email-request parser — the current shape with its work and proof, and
/// the first shape without — then its work and its verifier.
fn email(data: &[u8]) {
    let Ok(request) = EmailRequest::from_bytes(data) else {
        return;
    };
    assert!(data.len() <= MAX_PROVEN_REQUEST_BYTES);
    if let Some(address) = &request.address {
        assert!(
            check_email_address(address).is_ok(),
            "an email request with a bad address parsed"
        );
    }
    let _ = request.signing_payload();
    // The work is a hash of the signed payload and the nonce: any nonce, no
    // panic, and none without one.
    assert_eq!(request.work_bits().is_some(), request.work.is_some());
    for now in [request.timestamp, EXTREMES[0], EXTREMES[1]] {
        if let Ok(verified) = request.verify(now) {
            if request.key_set.is_none() {
                // No proof is carried, so only the root can have signed it.
                assert_eq!(
                    PlayerUuid::from_root_key(&verified.public_key),
                    verified.uuid,
                    "an email request verified for a key that is not the root"
                );
            }
            lineage_holds(
                &verified.lineage,
                &verified.uuid,
                &verified.public_key.to_bytes(),
                request.key_set.as_ref(),
            );
        }
    }
}

/// The recovery-request parser, then its verifier.
fn recovery(data: &[u8]) {
    let Ok(request) = RecoveryRequest::from_bytes(data) else {
        return;
    };
    assert!(data.len() <= MAX_PROVEN_REQUEST_BYTES);
    assert!(check_canonical(&request.name).is_ok());
    let _ = request.signing_payload();
    for now in [request.timestamp, EXTREMES[0], EXTREMES[1]] {
        if let Ok(verified) = request.verify(now) {
            if request.key_set.is_none() {
                assert_eq!(
                    PlayerUuid::from_root_key(&verified.public_key),
                    verified.new_uuid,
                    "a recovery verified for a key that is not the new identity's root"
                );
            }
            lineage_holds(
                &verified.lineage,
                &verified.new_uuid,
                &verified.public_key.to_bytes(),
                request.key_set.as_ref(),
            );
        }
    }
}

/// The cancellation parser, then its verifier.
fn cancellation(data: &[u8]) {
    let Ok(request) = CancelRecoveryRequest::from_bytes(data) else {
        return;
    };
    assert!(data.len() <= MAX_PROVEN_REQUEST_BYTES);
    assert!(check_canonical(&request.name).is_ok());
    let _ = request.signing_payload();
    for now in [request.timestamp, EXTREMES[0], EXTREMES[1]] {
        if let Ok(verified) = request.verify(now) {
            assert_eq!(verified.uuid.as_bytes(), &request.uuid);
            lineage_holds(
                &verified.lineage,
                &verified.uuid,
                &verified.public_key.to_bytes(),
                request.key_set.as_ref(),
            );
        }
    }
}

/// The recovery-address check on the bytes as text: it must never panic, and
/// what it accepts must be inside the shape it promises.
fn email_address(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if check_email_address(text).is_ok() {
        assert!(text.len() <= MAX_EMAIL_BYTES);
        assert!(text.is_ascii());
        assert_eq!(text.matches('@').count(), 1);
    }
}
