// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The registry over a real socket: status lines, caps, and a restart.
//!
//! These are the transport's tests, not the flows' — a claim, a renewal and a
//! recovery get their own files as they land. What is pinned here is what a
//! stranger can reach: the two public endpoints answer, an unknown path and a
//! wrong method are told apart, and a body over the cap is refused before it is
//! read.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tiamat_registry::clock::SystemClock;
use tiamat_registry::mail::SinkMailer;
use tiamat_registry::wire::KeysResponse;
use tiamat_registry::{Config, ServerHandle, serve_with};

const KEY_SEED: [u8; 32] = [7; 32];
const NEXT_HASH: [u8; 32] = [9; 32];

fn config(data_dir: Option<PathBuf>) -> Config {
    Config {
        data_dir,
        bind: "127.0.0.1:0".parse().expect("loopback"),
        key_seed: KEY_SEED,
        next_key_hash: Some(NEXT_HASH),
        base_url: "http://127.0.0.1".to_owned(),
        mail_from: None,
        smtp_url: None,
        trust_forwarded_for: false,
    }
}

fn start() -> ServerHandle {
    serve_with(
        config(None),
        Arc::new(SinkMailer::default()),
        Arc::new(SystemClock),
    )
    .expect("start")
}

/// A directory under the workspace target: this environment's sandbox denies
/// writes under the system temp directory, and the suite should run anywhere.
fn test_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/registry-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test dir");
    dir
}

/// Sends one request and reads the status and body.
fn request(addr: SocketAddr, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    request_with_length(addr, method, path, body.len(), body)
}

/// Sends one request with a `Content-Length` that may not match the body sent.
///
/// The mismatch is deliberate in one test: it proves the cap is applied to the
/// declared length *before* the body is read, which is the only version of the
/// check that stops an allocation.
fn request_with_length(
    addr: SocketAddr,
    method: &str,
    path: &str,
    declared: usize,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Length: {declared}\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).expect("write head");
    // The server may close the connection before the whole body is written
    // (that is what a 413 does); a failed write is the expected outcome there.
    let _ = stream.write_all(body);

    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).expect("read response");
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a response with headers");
    let head = String::from_utf8_lossy(&response[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .expect("a status line")
        .parse()
        .expect("a status code");
    (status, response[split + 4..].to_vec())
}

fn body_text(body: &[u8]) -> String {
    String::from_utf8(body.to_vec()).expect("utf8")
}

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
        request_with_length(handle.local_addr(), "POST", "/healthz", 40 * 1024, b"");
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
