// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The registry over a real socket: status lines, caps, and a restart.
//!
//! These are the transport's tests, not the flows' — a claim, a renewal and a
//! recovery get their own files. What is pinned here is what a stranger can
//! reach: the public endpoints answer, an unknown path and a wrong method are
//! told apart, and a body over the cap is refused before it is read.

mod common;

use std::sync::Arc;

use common::{
    KEY_SEED, NEXT_HASH, body_text, config, request, request_with_length, start, test_dir,
};
use tiamat_registry::clock::SystemClock;
use tiamat_registry::mail::SinkMailer;
use tiamat_registry::serve_with;
use tiamat_registry::wire::KeysResponse;

#[test]
fn healthz_says_the_database_answers() {
    let handle = start();
    let (status, body) = request(handle.local_addr(), "GET", "/healthz", b"");
    assert_eq!(status, 200);
    assert_eq!(body_text(&body), "{\"status\":\"ok\"}");
    assert!(handle.stop());
}

#[test]
fn the_keys_endpoint_reports_the_pinned_key_and_its_successor() {
    let handle = start();
    let (status, body) = request(handle.local_addr(), "GET", "/v1/keys", b"");
    assert_eq!(status, 200);
    let keys: KeysResponse = postcard::from_bytes(&body).expect("postcard");
    let expected = ed25519_dalek::SigningKey::from_bytes(&KEY_SEED)
        .verifying_key()
        .to_bytes();
    assert_eq!(keys.current, expected);
    assert_eq!(keys.next, Some(NEXT_HASH));
    assert!(keys.rotations.is_empty());
    assert!(handle.stop());
}

#[test]
fn an_unknown_path_is_not_found_and_a_wrong_method_is_not_allowed() {
    let handle = start();
    let (status, body) = request(handle.local_addr(), "GET", "/v1/nothing", b"");
    assert_eq!(status, 404);
    assert!(body_text(&body).contains("not_found"), "{body:?}");

    let (status, body) = request(handle.local_addr(), "POST", "/healthz", b"");
    assert_eq!(status, 405);
    assert!(body_text(&body).contains("method"), "{body:?}");
    assert!(handle.stop());
}

#[test]
fn a_body_over_the_cap_is_refused_before_it_is_read() {
    let handle = start();
    // 40 KiB declared, and not one byte of it sent: the answer arrives anyway,
    // which is the property — the service never waits for a body it has
    // already decided not to accept.
    let (status, body) =
        request_with_length(handle.local_addr(), "POST", "/v1/claim", 40 * 1024, b"");
    assert_eq!(status, 413);
    assert!(body_text(&body).contains("too_large"), "{body:?}");
    assert!(handle.stop());
}

#[test]
fn a_file_backed_registry_keeps_its_database_across_a_restart() {
    let dir = test_dir("http-restart");
    {
        let handle = serve_with(
            config(Some(dir.clone())),
            Arc::new(SinkMailer::default()),
            Arc::new(SystemClock),
        )
        .expect("start");
        let (status, _) = request(handle.local_addr(), "GET", "/healthz", b"");
        assert_eq!(status, 200);
        assert!(handle.stop());
    }
    assert!(dir.join("registry.sqlite").is_file(), "no database on disk");
    {
        let handle = serve_with(
            config(Some(dir)),
            Arc::new(SinkMailer::default()),
            Arc::new(SystemClock),
        )
        .expect("reopen");
        let (status, _) = request(handle.local_addr(), "GET", "/healthz", b"");
        assert_eq!(status, 200);
        assert!(handle.stop());
    }
}
