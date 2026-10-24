// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The handlers, one function per route.
//!
//! Kept apart from [`crate::http`] so the transport stays a transport: routing,
//! body caps and status lines are one concern, and what a route *means* is
//! another. Every handler is synchronous and does O(1) database work; the
//! service is small enough that a slow query is a bug, not a load problem to
//! schedule around.

use std::net::IpAddr;

use tiamat_core::identity::registry::{
    CancelRecoveryRequest, ClaimRequest, EmailRequest, NameCertificate, RecoveryRequest,
    RegistryError, ReleaseRequest,
};

use crate::http::Reply;
use crate::names::{self, ClaimOutcome, ReleaseOutcome, RenewOutcome};
use crate::recover::{
    self, CancelOutcome, ConfirmOutcome, EmailOutcome, FlowError, MailOutcome, StartOutcome,
};
use crate::service::Service;
use crate::wire::{ClaimResponse, KeysResponse};

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
        Err(err) => internal(err),
    }
}

/// `POST /v1/claim` — a claim (with work) or a renewal (without).
pub(crate) fn claim(service: &Service, body: &[u8], client: Option<IpAddr>) -> Reply {
    if let Err(error) = service.advance() {
        return internal(error);
    }
    let now = service.now();
    let Some(client) = client else {
        // A TCP request always has a peer; this is the impossibility guard.
        return Reply::error(400, "address", "the client's address could not be read");
    };
    let request = match ClaimRequest::from_bytes(body) {
        Ok(request) => request,
        Err(error) => return malformed("claim request", &error),
    };
    let required = match service.db().with(|connection| {
        crate::limits::required_work_bits(connection, client, now, service.config().work_bits)
    }) {
        Ok(bits) => bits,
        Err(error) => return internal(error),
    };

    let is_claim = request.work.is_some();
    let verified = if is_claim {
        match request.verify_claim(now, required) {
            Ok(verified) => verified,
            Err(error) => return refused(&error),
        }
    } else {
        match request.verify_renewal(now) {
            Ok(verified) => verified,
            Err(error) => return refused(&error),
        }
    };

    if is_claim {
        match service
            .db()
            .with(|connection| names::claim(connection, service.keys(), &verified, now, client))
        {
            Ok(ClaimOutcome::Issued {
                certificate,
                notice,
            }) => issued(certificate, notice),
            Ok(ClaimOutcome::NotRoot) => Reply::error(
                403,
                "not_root",
                "a name is claimed with the identity's root key; this key does not derive the UUID",
            ),
            Ok(ClaimOutcome::Reserved) => Reply::error(403, "reserved", "that name is reserved"),
            Ok(ClaimOutcome::Taken) => {
                Reply::error(409, "taken", "that name is registered to somebody else")
            }
            Ok(ClaimOutcome::AlreadyNamed) => Reply::error(
                409,
                "already_named",
                "this identity already holds a name; release it to take another",
            ),
            Ok(ClaimOutcome::Limited(limit)) => Reply::error(429, "limited", limit.message()),
            Err(error) => internal(error),
        }
    } else {
        match service
            .db()
            .with(|connection| names::renew(connection, service.keys(), &verified, now))
        {
            Ok(RenewOutcome::Renewed {
                certificate,
                notice,
            }) => issued(certificate, notice),
            Ok(RenewOutcome::Unknown) => {
                Reply::error(404, "unknown", "this identity holds no name to renew")
            }
            Ok(RenewOutcome::NotYours) => Reply::error(
                409,
                "not_yours",
                "this identity holds a different name, so it cannot renew this one",
            ),
            Err(error) => internal(error),
        }
    }
}

/// `POST /v1/release` — give a name up, with the thirty-day hold.
pub(crate) fn release(service: &Service, body: &[u8]) -> Reply {
    if let Err(error) = service.advance() {
        return internal(error);
    }
    let now = service.now();
    let request = match ReleaseRequest::from_bytes(body) {
        Ok(request) => request,
        Err(error) => return malformed("release request", &error),
    };
    let verified = match request.verify(now) {
        Ok(verified) => verified,
        Err(error) => return refused(&error),
    };
    match service
        .db()
        .with(|connection| names::release(connection, &verified, now))
    {
        Ok(ReleaseOutcome::Released) => Reply::json(
            200,
            format!(
                "{{\"released\":\"{}\"}}",
                crate::http::json_escape(&verified.name)
            ),
        ),
        Ok(ReleaseOutcome::Unknown) => {
            Reply::error(404, "unknown", "this identity holds no name to release")
        }
        Ok(ReleaseOutcome::NotYours) => Reply::error(
            409,
            "not_yours",
            "this identity holds a different name, so it cannot release this one",
        ),
        Err(error) => internal(error),
    }
}

/// `GET /v1/name/{canonical}` — whether a name is held, and by whom.
///
/// Public, as the design says, so a player can check before claiming and a
/// server with internet may ask. It reveals a UUID while a name is held and
/// stops as soon as it is released: the UUID was public by being the holder's
/// name, and a released name is not the old holder's business any more.
pub(crate) fn name_page(service: &Service, canonical: &str) -> Reply {
    if let Err(error) = names::check_name(canonical) {
        return Reply::error(400, "name", &error.to_string());
    }
    if let Err(error) = service.advance() {
        return internal(error);
    }
    let now = service.now();
    let status = match service
        .db()
        .with(|connection| names::status(connection, canonical, now))
    {
        Ok(status) => status,
        Err(error) => return internal(error),
    };

    let mut json = format!(
        "{{\"name\":\"{}\",\"held\":{}",
        crate::http::json_escape(&status.canonical),
        status.display.is_some()
    );
    if let Some(display) = &status.display {
        json.push_str(&format!(
            ",\"display\":\"{}\"",
            crate::http::json_escape(display)
        ));
    }
    if let Some(uuid) = &status.uuid {
        json.push_str(&format!(",\"uuid\":\"{uuid}\""));
    }
    json.push_str(&format!(
        ",\"reserved\":{},\"recovery_pending\":{}",
        status.reserved, status.recovery_pending
    ));
    if let Some(free_after) = status.free_after {
        json.push_str(&format!(",\"free_after\":{free_after}"));
    }
    json.push('}');
    Reply::json(200, json)
}

/// `POST /v1/email` — bind, change, or remove a recovery address.
///
/// Answers `202` when a link is on its way and `200` when an address was
/// removed; the address itself is not stored until the link is followed.
pub(crate) fn email(service: &Service, body: &[u8], client: Option<IpAddr>) -> Reply {
    let now = service.now();
    let Some(client) = client else {
        return Reply::error(400, "address", "the client's address could not be read");
    };
    let request = match EmailRequest::from_bytes(body) {
        Ok(request) => request,
        Err(error) => return malformed("email request", &error),
    };
    let verified = match request.verify(now) {
        Ok(verified) => verified,
        Err(error) => return refused(&error),
    };
    let base_url = service.config().base_url.clone();
    match service.db().try_with(|connection| {
        recover::bind_email(
            connection,
            service.mailer(),
            &base_url,
            &verified,
            now,
            client,
        )
    }) {
        Ok(EmailOutcome::Sent) => Reply::json(202, "{\"sent\":true}".to_owned()),
        Ok(EmailOutcome::Removed) => Reply::json(200, "{\"removed\":true}".to_owned()),
        Ok(EmailOutcome::NotRoot) => not_root(),
        Ok(EmailOutcome::Limited) => limited_mail(),
        Ok(EmailOutcome::Mail(outcome)) => mail_reply(outcome),
        Err(error) => flow_error(error),
    }
}

/// `GET /v1/email/verify?t=<token>` — the link in the verification mail.
pub(crate) fn email_verify(service: &Service, query: &str) -> Reply {
    let now = service.now();
    let Some(token) = token_from_query(query) else {
        return Reply::error(400, "token", "the link carries no token");
    };
    match service
        .db()
        .try_with(|connection| recover::verify_email(connection, token, now))
    {
        Ok(Some((_uuid, address))) => Reply::json(
            200,
            format!(
                "{{\"verified\":true,\"address\":\"{}\"}}",
                crate::http::json_escape(&address)
            ),
        ),
        Ok(None) => Reply::error(400, "token", "this link is unknown, used, or expired"),
        Err(error) => flow_error(error),
    }
}

/// `POST /v1/recover` — ask to move a lost name to a new identity.
pub(crate) fn recover_start(service: &Service, body: &[u8], client: Option<IpAddr>) -> Reply {
    let now = service.now();
    let Some(client) = client else {
        return Reply::error(400, "address", "the client's address could not be read");
    };
    let request = match RecoveryRequest::from_bytes(body) {
        Ok(request) => request,
        Err(error) => return malformed("recovery request", &error),
    };
    let verified = match request.verify(now) {
        Ok(verified) => verified,
        Err(error) => return refused(&error),
    };
    let base_url = service.config().base_url.clone();
    match service.db().try_with(|connection| {
        recover::start_recovery(
            connection,
            service.mailer(),
            &base_url,
            &verified,
            now,
            client,
        )
    }) {
        Ok(StartOutcome::Sent) => Reply::json(202, "{\"sent\":true}".to_owned()),
        Ok(StartOutcome::NotRoot) => not_root(),
        Ok(StartOutcome::UnknownName) => {
            Reply::error(404, "unknown", "no name by that form is held")
        }
        Ok(StartOutcome::NoEmail) => Reply::error(
            409,
            "no_email",
            "that name has no verified recovery address, so it cannot be recovered",
        ),
        Ok(StartOutcome::AlreadyHeld) => Reply::error(
            409,
            "already_held",
            "that name already belongs to the identity that asked",
        ),
        Ok(StartOutcome::Pending) => Reply::error(
            409,
            "pending",
            "a recovery notice is already running for that name",
        ),
        Ok(StartOutcome::Limited) => limited_mail(),
        Ok(StartOutcome::Mail(outcome)) => mail_reply(outcome),
        Err(error) => flow_error(error),
    }
}

/// `GET /v1/recover/confirm?t=<token>` — following the mail starts the notice.
pub(crate) fn recover_confirm(service: &Service, query: &str) -> Reply {
    let now = service.now();
    let Some(token) = token_from_query(query) else {
        return Reply::error(400, "token", "the link carries no token");
    };
    match service
        .db()
        .try_with(|connection| recover::confirm_recovery(connection, token, now))
    {
        Ok(ConfirmOutcome::Started { notice_until }) => {
            Reply::json(200, format!("{{\"notice_until\":{notice_until}}}"))
        }
        Ok(ConfirmOutcome::UnknownToken) => {
            Reply::error(400, "token", "this link is unknown, used, or expired")
        }
        Ok(ConfirmOutcome::UnknownName) => {
            Reply::error(404, "unknown", "that name is not held any more")
        }
        Ok(ConfirmOutcome::AlreadyHeld) => Reply::error(
            409,
            "already_held",
            "that name already belongs to the identity that asked",
        ),
        Ok(ConfirmOutcome::Pending) => Reply::error(
            409,
            "pending",
            "a recovery notice is already running for that name",
        ),
        Err(error) => flow_error(error),
    }
}

/// `POST /v1/recover/cancel` — the holder ends a notice in one request.
pub(crate) fn recover_cancel(service: &Service, body: &[u8]) -> Reply {
    let now = service.now();
    let request = match CancelRecoveryRequest::from_bytes(body) {
        Ok(request) => request,
        Err(error) => return malformed("recovery cancellation", &error),
    };
    let verified = match request.verify(now) {
        Ok(verified) => verified,
        Err(error) => return refused(&error),
    };
    match service
        .db()
        .try_with(|connection| recover::cancel_recovery(connection, &verified, now))
    {
        Ok(CancelOutcome::Cancelled) => Reply::json(200, "{\"cancelled\":true}".to_owned()),
        Ok(CancelOutcome::NoRecovery) => {
            Reply::error(404, "no_recovery", "no notice is running for that name")
        }
        Ok(CancelOutcome::NotYours) => Reply::error(
            409,
            "not_yours",
            "that request is not from the identity that holds the name",
        ),
        Err(error) => flow_error(error),
    }
}

/// The token from a link's query string, if it is the right shape.
fn token_from_query(query: &str) -> Option<&str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
        // 32 bytes as hex, the only length `tokens` issues.
        .filter(|value| value.len() == 64)
}

/// The answer to a mail that could not be handed off.
fn mail_reply(outcome: MailOutcome) -> Reply {
    match outcome {
        MailOutcome::Sent => Reply::json(202, "{\"sent\":true}".to_owned()),
        MailOutcome::NotConfigured => Reply::error(
            503,
            "mail",
            "this registry has no mail configured, so no link can be sent",
        ),
        MailOutcome::Failed => Reply::error(
            502,
            "mail",
            "the mail could not be sent; try again in a moment",
        ),
    }
}

/// The one refusal shared by everything a root key is needed for.
fn not_root() -> Reply {
    Reply::error(
        403,
        "not_root",
        "this request must be signed by the identity's root key",
    )
}

/// The mail allowance is spent.
fn limited_mail() -> Reply {
    Reply::error(
        429,
        "limited",
        "too many mails have been sent to this address or network this hour",
    )
}

/// A flow failure: the detail goes to the log, a 500 to the client.
fn flow_error(error: FlowError) -> Reply {
    internal(error)
}

/// A certificate, as a claim or renewal answers.
fn issued(certificate: NameCertificate, notice: Option<i64>) -> Reply {
    let response = ClaimResponse {
        certificate,
        pending_recovery_until: notice,
    };
    match postcard::to_allocvec(&response) {
        Ok(bytes) => Reply::octets(bytes),
        Err(error) => internal(error),
    }
}

/// A record that did not parse. The message says which record, because a client
/// that gets this is usually debugging its own encoder.
fn malformed(what: &str, error: &RegistryError) -> Reply {
    Reply::error(
        400,
        "malformed",
        &format!("the {what} could not be read: {error}"),
    )
}

/// A signed record that parsed but did not pass its own rules.
///
/// The mapping is where a client learns *why* in words it can act on: a stale
/// clock, a missing key, a bad signature, a name it may not use.
fn refused(error: &RegistryError) -> Reply {
    match error {
        RegistryError::InsufficientWork { found, required } => {
            Reply::error_work(400, *found, *required)
        }
        RegistryError::MissingWork => {
            Reply::error(400, "work", "a claim must carry a proof of work")
        }
        RegistryError::StaleRequest => Reply::error(
            400,
            "stale",
            "the request's timestamp is more than five minutes from this registry's clock",
        ),
        RegistryError::SignatureInvalid => {
            Reply::error(401, "signature", "the signature does not verify")
        }
        RegistryError::BadPublicKey => Reply::error(400, "key", "the public key is malformed"),
        RegistryError::NameReserved { .. } => {
            Reply::error(403, "reserved", "that name is reserved")
        }
        RegistryError::NameTooShort { .. }
        | RegistryError::NameTooLong { .. }
        | RegistryError::NameMustStartWithLetter
        | RegistryError::NameBadCharacter { .. }
        | RegistryError::DisplayMismatch { .. }
        | RegistryError::DisplayBadCharacter { .. } => {
            Reply::error(400, "name", &error.to_string())
        }
        other => Reply::error(400, "invalid", &other.to_string()),
    }
}

/// Something the registry could not do. The detail goes to the log, never to
/// the client: a database error is not a stranger's business.
fn internal(error: impl std::fmt::Display) -> Reply {
    tracing::error!(error = %error, "a request could not be answered");
    Reply::error(
        500,
        "internal",
        "the registry could not answer this request",
    )
}
