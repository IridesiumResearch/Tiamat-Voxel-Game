// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The shapes a client parses, `postcard` on the wire.
//!
//! **`postcard`, not JSON**, for everything a client reads: the records inside
//! are the ones `core::identity::registry` signs and verifies, and one encoding
//! means a byte of a certificate is the same byte in a claim, a rotation record
//! and an HTTP answer. The public name page is the one exception and is plain
//! text JSON, because its readers are people and third-party servers rather
//! than our client.
//!
//! These types are the wire contract with N4 (the client). Postcard encodes a
//! struct as its fields in declaration order with no tags, so **a field may be
//! appended but never inserted or removed** without agreeing a new endpoint
//! version — the same rule the protocol follows.

use tiamat_core::identity::registry::{NameCertificate, RotationRecord};

/// The answer to `GET /v1/keys`.
///
/// A client fetches this with a renewal: it checks that `current` is the key it
/// pinned (or that a rotation it holds moves it there), records `next` as the
/// commitment to refuse a future key nobody committed to, and appends
/// `rotations` to what it has.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeysResponse {
    /// The registry's current public key, raw bytes.
    pub current: [u8; 32],
    /// `BLAKE3` of the pre-committed successor, if there is one.
    pub next: Option<[u8; 32]>,
    /// Every rotation the registry has published, oldest first.
    pub rotations: Vec<RotationRecord>,
}

/// The answer to `POST /v1/claim`, for both a claim and a renewal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClaimResponse {
    /// The certificate to present when joining a server.
    pub certificate: NameCertificate,
    /// Set when a recovery is in flight against this name: the Unix time its
    /// notice ends. The holder renewing during the notice is how a stolen
    /// mailbox is caught (§4), so the client shows it as a notice with a
    /// cancel.
    pub pending_recovery_until: Option<i64>,
}
