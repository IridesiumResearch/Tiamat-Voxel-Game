// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The handlers, one function per route.
//!
//! Kept apart from [`crate::http`] so the transport stays a transport: routing,
//! body caps and status lines are one concern, and what a route *means* is
//! another. Every handler is synchronous and does O(1) database work; the
//! service is small enough that a slow query is a bug, not a load problem to
//! schedule around.
//!
//! Every handler that writes takes the request's [`Deadline`] and hands it to
//! the commit; a write that reaches its commit late is rolled back and answered
//! with the same `504` the worker gives a request it abandons ([`late`]).
//!
//! Every signed request, once it verifies, passes the registry's memory of
//! retired keys ([`admit`], `crate::retired`) before its own work runs: a key a
//! rotation retired is refused everywhere, whatever the request asks for.

use std::net::IpAddr;

use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::{
    CancelRecoveryRequest, ClaimRequest, EmailRequest, KeyLineage, NameCertificate,
    RecoveryRequest, RegistryError, ReleaseRequest,
};

use crate::deadline::Deadline;
use crate::http::Reply;
use crate::mailquota::MailLimit;
use crate::names::{self, ClaimOutcome, ReleaseOutcome, RenewOutcome};
use crate::recover::{
    self, CancelOutcome, ConfirmOutcome, EmailOutcome, FlowError, MailFlow, MailOutcome,
    StartOutcome, VerifyOutcome,
};
use crate::retired::Admission;
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
pub(crate) fn claim(
    service: &Service,
    body: &[u8],
    client: Option<IpAddr>,
    deadline: &Deadline,
) -> Reply {
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
    if let Some(reply) = admit(
        service,
        &verified.uuid,
        &verified.lineage,
        verified.timestamp,
        now,
        Some(client),
        deadline,
    ) {
        return reply;
    }

    if is_claim {
        match service.db().with(|connection| {
            names::claim(connection, service.keys(), &verified, now, client, deadline)
        }) {
            Ok(ClaimOutcome::Issued {
                certificate,
                notice,
            }) => issued(certificate, notice),
            Ok(ClaimOutcome::Reserved) => Reply::error(403, "reserved", "that name is reserved"),
            Ok(ClaimOutcome::Taken) => {
                Reply::error(409, "taken", "that name is registered to somebody else")
            }
            Ok(ClaimOutcome::AlreadyNamed) => Reply::error(
                409,
                "already_named",
                "this identity already holds a name; release it to take another",
            ),
            Ok(ClaimOutcome::Revoked) => Reply::error(
                403,
                "revoked",
                "the registry's operator revoked that name from this identity",
            ),
            Ok(ClaimOutcome::Limited(limit)) => Reply::error(429, "limited", limit.message()),
            Ok(ClaimOutcome::Late) => late(),
            Err(error) => internal(error),
        }
    } else {
        match service
            .db()
            .with(|connection| names::renew(connection, service.keys(), &verified, now, deadline))
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
            Ok(RenewOutcome::Limited(limit)) => Reply::error(429, "limited", limit.message()),
            Ok(RenewOutcome::Late) => late(),
            Err(error) => internal(error),
        }
    }
}

/// `POST /v1/release` — give a name up, with the thirty-day hold.
pub(crate) fn release(
    service: &Service,
    body: &[u8],
    client: Option<IpAddr>,
    deadline: &Deadline,
) -> Reply {
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
    if let Some(reply) = admit(
        service,
        &verified.uuid,
        &verified.lineage,
        verified.timestamp,
        now,
        client,
        deadline,
    ) {
        return reply;
    }
    match service
        .db()
        .with(|connection| names::release(connection, &verified, now, deadline))
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
        Ok(ReleaseOutcome::Late) => late(),
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
    if let Some(until) = status.recovery_notice_until {
        json.push_str(&format!(",\"recovery_notice_until\":{until}"));
    }
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
///
/// An identity that holds no name pays for the mail with the proof of work in
/// the request's own `work` field (`EmailRequest::solve_work`), at the
/// difficulty a claim from the same network would; without enough, the
/// refusal is the claim's `work` error, with the bits to mine for.
pub(crate) fn email(
    service: &Service,
    body: &[u8],
    client: Option<IpAddr>,
    deadline: &Deadline,
) -> Reply {
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
    if let Some(reply) = admit(
        service,
        &verified.uuid,
        &verified.lineage,
        verified.timestamp,
        now,
        Some(client),
        deadline,
    ) {
        return reply;
    }
    let work_bits = request.work_bits();
    // `bind_email` takes the database itself, not a held connection: it sends
    // the mail after releasing the lock, so an SMTP round trip never stalls
    // the requests queued behind it.
    match recover::bind_email(
        service.db(),
        mail_flow(service, client),
        &verified,
        work_bits,
        service.config().work_bits,
        now,
        deadline,
    ) {
        Ok(EmailOutcome::Sent) => Reply::json(202, "{\"sent\":true}".to_owned()),
        Ok(EmailOutcome::Removed) => Reply::json(200, "{\"removed\":true}".to_owned()),
        Ok(EmailOutcome::NotOwnKey) => not_own_key(),
        Ok(EmailOutcome::Work { found, required }) => Reply::error_work_for(
            400,
            found,
            required,
            "an identity that holds no name pays for a mail with a proof of work",
        ),
        Ok(EmailOutcome::Limited(limit)) => limited_mail(limit),
        Ok(EmailOutcome::Mail(outcome)) => mail_reply(outcome),
        Ok(EmailOutcome::Late) => late(),
        Err(error) => flow_error(error),
    }
}

/// The mail flows' settings for one request.
fn mail_flow(service: &Service, client: IpAddr) -> MailFlow<'_> {
    MailFlow {
        mailer: service.mailer(),
        base_url: &service.config().base_url,
        per_hour: service.config().mail_per_hour,
        client,
    }
}

/// `GET /v1/email/verify?t=<token>` — the link in the verification mail.
///
/// **A page, not the action.** Mail gateways and link scanners follow every
/// link in a message to look for malware; if following one bound the address,
/// a scanner would bind it for whoever asked. So the link opens a page with
/// one button, and the button's `POST` ([`email_verify`]) does the work. The
/// token is not looked up here, so a scanner spends nothing.
pub(crate) fn email_verify_page(query: &str) -> Reply {
    let Some(token) = token_from_query(query) else {
        return Reply::error(400, "token", "the link carries no token");
    };
    confirmation_page(
        "Bind your Tiamat recovery address",
        "Press the button to bind this address to your Tiamat identity. It will \
         only ever be used to confirm a name recovery you ask for.",
        "/v1/email/verify",
        token,
        "Bind this address",
    )
}

/// `POST /v1/email/verify` — the page's button: binds the address, once.
pub(crate) fn email_verify(
    service: &Service,
    query: &str,
    body: &[u8],
    deadline: &Deadline,
) -> Reply {
    let now = service.now();
    let Some(token) = token_from_form(body, query) else {
        return Reply::error(400, "token", "the request carries no token");
    };
    match service
        .db()
        .try_with(|connection| recover::verify_email(connection, token, now, deadline))
    {
        Ok(VerifyOutcome::Bound { address, .. }) => Reply::json(
            200,
            format!(
                "{{\"verified\":true,\"address\":\"{}\"}}",
                crate::http::json_escape(&address)
            ),
        ),
        Ok(VerifyOutcome::Unknown) => {
            Reply::error(400, "token", "this link is unknown, used, or expired")
        }
        Ok(VerifyOutcome::Late) => late(),
        Err(error) => flow_error(error),
    }
}

/// `POST /v1/recover` — ask to move a lost name to a new identity.
pub(crate) fn recover_start(
    service: &Service,
    body: &[u8],
    client: Option<IpAddr>,
    deadline: &Deadline,
) -> Reply {
    // Finish any notice that is due first: a name whose recovery has just
    // completed must not be asked about as if it still had its old holder.
    if let Err(error) = service.advance() {
        return internal(error);
    }
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
    if let Some(reply) = admit(
        service,
        &verified.new_uuid,
        &verified.lineage,
        verified.timestamp,
        now,
        Some(client),
        deadline,
    ) {
        return reply;
    }
    // As for `email`: the mail goes out after the lock is released.
    match recover::start_recovery(
        service.db(),
        mail_flow(service, client),
        &verified,
        now,
        deadline,
    ) {
        Ok(StartOutcome::Sent) => Reply::json(202, "{\"sent\":true}".to_owned()),
        Ok(StartOutcome::NotOwnKey) => not_own_key(),
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
        Ok(StartOutcome::Limited(limit)) => limited_mail(limit),
        Ok(StartOutcome::Mail(outcome)) => mail_reply(outcome),
        Ok(StartOutcome::Late) => late(),
        Err(error) => flow_error(error),
    }
}

/// `GET /v1/recover/confirm?t=<token>` — the link in the recovery mail.
///
/// A page with a button, for the reason [`email_verify_page`] gives, and with
/// more at stake: a scanner following this link would start a fourteen-day
/// notice to move somebody's name without anyone having pressed anything.
pub(crate) fn recover_confirm_page(query: &str) -> Reply {
    let Some(token) = token_from_query(query) else {
        return Reply::error(400, "token", "the link carries no token");
    };
    confirmation_page(
        "Confirm a Tiamat name recovery",
        "Pressing the button starts a fourteen-day public notice. When it ends, the \
         name moves to the identity that asked for it, unless its current holder \
         cancels. If you did not ask for this, close this page: nothing happens \
         until the button is pressed.",
        "/v1/recover/confirm",
        token,
        "Start the recovery notice",
    )
}

/// `POST /v1/recover/confirm` — the page's button: starts the notice.
pub(crate) fn recover_confirm(
    service: &Service,
    query: &str,
    body: &[u8],
    deadline: &Deadline,
) -> Reply {
    if let Err(error) = service.advance() {
        return internal(error);
    }
    let now = service.now();
    let Some(token) = token_from_form(body, query) else {
        return Reply::error(400, "token", "the request carries no token");
    };
    match service
        .db()
        .try_with(|connection| recover::confirm_recovery(connection, token, now, deadline))
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
        Ok(ConfirmOutcome::Late) => late(),
        Err(error) => flow_error(error),
    }
}

/// `POST /v1/recover/cancel` — the holder ends a notice in one request.
pub(crate) fn recover_cancel(
    service: &Service,
    body: &[u8],
    client: Option<IpAddr>,
    deadline: &Deadline,
) -> Reply {
    // A notice that has run its fourteen days completes before a late cancel
    // is read, rather than being undone by it.
    if let Err(error) = service.advance() {
        return internal(error);
    }
    let now = service.now();
    let request = match CancelRecoveryRequest::from_bytes(body) {
        Ok(request) => request,
        Err(error) => return malformed("recovery cancellation", &error),
    };
    let verified = match request.verify(now) {
        Ok(verified) => verified,
        Err(error) => return refused(&error),
    };
    if let Some(reply) = admit(
        service,
        &verified.uuid,
        &verified.lineage,
        verified.timestamp,
        now,
        client,
        deadline,
    ) {
        return reply;
    }
    match service
        .db()
        .try_with(|connection| recover::cancel_recovery(connection, &verified, now, deadline))
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
        Ok(CancelOutcome::Late) => late(),
        Err(error) => flow_error(error),
    }
}

/// `GET /v1/revocations` — the signed list of revoked certificate serials.
///
/// Servers that have internet may fetch it and hand it to their verifier;
/// servers that do not rely on the certificate's ninety-day lifetime. The list
/// is signed with the current key and carries, per entry, the key that issued
/// the revoked certificate.
pub(crate) fn revocations(service: &Service) -> Reply {
    let now = service.now();
    // Only revocations whose certificate could still verify somewhere: the
    // list is capped, and an expired serial on it would push off one that
    // matters.
    let entries = match service
        .db()
        .with(|connection| names::revocations_in_force(connection, now))
    {
        Ok(entries) => entries,
        Err(error) => return internal(error),
    };
    let list = service.keys().sign_revocations(now, entries);
    match postcard::to_allocvec(&list) {
        Ok(bytes) => Reply::octets(bytes),
        Err(error) => internal(error),
    }
}

/// The token from a link's query string, if it is the right shape.
///
/// The right shape is exactly what `tokens` issues — 32 bytes as 64 hex
/// characters — and nothing else, because the page echoes it into HTML: a
/// value that is only hex digits cannot be markup.
fn token_from_query(query: &str) -> Option<&str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The token from a form post (`t=<hex>` in the body), or failing that from
/// the query string, for a client that posts to the link itself.
fn token_from_form<'a>(body: &'a [u8], query: &'a str) -> Option<&'a str> {
    std::str::from_utf8(body)
        .ok()
        .map(str::trim)
        .and_then(token_from_query)
        .or_else(|| token_from_query(query))
}

/// A page with one sentence and one button that posts the token back.
///
/// Plain HTML with nothing to load: no script, no style sheet, no image — the
/// response's content security policy forbids all of them, and the page needs
/// none. `token` is already known to be 64 hex digits (`token_from_query`), so
/// it is safe to place in an attribute; the other strings are this module's
/// own constants.
fn confirmation_page(
    title: &str,
    explanation: &str,
    action: &str,
    token: &str,
    button: &str,
) -> Reply {
    Reply::html(
        200,
        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <meta name=\"robots\" content=\"noindex\">\n<title>{title}</title>\n</head>\n\
             <body>\n<h1>{title}</h1>\n<p>{explanation}</p>\n\
             <form method=\"post\" action=\"{action}\">\n\
             <input type=\"hidden\" name=\"t\" value=\"{token}\">\n\
             <button type=\"submit\">{button}</button>\n</form>\n</body>\n</html>\n"
        ),
    )
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

/// A verified value whose key is not shown to be its identity's: the same
/// refusal core gives a request that carries no proof for a key that is not
/// the root.
fn not_own_key() -> Reply {
    refused(&RegistryError::RootKeyRequired)
}

/// Runs a verified request past the registry's memory of retired keys
/// (`crate::retired::admit`), and answers it there if the memory refuses it.
///
/// `None` lets the request's own work run. A refusal is `403` with the
/// memory's own reason — `retired_key`, `commitment` or `too_many_keys` — so a
/// client told to sign with its successor is not mistaken for one with a bad
/// signature.
fn admit(
    service: &Service,
    uuid: &PlayerUuid,
    lineage: &KeyLineage,
    signed_at: i64,
    now: i64,
    client: Option<IpAddr>,
    deadline: &Deadline,
) -> Option<Reply> {
    match service.db().with(|connection| {
        crate::retired::admit(connection, uuid, lineage, signed_at, now, client, deadline)
    }) {
        Ok(Admission::Admitted) => None,
        Ok(Admission::Refused(refusal)) => {
            Some(Reply::error(403, refusal.reason(), refusal.message()))
        }
        Ok(Admission::Late) => Some(late()),
        Err(error) => Some(internal(error)),
    }
}

/// A mail allowance is spent; the message says which.
fn limited_mail(limit: MailLimit) -> Reply {
    Reply::error(429, "limited", limit.message())
}

/// The work reached its commit after the deadline and was rolled back: the
/// same answer the worker gives a request it abandons.
fn late() -> Reply {
    Reply::late()
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
        RegistryError::RootKeyRequired => Reply::error(
            403,
            "not_root",
            "this must be signed by the identity's root key, or carry the key-set proof \
             that reaches the signing key from it",
        ),
        RegistryError::ProofNotThisIdentity
        | RegistryError::ProofBrokenChain
        | RegistryError::ProofNotForThisKey
        | RegistryError::ProofEmpty
        | RegistryError::ProofTooLong { .. }
        | RegistryError::ProofRotationUncommitted
        | RegistryError::ProofSuccessorMismatch
        | RegistryError::ProofKeyReused
        | RegistryError::ProofMisplacedCommitment => Reply::error(403, "proof", &error.to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tiamat_core::identity::Identity;
    use tiamat_core::identity::registry::Name;

    use crate::clock::Clock;
    use crate::config::Config;
    use crate::mail::SinkMailer;

    const NOW: i64 = 1_800_000_000;

    #[derive(Debug)]
    struct Fixed;

    impl Clock for Fixed {
        fn now(&self) -> i64 {
            NOW
        }
    }

    fn service() -> (Service, Arc<SinkMailer>) {
        let mailer = Arc::new(SinkMailer::default());
        let config = Config {
            data_dir: None,
            bind: "127.0.0.1:0".parse().expect("bind"),
            key_seed: [7; 32],
            next_key_hash: None,
            base_url: "https://registry.example".to_owned(),
            mail_from: None,
            smtp_url: None,
            trust_forwarded_for: false,
            work_bits: 6,
            reserved: Vec::new(),
            workers: 1,
            mail_per_hour: 60,
            request_timeout: crate::http::REQUEST_TIMEOUT,
        };
        let service = Service::open(config, mailer.clone(), Arc::new(Fixed)).expect("service");
        (service, mailer)
    }

    fn client() -> Option<IpAddr> {
        Some("203.0.113.9".parse().expect("ip"))
    }

    fn claim_body(identity: &Identity, canonical: &str) -> Vec<u8> {
        let name = Name::from_parts(canonical, canonical).expect("name");
        let mut request = ClaimRequest::new(identity, &name, NOW);
        request.solve_work(6).expect("work");
        request.sign(identity);
        postcard::to_allocvec(&request).expect("encode")
    }

    fn email_request(identity: &Identity, address: &str) -> EmailRequest {
        let mut request = EmailRequest::new(identity, Some(address), NOW);
        request.sign(identity);
        request
    }

    fn count(service: &Service, table: &str) -> i64 {
        service
            .db()
            .with(|connection| {
                connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
            })
            .expect("count")
    }

    #[test]
    fn a_claim_past_its_deadline_is_answered_504_and_commits_nothing() {
        let (service, _mailer) = service();
        let alice = Identity::from_seed(&[1; 32]);
        let body = claim_body(&alice, "alice");
        let late = Deadline::starting(Instant::now(), Duration::ZERO);
        let reply = claim(&service, &body, client(), &late);
        assert_eq!(reply.status(), 504, "{}", reply.text());
        assert!(
            reply.text().contains("nothing was changed"),
            "{}",
            reply.text()
        );
        for table in ["names", "issued", "limits", "audit"] {
            assert_eq!(count(&service, table), 0, "{table}");
        }
        // The client's retry — the same body — is the claim, not `already_named`.
        let retry = claim(&service, &body, client(), &Deadline::none());
        assert_eq!(retry.status(), 200, "{}", retry.text());
    }

    #[test]
    fn an_identity_with_no_name_pays_for_its_mail_and_one_with_a_name_does_not() {
        let (service, mailer) = service();
        let stranger = Identity::from_seed(&[2; 32]);
        let mut request = email_request(&stranger, "stranger@example.com");
        let body = postcard::to_allocvec(&request).expect("encode");

        // No work: refused with the difficulty to mine, as a claim is.
        let reply = email(&service, &body, client(), &Deadline::none());
        assert_eq!(reply.status(), 400, "{}", reply.text());
        assert!(
            reply.text().contains("\"error\":\"work\""),
            "{}",
            reply.text()
        );
        assert!(
            reply.text().contains("\"required_bits\":6"),
            "{}",
            reply.text()
        );
        // Too little work: refused the same way, with what it found. The nonce
        // is in the body, outside the signature, so the request needs no new
        // signature for each one tried.
        let weak = (0..)
            .find(|nonce| {
                request.work = Some(*nonce);
                request.work_bits().is_some_and(|bits| bits < 6)
            })
            .expect("a weak nonce");
        request.work = Some(weak);
        let body = postcard::to_allocvec(&request).expect("encode");
        let reply = email(&service, &body, client(), &Deadline::none());
        assert_eq!(reply.status(), 400, "{}", reply.text());
        assert!(reply.text().contains("\"found_bits\""), "{}", reply.text());
        assert_eq!(mailer.count(), 0, "a refused request was mailed");
        assert_eq!(count(&service, "tokens"), 0);

        // Enough work: the link goes out.
        request.solve_work(6).expect("work");
        let body = postcard::to_allocvec(&request).expect("encode");
        let reply = email(&service, &body, client(), &Deadline::none());
        assert_eq!(reply.status(), 202, "{}", reply.text());
        assert_eq!(mailer.take().len(), 1);

        // An identity holding a name carries nothing.
        let holder = Identity::from_seed(&[3; 32]);
        let reply = claim(
            &service,
            &claim_body(&holder, "holder"),
            client(),
            &Deadline::none(),
        );
        assert_eq!(reply.status(), 200, "{}", reply.text());
        let body =
            postcard::to_allocvec(&email_request(&holder, "holder@example.com")).expect("encode");
        let reply = email(&service, &body, client(), &Deadline::none());
        assert_eq!(reply.status(), 202, "{}", reply.text());
    }

    #[test]
    fn a_mail_refusal_names_the_allowance_that_was_spent() {
        let (service, _mailer) = service();
        let holder = Identity::from_seed(&[4; 32]);
        assert_eq!(
            claim(
                &service,
                &claim_body(&holder, "holder"),
                client(),
                &Deadline::none()
            )
            .status(),
            200
        );
        for n in 0..3 {
            let body = postcard::to_allocvec(&email_request(&holder, &format!("h{n}@example.com")))
                .expect("encode");
            assert_eq!(
                email(&service, &body, client(), &Deadline::none()).status(),
                202
            );
        }
        let body =
            postcard::to_allocvec(&email_request(&holder, "h9@example.com")).expect("encode");
        let reply = email(&service, &body, client(), &Deadline::none());
        assert_eq!(reply.status(), 429, "{}", reply.text());
        assert!(
            reply.text().contains("three mails today"),
            "{}",
            reply.text()
        );
    }
}
