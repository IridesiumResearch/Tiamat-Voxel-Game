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

use common::{body_text, config, request, request_with_headers, start_with_mail};
use tiamat_core::identity::Identity;
use tiamat_core::identity::keyset::add_key_payload;
use tiamat_core::identity::registry::{
    CANCEL_VERSION, CancelRecoveryRequest, ClaimRequest, EmailRequest, KEY_SET_PROOF_VERSION,
    KeyAuthorisation, KeySetProof, LinkKind, Name, RecoveryRequest, Signature64,
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
    let mut request = EmailRequest::new(identity, address, timestamp);
    request.sign(identity);
    postcard::to_allocvec(&request).expect("encode")
}

/// An email request from an identity that holds no name, carrying its proof
/// of work at `bits` in the request itself.
fn paid_email(identity: &Identity, address: &str, timestamp: i64, bits: u32) -> Vec<u8> {
    let mut request = EmailRequest::new(identity, Some(address), timestamp);
    request.solve_work(bits).expect("work");
    request.sign(identity);
    postcard::to_allocvec(&request).expect("encode")
}

fn recovery_body(identity: &Identity, canonical: &str, timestamp: i64) -> Vec<u8> {
    let mut request = RecoveryRequest::new(identity, canonical, timestamp);
    request.sign(identity);
    postcard::to_allocvec(&request).expect("encode")
}

fn cancel_body(identity: &Identity, canonical: &str, timestamp: i64) -> Vec<u8> {
    let mut request = CancelRecoveryRequest::new(identity, canonical, timestamp);
    assert_eq!(request.version, CANCEL_VERSION);
    request.sign(identity);
    postcard::to_allocvec(&request).expect("encode")
}

/// A cancellation signed by `signer` that names `holder`'s UUID.
fn cancel_body_naming(signer: &Identity, holder: &Identity, canonical: &str, at: i64) -> Vec<u8> {
    let mut request = CancelRecoveryRequest::new(signer, canonical, at);
    request.uuid = *holder.uuid_as_root().as_bytes();
    request.sign(signer);
    postcard::to_allocvec(&request).expect("encode")
}

/// A cancellation from the holder's **second device**, with its key-set proof.
fn device_cancel_body(root: &Identity, device: &Identity, canonical: &str, at: i64) -> Vec<u8> {
    let uuid = root.uuid_as_root();
    let payload = add_key_payload(&uuid, &device.public_key(), None);
    let proof = KeySetProof {
        version: KEY_SET_PROOF_VERSION,
        root: root.public_key().to_bytes(),
        chain: vec![KeyAuthorisation {
            kind: LinkKind::AddKey,
            key: device.public_key().to_bytes(),
            next_key_hash: None,
            added_by: root.public_key().to_bytes(),
            signature: Signature64(root.sign(&payload).to_bytes()),
        }],
    };
    let mut request = CancelRecoveryRequest::for_device(device, &uuid, proof, canonical, at);
    request.sign(device);
    postcard::to_allocvec(&request).expect("encode")
}

/// Presses the button on a mailed link's page: the POST that does the work.
///
/// The link itself (a GET) only shows the page, so a mail scanner that follows
/// it changes nothing; `a_mailed_link_only_opens_a_page` pins that.
fn follow(addr: std::net::SocketAddr, path: &str, token: &str) -> (u16, Vec<u8>) {
    request(addr, "POST", path, format!("t={token}").as_bytes())
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
    let (status, body) = follow(addr, "/v1/email/verify", &token);
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
    let (status, body) = follow(addr, "/v1/recover/confirm", &token);
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

    // The name has not moved yet, and its public page says a move is coming
    // and when: the notice the design promises anyone who looks.
    let (_, body) = request(addr, "GET", "/v1/name/alice", b"");
    let page = body_text(&body);
    assert!(page.contains(&alice.uuid_as_root().to_hex()), "{page}");
    assert!(page.contains("\"recovery_pending\":true"), "{page}");
    assert!(
        page.contains(&format!("\"recovery_notice_until\":{}", NOW + NOTICE)),
        "{page}"
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
    let (status, _) = follow(addr, "/v1/email/verify", &token);
    assert_eq!(status, 200);

    let (status, _) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&mallory, "bobby", NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, _) = follow(addr, "/v1/recover/confirm", &token);
    assert_eq!(status, 200);

    // A stranger cannot cancel it by naming Bob's UUID (public on the name
    // page) and signing with their own key: that would let anyone keep a lost
    // name lost.
    let eve = player(5);
    let (status, body) = request(
        addr,
        "POST",
        "/v1/recover/cancel",
        &cancel_body_naming(&eve, &bob, "bobby", NOW + 30),
    );
    assert_eq!(status, 403, "{}", body_text(&body));
    assert!(
        body_text(&body).contains("not_root"),
        "{}",
        body_text(&body)
    );

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
fn the_holders_second_device_can_cancel_a_recovery() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock.clone());
    let addr = handle.local_addr();
    let carol = player(8);
    let carols_phone = player(9);
    let mallory = player(10);

    let (status, _) = request(addr, "POST", "/v1/claim", &claim_body(&carol, "carol", NOW));
    assert_eq!(status, 200);
    let (status, _) = request(
        addr,
        "POST",
        "/v1/email",
        &email_body(&carol, Some("carol@example.com"), NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, _) = follow(addr, "/v1/email/verify", &token);
    assert_eq!(status, 200);
    let (status, _) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&mallory, "carol", NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, _) = follow(addr, "/v1/recover/confirm", &token);
    assert_eq!(status, 200);

    // The design's "the current key can cancel": the phone is a key of the
    // set, not the root, and it vetoes with the proof that reaches it.
    let (status, body) = request(
        addr,
        "POST",
        "/v1/recover/cancel",
        &device_cancel_body(&carol, &carols_phone, "carol", NOW + 60),
    );
    assert_eq!(status, 200, "{}", body_text(&body));

    clock.set(NOW + NOTICE + 1);
    let (_, body) = request(addr, "GET", "/v1/name/carol", b"");
    assert!(
        body_text(&body).contains(&carol.uuid_as_root().to_hex()),
        "{}",
        body_text(&body)
    );
    assert!(handle.stop());
}

#[test]
fn a_mailed_link_only_opens_a_page() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock);
    let addr = handle.local_addr();
    let erin = player(11);
    let mallory = player(12);
    let (status, _) = request(addr, "POST", "/v1/claim", &claim_body(&erin, "erin", NOW));
    assert_eq!(status, 200);

    // The address link, followed by a scanner: a page, and nothing bound.
    let (status, _) = request(
        addr,
        "POST",
        "/v1/email",
        &email_body(&erin, Some("erin@example.com"), NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    for _ in 0..3 {
        let (status, body) = request(addr, "GET", &format!("/v1/email/verify?t={token}"), b"");
        assert_eq!(status, 200);
        let page = body_text(&body);
        assert!(page.contains("<form method=\"post\""), "{page}");
        assert!(page.contains(&token), "{page}");
    }
    // Without a bound address there is nothing to recover through, which is
    // how "nothing was bound" shows from outside.
    let (status, body) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&mallory, "erin", NOW),
    );
    assert_eq!(status, 409, "{}", body_text(&body));
    assert!(body_text(&body).contains("no_email"), "{body:?}");

    // The button binds it, once; the link's token is then spent.
    let (status, body) = follow(addr, "/v1/email/verify", &token);
    assert_eq!(status, 200, "{}", body_text(&body));
    let (status, _) = follow(addr, "/v1/email/verify", &token);
    assert_eq!(status, 400);

    // The recovery link, followed by a scanner: no notice starts.
    let (status, _) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&mallory, "erin", NOW),
    );
    assert_eq!(status, 202);
    let token = token_of(&mailer);
    let (status, body) = request(addr, "GET", &format!("/v1/recover/confirm?t={token}"), b"");
    assert_eq!(status, 200);
    assert!(body_text(&body).contains("action=\"/v1/recover/confirm\""));
    let (_, body) = request(addr, "GET", "/v1/name/erin", b"");
    assert!(
        body_text(&body).contains("\"recovery_pending\":false"),
        "a GET started the notice: {}",
        body_text(&body)
    );
    // A token that is not one is refused before it reaches a page.
    let (status, _) = request(
        addr,
        "GET",
        "/v1/recover/confirm?t=<script>alert(1)</script>",
        b"",
    );
    assert_eq!(status, 400);

    // The button starts it.
    let (status, body) = follow(addr, "/v1/recover/confirm", &token);
    assert_eq!(status, 200, "{}", body_text(&body));
    let (_, body) = request(addr, "GET", "/v1/name/erin", b"");
    assert!(body_text(&body).contains("\"recovery_pending\":true"));
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

#[test]
fn an_identity_with_no_name_mails_only_with_a_proof_of_work() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock);
    let addr = handle.local_addr();
    let fresh = player(30);

    // Without work, a fresh key is refused the way a claim is, with the bits.
    let (status, body) = request(
        addr,
        "POST",
        "/v1/email",
        &email_body(&fresh, Some("fresh@example.com"), NOW),
    );
    assert_eq!(status, 400, "{}", body_text(&body));
    assert!(
        body_text(&body).contains("\"error\":\"work\""),
        "{}",
        body_text(&body)
    );
    assert!(
        body_text(&body).contains("\"required_bits\":4"),
        "{}",
        body_text(&body)
    );
    assert_eq!(mailer.count(), 0);

    // With the work in the request, it pays and the link goes out.
    let body = paid_email(&fresh, "fresh@example.com", NOW, 4);
    let (status, answer) = request(addr, "POST", "/v1/email", &body);
    assert_eq!(status, 202, "{}", body_text(&answer));
    assert_eq!(mailer.take().len(), 1);

    // The old header carries nothing now: a request with work only there is
    // a request without work.
    let header = format!("Tiamat-Work: {}", u64::MAX);
    let (status, answer) = request_with_headers(
        addr,
        "POST",
        "/v1/email",
        &[header],
        &email_body(&player(31), Some("other@example.com"), NOW),
    );
    assert_eq!(status, 400, "{}", body_text(&answer));
    assert!(body_text(&answer).contains("\"error\":\"work\""));
    assert!(handle.stop());
}

#[test]
fn an_address_s_tags_and_casing_share_one_allowance() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock);
    let addr = handle.local_addr();
    // Three identities, three spellings of one mailbox.
    for (seed, address) in [
        (31u8, "victim+1@example.com"),
        (32, "VICTIM+2@Example.com"),
        (33, "victim@EXAMPLE.COM"),
    ] {
        let body = paid_email(&player(seed), address, NOW, 4);
        let (status, answer) = request(addr, "POST", "/v1/email", &body);
        assert_eq!(status, 202, "{address}: {}", body_text(&answer));
    }
    let body = paid_email(&player(34), "victim+4@example.com", NOW, 4);
    let (status, answer) = request(addr, "POST", "/v1/email", &body);
    assert_eq!(status, 429, "{}", body_text(&answer));
    assert!(
        body_text(&answer).contains("that address has been mailed three times"),
        "{}",
        body_text(&answer)
    );
    assert_eq!(mailer.take().len(), 3);
    assert!(handle.stop());
}

#[test]
fn the_registry_stops_mailing_at_its_hourly_ceiling() {
    let clock = Arc::new(TestClock::new(NOW));
    let mut settings = config(None);
    settings.mail_per_hour = 2;
    let (handle, mailer) = start_with_mail(settings, clock.clone());
    let addr = handle.local_addr();
    for seed in 40u8..42 {
        let body = paid_email(&player(seed), &format!("p{seed}@example.com"), NOW, 4);
        let (status, answer) = request(addr, "POST", "/v1/email", &body);
        assert_eq!(status, 202, "{}", body_text(&answer));
    }
    let body = paid_email(&player(42), "p42@example.com", NOW, 4);
    let (status, answer) = request(addr, "POST", "/v1/email", &body);
    assert_eq!(status, 429, "{}", body_text(&answer));
    assert!(
        body_text(&answer).contains("all the mail it sends in an hour"),
        "{}",
        body_text(&answer)
    );

    // The next hour, the ceiling is back.
    let later = NOW + 60 * 60;
    clock.set(later);
    let body = paid_email(&player(42), "p42@example.com", later, 4);
    let (status, answer) = request(addr, "POST", "/v1/email", &body);
    assert_eq!(status, 202, "{}", body_text(&answer));
    assert_eq!(mailer.take().len(), 3);
    assert!(handle.stop());
}

#[test]
fn a_holder_s_address_is_mailed_six_times_a_day_whoever_asks() {
    let clock = Arc::new(TestClock::new(NOW));
    let (handle, mailer) = start_with_mail(config(None), clock.clone());
    let addr = handle.local_addr();
    let holder = player(60);
    let (status, _) = request(
        addr,
        "POST",
        "/v1/claim",
        &claim_body(&holder, "holder", NOW),
    );
    assert_eq!(status, 200);
    let (status, body) = request(
        addr,
        "POST",
        "/v1/email",
        &email_body(&holder, Some("holder@example.com"), NOW),
    );
    assert_eq!(status, 202, "{}", body_text(&body));
    let (status, _) = follow(addr, "/v1/email/verify", &token_of(&mailer));
    assert_eq!(status, 200);

    // Fresh identities, three an hour — the hourly allowance — for four hours
    // of one day (`NOW` is 08:00): the bind spent one of the day's six, so
    // five recovery mails go out and the rest are refused for the day.
    let mut accepted = 0;
    let mut stranger = 100u8;
    for hour in 1..=4 {
        let at = NOW + hour * 60 * 60;
        clock.set(at);
        for _ in 0..3 {
            stranger += 1;
            let (status, body) = request(
                addr,
                "POST",
                "/v1/recover",
                &recovery_body(&player(stranger), "holder", at),
            );
            match status {
                202 => accepted += 1,
                429 => assert!(
                    body_text(&body).contains("six times today"),
                    "{}",
                    body_text(&body)
                ),
                other => panic!("recovery answered {other}: {}", body_text(&body)),
            }
        }
    }
    assert_eq!(accepted, 5);
    assert_eq!(mailer.take().len(), 5);

    // The next day the address has its allowance back.
    let tomorrow = NOW + 24 * 60 * 60;
    clock.set(tomorrow);
    let (status, body) = request(
        addr,
        "POST",
        "/v1/recover",
        &recovery_body(&player(200), "holder", tomorrow),
    );
    assert_eq!(status, 202, "{}", body_text(&body));
    assert!(handle.stop());
}
