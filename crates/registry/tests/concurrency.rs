// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The service's concurrency shape over a real socket (review B5 and B11).
//!
//! Three promises: a client trickling its body stalls one worker and not the
//! service; a full pool says `503 busy` at once rather than queueing without
//! end; and a request answered `504` was not carried out — the review's
//! "claim answered 504 then succeeds, so the retry gets `already_named`".

mod common;

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{body_text, config, open, read_response, request, start, start_with, test_dir};
use tiamat_core::identity::Identity;
use tiamat_core::identity::registry::{ClaimRequest, EmailRequest, Name};
use tiamat_registry::mail::{Mail, MailError, Mailer};
use tiamat_registry::{Clock, serve_with};

const NOW: i64 = 1_800_000_000;

/// A clock that stands still, so a claim signed at `NOW` is always fresh.
#[derive(Debug)]
struct Still;

impl Clock for Still {
    fn now(&self) -> i64 {
        NOW
    }
}

/// More than `tiny_http` reads itself: a body of 1024 bytes or fewer it reads
/// on its own connection thread before handing the request over, so a slow
/// body has to be longer than that to reach the registry's workers.
const SLOW_BODY: usize = 4000;

fn claim_body(identity: &Identity, canonical: &str) -> Vec<u8> {
    let name = Name::from_parts(canonical, canonical).expect("name");
    let mut claim = ClaimRequest::new(identity, &name, NOW);
    claim.solve_work(4).expect("work");
    claim.sign(identity);
    postcard::to_allocvec(&claim).expect("encode")
}

#[test]
fn a_slow_body_holds_one_worker_and_not_the_service() {
    let handle = start();
    let addr = handle.local_addr();

    // A claim whose body starts and then stops.
    let mut slow = open(addr, "POST", "/v1/claim", SLOW_BODY, &[]);
    slow.write_all(&[0; 10]).expect("the first trickle");
    std::thread::sleep(Duration::from_millis(200));

    // Everyone else is served meanwhile — before the pool, this waited for the
    // slow body to finish, however long that took.
    let started = Instant::now();
    let (status, body) = request(addr, "GET", "/healthz", b"");
    assert_eq!(status, 200, "{}", body_text(&body));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a slow body stalled another request for {:?}",
        started.elapsed()
    );

    // The rest of the slow body arrives after its deadline: it is refused as
    // late, never handed to a handler.
    std::thread::sleep(Duration::from_millis(2_100));
    let _ = slow.write_all(&[0; SLOW_BODY - 10]);
    let (status, body) = read_response(&mut slow);
    assert_eq!(status, 408, "{}", body_text(&body));
    assert!(
        body_text(&body).contains("\"error\":\"timeout\""),
        "{body:?}"
    );
    assert!(handle.stop());
}

#[test]
fn a_full_pool_answers_busy_at_once() {
    let mut settings = config(None);
    settings.workers = 1;
    let handle = start_with(settings, Arc::new(Still));
    let addr = handle.local_addr();

    // The one worker is held by a slow body; one request waits in the queue.
    let mut slow = open(addr, "POST", "/v1/claim", SLOW_BODY, &[]);
    slow.write_all(&[0; 10]).expect("the first trickle");
    std::thread::sleep(Duration::from_millis(200));
    let queued = std::thread::spawn(move || request(addr, "GET", "/healthz", b"").0);
    std::thread::sleep(Duration::from_millis(200));

    // The next is told at once, by the listener, without a worker.
    let started = Instant::now();
    let (status, body) = request(addr, "GET", "/healthz", b"");
    assert_eq!(status, 503, "{}", body_text(&body));
    assert!(body_text(&body).contains("\"error\":\"busy\""), "{body:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the busy answer took {:?}",
        started.elapsed()
    );

    // The queued request is answered when the worker gives up on the slow
    // body: served if its own deadline allows, told the registry is busy if
    // not — never left hanging.
    let queued = queued.join().expect("the queued request");
    assert!(
        queued == 200 || queued == 503,
        "the queued request got {queued}"
    );
    drop(slow);
    assert!(handle.stop());
}

#[test]
fn a_claim_answered_504_was_not_made() {
    let dir = test_dir("deadline-commit");
    let mut settings = config(Some(dir.clone()));
    settings.request_timeout = Duration::from_millis(500);
    let handle = start_with(settings, Arc::new(Still));
    let alice = Identity::from_seed(&[1; 32]);
    let body = claim_body(&alice, "alice");

    // Another writer — the admin CLI, say — holds SQLite's write lock for
    // longer than the request's deadline, so the claim reaches its commit late.
    let blocker = rusqlite::Connection::open(dir.join("registry.sqlite")).expect("db");
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold the lock");
    let started = Instant::now();
    let (status, answer) = request(handle.local_addr(), "POST", "/v1/claim", &body);
    assert_eq!(status, 504, "{}", body_text(&answer));
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "the 504 waited for the work: {:?}",
        started.elapsed()
    );

    // The lock goes, the abandoned claim reaches its commit — and is refused.
    std::thread::sleep(Duration::from_millis(1_200).saturating_sub(started.elapsed()));
    blocker.execute_batch("ROLLBACK").expect("release the lock");
    std::thread::sleep(Duration::from_millis(600));
    let count = |sql: &str| -> i64 { blocker.query_row(sql, [], |row| row.get(0)).expect("count") };
    assert_eq!(count("SELECT COUNT(*) FROM names"), 0, "the claim was made");
    assert_eq!(count("SELECT COUNT(*) FROM issued"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM limits"), 0);
    assert_eq!(
        count("SELECT COUNT(*) FROM audit WHERE what LIKE 'claim %'"),
        0
    );
    assert!(handle.stop());

    // The client's retry — the same request — is a claim, not `already_named`.
    let handle = start_with(config(Some(dir)), Arc::new(Still));
    let (status, answer) = request(handle.local_addr(), "POST", "/v1/claim", &body);
    assert_eq!(status, 200, "{}", body_text(&answer));
    assert!(handle.stop());
}

/// A transport that takes longer than a request's deadline to hand a mail off.
struct SlowMail;

impl Mailer for SlowMail {
    fn send(&self, _mail: &Mail) -> Result<(), MailError> {
        std::thread::sleep(Duration::from_millis(2_500));
        Ok(())
    }
}

#[test]
fn work_that_committed_is_answered_with_its_result_however_late() {
    // The review's case after B6: the link is committed, then the mail takes
    // longer than the deadline. Before, the client was told `504` for a link
    // that was stored and then delivered; the answer is now the truth.
    let handle = serve_with(config(None), Arc::new(SlowMail), Arc::new(Still)).expect("start");
    let addr = handle.local_addr();
    let alice = Identity::from_seed(&[2; 32]);
    let (status, answer) = request(addr, "POST", "/v1/claim", &claim_body(&alice, "alice"));
    assert_eq!(status, 200, "{}", body_text(&answer));

    let mut email = EmailRequest::new(&alice, Some("alice@example.com"), NOW);
    email.sign(&alice);
    let started = Instant::now();
    let (status, answer) = request(
        addr,
        "POST",
        "/v1/email",
        &postcard::to_allocvec(&email).expect("encode"),
    );
    assert_eq!(status, 202, "{}", body_text(&answer));
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the mail was not slow"
    );
    assert!(handle.stop());
}
