// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `registry healthcheck`: the probe the container runs against itself.
//!
//! The runtime image has no `curl` and no shell worth the name, and a
//! container's health check runs *inside* the container (review B17). So the
//! binary carries its own probe: one blocking HTTP/1.1 `GET /healthz` over TCP
//! to the address the service binds, answered healthy only by a `200` — which
//! the service gives only when its database answers. Plain `std::net`, because
//! a probe that pulled in an HTTP client would be the largest thing in the
//! image.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// How long each step of the probe may take: connecting, then the exchange.
///
/// Two seconds each keeps the whole probe inside the image's five-second
/// `HEALTHCHECK --timeout`, so a hung service is reported as unhealthy by the
/// probe's own exit code rather than by Docker killing it.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The most of a response the probe reads: the status line is all it needs.
const MAX_RESPONSE_BYTES: u64 = 4096;

/// Why the service is not healthy.
#[derive(Debug, thiserror::Error)]
pub enum HealthError {
    /// Nothing answered at the address.
    #[error("could not connect to {addr}")]
    Connect {
        /// Where the probe knocked.
        addr: SocketAddr,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },

    /// The connection failed or timed out mid-exchange.
    #[error("the health check to {addr} failed mid-exchange")]
    Exchange {
        /// Where the probe knocked.
        addr: SocketAddr,
        /// What went wrong.
        #[source]
        source: std::io::Error,
    },

    /// The service answered, and not with `200`.
    #[error("the registry answered {status} to GET /healthz")]
    Status {
        /// The status code it gave.
        status: u16,
    },

    /// What came back was not an HTTP response.
    #[error("the answer to GET /healthz was not an HTTP response")]
    Malformed,
}

/// The address to probe for a service bound at `bind`.
///
/// A service bound to the unspecified address (`0.0.0.0`, the image's default)
/// is reached on loopback; one bound to a specific address is reached there.
#[must_use]
pub fn target(bind: SocketAddr) -> SocketAddr {
    let ip = match bind.ip() {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    SocketAddr::new(ip, bind.port())
}

/// Asks `GET /healthz` at `addr` and succeeds only on a `200`.
///
/// # Errors
///
/// [`HealthError`] saying which step failed; the binary prints it, so the
/// container's health log says why.
pub fn check(addr: SocketAddr, timeout: Duration) -> Result<(), HealthError> {
    let exchange = |source| HealthError::Exchange { addr, source };
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|source| HealthError::Connect { addr, source })?;
    stream.set_read_timeout(Some(timeout)).map_err(exchange)?;
    stream.set_write_timeout(Some(timeout)).map_err(exchange)?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(exchange)?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut response)
        .map_err(exchange)?;
    match status_of(&response) {
        Some(200) => Ok(()),
        Some(status) => Err(HealthError::Status { status }),
        None => Err(HealthError::Malformed),
    }
}

/// The status code of an HTTP/1.x response, from its status line.
fn status_of(response: &[u8]) -> Option<u16> {
    let line_end = response.windows(2).position(|pair| pair == b"\r\n")?;
    let line = std::str::from_utf8(&response[..line_end]).ok()?;
    let mut parts = line.split(' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unspecified_bind_is_probed_on_loopback() {
        let v4: SocketAddr = "0.0.0.0:8080".parse().expect("addr");
        assert_eq!(target(v4), "127.0.0.1:8080".parse().expect("addr"));
        let v6: SocketAddr = "[::]:8080".parse().expect("addr");
        assert_eq!(target(v6), "[::1]:8080".parse().expect("addr"));
        let fixed: SocketAddr = "10.0.0.5:9000".parse().expect("addr");
        assert_eq!(target(fixed), fixed);
    }

    #[test]
    fn the_status_line_is_read_and_anything_else_is_malformed() {
        assert_eq!(status_of(b"HTTP/1.1 200 OK\r\n\r\n"), Some(200));
        assert_eq!(
            status_of(b"HTTP/1.0 503 Service Unavailable\r\n"),
            Some(503)
        );
        assert_eq!(status_of(b"SSH-2.0-OpenSSH\r\n"), None);
        assert_eq!(status_of(b"HTTP/1.1 200 OK"), None, "no line end");
        assert_eq!(status_of(b""), None);
    }
}
