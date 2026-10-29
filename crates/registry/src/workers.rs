// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The bounded pool that serves requests, so one slow client is one busy
//! worker and never a stalled service (review B5).
//!
//! # The shape
//!
//! The listener hands each request, untouched, to a bounded queue
//! ([`spawn`]): as many may wait as there are workers, and when the queue is
//! full the listener answers `503 busy` itself, without reading anything. A
//! worker takes one request at a time and owns it to its answer, under the
//! request's [`Deadline`] (two seconds from leaving the listener):
//!
//! 1. **The body** is read on a helper thread, because a blocking read has no
//!    timeout of its own — `tiny_http` sets none on its sockets. The worker
//!    waits for the body until the deadline; if it is still arriving then, the
//!    worker gives up and takes the next request, and the helper answers
//!    `408` itself whenever the read finally ends. A trickled body costs one
//!    worker two seconds, however long the client trickles.
//! 2. **The work** runs on the same helper, with the deadline in hand. The
//!    worker waits for its answer until the deadline. If it is late, the worker
//!    tries to abandon it: when that succeeds, nothing has committed and
//!    nothing will ([`crate::deadline`]), and the client gets `504`; when it
//!    fails, the work has committed, and the worker waits for the real answer —
//!    a `504` is never sent for work that then took effect (review B11).
//!
//! In production Traefik's buffering middleware reads the whole body before it
//! forwards a request (`docs/registry-ops.md`), so step 1 only ever waits on a
//! local socket; the pool is what holds when that is misconfigured.
//!
//! # What a stalled body still costs
//!
//! A helper abandoned in step 1 stays blocked in its read until the client
//! sends or hangs up: one thread beside the one `tiny_http` already holds for
//! that connection, bounded by the same thing — the proxy's timeouts. It is not
//! capped here, because a cap would turn a crowd of stalled clients into
//! `503`s for everyone, and the proxy, not this pool, is where a slow client
//! belongs.

use std::net::IpAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SendError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use tiny_http::Request;

use crate::deadline::Deadline;
use crate::http::{Reply, client_ip, dispatch, read_body};
use crate::service::Service;

/// A request as the listener hands it over: untouched, with the moment it left
/// the listener, which is when its deadline starts.
pub struct Job {
    request: Request,
    accepted: Instant,
}

impl Job {
    /// Wraps a request just taken off the listener.
    #[must_use]
    pub fn new(request: Request) -> Self {
        Self {
            request,
            accepted: Instant::now(),
        }
    }

    /// Answers the request without serving it: the listener's `503` when the
    /// queue is full.
    pub fn refuse(self, reply: Reply) {
        respond(self.request, reply);
    }
}

/// Starts `count` workers over one bounded queue of `count` places.
///
/// Returns the queue's sending end, for the listener, and the workers'
/// handles. A worker ends when the sending end is dropped and the queue is
/// empty, which is how [`crate::ServerHandle::stop`] drains them.
///
/// # Errors
///
/// The operating system's refusal to start a thread.
pub fn spawn(
    count: usize,
    service: &Arc<Service>,
) -> std::io::Result<(SyncSender<Job>, Vec<JoinHandle<()>>)> {
    let (sender, receiver) = sync_channel::<Job>(count);
    let receiver = Arc::new(Mutex::new(receiver));
    let mut workers = Vec::with_capacity(count);
    for index in 0..count {
        let receiver = Arc::clone(&receiver);
        let service = Arc::clone(service);
        workers.push(
            std::thread::Builder::new()
                .name(format!("registry-worker-{index}"))
                .spawn(move || work(&receiver, &service))?,
        );
    }
    Ok((sender, workers))
}

/// One worker's loop: a request at a time, until the queue closes.
fn work(receiver: &Mutex<Receiver<Job>>, service: &Arc<Service>) {
    loop {
        // The lock is held only while waiting for the next job; a worker that
        // panicked holding it left nothing half-done behind it.
        let next = {
            let guard = receiver
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.recv()
        };
        let Ok(job) = next else {
            return;
        };
        // A bug in one request must not cost the pool a worker. The request is
        // dropped by the unwind, and `tiny_http` answers a dropped request 500.
        if catch_unwind(AssertUnwindSafe(|| serve(job, service))).is_err() {
            tracing::error!("a request worker caught a panic and carries on");
        }
    }
}

/// Serves one request to its answer, as the module documentation describes.
fn serve(job: Job, service: &Arc<Service>) {
    let deadline = Arc::new(Deadline::starting(
        job.accepted,
        service.config().request_timeout,
    ));
    if deadline.expired() {
        // It waited out its whole budget in the queue: the service is busy,
        // and no work has started that could be told apart from that.
        respond(job.request, Reply::busy());
        return;
    }
    let method = job.request.method().as_str().to_owned();
    let url = job.request.url().to_owned();
    let client: Option<IpAddr> = client_ip(&job.request, service.config().trust_forwarded_for);

    // The helper receives the request through `give` only once it exists, so a
    // failure to start it still leaves the request here to be answered.
    let (give, take) = sync_channel::<Request>(1);
    // Rendezvous: a hand-back either meets the waiting worker or fails and
    // returns the request to the helper — it is never left in a buffer that
    // nobody will read.
    let (hand_back, handed) = sync_channel::<Request>(0);
    let (answer, answered) = sync_channel::<Reply>(1);
    let helper = {
        let service = Arc::clone(service);
        let deadline = Arc::clone(&deadline);
        let (method, url) = (method.clone(), url.clone());
        move || {
            let Ok(mut request) = take.recv() else {
                return;
            };
            let body = match read_body(&mut request, &deadline) {
                Ok(body) => body,
                Err(reply) => return respond(request, reply),
            };
            if deadline.abandoned() || deadline.expired() {
                return respond(request, Reply::slow_body());
            }
            if let Err(SendError(request)) = hand_back.send(request) {
                // The worker stopped waiting for this body.
                return respond(request, Reply::slow_body());
            }
            let reply = dispatch(&service, &method, &url, client, &body, &deadline);
            // The worker may have answered already (a `504`); then this goes
            // nowhere, which is the point.
            let _ = answer.send(reply);
        }
    };
    if std::thread::Builder::new()
        .name("registry-request".to_owned())
        .spawn(helper)
        .is_err()
    {
        respond(job.request, Reply::busy());
        return;
    }
    if let Err(SendError(request)) = give.send(job.request) {
        // The helper is gone before it began: nothing was read or done.
        respond(request, Reply::busy());
        return;
    }

    // The body.
    let request = match handed.recv_timeout(deadline.remaining()) {
        Ok(request) => request,
        // The helper answered on its own: a refused body, or a panic.
        Err(RecvTimeoutError::Disconnected) => return,
        Err(RecvTimeoutError::Timeout) => {
            // Still arriving. Nothing has been dispatched, so abandoning always
            // succeeds; dropping `handed` makes the helper's hand-back fail, and
            // it answers when its read ends.
            let _ = deadline.abandon();
            tracing::warn!(%method, %url, "a request body was still arriving at its deadline");
            return;
        }
    };

    // The work.
    match answered.recv_timeout(deadline.remaining()) {
        Ok(reply) => respond(request, reply),
        Err(RecvTimeoutError::Disconnected) => respond(request, Reply::internal_failure()),
        Err(RecvTimeoutError::Timeout) => {
            if deadline.abandon() {
                tracing::warn!(%method, %url, "a request missed its deadline and was rolled back");
                respond(request, Reply::late());
            } else {
                // It committed: whatever it says now is the truth, however late.
                match answered.recv() {
                    Ok(reply) => respond(request, reply),
                    Err(_) => respond(request, Reply::internal_failure()),
                }
            }
        }
    }
}

/// Writes an answer, logging a client that hung up first.
fn respond(request: Request, reply: Reply) {
    if let Err(error) = request.respond(reply.into_response()) {
        tracing::warn!(%error, "a response could not be written");
    }
}
