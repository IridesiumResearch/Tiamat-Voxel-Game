// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The claim, renewal and release flows over HTTP, with a moved clock.
//!
//! These drive the real service through a real socket, and verify what comes
//! back with the *engine's* verifier — the point of the certificate is that a
//! server checks it offline, so a test that only looked at the JSON would prove
//! nothing about the thing that matters.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use common::{KEY_SEED, NEXT_HASH, body_text, config, request, start_with};
use tiamat_core::identity::Identity;
use tiamat_core::identity::registry::{
    ClaimRequest, Name, RegistryTrust, ReleaseRequest, verify_certificate,
};
use tiamat_registry::Clock;
use tiamat_registry::wire::ClaimResponse;

const NOW: i64 = 1_800_000_000;

/// A clock the test moves.
#[derive(Debug)]
struct TestClock(AtomicI64);

impl TestClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }
}

impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

fn player(seed: u8) -> Identity {
    Identity::from_seed(&[seed; 32])
}

/// A claim body. `mine` false makes it a renewal (no proof of work).
fn claim_body(
    identity: &Identity,
    canonical: &str,
    display: &str,
    timestamp: i64,
    mine: bool,
) -> Vec<u8> {
    let name = Name::from_parts(canonical, display).expect("a form-valid name");
    let mut claim = ClaimRequest::new(identity, &name, timestamp);
    if mine {
        // The test config asks for four bits; production asks for twenty.
        claim.solve_work(4).expect("a nonce");
    }
    claim.sign(identity);
    postcard::to_allocvec(&claim).expect("encode")
}

fn release_body(identity: &Identity, canonical: &str, timestamp: i64) -> Vec<u8> {
    let mut release = ReleaseRequest::new(identity, canonical, timestamp);
    release.sign(identity);
    postcard::to_allocvec(&release).expect("encode")
}

fn trust() -> RegistryTrust {
    RegistryTrust::new(
        ed25519_dalek::SigningKey::from_bytes(&KEY_SEED).verifying_key(),
        Some(NEXT_HASH),
    )
}

#[test]
fn a_claim_issues_a_certificate_the_engine_can_verify() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let alice = player(1);

    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&alice, "alice", "Alice", NOW, true),
    );
    assert_eq!(status, 200, "{}", body_text(&body));
    let response: ClaimResponse = postcard::from_bytes(&body).expect("postcard");
    assert_eq!(response.pending_recovery_until, None);

    let verified = verify_certificate(
        &response.certificate.to_bytes().expect("bytes"),
        &alice.uuid_as_root(),
        NOW,
        &trust(),
        None,
    )
    .expect("a server verifies the registry's certificate offline");
    assert_eq!(verified.canonical(), "alice");
    assert_eq!(verified.display(), "Alice");

    let (status, body) = request(handle.local_addr(), "GET", "/v1/name/alice", b"");
    assert_eq!(status, 200);
    let page = body_text(&body);
    assert!(page.contains("\"held\":true"), "{page}");
    assert!(page.contains("\"display\":\"Alice\""), "{page}");
    assert!(page.contains("\"reserved\":false"), "{page}");
    assert!(handle.stop());
}

#[test]
fn a_taken_name_is_refused_and_a_free_one_is_reported() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let alice = player(1);
    let mallory = player(2);

    let (status, _) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&alice, "alice", "Alice", NOW, true),
    );
    assert_eq!(status, 200);

    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&mallory, "alice", "Alice", NOW, true),
    );
    assert_eq!(status, 409, "{}", body_text(&body));
    assert!(body_text(&body).contains("\"error\":\"taken\""), "{body:?}");

    let (status, body) = request(handle.local_addr(), "GET", "/v1/name/nobody", b"");
    assert_eq!(status, 200);
    assert!(body_text(&body).contains("\"held\":false"), "{body:?}");
    assert!(handle.stop());
}

#[test]
fn a_renewal_issues_a_fresh_certificate_and_a_claim_without_work_does_not() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let alice = player(1);

    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&alice, "alice", "Alice", NOW, true),
    );
    assert_eq!(status, 200);
    let first: ClaimResponse = postcard::from_bytes(&body).expect("postcard");

    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&alice, "alice", "ALICE", NOW + 60, false),
    );
    assert_eq!(status, 200, "{}", body_text(&body));
    let renewed: ClaimResponse = postcard::from_bytes(&body).expect("postcard");
    assert_eq!(renewed.certificate.serial, first.certificate.serial + 1);
    assert_eq!(renewed.certificate.display, "ALICE");

    // A first claim with no work is a renewal for a name that does not exist.
    let bob = player(3);
    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&bob, "bobby", "Bobby", NOW, false),
    );
    assert_eq!(status, 404, "{}", body_text(&body));
    assert!(body_text(&body).contains("unknown"), "{body:?}");
    assert!(handle.stop());
}

#[test]
fn a_release_holds_the_name_for_its_holder() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let alice = player(1);
    let mallory = player(2);

    request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&alice, "alice", "Alice", NOW, true),
    );
    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/release",
        &release_body(&alice, "alice", NOW),
    );
    assert_eq!(status, 200, "{}", body_text(&body));
    assert!(
        body_text(&body).contains("\"released\":\"alice\""),
        "{body:?}"
    );

    let (status, body) = request(handle.local_addr(), "GET", "/v1/name/alice", b"");
    assert_eq!(status, 200);
    assert!(body_text(&body).contains("\"held\":false"), "{body:?}");
    assert!(body_text(&body).contains("\"free_after\""), "{body:?}");

    // Still inside the thirty-day hold, so nobody else may take it.
    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&mallory, "alice", "Alice", NOW, true),
    );
    assert_eq!(status, 409, "{}", body_text(&body));
    assert!(body_text(&body).contains("\"error\":\"taken\""), "{body:?}");
    assert!(handle.stop());
}

#[test]
fn a_reserved_name_is_refused_with_a_reason() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let alice = player(1);
    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&alice, "admin", "Admin", NOW, true),
    );
    assert_eq!(status, 403, "{}", body_text(&body));
    assert!(
        body_text(&body).contains("\"error\":\"reserved\""),
        "{body:?}"
    );
    assert!(handle.stop());
}

#[test]
fn a_stale_claim_is_refused() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let alice = player(1);
    // Six minutes old: past the five-minute skew.
    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&alice, "alice", "Alice", NOW - 360, true),
    );
    assert_eq!(status, 400, "{}", body_text(&body));
    assert!(body_text(&body).contains("\"error\":\"stale\""), "{body:?}");
    assert!(handle.stop());
}

#[test]
fn too_little_work_is_refused_with_the_difficulty_to_mine() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let alice = player(1);
    let name = Name::from_parts("alice", "Alice").expect("name");

    // A nonce with fewer than the four bits this config asks for. The loop is
    // what makes the test deterministic: a nonce that happens to have four
    // bits would pass, and "solve at three" has a fifty-fifty chance of that.
    let mut claim = ClaimRequest::new(&alice, &name, NOW);
    claim.work = Some(0);
    while claim.work_bits().expect("work") >= 4 {
        claim.work = Some(claim.work.expect("work") + 1);
    }
    claim.sign(&alice);
    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &postcard::to_allocvec(&claim).expect("encode"),
    );
    assert_eq!(status, 400, "{}", body_text(&body));
    let text = body_text(&body);
    assert!(text.contains("\"error\":\"work\""), "{text}");
    assert!(text.contains("\"required_bits\":4"), "{text}");
    assert!(handle.stop());
}

#[test]
fn an_address_gets_two_claims_a_day() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    for (seed, name) in [(1u8, "alice"), (2, "bobby")] {
        let (status, body) = request(
            handle.local_addr(),
            "POST",
            "/v1/claim",
            &claim_body(&player(seed), name, name, NOW, true),
        );
        assert_eq!(status, 200, "{name}: {}", body_text(&body));
    }
    let (status, body) = request(
        handle.local_addr(),
        "POST",
        "/v1/claim",
        &claim_body(&player(3), "carol", "carol", NOW, true),
    );
    assert_eq!(status, 429, "{}", body_text(&body));
    assert!(
        body_text(&body).contains("\"error\":\"limited\""),
        "{body:?}"
    );
    assert!(handle.stop());
}

#[test]
fn a_malformed_body_is_refused_without_a_handler_running() {
    let clock = Arc::new(TestClock::new(NOW));
    let handle = start_with(config(None), clock);
    let (status, body) = request(handle.local_addr(), "POST", "/v1/claim", b"not a claim");
    assert_eq!(status, 400);
    assert!(body_text(&body).contains("malformed"), "{body:?}");
    assert!(handle.stop());
}
