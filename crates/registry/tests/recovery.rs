// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The email and recovery flows over HTTP, with the mails read from a sink.
//!
//! This is the design's §4 walked end to end: a bound address, a recovery from
//! a fresh identity, the public notice, the holder's renewal carrying it, and
//! the name moving when the fourteen days are up — plus the cancellation that
//! stops a stolen mailbox.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use common::{body_text, config, request, start_with_mail};
use tiamat_core::identity::Identity;
use tiamat_core::identity::registry::{
    CANCEL_VERSION, CancelRecoveryRequest, ClaimRequest, EMAIL_VERSION, EmailRequest, Name,
    RECOVERY_VERSION, RecoveryRequest,
};
use tiamat_registry::Clock;
use tiamat_registry::mail::SinkMailer;
use tiamat_registry::wire::ClaimResponse;

const NOW: i64 = 1_800_000_000;
const NOTICE: i64 = 14 * 24 * 60 * 60;

#[derive(Debug)]
struct TestClock(AtomicI64);

impl TestClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::Relaxed);
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

fn claim_body(identity: &Identity, canonical: &str, timestamp: i64) -> Vec<u8> {
    let name = Name::from_parts(canonical, canonical).expect("name");
    let mut claim = ClaimRequest::new(identity, &name, timestamp);
    claim.solve_work(4).expect("work");
    claim.sign(identity);
    postcard::to_allocvec(&claim).expect("encode")
}

fn email_body(identity: &Identity, address: Option<&str>, timestamp: i64) -> Vec<u8> {
    let mut request = EmailRequest {
        version: EMAIL_VERSION,
        uuid: *identity.uuid_as_root().as_bytes(),
        public_key: [0; 32],
        address: address.map(str::to_owned),
        timestamp,
        signature: tiamat_core::identity::registry::Signature64([0; 64]),
    };
    request.sign(identity);
    postcard::to_allocvec(&request).expect("encode")
}

fn recovery_body(identity: &Identity, canonical: &str, timestamp: i64) -> Vec<u8> {
    let mut request = RecoveryRequest {
        version: RECOVERY_VERSION,
        name: canonical.to_owned(),
        new_uuid: *identity.uuid_as_root().as_bytes(),
        public_key: [0; 32],
        timestamp,
        signature: tiamat_core::identity::registry::Signature64([0; 64]),
    };
    request.sign(identity);
    postcard::to_allocvec(&request).expect("encode")
}

fn cancel_body(identity: &Identity, canonical: &str, timestamp: i64) -> Vec<u8> {
    let mut request = CancelRecoveryRequest {
        version: CANCEL_VERSION,
        name: canonical.to_owned(),
        uuid: *identity.uuid_as_root().as_bytes(),
        public_key: [0; 32],
        timestamp,
        signature: tiamat_core::identity::registry::Signature64([0; 64]),
    };
    request.sign(identity);
    postcard::to_allocvec(&request).expect("encode")
}

/// The token from the one link in the mail.
fn token_of(mailer: &SinkMailer) -> String {
    let mails = mailer.take();
    assert_eq!(mails.len(), 1, "expected exactly one mail");
    mails[0]
        .body
        .split("t=")
        .nth(1)
        .expect("a link")
        .split_whitespace()
        .next()
        .expect("a token")
        .to_owned()
}

#[test]
fn a_recovery_moves_a_name_to_its_new_identity() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock.clone());
    let addr = handle.local_addr();
    let alice = player(1);
    let new_device = player(2);

    // The name, held by the old identity.
    let (status, body) = request(addr, "POST", "/v1/claim", &claim_body(&alice, "alice", NOW));
    assert_eq!(status, 200, "{}", body_text(&body));

    // A recovery address, bound only by following the link.
    let (status, _) = request(
        addr,
        "POST",
        "/v1/email",
        &email_body(&alice, Some("alice@example.com"), NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, body) = request(addr, "GET", &format!("/v1/email/verify?t={token}"), b"");
    assert_eq!(status, 200, "{}", body_text(&body));

    // A fresh identity asks to recover it; the link mails the bound address.
    let (status, _) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&new_device, "alice", NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, body) = request(addr, "GET", &format!("/v1/recover/confirm?t={token}"), b"");
    assert_eq!(status, 200, "{}", body_text(&body));
    assert!(
        body_text(&body).contains(&format!("\"notice_until\":{}", NOW + NOTICE)),
        "{body:?}"
    );

    // The holder's renewal carries the notice, which is how they learn of it.
    let mut renewal = ClaimRequest::new(
        &alice,
        &Name::from_parts("alice", "alice").expect("name"),
        NOW + 10,
    );
    renewal.sign(&alice);
    let (status, body) = request(
        addr,
        "POST",
        "/v1/claim",
        &postcard::to_allocvec(&renewal).expect("encode"),
    );
    assert_eq!(status, 200, "{}", body_text(&body));
    let renewed: ClaimResponse = postcard::from_bytes(&body).expect("postcard");
    assert_eq!(renewed.pending_recovery_until, Some(NOW + NOTICE));

    // The name has not moved yet.
    let (_, body) = request(addr, "GET", "/v1/name/alice", b"");
    assert!(
        body_text(&body).contains(&alice.uuid_as_root().to_hex()),
        "{body:?}"
    );

    // Fourteen days later, reading the name completes the move.
    clock.set(NOW + NOTICE);
    let (status, body) = request(addr, "GET", "/v1/name/alice", b"");
    assert_eq!(status, 200);
    let page = body_text(&body);
    assert!(
        page.contains(&new_device.uuid_as_root().to_hex()),
        "the name did not move: {page}"
    );
    assert!(page.contains("\"held\":true"), "{page}");
    assert!(handle.stop());
}

#[test]
fn the_holder_can_cancel_a_recovery() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock.clone());
    let addr = handle.local_addr();
    let bob = player(3);
    let mallory = player(4);

    let (status, _) = request(addr, "POST", "/v1/claim", &claim_body(&bob, "bobby", NOW));
    assert_eq!(status, 200);
    let (status, _) = request(
        addr,
        "POST",
        "/v1/email",
        &email_body(&bob, Some("bob@example.com"), NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, _) = request(addr, "GET", &format!("/v1/email/verify?t={token}"), b"");
    assert_eq!(status, 200);

    let (status, _) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&mallory, "bobby", NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, _) = request(addr, "GET", &format!("/v1/recover/confirm?t={token}"), b"");
    assert_eq!(status, 200);

    // The holder, still playing, ends it with one signed request.
    let (status, body) = request(
        addr,
        "POST",
        "/v1/recover/cancel",
        &cancel_body(&bob, "bobby", NOW + 60),
    );
    assert_eq!(status, 200, "{}", body_text(&body));

    // After the notice would have ended, the name is still Bob's.
    clock.set(NOW + NOTICE + 1);
    let (_, body) = request(addr, "GET", "/v1/name/bobby", b"");
    assert!(
        body_text(&body).contains(&bob.uuid_as_root().to_hex()),
        "{}",
        body_text(&body)
    );
    assert!(handle.stop());
}

#[test]
fn a_recovery_without_a_bound_address_is_refused() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, _mailer) = start_with_mail(config(None), clock);
    let addr = handle.local_addr();
    let alice = player(5);
    let mallory = player(6);
    let (status, _) = request(addr, "POST", "/v1/claim", &claim_body(&alice, "alice", NOW));
    assert_eq!(status, 200);

    let (status, body) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&mallory, "alice", NOW),
    );
    assert_eq!(status, 409, "{}", body_text(&body));
    assert!(body_text(&body).contains("no_email"), "{body:?}");
    assert!(handle.stop());
}

#[test]
fn mail_is_limited_per_address() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock);
    let addr = handle.local_addr();
    let alice = player(7);
    request(addr, "POST", "/v1/claim", &claim_body(&alice, "alice", NOW));
    for _ in 0..3 {
        let (status, body) = request(
            addr,
            "POST",
            "/v1/email",
            &email_body(&alice, Some("alice@example.com"), NOW),
        );
        assert_eq!(status, 202, "{}", body_text(&body));
        let _ = mailer.take();
    }
    let (status, body) = request(
        addr,
        "POST",
        "/v1/email",
        &email_body(&alice, Some("alice@example.com"), NOW),
    );
    assert_eq!(status, 429, "{}", body_text(&body));
    assert!(body_text(&body).contains("limited"), "{body:?}");
    assert!(handle.stop());
}
