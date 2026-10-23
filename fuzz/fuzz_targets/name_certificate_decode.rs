// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Fuzzes the name-registry parsers.
//!
//! **Charter rule 14: a parser ships with its fuzz target.** Every one of these
//! records arrives from a peer a server does not trust: a client's `Hello`
//! carries a certificate, and the registry service reads claims, releases,
//! rotation records and revocation lists straight off HTTP. So until a
//! signature holds, these bytes are anybody's — a captured connection, a proxy
//! in the way, an operator pasting a record from somewhere.
//!
//! The property is that every parse is **total and capped**: any input is a
//! value or a `RegistryError`, never a panic; a certificate that parsed holds
//! the name rules and a sane, forward lifetime; a revocation list is inside its
//! entry cap; and the signing layout of a parsed record is a function that
//! cannot panic on any field the parser let through. Then verification's clock
//! arithmetic, exercised at the extremes of `i64` so an overflow would be a
//! finding rather than a surprise in production.
//!
//! Run: `cargo +nightly fuzz run name_certificate_decode`

#![no_main]

use libfuzzer_sys::fuzz_target;
use tiamat_core::identity::registry::{
    ClaimRequest, MAX_CERTIFICATE_BYTES, MAX_CERTIFICATE_LIFETIME_SECONDS, MAX_NAME_CHARS,
    MAX_REVOCATIONS, MIN_NAME_CHARS, Name, NameCertificate, ReleaseRequest, RevocationList,
    RotationRecord,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(certificate) = NameCertificate::from_bytes(data) {
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
        // The signed layout and the wire form are functions of the parsed
        // value; neither may panic on any field the parser allowed.
        let _ = certificate.signing_payload();
        let _ = certificate.to_bytes();
    }

    if let Ok(list) = RevocationList::from_bytes(data) {
        assert!(
            list.entries.len() <= MAX_REVOCATIONS,
            "a revocation list over the entry cap parsed"
        );
        let _ = list.signing_payload();
        let _ = list.is_revoked(&[0; 32], 0);
    }

    if let Ok(record) = RotationRecord::from_bytes(data) {
        let _ = record.signing_payload();
    }

    if let Ok(claim) = ClaimRequest::from_bytes(data) {
        assert!(
            Name::from_parts(&claim.name, &claim.display).is_ok(),
            "a claim whose name breaks the rules parsed"
        );
        let _ = claim.signing_payload();
        let _ = claim.work_bits();
        // The clock check runs before the signature, so a parsed request with
        // any timestamp reaches the arithmetic that could overflow.
        let _ = claim.verify_claim(i64::MAX, 0);
        let _ = claim.verify_claim(i64::MIN, 0);
        let _ = claim.verify_renewal(i64::MIN);
    }

    if let Ok(release) = ReleaseRequest::from_bytes(data) {
        let _ = release.signing_payload();
        let _ = release.verify(i64::MIN);
    }
});
