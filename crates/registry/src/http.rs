// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The transport: one blocking HTTP server, a body cap, a request deadline, and
//! a status line for everything that is not a handler.
//!
//! `tiny_http` is blocking and thread-per-connection. That is the right size
//! for this service — a handful of requests per player per ninety days — and it
//! has no TLS of its own, deliberately: TLS is the reverse proxy's job at
//! Dokploy, and so is timing out a slow **client**. What this module owns is the
//! part that touches a stranger's bytes, so it is where the caps live: a request
//! body is refused **before** it is read, a handler's work is bounded at two
//! seconds ([`REQUEST_TIMEOUT`]), and a path this service does not serve gets a
//! status line rather than a handler.

use std::io::{ErrorKind, Read};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use tiny_http::{Header, ListenAddr, Request, Response, Server, StatusCode};

use crate::api;
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::mail::Mailer;
use crate::service::{Service, StartError};

/// The largest request body the registry will read.
///
/// The largest record a client sends is a claim: a name, a display, a UUID, a
/// public key, a timestamp, a nonce and a signature — under two hundred bytes.
/// Thirty-two kilobytes is the design's cap, and it is checked from
/// `Content-Length` before a byte of body is read as well as enforced on the
/// read itself, so a lying header cannot make the service allocate.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

/// A running registry.
pub struct ServerHandle {
    addr: SocketAddr,
    server: Arc<Server>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ServerHandle {
    /// The address actually bound, which matters when the config asked for port
    /// zero (tests do).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops the listener and waits for its thread.
    ///
    /// Returns whether the thread ended cleanly. Idempotent, and `Drop` does
    /// the same thing, so a test that forgets cannot leak a thread.
    pub fn stop(mut self) -> bool {
        self.shutdown()
    }

    fn shutdown(&mut self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        // Wake a thread parked in `recv_timeout`. The timeout alone would also
        // do it within 200 ms; this makes stopping immediate.
        self.server.unblock();
        match self.thread.take() {
            Some(thread) => thread.join().is_ok(),
            None => true,
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

/// Starts the registry with the system clock.
///
/// # Errors
///
/// [`StartError`] when the service cannot open its database, its key does not
/// match the chain, or the port cannot be bound. A configuration error is a
/// failed start, never a half-serving process.
pub fn serve(config: Config, mailer: Arc<dyn Mailer>) -> Result<ServerHandle, StartError> {
    serve_with(config, mailer, Arc::new(SystemClock))
}

/// Starts the registry with an injected clock, which is what the bot tests use.
///
/// # Errors
///
/// As [`serve`].
pub fn serve_with(
    config: Config,
    mailer: Arc<dyn Mailer>,
    clock: Arc<dyn Clock>,
) -> Result<ServerHandle, StartError> {
    let service = Arc::new(Service::open(config, mailer, clock)?);
    let asked = service.config().bind;
    let server = Arc::new(Server::http(asked).map_err(|source| StartError::Listen {
        addr: asked,
        source,
    })?);
    // `ListenAddr` has one variant with the feature set this crate uses (no
    // TLS, no Unix sockets), so this destructuring is irrefutable.
    let ListenAddr::IP(addr) = server.server_addr();

    let stop = Arc::new(AtomicBool::new(false));
    let thread_server = Arc::clone(&server);
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("registry-http".to_owned())
        .spawn(move || run(&thread_server, &service, &thread_stop))
        .map_err(StartError::Thread)?;

    Ok(ServerHandle {
        addr,
        server,
        stop,
        thread: Some(thread),
    })
}

/// The accept loop, until the stop flag is set.
fn run(server: &Server, service: &Arc<Service>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(request)) => serve_one(request, Arc::clone(service)),
            Ok(None) => {}
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(err) => {
                // A listener that cannot accept is not a service. Stop rather
                // than spin: the supervisor restarts the container.
                tracing::error!(error = %err, "the registry stopped accepting connections");
                break;
            }
        }
    }
}

/// How long a request's work may take before the client is told to retry.
///
/// The design's two seconds. The database's own `busy_timeout` is the same
/// number, but this bounds the *whole* handler: a query that waits, a
/// verification that is slow, a machine whose disk has stopped answering. A
/// client gets a status line instead of a socket that never closes.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// The outcome of work done under a deadline.
enum Deadline<T> {
    /// It finished in time.
    Met(T),
    /// It was still running when the deadline passed; its thread continues
    /// into a dropped channel and its result is discarded.
    Missed,
    /// No thread could be started for it.
    NoThread,
}

/// Runs `work` on its own thread and waits at most `timeout` for the result.
///
/// The serving thread must keep owning the request — only it can answer — so
/// the handler runs beside it and the answer is handed back through a channel.
/// A timed-out worker is **not** killed (Rust cannot do that safely); it ends
/// when its database call does, which is bounded by the same two seconds.
fn within<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Deadline<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("registry-request".to_owned())
        .spawn(move || {
            let _ = sender.send(work());
        });
    match spawned {
        Ok(_) => match receiver.recv_timeout(timeout) {
            Ok(value) => Deadline::Met(value),
            Err(_) => Deadline::Missed,
        },
        Err(_) => Deadline::NoThread,
    }
}

/// Reads the body (under its cap), dispatches under the deadline, and answers.
fn serve_one(mut request: Request, service: Arc<Service>) {
    let method = request.method().as_str().to_owned();
    let url = request.url().to_owned();
    let client = client_ip(&request, service.config().trust_forwarded_for);
    let reply = match read_body(&mut request) {
        Ok(body) => {
            let worker_service = Arc::clone(&service);
            match within(REQUEST_TIMEOUT, move || {
                dispatch(&worker_service, &method, &url, client, &body)
            }) {
                Deadline::Met(reply) => reply,
                Deadline::Missed => Reply::error(
                    504,
                    "timeout",
                    "the registry took longer than two seconds to answer; try again",
                ),
                Deadline::NoThread => Reply::error(
                    503,
                    "busy",
                    "the registry could not start a request worker; try again in a moment",
                ),
            }
        }
        Err(reply) => reply,
    };
    if let Err(err) = request.respond(reply.into_response()) {
        tracing::warn!(error = %err, "a response could not be written");
    }
}

/// The prefix of the public name page.
const NAME_PREFIX: &str = "/v1/name/";

/// Routes a request to a handler, or to the status line that says why not.
fn dispatch(
    service: &Service,
    method: &str,
    url: &str,
    client: Option<IpAddr>,
    body: &[u8],
) -> Reply {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    match (method, path) {
        ("GET", "/healthz") => api::health(service),
        ("GET", "/v1/keys") => api::keys(service),
        ("GET", "/v1/revocations") => api::revocations(service),
        ("POST", "/v1/claim") => api::claim(service, body, client),
        ("POST", "/v1/release") => api::release(service, body),
        ("POST", "/v1/email") => api::email(service, body, client),
        ("GET", "/v1/email/verify") => api::email_verify(service, query),
        ("POST", "/v1/recover") => api::recover_start(service, body, client),
        ("GET", "/v1/recover/confirm") => api::recover_confirm(service, query),
        ("POST", "/v1/recover/cancel") => api::recover_cancel(service, body),
        ("GET", path) if path.starts_with(NAME_PREFIX) => {
            api::name_page(service, &path[NAME_PREFIX.len()..])
        }
        _ if is_known(path) => Reply::error(405, "method", "this path does not answer that method"),
        _ => Reply::error(404, "not_found", "there is nothing at this path"),
    }
}

/// Whether a path exists at all, for telling 404 from 405.
fn is_known(path: &str) -> bool {
    matches!(
        path,
        "/healthz"
            | "/v1/keys"
            | "/v1/revocations"
            | "/v1/claim"
            | "/v1/release"
            | "/v1/email"
            | "/v1/email/verify"
            | "/v1/recover"
            | "/v1/recover/confirm"
            | "/v1/recover/cancel"
    ) || path.starts_with(NAME_PREFIX)
}

/// Reads a request body, refusing an oversized one before reading it.
fn read_body(request: &mut Request) -> Result<Vec<u8>, Reply> {
    let declared = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Content-Length"))
        .and_then(|header| header.value.as_str().trim().parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(Reply::error(
            413,
            "too_large",
            &format!("a request body may be at most {MAX_BODY_BYTES} bytes"),
        ));
    }

    let mut body = Vec::with_capacity(declared.unwrap_or(0).min(MAX_BODY_BYTES));
    request
        .as_reader()
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|err| {
            Reply::error(
                400,
                "body",
                &format!("the request body could not be read: {err}"),
            )
        })?;
    if body.len() > MAX_BODY_BYTES {
        return Err(Reply::error(
            413,
            "too_large",
            &format!("a request body may be at most {MAX_BODY_BYTES} bytes"),
        ));
    }
    Ok(body)
}

/// The address to rate-limit a request against.
///
/// Behind Dokploy, Traefik appends the address it sees to whatever
/// `X-Forwarded-For` the client sent, so the **last** entry is the one that
/// actually talked to the proxy; earlier entries are the client's own claim and
/// are not believed. Off by default: believing a header on a public port is how
/// a rate limit becomes decorative.
fn client_ip(request: &Request, trust_forwarded_for: bool) -> Option<IpAddr> {
    if trust_forwarded_for
        && let Some(value) = header(request, "X-Forwarded-For")
        && let Some(last) = value
            .split(',')
            .map(str::trim)
            .rfind(|part| !part.is_empty())
        && let Ok(ip) = last.parse()
    {
        return Some(ip);
    }
    request.remote_addr().map(|addr| addr.ip())
}

/// One request header, if present.
///
/// Compared by hand rather than with `HeaderField::equiv`, whose argument is
/// `&'static str`: the two names this module looks for are literals, but the
/// helper should stay usable with a name that is not.
fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

/// A status code, a content type and a body, before it becomes a tiny_http
/// response.
pub(crate) struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    /// A postcard body, for everything the client parses.
    pub(crate) fn octets(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "application/octet-stream",
            body,
        }
    }

    /// A JSON body, for errors and the public name page.
    pub(crate) fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.into_bytes(),
        }
    }

    /// The one error shape: `{"error": "<code>", "message": "<sentence>"}`.
    ///
    /// The code is for a client to match on; the message is for a person. Both
    /// are escaped, because a message may quote a name.
    pub(crate) fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(
            status,
            format!(
                "{{\"error\":\"{}\",\"message\":\"{}\"}}",
                json_escape(code),
                json_escape(message)
            ),
        )
    }

    /// The proof-of-work refusal, which carries the difficulty to re-mine at.
    ///
    /// The service raises the difficulty when a subnet is busy (`limits::
    /// required_work_bits`), so a client that mined at the default needs the
    /// new number to retry without guessing. It is the one error a machine is
    /// expected to read.
    pub(crate) fn error_work(status: u16, found: u32, required: u32) -> Self {
        Self::json(
            status,
            format!(
                "{{\"error\":\"work\",\"message\":\"this network's names are busy right now: \
                 the proof of work needs {required} leading zero bits and this request has \
                 {found}\",\"required_bits\":{required},\"found_bits\":{found}}}"
            ),
        )
    }

    /// Wraps it for tiny_http.
    fn into_response(self) -> Response<std::io::Cursor<Vec<u8>>> {
        let mut response = Response::from_data(self.body).with_status_code(StatusCode(self.status));
        if let Ok(header) =
            Header::from_bytes(b"Content-Type".as_slice(), self.content_type.as_bytes())
        {
            response = response.with_header(header);
        }
        if let Ok(header) = Header::from_bytes(b"Cache-Control".as_slice(), b"no-store".as_slice())
        {
            response = response.with_header(header);
        }
        response
    }
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reply")
            .field("status", &self.status)
            .field("bytes", &self.body.len())
            .finish()
    }
}

/// Escapes a string for a JSON value.
///
/// The messages this service writes are its own sentences, but a message may
/// quote a name, and a name arrives from the network. `serde_json` would do
/// this properly and is a dependency for one function; the rule here is the
/// conservative one — escape everything that is not printable ASCII.
pub(crate) fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_ascii() && !character.is_ascii_control() => {
                out.push(character);
            }
            character => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_under_a_deadline_finishes_or_times_out() {
        assert!(matches!(
            within(Duration::from_millis(500), || 7),
            Deadline::Met(7)
        ));
        // The slow worker is still running when this returns; it ends on its
        // own and its answer goes into a dropped channel.
        assert!(matches!(
            within(Duration::from_millis(10), || {
                std::thread::sleep(Duration::from_millis(80));
                7
            }),
            Deadline::Missed
        ));
    }

    #[test]
    fn json_escaping_covers_quotes_backslashes_and_controls() {
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("a\"b"), "a\\\"b");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("é"), "\\u00e9");
        // A name may not contain any of these, but a message that quotes one
        // must not be able to end the JSON string early.
        assert_eq!(json_escape("\"}"), "\\\"}");
    }

    #[test]
    fn an_error_is_json_with_both_parts_escaped() {
        let reply = Reply::error(409, "taken", "the name \"alice\" is gone");
        let body = String::from_utf8(reply.body.clone()).expect("utf8");
        assert_eq!(
            body,
            "{\"error\":\"taken\",\"message\":\"the name \\\"alice\\\" is gone\"}"
        );
        assert_eq!(reply.status, 409);
    }
}
