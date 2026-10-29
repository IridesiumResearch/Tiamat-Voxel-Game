// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The transport: a listener, a bounded pool of workers, a body cap, a request
//! deadline, and a status line for everything that is not a handler.
//!
//! `tiny_http` is blocking: its own threads parse each connection's request
//! line and headers, and hand over a request whose body has not been read. It
//! has no TLS of its own, deliberately — TLS is the reverse proxy's job at
//! Dokploy, and so is buffering a slow client's body. What this module owns is
//! the part that touches a stranger's bytes, so it is where the caps live: a
//! request body is refused **before** it is read if it declares too much, a
//! request has two seconds from the listener to its answer
//! ([`REQUEST_TIMEOUT`]), and a path this service does not serve gets a status
//! line rather than a handler.
//!
//! # One slow client is one busy worker
//!
//! The listener never reads a body or runs a handler. It hands each request to
//! a bounded pool ([`crate::workers`], `REGISTRY_WORKERS` of them, as many
//! again waiting) and answers `503 busy` itself when the pool is full, so a
//! client trickling a body stalls one worker for at most the deadline, never
//! every other request (review B5).
//!
//! # A listener that stops is a process that exits
//!
//! When the accept loop ends for any reason but [`ServerHandle::stop`], the
//! handle reports it ([`ServerHandle::failure`], [`ServerHandle::wait`]) and the
//! binary exits non-zero, so the container's supervisor restarts it rather than
//! leaving a process that answers nothing (review B17).

use std::io::{ErrorKind, Read};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use tiny_http::{Header, Request, Response, Server, StatusCode};

use crate::api;
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::deadline::Deadline;
use crate::mail::Mailer;
use crate::service::{Service, StartError};
use crate::workers::{self, Job};

/// The largest request body the registry will read.
///
/// The largest record a client sends is a claim, release or cancellation
/// carrying a key-set proof: at most
/// [`MAX_PROVEN_REQUEST_BYTES`](tiamat_core::identity::registry::MAX_PROVEN_REQUEST_BYTES),
/// about two and a half kilobytes, which each record's own parser enforces.
/// Thirty-two kilobytes is the design's cap, and it is checked from
/// `Content-Length` before a byte of body is read as well as enforced on the
/// read itself, so a lying header cannot make the service allocate.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

// The transport's cap must never be the one that refuses a record its parser
// would accept: a second device's request would fail as "too large" with no
// way for the client to know why.
const _: () = assert!(MAX_BODY_BYTES >= tiamat_core::identity::registry::MAX_PROVEN_REQUEST_BYTES);

/// How long a request may take, from leaving the listener to its answer,
/// before the client is told to retry.
///
/// The design's two seconds. The database's own `busy_timeout` is the same
/// number, but this bounds the *whole* request: a body that trickles in, a
/// query that waits, a verification that is slow, a disk that has stopped
/// answering. A client gets a status line instead of a socket that never
/// closes — and a `504` only for work that did not, and will not, commit.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the accept loop looks at its stop flag, and how often
/// [`ServerHandle::wait`] looks at the listener and the interrupt.
const POLL: Duration = Duration::from_millis(200);

/// A running registry.
pub struct ServerHandle {
    addr: SocketAddr,
    server: Arc<Server>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
    failure: Arc<OnceLock<String>>,
}

/// Why [`ServerHandle::wait`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The operator asked the process to stop.
    Interrupted,
    /// The accept loop ended on its own; the reason, for the log.
    Failed(String),
}

impl ServerHandle {
    /// The address actually bound, which matters when the config asked for port
    /// zero (tests do).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Why the accept loop ended, if it ended without [`ServerHandle::stop`].
    ///
    /// `None` while it is serving. A listener that cannot accept (`tiny_http`'s
    /// own accept thread exits on its first error), a pool whose workers have
    /// all gone, or a panic in the loop all count: each leaves a process that
    /// answers nothing, which a supervisor can only fix by restarting it.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        if let Some(reason) = self.failure.get() {
            return Some(reason.clone());
        }
        match &self.accept {
            Some(thread) if thread.is_finished() && !self.stop.load(Ordering::Relaxed) => {
                Some("the accept loop ended without a reason (a panic?)".to_owned())
            }
            _ => None,
        }
    }

    /// Blocks until `interrupt` delivers (or its sender is dropped) or the
    /// accept loop fails, and says which.
    ///
    /// The binary's whole `serve` loop: it stops cleanly on an interrupt and
    /// exits non-zero on a failure, so the supervisor restarts it.
    #[must_use]
    pub fn wait(&self, interrupt: &Receiver<()>) -> Exit {
        supervise(interrupt, || self.failure(), POLL)
    }

    /// Stops the listener, drains the workers, and waits for both.
    ///
    /// Returns whether every thread ended cleanly. Idempotent, and `Drop` does
    /// the same thing, so a test that forgets cannot leak a thread. A request a
    /// worker is serving is finished first, which its deadline bounds.
    pub fn stop(mut self) -> bool {
        self.shutdown()
    }

    fn shutdown(&mut self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        // Wake a thread parked in `recv_timeout`. The timeout alone would also
        // do it within the poll interval; this makes stopping immediate.
        self.server.unblock();
        let mut clean = match self.accept.take() {
            Some(thread) => thread.join().is_ok(),
            None => true,
        };
        // The accept thread owned the queue's only sender; with it gone, each
        // worker finishes what is queued and ends.
        for worker in self.workers.drain(..) {
            clean &= worker.join().is_ok();
        }
        clean
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
            .field("workers", &self.workers.len())
            .finish_non_exhaustive()
    }
}

/// Waits for an interrupt or a failure, looking every `poll`.
///
/// A dropped interrupt sender counts as an interrupt: nobody is left to ask
/// for a stop, and waiting for ever on a channel that cannot deliver would be
/// a process that never exits.
fn supervise(
    interrupt: &Receiver<()>,
    failure: impl Fn() -> Option<String>,
    poll: Duration,
) -> Exit {
    loop {
        match interrupt.recv_timeout(poll) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return Exit::Interrupted,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(reason) = failure() {
            return Exit::Failed(reason);
        }
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
    // `ListenAddr` grows a `Unix` variant on Unix targets, so this is not an
    // irrefutable pattern there even though `Server::http` only ever binds a
    // TCP address. Refusing to start is the honest answer to the impossible.
    let Some(addr) = server.server_addr().to_ip() else {
        return Err(StartError::NotTcp);
    };

    let (jobs, workers) =
        workers::spawn(service.config().workers.max(1), &service).map_err(StartError::Thread)?;
    let stop = Arc::new(AtomicBool::new(false));
    let failure = Arc::new(OnceLock::new());
    let accept = {
        let server = Arc::clone(&server);
        let stop = Arc::clone(&stop);
        let failure = Arc::clone(&failure);
        std::thread::Builder::new()
            .name("registry-http".to_owned())
            .spawn(move || {
                if let Err(reason) = run(server.as_ref(), &jobs, &stop) {
                    tracing::error!(%reason, "the registry stopped accepting connections");
                    let _ = failure.set(reason);
                }
            })
            .map_err(StartError::Thread)?
    };

    Ok(ServerHandle {
        addr,
        server,
        stop,
        accept: Some(accept),
        workers,
        failure,
    })
}

/// Where the accept loop's requests come from: the listener, or a test's.
trait Incoming {
    /// The next request, waiting at most `timeout`.
    fn next_request(&self, timeout: Duration) -> std::io::Result<Option<Request>>;
}

impl Incoming for Server {
    fn next_request(&self, timeout: Duration) -> std::io::Result<Option<Request>> {
        self.recv_timeout(timeout)
    }
}

/// The accept loop, until the stop flag is set or the listener fails.
///
/// `Ok` means it was asked to stop; `Err` carries why it could not go on.
fn run(incoming: &impl Incoming, jobs: &SyncSender<Job>, stop: &AtomicBool) -> Result<(), String> {
    while !stop.load(Ordering::Relaxed) {
        match incoming.next_request(POLL) {
            Ok(Some(request)) => match jobs.try_send(Job::new(request)) {
                Ok(()) => {}
                // Every worker busy and the queue full: say so at once rather
                // than let the request wait out its deadline in a longer line.
                // The answer is a few hundred bytes into an idle socket, which
                // does not block — except for a request pipelined behind
                // another on one connection, whose answer `tiny_http` holds
                // until the first is answered (within its deadline). Traefik
                // does not pipeline to its backends.
                Err(TrySendError::Full(job)) => job.refuse(Reply::busy()),
                Err(TrySendError::Disconnected(job)) => {
                    job.refuse(Reply::busy());
                    return Err("every request worker has stopped".to_owned());
                }
            },
            Ok(None) => {}
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            // A listener that cannot accept is not a service. Stop rather than
            // spin: the binary exits and the supervisor restarts the container.
            Err(err) => return Err(format!("the listener failed: {err}")),
        }
    }
    Ok(())
}

/// The prefix of the public name page.
const NAME_PREFIX: &str = "/v1/name/";

/// Routes a request to a handler, or to the status line that says why not.
///
/// `client` is the address a request's limits and audit rows are kept
/// against, and `deadline` is what every handler that writes checks before it
/// commits.
pub(crate) fn dispatch(
    service: &Service,
    method: &str,
    url: &str,
    client: Option<IpAddr>,
    body: &[u8],
    deadline: &Deadline,
) -> Reply {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    match (method, path) {
        ("GET", "/healthz") => api::health(service),
        ("GET", "/v1/keys") => api::keys(service),
        ("GET", "/v1/revocations") => api::revocations(service),
        ("POST", "/v1/claim") => api::claim(service, body, client, deadline),
        ("POST", "/v1/release") => api::release(service, body, client, deadline),
        ("POST", "/v1/email") => api::email(service, body, client, deadline),
        // A link in a mail is a page; only its button's POST changes anything,
        // so a mail scanner that follows every link changes nothing.
        ("GET", "/v1/email/verify") => api::email_verify_page(query),
        ("POST", "/v1/email/verify") => api::email_verify(service, query, body, deadline),
        ("POST", "/v1/recover") => api::recover_start(service, body, client, deadline),
        ("GET", "/v1/recover/confirm") => api::recover_confirm_page(query),
        ("POST", "/v1/recover/confirm") => api::recover_confirm(service, query, body, deadline),
        ("POST", "/v1/recover/cancel") => api::recover_cancel(service, body, client, deadline),
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

/// Reads a request body under its cap and its deadline.
///
/// An oversized `Content-Length` is refused before anything is read; the read
/// itself stops one byte past the cap, so a lying header cannot make the
/// service allocate; and the deadline is checked after every chunk, so a body
/// that trickles in is refused `408` at the deadline rather than accepted
/// whenever it ends. (A read that blocks outright is waited out by the worker,
/// not here: see [`crate::workers`].)
pub(crate) fn read_body(request: &mut Request, deadline: &Deadline) -> Result<Vec<u8>, Reply> {
    let too_large = || {
        Reply::error(
            413,
            "too_large",
            &format!("a request body may be at most {MAX_BODY_BYTES} bytes"),
        )
    };
    let declared = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Content-Length"))
        .and_then(|header| header.value.as_str().trim().parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(too_large());
    }

    let mut body = Vec::with_capacity(declared.unwrap_or(0).min(MAX_BODY_BYTES));
    let mut reader = request.as_reader().take(MAX_BODY_BYTES as u64 + 1);
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(Reply::error(
                    400,
                    "body",
                    &format!("the request body could not be read: {err}"),
                ));
            }
        }
        if body.len() > MAX_BODY_BYTES {
            return Err(too_large());
        }
        if deadline.expired() || deadline.abandoned() {
            return Err(Reply::slow_body());
        }
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
pub(crate) fn client_ip(request: &Request, trust_forwarded_for: bool) -> Option<IpAddr> {
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

    /// An HTML page: only the confirmation pages a mailed link opens.
    pub(crate) fn html(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: HTML,
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
        Self::error_work_for(
            status,
            found,
            required,
            "this network's names are busy right now",
        )
    }

    /// [`Reply::error_work`] with the reason the work is asked for: the email
    /// request's refusal shares the claim's shape so one client path re-mines
    /// both.
    pub(crate) fn error_work_for(status: u16, found: u32, required: u32, why: &str) -> Self {
        Self::json(
            status,
            format!(
                "{{\"error\":\"work\",\"message\":\"{}: the proof of work needs {required} \
                 leading zero bits and this request has {found}\",\"required_bits\":{required},\
                 \"found_bits\":{found}}}",
                json_escape(why)
            ),
        )
    }

    /// Every worker is busy and as many requests again are waiting, or a
    /// request waited out its deadline in that queue.
    pub(crate) fn busy() -> Self {
        Self::error(
            503,
            "busy",
            "the registry is serving as many requests as it can; try again in a moment",
        )
    }

    /// The request's work missed its deadline and was rolled back.
    pub(crate) fn late() -> Self {
        Self::error(
            504,
            "timeout",
            "the registry took longer than two seconds to answer; nothing was changed, so \
             try again",
        )
    }

    /// The request's body did not arrive within its deadline.
    pub(crate) fn slow_body() -> Self {
        Self::error(
            408,
            "timeout",
            "the request body did not arrive within two seconds; try again",
        )
    }

    /// The handler ended without an answer (a bug, logged where it happened).
    pub(crate) fn internal_failure() -> Self {
        Self::error(
            500,
            "internal",
            "the registry could not answer this request",
        )
    }

    /// The status code, for tests.
    #[cfg(test)]
    pub(crate) fn status(&self) -> u16 {
        self.status
    }

    /// The body as text, for tests.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Wraps it for tiny_http.
    pub(crate) fn into_response(self) -> Response<std::io::Cursor<Vec<u8>>> {
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
        if let Ok(header) =
            Header::from_bytes(b"X-Content-Type-Options".as_slice(), b"nosniff".as_slice())
        {
            response = response.with_header(header);
        }
        if self.content_type == HTML
            && let Ok(header) = Header::from_bytes(
                b"Content-Security-Policy".as_slice(),
                PAGE_POLICY.as_bytes(),
            )
        {
            response = response.with_header(header);
        }
        response
    }
}

/// The content type of the confirmation pages.
const HTML: &str = "text/html; charset=utf-8";

/// What a confirmation page may do: post its one form back here, and nothing
/// else — no script, no style, no frame around it, no other origin.
const PAGE_POLICY: &str =
    "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

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

    /// An `Incoming` whose listener has died, as `tiny_http`'s does when its
    /// accept thread meets an error.
    struct Broken;

    impl Incoming for Broken {
        fn next_request(&self, _timeout: Duration) -> std::io::Result<Option<Request>> {
            Err(std::io::Error::other("too many open files"))
        }
    }

    /// An `Incoming` that only ever times out.
    struct Quiet;

    impl Incoming for Quiet {
        fn next_request(&self, timeout: Duration) -> std::io::Result<Option<Request>> {
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            Ok(None)
        }
    }

    #[test]
    fn a_listener_that_fails_ends_the_accept_loop_with_its_reason() {
        let (jobs, _queue) = std::sync::mpsc::sync_channel(1);
        let stop = AtomicBool::new(false);
        let ended = run(&Broken, &jobs, &stop).expect_err("a failed listener is not a stop");
        assert!(ended.contains("too many open files"), "{ended}");
    }

    #[test]
    fn a_requested_stop_ends_the_accept_loop_cleanly() {
        let (jobs, _queue) = std::sync::mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || run(&Quiet, &jobs, &stop))
        };
        stop.store(true, Ordering::Relaxed);
        assert_eq!(thread.join().expect("join"), Ok(()));
    }

    #[test]
    fn the_supervisor_returns_on_an_interrupt_or_a_failure() {
        let (interrupt, interrupted) = std::sync::mpsc::channel();
        interrupt.send(()).expect("send");
        assert_eq!(
            supervise(&interrupted, || None, Duration::from_millis(1)),
            Exit::Interrupted
        );

        let (_interrupt, quiet) = std::sync::mpsc::channel::<()>();
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let exit = supervise(
            &quiet,
            || (polls.fetch_add(1, Ordering::Relaxed) >= 3).then(|| "gone".to_owned()),
            Duration::from_millis(1),
        );
        assert_eq!(exit, Exit::Failed("gone".to_owned()));

        // Nobody left to send an interrupt is an interrupt, not a hang.
        let (interrupt, orphaned) = std::sync::mpsc::channel::<()>();
        drop(interrupt);
        assert_eq!(
            supervise(&orphaned, || None, Duration::from_millis(1)),
            Exit::Interrupted
        );
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
