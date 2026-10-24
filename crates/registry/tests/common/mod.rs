// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Shared plumbing for the registry's socket tests.
//!
//! Each integration-test binary gets its own copy of this module, so an
//! individual test file uses part of it and the rest is dead code there. The
//! allow is for the copies, not for the service.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tiamat_registry::clock::{Clock, SystemClock};
use tiamat_registry::mail::SinkMailer;
use tiamat_registry::{Config, ServerHandle, serve_with};

/// The registry key the tests pin.
pub const KEY_SEED: [u8; 32] = [7; 32];

/// The successor commitment the tests pin.
pub const NEXT_HASH: [u8; 32] = [9; 32];

/// A config that is fast to mine and binds loopback.
pub fn config(data_dir: Option<PathBuf>) -> Config {
    Config {
        data_dir,
        bind: "127.0.0.1:0".parse().expect("loopback"),
        key_seed: KEY_SEED,
        next_key_hash: Some(NEXT_HASH),
        base_url: "http://127.0.0.1".to_owned(),
        mail_from: None,
        smtp_url: None,
        trust_forwarded_for: false,
        // Four bits, not twenty: a test that mines for a second per request is
        // a test nobody runs.
        work_bits: 4,
    }
}

/// A service with no data directory and the system clock.
pub fn start() -> ServerHandle {
    serve_with(
        config(None),
        Arc::new(SinkMailer::default()),
        Arc::new(SystemClock),
    )
    .expect("start")
}

/// A service with a chosen clock.
pub fn start_with(config: Config, clock: Arc<dyn Clock>) -> ServerHandle {
    serve_with(config, Arc::new(SinkMailer::default()), clock).expect("start")
}

/// A service with a chosen clock, and the mail sink it sends to.
///
/// The recovery tests follow the links out of the sink, which is the whole
/// reason the transport is a trait.
pub fn start_with_mail(config: Config, clock: Arc<dyn Clock>) -> (ServerHandle, Arc<SinkMailer>) {
    let sink = Arc::new(SinkMailer::default());
    let handle = serve_with(config, sink.clone(), clock).expect("start");
    (handle, sink)
}

/// A directory under the workspace target: this environment's sandbox denies
/// writes under the system temp directory, and the suite should run anywhere.
pub fn test_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/registry-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test dir");
    dir
}

/// Sends one request and reads the status and body.
pub fn request(addr: SocketAddr, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    request_with_length(addr, method, path, body.len(), body)
}

/// Sends one request with a `Content-Length` that may not match the body sent.
pub fn request_with_length(
    addr: SocketAddr,
    method: &str,
    path: &str,
    declared: usize,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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

/// The body as text.
pub fn body_text(body: &[u8]) -> String {
    String::from_utf8(body.to_vec()).expect("utf8")
}
