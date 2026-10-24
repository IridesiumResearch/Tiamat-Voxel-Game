// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The handlers, one function per route.
//!
//! Kept apart from [`crate::http`] so the transport stays a transport: routing,
//! body caps and status lines are one concern, and what a route *means* is
//! another. Every handler is synchronous and does O(1) database work; the
//! service is small enough that a slow query is a bug, not a load problem to
//! schedule around.

use crate::http::Reply;
use crate::service::Service;
use crate::wire::KeysResponse;

/// `GET /healthz` — up, and the database answers.
pub(crate) fn health(service: &Service) -> Reply {
    let answered = service
        .db()
        .with(|connection| connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)));
    match answered {
        Ok(1) => Reply::json(200, "{\"status\":\"ok\"}".to_owned()),
        _ => Reply::error(503, "database", "the database did not answer"),
    }
}

/// `GET /v1/keys` — the current key, its successor's commitment, and the chain.
///
/// A client fetches this with its renewal and checks that the current key is
/// the one it trusts (or a successor it has already accepted). It is also what
/// the nightly ops check asks for, which is why it needs no authentication: a
/// public key is public.
pub(crate) fn keys(service: &Service) -> Reply {
    let response = KeysResponse {
        current: service.keys().public_key().to_bytes(),
        next: service.keys().next_commitment(),
        rotations: service.keys().rotations().to_vec(),
    };
    match postcard::to_allocvec(&response) {
        Ok(bytes) => Reply::octets(bytes),
        Err(err) => {
            tracing::error!(error = %err, "the keys response could not be encoded");
            Reply::error(500, "wire", "the response could not be encoded")
        }
    }
}
