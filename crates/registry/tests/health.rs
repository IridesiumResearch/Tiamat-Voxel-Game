// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `registry healthcheck`, the image's `HEALTHCHECK`, run as the binary it is.
//!
//! The probe is what a container supervisor believes, so it is tested the way
//! Docker runs it: the built binary, a process of its own, judged by its exit
//! code — against a live registry, against nothing, and against a registry
//! that answers but is not healthy.

mod common;

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use common::start;

/// Runs `registry healthcheck` against `bind`, with none of the secrets set.
fn healthcheck(bind: SocketAddr) -> Output {
    Command::new(env!("CARGO_BIN_EXE_registry"))
        .arg("healthcheck")
        .env("REGISTRY_BIND", bind.to_string())
        // The probe must not need the key, the data or the base URL.
        .env_remove("REGISTRY_KEY")
        .env_remove("REGISTRY_DATA")
        .env_remove("REGISTRY_BASE_URL")
        .env_remove("REGISTRY_SMTP_URL")
        .output()
        .expect("run the registry binary")
}

#[test]
fn a_live_registry_is_healthy() {
    let handle = start();
    let output = healthcheck(handle.local_addr());
    assert!(
        output.status.success(),
        "exit {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "healthy");
    assert!(handle.stop());
}

#[test]
fn nothing_listening_is_unhealthy() {
    // A port that was free a moment ago and is closed now.
    let addr = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr");
    let output = healthcheck(addr);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("could not connect"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_answer_other_than_200_is_unhealthy() {
    // A stand-in for a registry whose database has stopped answering. It gives
    // up after ten seconds, so a probe that never knocks fails the test rather
    // than hanging it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    listener.set_nonblocking(true).expect("nonblocking");
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() > deadline {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return false,
            }
        };
        let _ = stream.set_nonblocking(false);
        let mut buffer = [0u8; 512];
        let _ = stream.read(&mut buffer);
        stream
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .is_ok()
    });
    let output = healthcheck(addr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("503"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(server.join().expect("stand-in"), "the probe never asked");
}
