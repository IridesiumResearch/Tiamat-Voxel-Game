// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The rules of a name: who may claim one, how long they keep it, and what
//! happens when they let it go.
//!
//! Everything here runs inside **one transaction** and returns a value the
//! transport turns into a status code. That shape is deliberate: a claim is
//! several writes (a binding, a serial, three quota counters, an audit row) and
//! a failure at any of them must leave the database exactly as it was. A
//! rejected claim therefore returns a value from inside the transaction, and
//! the transaction's drop is the rollback.
//!
//! # The rules, in one place
//!
//! - **One name per identity.** A second active binding is refused before the
//!   new name is even consulted.
//! - **A released name waits thirty days** for anyone but its old holder, which
//!   is what makes a slip undoable (§3).
//! - **An unrenewed name expires** ninety days after it was claimed, and a
//!   renewed one twelve months after its last renewal; both then wait the same
//!   thirty days before another player may take them. This is the anti-squatter
//!   rule (§4).
//! - **Reserved names cannot be claimed**, from core's constant list or the
//!   operator's table — **nor can their lookalikes** (`lridesium`,
//!   `c0re_testkit`), by the fold in [`crate::confusable`]. The fold is for the
//!   reserved list only; between players' names it changes nothing.
//! - **A name the operator revoked from an identity stays out of its reach.**
//!   A release is its holder's to undo; a revocation is not (`revoked_holders`).
//! - **A renewal mints a certificate only when one is due.** Until the current
//!   certificate is inside its renewal window ([`RENEW_BEFORE_SECONDS`]), a
//!   renewal is answered with that same certificate — same serial, same expiry
//!   — so a client looping on renew cannot mint serials, and a recovery's
//!   revocation of "every certificate the old identity holds" stays small.
//! - **A claim names an identity, not just a key.** `verify_claim` has already
//!   shown the signing key to be the identity's own — the root, or a key a
//!   signed key-set chain reaches from it — and the handler has passed it
//!   through the memory of retired keys (`crate::retired`), so this layer
//!   takes the UUID as given and keeps no key store.
//! - **Nothing commits past the request's deadline.** Every write path ends in
//!   [`crate::deadline::commit`], so a client told "timed out" was told the
//!   truth: the work rolled back (review B11).

use std::net::IpAddr;

use rusqlite::{Connection, OptionalExtension, params};
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::{
    CERTIFICATE_LIFETIME_SECONDS, CLOCK_SKEW_SECONDS, MAX_REVOCATIONS, NameCertificate,
    RENEW_BEFORE_SECONDS, RESERVED_NAMES, Revocation, VerifiedClaim, VerifiedRelease, is_reserved,
};

use crate::db::write_transaction;
use crate::deadline::{Deadline, commit};
use crate::keys::RegistryKeys;
use crate::limits::{self, DAY};

/// How long a released name is held for the UUID that released it.
pub const HOLD_AFTER_RELEASE: i64 = 30 * DAY;

/// When a name never renewed since its claim stops being the holder's.
pub const EXPIRY_NEVER_RENEWED: i64 = 90 * DAY;

/// When a name that has been renewed stops being the holder's.
pub const EXPIRY_AFTER_RENEWAL: i64 = 365 * DAY;

/// The least time between two certificates minted for one binding: a day.
///
/// The renewal window ([`RENEW_BEFORE_SECONDS`]) already implies it at today's
/// lifetimes; it is stated on its own so a later, shorter lifetime cannot turn
/// renewals back into a serial mint.
pub const MIN_REISSUE_INTERVAL: i64 = DAY;

/// How long after its issue a certificate can still verify somewhere.
///
/// Its lifetime plus the clock skew a verifier allows. A revocation of a
/// certificate older than this protects nobody, which is what lets a recovery
/// and the published list leave it out.
pub const CERTIFICATE_LIVE_SECONDS: i64 = CERTIFICATE_LIFETIME_SECONDS + CLOCK_SKEW_SECONDS;

/// A binding as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) canonical: String,
    pub(crate) display: String,
    pub(crate) uuid: [u8; 32],
    pub(crate) claimed: i64,
    pub(crate) renewed: i64,
    pub(crate) released: Option<i64>,
    pub(crate) hold_until: Option<i64>,
    pub(crate) renewals: i64,
}

/// Which quota refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Two claims a day from this address.
    Ip,
    /// Six claims a day from this neighbourhood.
    Subnet,
    /// One claim per identity per thirty days.
    Claim,
    /// Three renames per identity per thirty days.
    Rename,
    /// Three casing changes per identity per thirty days, outside a renewal
    /// that mints.
    Display,
}

impl Limit {
    /// The sentence a player is shown.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Ip => "this address has claimed its two names for today; try again tomorrow",
            Self::Subnet => "this network has claimed its six names for today; try again tomorrow",
            Self::Claim => "this identity has claimed its name for this month",
            Self::Rename => "this identity has renamed three times in thirty days",
            Self::Display => {
                "this identity has changed its name's casing three times in thirty days; \
                 renew in the casing it has"
            }
        }
    }
}

/// What a claim did.
///
/// The certificate is the payload of the success path and the enum lives for
/// one request, so boxing it to even out the variants would cost an allocation
/// on every claim to save stack space nothing is short of.
#[allow(
    clippy::large_enum_variant,
    reason = "the certificate is the success path's payload; this enum lives for one request"
)]
#[derive(Debug)]
pub enum ClaimOutcome {
    /// The binding is written and here is its certificate.
    Issued {
        /// The signed certificate.
        certificate: NameCertificate,
        /// Set when a recovery is in flight against the name.
        notice: Option<i64>,
    },
    /// The name is reserved.
    Reserved,
    /// Somebody else holds it.
    Taken,
    /// This identity already has an active name.
    AlreadyNamed,
    /// The operator revoked this name from this identity.
    Revoked,
    /// A quota refused it.
    Limited(Limit),
    /// The request's deadline passed before the commit; nothing was written.
    Late,
}

/// What a renewal did.
#[allow(
    clippy::large_enum_variant,
    reason = "as `ClaimOutcome`: the certificate is the payload and the enum is one request long"
)]
#[derive(Debug)]
pub enum RenewOutcome {
    /// A fresh certificate for the standing binding.
    Renewed {
        /// The signed certificate.
        certificate: NameCertificate,
        /// Set when a recovery is in flight against the name.
        notice: Option<i64>,
    },
    /// No active binding for this identity.
    Unknown,
    /// The identity's name is not the one asked for.
    NotYours,
    /// A casing change the identity's allowance does not cover.
    Limited(Limit),
    /// The request's deadline passed before the commit; nothing was written.
    Late,
}

/// What a release did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// The binding is released and held for thirty days.
    Released,
    /// No active binding for this identity.
    Unknown,
    /// The identity's name is not the one asked for.
    NotYours,
    /// The request's deadline passed before the commit; nothing was written.
    Late,
}

/// Claims `claimed.name` for `claimed.uuid`, or says why not.
///
/// # Errors
///
/// Any SQL failure. A rule violation is an outcome, not an error.
pub fn claim(
    connection: &mut Connection,
    keys: &RegistryKeys,
    claimed: &VerifiedClaim,
    now: i64,
    client: IpAddr,
    deadline: &Deadline,
) -> rusqlite::Result<ClaimOutcome> {
    let canonical = claimed.name.canonical().to_owned();
    let uuid = claimed.uuid.to_hex();
    let transaction = write_transaction(connection)?;

    if reserved_or_lookalike(&transaction, &canonical)? {
        return Ok(ClaimOutcome::Reserved);
    }
    if active_binding(&transaction, &claimed.uuid)?.is_some() {
        return Ok(ClaimOutcome::AlreadyNamed);
    }
    // Before `is_free`, whose shortcut lets a releasing holder straight back
    // in: a revocation is not a release, and its target is not let back in.
    if revoked_from(&transaction, &canonical, &claimed.uuid)? {
        return Ok(ClaimOutcome::Revoked);
    }
    if let Some(existing) = binding(&transaction, &canonical)?
        && !is_free(&existing, &claimed.uuid, now)
    {
        return Ok(ClaimOutcome::Taken);
    }

    // Quotas last, so a claim that cannot be made does not spend one. All of
    // this is inside the transaction, so an early outcome rolls the takes back.
    let day = limits::window_start(now, DAY);
    if !limits::take(
        &transaction,
        &limits::ip_scope(client),
        day,
        limits::CLAIMS_PER_IP,
    )? {
        return Ok(ClaimOutcome::Limited(Limit::Ip));
    }
    if !limits::take(
        &transaction,
        &limits::subnet_scope(client),
        day,
        limits::CLAIMS_PER_SUBNET,
    )? {
        return Ok(ClaimOutcome::Limited(Limit::Subnet));
    }
    let renamed_before = any_binding(&transaction, &claimed.uuid)?;
    let thirty = limits::window_start(now, limits::THIRTY_DAYS);
    let (scope, limit, refused) = if renamed_before {
        (
            limits::rename_scope(&uuid),
            limits::RENAMES_PER_IDENTITY,
            Limit::Rename,
        )
    } else {
        (
            limits::claim_scope(&uuid),
            limits::CLAIMS_PER_IDENTITY,
            Limit::Claim,
        )
    };
    if !limits::take(&transaction, &scope, thirty, limit)? {
        return Ok(ClaimOutcome::Limited(refused));
    }

    let serial = next_serial(&transaction)?;
    write_binding(
        &transaction,
        &canonical,
        claimed.name.display(),
        &claimed.uuid,
        now,
    )?;
    record_issue(
        &transaction,
        serial,
        &claimed.uuid,
        &canonical,
        &keys.public_key().to_bytes(),
        now,
    )?;
    audit(
        &transaction,
        &uuid,
        &format!("claim {canonical}"),
        now,
        "claim",
        Some(client),
    )?;
    // What the claim's proof showed about the identity's keys: for a first
    // claim this is the first row the registry keeps of it, and the memory of
    // retired keys starts here (`crate::retired`, "When it is written").
    crate::retired::record(
        &transaction,
        &claimed.uuid,
        &claimed.lineage,
        claimed.timestamp,
        now,
        Some(client),
    )?;
    let certificate = keys.issue(&claimed.name, claimed.uuid, now, serial);
    let notice = crate::recover::pending_notice(&transaction, &canonical, now)?;
    if !commit(transaction, deadline)? {
        return Ok(ClaimOutcome::Late);
    }
    Ok(ClaimOutcome::Issued {
        certificate,
        notice,
    })
}

/// Renews the binding `claimed.name` already holds, or says why not.
///
/// A renewal before the current certificate is due (more than
/// [`RENEW_BEFORE_SECONDS`] left, or minted less than [`MIN_REISSUE_INTERVAL`]
/// ago) is answered with **that certificate again** — the same serial, issue
/// time and expiry, in the casing asked for — rather than refused, because a
/// client restoring from its phrase on a new device needs the certificate it
/// lost, and a refusal would leave it nameless for two months. It is not a
/// renewal for the expiry rules either: only a certificate minted inside the
/// window moves `renewed`, so claiming and renewing at once does not buy a
/// squatter the renewed name's twelve months.
///
/// A casing change on that path is counted against the identity's allowance
/// ([`limits::DISPLAY_CHANGES_PER_IDENTITY`] in thirty days) and audited; past
/// the allowance the renewal is refused, so a client looping on renew with
/// alternating casings cannot grow the audit table without bound. A renewal
/// that mints carries its casing in its own audit row and spends nothing.
///
/// # Errors
///
/// Any SQL failure.
pub fn renew(
    connection: &mut Connection,
    keys: &RegistryKeys,
    claimed: &VerifiedClaim,
    now: i64,
    deadline: &Deadline,
) -> rusqlite::Result<RenewOutcome> {
    let canonical = claimed.name.canonical().to_owned();
    let uuid = claimed.uuid.to_hex();
    let transaction = write_transaction(connection)?;
    let Some(existing) = active_binding(&transaction, &claimed.uuid)? else {
        return Ok(RenewOutcome::Unknown);
    };
    if existing.canonical != canonical {
        return Ok(RenewOutcome::NotYours);
    }

    let current_key = keys.public_key().to_bytes();
    if let Some(latest) = latest_issue(&transaction, &claimed.uuid, &canonical)?
        // A certificate from a retired key is replaced at once: after a
        // rotation every client is meant to move to the new key.
        && latest.registry_key == current_key
        && (now - latest.issued < MIN_REISSUE_INTERVAL
            || latest.issued + CERTIFICATE_LIFETIME_SECONDS - now > RENEW_BEFORE_SECONDS)
    {
        if existing.display != claimed.name.display() {
            if !limits::take(
                &transaction,
                &limits::display_scope(&uuid),
                limits::window_start(now, limits::THIRTY_DAYS),
                limits::DISPLAY_CHANGES_PER_IDENTITY,
            )? {
                return Ok(RenewOutcome::Limited(Limit::Display));
            }
            transaction.execute(
                "UPDATE names SET display = ?1 WHERE canonical = ?2",
                params![claimed.name.display(), canonical],
            )?;
            audit(
                &transaction,
                &uuid,
                &format!("display {canonical}"),
                now,
                "renewal",
                None,
            )?;
        }
        let certificate = keys.issue(&claimed.name, claimed.uuid, latest.issued, latest.serial);
        let notice = crate::recover::pending_notice(&transaction, &canonical, now)?;
        if !commit(transaction, deadline)? {
            return Ok(RenewOutcome::Late);
        }
        return Ok(RenewOutcome::Renewed {
            certificate,
            notice,
        });
    }

    let serial = next_serial(&transaction)?;
    // The display casing is the holder's to change; the canonical form cannot
    // move, which is what makes renewals safe to allow.
    transaction.execute(
        "UPDATE names SET renewed = ?1, renewals = renewals + 1, display = ?2
         WHERE canonical = ?3",
        params![now, claimed.name.display(), canonical],
    )?;
    record_issue(
        &transaction,
        serial,
        &claimed.uuid,
        &canonical,
        &keys.public_key().to_bytes(),
        now,
    )?;
    audit(
        &transaction,
        &uuid,
        &format!("renew {canonical}"),
        now,
        "renewal",
        None,
    )?;
    let certificate = keys.issue(&claimed.name, claimed.uuid, now, serial);
    let notice = crate::recover::pending_notice(&transaction, &canonical, now)?;
    if !commit(transaction, deadline)? {
        return Ok(RenewOutcome::Late);
    }
    Ok(RenewOutcome::Renewed {
        certificate,
        notice,
    })
}

/// Releases the binding `request.name` holds, or says why not.
///
/// # Errors
///
/// Any SQL failure.
pub fn release(
    connection: &mut Connection,
    request: &VerifiedRelease,
    now: i64,
    deadline: &Deadline,
) -> rusqlite::Result<ReleaseOutcome> {
    let uuid = request.uuid.to_hex();
    let transaction = write_transaction(connection)?;
    let Some(existing) = active_binding(&transaction, &request.uuid)? else {
        return Ok(ReleaseOutcome::Unknown);
    };
    if existing.canonical != request.name {
        return Ok(ReleaseOutcome::NotYours);
    }
    // **The rename allowance is spent by the claim, not here.** A rename is a
    // release followed by a claim, and counting both halves would let a player
    // make one and a half renames; counting the claim alone makes the limit say
    // what it means. A player who releases and never claims again has spent
    // nothing, and a name freed stays free.
    transaction.execute(
        "UPDATE names SET released = ?1, hold_until = ?2 WHERE canonical = ?3",
        params![now, now + HOLD_AFTER_RELEASE, existing.canonical],
    )?;
    audit(
        &transaction,
        &uuid,
        &format!("release {}", existing.canonical),
        now,
        "release",
        None,
    )?;
    if !commit(transaction, deadline)? {
        return Ok(ReleaseOutcome::Late);
    }
    Ok(ReleaseOutcome::Released)
}

/// What the public name page says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameStatus {
    /// The canonical name asked about.
    pub canonical: String,
    /// The holder's casing, while the name is held.
    pub display: Option<String>,
    /// The holder, while the name is held and not released.
    pub uuid: Option<String>,
    /// Whether the name is reserved, and so not claimable by anyone.
    pub reserved: bool,
    /// Whether a recovery is in flight against it.
    pub recovery_pending: bool,
    /// When that recovery's public notice ends, while one is running.
    ///
    /// This is the notice the design promises (§4): the page is where anyone —
    /// the holder on another machine, a friend, a server operator — sees that
    /// the name is about to move, and when.
    pub recovery_notice_until: Option<i64>,
    /// When a name that is not free becomes claimable, if that is knowable.
    pub free_after: Option<i64>,
}

/// Reads the public state of a name.
///
/// # Errors
///
/// Any SQL failure.
pub fn status(connection: &Connection, canonical: &str, now: i64) -> rusqlite::Result<NameStatus> {
    // The page says what a claim would be told: a lookalike of a reserved name
    // is reported reserved, so a player who checks first is not surprised.
    let reserved = reserved_or_lookalike(connection, canonical)?;
    match binding(connection, canonical)? {
        None => Ok(NameStatus {
            canonical: canonical.to_owned(),
            display: None,
            uuid: None,
            reserved,
            recovery_pending: false,
            recovery_notice_until: None,
            free_after: None,
        }),
        Some(binding) => {
            let free_at = free_at(&binding);
            let held = binding.released.is_none() && now < free_at;
            // Only a held name can be recovered: a notice against a released
            // one is cancelled when the notice would end, so it is not news.
            let notice = if held {
                crate::recover::pending_notice(connection, canonical, now)?
            } else {
                None
            };
            Ok(NameStatus {
                canonical: canonical.to_owned(),
                display: held.then(|| binding.display.clone()),
                uuid: held.then(|| PlayerUuid::from_bytes(binding.uuid).to_hex()),
                reserved,
                recovery_pending: notice.is_some(),
                recovery_notice_until: notice,
                free_after: (!held && free_at > now).then_some(free_at),
            })
        }
    }
}

/// The newest certificate minted for one identity's binding of one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Issue {
    serial: u64,
    registry_key: [u8; 32],
    issued: i64,
}

/// The newest row of `issued` for `uuid` and `canonical`, if any.
fn latest_issue(
    connection: &Connection,
    uuid: &PlayerUuid,
    canonical: &str,
) -> rusqlite::Result<Option<Issue>> {
    let row: Option<(i64, Vec<u8>, i64)> = connection
        .query_row(
            "SELECT serial, registry_key, issued FROM issued
             WHERE uuid = ?1 AND canonical = ?2 ORDER BY serial DESC LIMIT 1",
            params![uuid.as_bytes().as_slice(), canonical],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    // Written by this service: a serial is never negative and a key is always
    // 32 bytes. A row that is neither is treated as absent, which mints a fresh
    // certificate rather than re-signing a guess.
    Ok(row.and_then(|(serial, key, issued)| {
        Some(Issue {
            serial: u64::try_from(serial).ok()?,
            registry_key: <[u8; 32]>::try_from(key.as_slice()).ok()?,
            issued,
        })
    }))
}

/// The certificates issued to `uuid` that could still verify at `now`.
///
/// What a recovery or an operator's revocation puts on the list: an older one
/// has expired everywhere already, and listing it would only crowd out the
/// revocations that matter (the list is capped at [`MAX_REVOCATIONS`]).
///
/// # Errors
///
/// Any SQL failure.
pub(crate) fn live_issues(
    connection: &Connection,
    uuid: &[u8],
    now: i64,
) -> rusqlite::Result<Vec<(Vec<u8>, i64)>> {
    let mut statement = connection
        .prepare("SELECT registry_key, serial FROM issued WHERE uuid = ?1 AND issued > ?2")?;
    let rows = statement.query_map(params![uuid, now - CERTIFICATE_LIVE_SECONDS], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    rows.collect()
}

/// The revocations worth publishing at `now`, newest first, capped.
///
/// A revocation whose certificate has expired everywhere is left out. One with
/// no `issued` row (nothing this service writes, but an operator's hand-made
/// row is possible) is kept, because its expiry cannot be known and leaving
/// out a revocation that still matters is the worse mistake.
///
/// # Errors
///
/// Any SQL failure.
pub fn revocations_in_force(
    connection: &Connection,
    now: i64,
) -> rusqlite::Result<Vec<Revocation>> {
    let mut statement = connection.prepare(
        "SELECT r.registry_key, r.serial FROM revocations r
         LEFT JOIN issued i ON i.serial = r.serial AND i.registry_key = r.registry_key
         WHERE i.serial IS NULL OR i.issued > ?1
         ORDER BY r.when_at DESC, r.serial DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![now - CERTIFICATE_LIVE_SECONDS, MAX_REVOCATIONS as i64],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let mut entries = Vec::new();
    for row in rows {
        let (key, serial) = row?;
        if let (Ok(key), Ok(serial)) = (<[u8; 32]>::try_from(key.as_slice()), u64::try_from(serial))
        {
            entries.push(Revocation {
                registry_key: key,
                serial,
            });
        }
    }
    Ok(entries)
}

/// Deletes revocations whose certificate can no longer verify anywhere.
///
/// The audit table keeps the action; this keeps the published list to the
/// revocations that still protect someone.
///
/// # Errors
///
/// Any SQL failure.
pub fn prune_revocations(connection: &Connection, now: i64) -> rusqlite::Result<usize> {
    connection.execute(
        "DELETE FROM revocations WHERE EXISTS (
             SELECT 1 FROM issued i
             WHERE i.serial = revocations.serial
               AND i.registry_key = revocations.registry_key
               AND i.issued <= ?1)",
        [now - CERTIFICATE_LIVE_SECONDS],
    )
}

/// Whether the operator revoked `canonical` from `uuid`.
fn revoked_from(
    connection: &Connection,
    canonical: &str,
    uuid: &PlayerUuid,
) -> rusqlite::Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM revoked_holders WHERE canonical = ?1 AND uuid = ?2",
        params![canonical, uuid.as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Whether a name is reserved, or reads as one that is.
///
/// Reserved means core's constant list or the operator's table; "reads as" is
/// [`crate::confusable::lookalikes`], applied to those names and nothing else.
/// The table is small (the host's list and the operator's additions), so it is
/// read whole rather than indexed by skeleton.
pub(crate) fn reserved_or_lookalike(
    connection: &Connection,
    canonical: &str,
) -> rusqlite::Result<bool> {
    if is_reserved(canonical)
        || RESERVED_NAMES
            .iter()
            .any(|reserved| crate::confusable::lookalikes(canonical, reserved))
    {
        return Ok(true);
    }
    let mut statement = connection.prepare_cached("SELECT canonical FROM reserved")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for reserved in rows {
        if crate::confusable::lookalikes(canonical, &reserved?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `uuid` holds a name right now: bound, not released, not lapsed.
///
/// What the email flow asks before waiving its proof of work. A lapsed binding
/// (past its expiry and hold) is not a name held, though its row may remain
/// until the retention sweep or a new claim replaces it.
///
/// # Errors
///
/// Any SQL failure.
pub(crate) fn holds_name(
    connection: &Connection,
    uuid: &PlayerUuid,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(active_binding(connection, uuid)?.is_some_and(|binding| now < free_at(&binding)))
}

/// One binding by canonical name.
pub(crate) fn binding(
    connection: &Connection,
    canonical: &str,
) -> rusqlite::Result<Option<Binding>> {
    connection
        .query_row(
            "SELECT canonical, display, uuid, claimed, renewed, released, hold_until, renewals
             FROM names WHERE canonical = ?1",
            [canonical],
            binding_from_row,
        )
        .optional()
}

/// The active binding of one identity, if it has one.
pub(crate) fn active_binding(
    connection: &Connection,
    uuid: &PlayerUuid,
) -> rusqlite::Result<Option<Binding>> {
    connection
        .query_row(
            "SELECT canonical, display, uuid, claimed, renewed, released, hold_until, renewals
             FROM names WHERE uuid = ?1 AND released IS NULL",
            [uuid.as_bytes().as_slice()],
            binding_from_row,
        )
        .optional()
}

/// Whether an identity has ever bound a name, released or not.
fn any_binding(connection: &Connection, uuid: &PlayerUuid) -> rusqlite::Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM names WHERE uuid = ?1",
        [uuid.as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Reads a row into a [`Binding`].
fn binding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Binding> {
    Ok(Binding {
        canonical: row.get(0)?,
        display: row.get(1)?,
        uuid: row.get(2)?,
        claimed: row.get(3)?,
        renewed: row.get(4)?,
        released: row.get(5)?,
        hold_until: row.get(6)?,
        renewals: row.get(7)?,
    })
}

/// Writes or replaces the binding for a claim.
///
/// `ON CONFLICT` is how a released or expired row is reused: the holder's old
/// history is replaced by a new binding for whoever claims it next.
fn write_binding(
    connection: &Connection,
    canonical: &str,
    display: &str,
    uuid: &PlayerUuid,
    now: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO names
             (canonical, display, uuid, claimed, renewed, released, hold_until, renewals)
         VALUES (?1, ?2, ?3, ?4, ?4, NULL, NULL, 0)
         ON CONFLICT(canonical) DO UPDATE SET
             display = excluded.display,
             uuid = excluded.uuid,
             claimed = excluded.claimed,
             renewed = excluded.renewed,
             released = NULL,
             hold_until = NULL,
             renewals = 0",
        params![canonical, display, uuid.as_bytes().as_slice(), now],
    )?;
    Ok(())
}

/// When a binding becomes claimable by anyone.
fn free_at(binding: &Binding) -> i64 {
    if binding.released.is_some() {
        return binding
            .hold_until
            .unwrap_or_else(|| binding.released.unwrap_or(0) + HOLD_AFTER_RELEASE);
    }
    let expiry = if binding.renewals > 0 {
        binding.renewed + EXPIRY_AFTER_RENEWAL
    } else {
        binding.claimed + EXPIRY_NEVER_RENEWED
    };
    expiry + HOLD_AFTER_RELEASE
}

/// Whether `claimant` may take this binding now.
fn is_free(binding: &Binding, claimant: &PlayerUuid, now: i64) -> bool {
    // A released name is its old holder's to reclaim immediately: that is what
    // makes a rename a release plus a claim rather than a thirty-day wait.
    if binding.uuid == *claimant.as_bytes() {
        return true;
    }
    now >= free_at(binding)
}

/// Allocates the next certificate serial.
///
/// Monotonic per registry key and never reused; a gap is harmless, because a
/// serial is an identifier for revocation rather than a count of anything.
fn next_serial(connection: &Connection) -> rusqlite::Result<u64> {
    let current: Option<Vec<u8>> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'next_serial'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let next = current
        .and_then(|bytes| <[u8; 8]>::try_from(bytes.as_slice()).ok())
        .map_or(1, u64::from_le_bytes);
    connection.execute(
        "INSERT INTO meta (key, value) VALUES ('next_serial', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![next.saturating_add(1).to_le_bytes().to_vec()],
    )?;
    Ok(next)
}

/// Records a certificate the registry has issued, for a later revocation.
///
/// The certificate itself is the client's; this is the serial and the key that
/// signed it, which is all a revocation names. A recovery needs every serial of
/// the old identity, and there is no way to reconstruct them from a hash.
fn record_issue(
    connection: &Connection,
    serial: u64,
    uuid: &PlayerUuid,
    canonical: &str,
    registry_key: &[u8; 32],
    now: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO issued (serial, registry_key, uuid, canonical, issued)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            serial,
            registry_key.as_slice(),
            uuid.as_bytes().as_slice(),
            canonical,
            now
        ],
    )?;
    Ok(())
}

/// Writes one audit row.
///
/// `who` is a UUID rather than a name: names change, identities do not. `why`
/// is a fixed word or the operator's reason, **never** an address: the client
/// address a rate limit needs goes in `ip`, which the retention sweep clears
/// after seven days (§8) while the row itself is kept.
pub(crate) fn audit(
    connection: &Connection,
    who: &str,
    what: &str,
    now: i64,
    why: &str,
    ip: Option<IpAddr>,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO audit (who, what, when_at, why, ip) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![who, what, now, why, ip.map(|ip| ip.to_string())],
    )?;
    Ok(())
}

/// The canonical form, checked the way the wire enforces it.
///
/// Exposed for the name page, which must not echo back something that is not a
/// name at all.
///
/// # Errors
///
/// [`tiamat_core::identity::registry::RegistryError`] when it is not canonical.
pub fn check_name(canonical: &str) -> Result<(), tiamat_core::identity::registry::RegistryError> {
    tiamat_core::identity::registry::check_canonical(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::keys::RegistryKeys;
    use ed25519_dalek::SigningKey;
    use tiamat_core::identity::registry::{KeyLineage, Name, verify_certificate};

    const NOW: i64 = 1_800_000_000;

    struct Fixture {
        db: Db,
        keys: RegistryKeys,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                db: Db::in_memory().expect("db"),
                keys: RegistryKeys::open([7; 32], None, Vec::new()).expect("keys"),
            }
        }

        fn claim(
            &mut self,
            canonical: &str,
            display: &str,
            player: u8,
            ip: &str,
            now: i64,
        ) -> ClaimOutcome {
            let identity = SigningKey::from_bytes(&[player; 32]);
            let verified = VerifiedClaim {
                name: Name::from_parts(canonical, display).expect("name"),
                uuid: PlayerUuid::from_root_key(&identity.verifying_key()),
                public_key: identity.verifying_key(),
                timestamp: now,
                lineage: KeyLineage::of_root(identity.verifying_key()),
            };
            let ip: IpAddr = ip.parse().expect("ip");
            self.db
                .with(|connection| {
                    claim(
                        connection,
                        &self.keys,
                        &verified,
                        now,
                        ip,
                        &Deadline::none(),
                    )
                })
                .expect("claim")
        }

        fn renew(&mut self, canonical: &str, display: &str, player: u8, now: i64) -> RenewOutcome {
            let identity = SigningKey::from_bytes(&[player; 32]);
            let verified = VerifiedClaim {
                name: Name::from_parts(canonical, display).expect("name"),
                uuid: PlayerUuid::from_root_key(&identity.verifying_key()),
                public_key: identity.verifying_key(),
                timestamp: now,
                lineage: KeyLineage::of_root(identity.verifying_key()),
            };
            self.db
                .with(|connection| renew(connection, &self.keys, &verified, now, &Deadline::none()))
                .expect("renew")
        }

        fn release(&mut self, canonical: &str, player: u8, now: i64) -> ReleaseOutcome {
            let identity = SigningKey::from_bytes(&[player; 32]);
            let verified = VerifiedRelease {
                name: canonical.to_owned(),
                uuid: PlayerUuid::from_root_key(&identity.verifying_key()),
                public_key: identity.verifying_key(),
                timestamp: now,
                lineage: KeyLineage::of_root(identity.verifying_key()),
            };
            self.db
                .with(|connection| release(connection, &verified, now, &Deadline::none()))
                .expect("release")
        }

        fn uuid_of(player: u8) -> PlayerUuid {
            PlayerUuid::from_root_key(&SigningKey::from_bytes(&[player; 32]).verifying_key())
        }

        fn certificate_of(outcome: ClaimOutcome) -> NameCertificate {
            match outcome {
                ClaimOutcome::Issued { certificate, .. } => certificate,
                other => panic!("expected a certificate, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_claim_issues_a_certificate_that_verifies_offline() {
        let mut fixture = Fixture::new();
        let certificate =
            Fixture::certificate_of(fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW));
        assert_eq!(certificate.name, "alice");
        assert_eq!(certificate.display, "Alice");
        assert_eq!(certificate.serial, 1);

        let wire = certificate.to_bytes().expect("encode");
        let name = verify_certificate(&wire, &Fixture::uuid_of(1), NOW, fixture.keys.trust(), None)
            .expect("the registry's own certificate");
        assert_eq!(name.display(), "Alice");
    }

    #[test]
    fn a_taken_name_is_refused_and_a_released_one_waits() {
        let mut fixture = Fixture::new();
        fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW);
        assert!(matches!(
            fixture.claim("alice", "Alice", 2, "203.0.113.2", NOW + 10),
            ClaimOutcome::Taken
        ));

        assert_eq!(
            fixture.release("alice", 1, NOW + 20),
            ReleaseOutcome::Released
        );
        // Inside the thirty-day hold: nobody else.
        assert!(matches!(
            fixture.claim("alice", "ALICE", 2, "203.0.113.2", NOW + 21),
            ClaimOutcome::Taken
        ));
        // After the hold, anyone may take it.
        assert!(matches!(
            fixture.claim(
                "alice",
                "Alice",
                2,
                "198.51.100.7",
                NOW + 20 + HOLD_AFTER_RELEASE
            ),
            ClaimOutcome::Issued { .. }
        ));

        // A released name is its old holder's to reclaim immediately; that is
        // what makes a rename a release plus a claim rather than a wait.
        fixture.claim("bobby", "Bobby", 3, "203.0.113.3", NOW);
        assert_eq!(
            fixture.release("bobby", 3, NOW + 30),
            ReleaseOutcome::Released
        );
        assert!(matches!(
            fixture.claim("bobby", "BOBBY", 3, "203.0.113.4", NOW + 31),
            ClaimOutcome::Issued { .. }
        ));
    }

    #[test]
    fn one_active_name_per_identity() {
        let mut fixture = Fixture::new();
        fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW);
        assert!(matches!(
            fixture.claim("bob", "Bob", 1, "203.0.113.1", NOW + 1),
            ClaimOutcome::AlreadyNamed
        ));
    }

    #[test]
    fn reserved_names_are_refused_from_the_table_and_from_core() {
        let mut fixture = Fixture::new();
        fixture
            .db
            .with(|connection| {
                connection.execute("INSERT INTO reserved (canonical) VALUES ('secretname')", [])?;
                Ok(())
            })
            .expect("reserve");
        assert!(matches!(
            fixture.claim("secretname", "SecretName", 1, "203.0.113.1", NOW),
            ClaimOutcome::Reserved
        ));
        // The core list is checked earlier in the flow (`Name::claim`), but the
        // claim function must not depend on that having happened.
        assert!(matches!(
            fixture.claim("admin", "Admin", 2, "203.0.113.2", NOW),
            ClaimOutcome::Reserved
        ));
    }

    #[test]
    fn the_daily_address_and_network_quotas_hold() {
        let mut fixture = Fixture::new();
        // Two claims from `203.0.113.1`, which is that address's day.
        fixture.claim("alice", "alice", 1, "203.0.113.1", NOW);
        fixture.claim("bobby", "bobby", 2, "203.0.113.1", NOW + 1);
        assert!(matches!(
            fixture.claim("carol", "carol", 3, "203.0.113.1", NOW + 2),
            ClaimOutcome::Limited(Limit::Ip)
        ));

        // Three more from elsewhere in the /24 makes six for the network; the
        // seventh from the same neighbourhood is refused even from a new
        // address it has not seen.
        fixture.claim("carol", "carol", 3, "203.0.113.2", NOW + 3);
        fixture.claim("dave", "dave", 4, "203.0.113.3", NOW + 4);
        fixture.claim("erin", "erin", 5, "203.0.113.4", NOW + 5);
        fixture.claim("frank", "frank", 6, "203.0.113.5", NOW + 6);
        assert!(matches!(
            fixture.claim("grace", "grace", 7, "203.0.113.6", NOW + 7),
            ClaimOutcome::Limited(Limit::Subnet)
        ));
    }

    #[test]
    fn three_renames_a_month_and_then_no_more() {
        let mut fixture = Fixture::new();
        fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW);
        // A different address for each, so the per-address cap is not what
        // refuses the fourth rename — the identity's own allowance is.
        let names = ["alice2", "alice3", "alice4", "alice5"];
        let mut current = "alice".to_owned();
        for (index, name) in names.iter().enumerate() {
            let at = NOW + 100 + index as i64;
            let ip = format!("198.51.100.{}", index + 1);
            assert_eq!(
                fixture.release(&current, 1, at),
                ReleaseOutcome::Released,
                "{current}"
            );
            let outcome = fixture.claim(name, name, 1, &ip, at + 1);
            if index < 3 {
                assert!(
                    matches!(outcome, ClaimOutcome::Issued { .. }),
                    "{name}: {outcome:?}"
                );
                current = (*name).to_owned();
            } else {
                assert!(matches!(outcome, ClaimOutcome::Limited(Limit::Rename)));
            }
        }
    }

    #[test]
    fn a_renewal_issues_a_fresh_serial_and_keeps_the_binding() {
        let mut fixture = Fixture::new();
        let first = Fixture::certificate_of(fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW));
        // Inside the renewal window (under thirty days left): a new serial.
        let due = NOW + 61 * DAY;
        match fixture.renew("alice", "ALICE", 1, due) {
            RenewOutcome::Renewed { certificate, .. } => {
                assert_eq!(certificate.serial, first.serial + 1);
                assert_eq!(certificate.display, "ALICE");
                assert_eq!(certificate.expires, due + 90 * DAY);
            }
            other => panic!("expected a renewal, got {other:?}"),
        }
        assert!(matches!(
            fixture.renew("bob", "Bob", 1, NOW + 101),
            RenewOutcome::NotYours
        ));
        assert!(matches!(
            fixture.renew("alice", "Alice", 2, NOW + 102),
            RenewOutcome::Unknown
        ));
    }

    #[test]
    fn an_unrenewed_name_expires_after_ninety_days_plus_the_hold() {
        let mut fixture = Fixture::new();
        fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW);
        let too_soon = NOW + EXPIRY_NEVER_RENEWED + HOLD_AFTER_RELEASE - 1;
        assert!(matches!(
            fixture.claim("alice", "Alice", 2, "198.51.100.1", too_soon),
            ClaimOutcome::Taken
        ));
        let free = NOW + EXPIRY_NEVER_RENEWED + HOLD_AFTER_RELEASE;
        assert!(matches!(
            fixture.claim("alice", "Alice", 2, "198.51.100.1", free),
            ClaimOutcome::Issued { .. }
        ));
    }

    #[test]
    fn a_renewal_before_it_is_due_answers_the_current_certificate() {
        let mut fixture = Fixture::new();
        let first = Fixture::certificate_of(fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW));
        // A client looping on renew: every answer is the same certificate,
        // and the registry has minted exactly one serial for this identity.
        for step in 1..=50 {
            match fixture.renew("alice", "Alice", 1, NOW + step * 60) {
                RenewOutcome::Renewed { certificate, .. } => assert_eq!(certificate, first),
                other => panic!("expected the current certificate, got {other:?}"),
            }
        }
        // A new casing is honoured on the same serial and the same expiry.
        match fixture.renew("alice", "ALICE", 1, NOW + 59 * DAY) {
            RenewOutcome::Renewed { certificate, .. } => {
                assert_eq!(certificate.serial, first.serial);
                assert_eq!(certificate.expires, first.expires);
                assert_eq!(certificate.display, "ALICE");
            }
            other => panic!("expected the current certificate, got {other:?}"),
        }
        let (minted, renewals): (i64, i64) = fixture
            .db
            .with(|connection| {
                Ok((
                    connection.query_row("SELECT COUNT(*) FROM issued", [], |row| row.get(0))?,
                    connection.query_row(
                        "SELECT renewals FROM names WHERE canonical = 'alice'",
                        [],
                        |row| row.get(0),
                    )?,
                ))
            })
            .expect("counts");
        assert_eq!(minted, 1);
        // Not a renewal for the expiry rules: claim-and-renew does not buy the
        // twelve months a genuinely renewed name gets.
        assert_eq!(renewals, 0);
    }

    #[test]
    fn a_recovery_or_the_list_names_only_certificates_that_can_still_verify() {
        let fixture = Fixture::new();
        let uuid = Fixture::uuid_of(1);
        let key = fixture.keys.public_key().to_bytes();
        fixture
            .db
            .with(|connection| {
                for (serial, issued) in [
                    (1_i64, NOW - 400 * DAY),
                    (2, NOW - 100 * DAY),
                    (3, NOW - DAY),
                ] {
                    connection.execute(
                        "INSERT INTO issued (serial, registry_key, uuid, canonical, issued)
                         VALUES (?1, ?2, ?3, 'alice', ?4)",
                        params![serial, key.as_slice(), uuid.as_bytes().as_slice(), issued],
                    )?;
                    connection.execute(
                        "INSERT INTO revocations (registry_key, serial, when_at, reason)
                         VALUES (?1, ?2, ?3, 'test')",
                        params![key.as_slice(), serial, NOW],
                    )?;
                }
                // A hand-made revocation with no issue row is kept.
                connection.execute(
                    "INSERT INTO revocations (registry_key, serial, when_at, reason)
                     VALUES (?1, 99, ?2, 'operator')",
                    params![key.as_slice(), NOW],
                )?;
                Ok(())
            })
            .expect("seed");

        let live = fixture
            .db
            .with(|connection| live_issues(connection, uuid.as_bytes(), NOW))
            .expect("live");
        assert_eq!(
            live.iter().map(|(_, serial)| *serial).collect::<Vec<_>>(),
            vec![3]
        );

        let published = fixture
            .db
            .with(|connection| revocations_in_force(connection, NOW))
            .expect("published");
        let mut serials: Vec<u64> = published.iter().map(|entry| entry.serial).collect();
        serials.sort_unstable();
        assert_eq!(serials, vec![3, 99]);

        let pruned = fixture
            .db
            .with(|connection| prune_revocations(connection, NOW))
            .expect("prune");
        assert_eq!(pruned, 2);
    }

    #[test]
    fn a_renewed_name_expires_a_year_after_its_last_renewal() {
        let mut fixture = Fixture::new();
        fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW);
        let renewed = NOW + 61 * DAY;
        fixture.renew("alice", "Alice", 1, renewed);
        // Long past the never-renewed expiry, still the holder's.
        assert!(matches!(
            fixture.claim(
                "alice",
                "Alice",
                2,
                "198.51.100.1",
                NOW + EXPIRY_NEVER_RENEWED + HOLD_AFTER_RELEASE + 1
            ),
            ClaimOutcome::Taken
        ));
        let free = renewed + EXPIRY_AFTER_RENEWAL + HOLD_AFTER_RELEASE;
        assert!(matches!(
            fixture.claim("alice", "Alice", 2, "198.51.100.1", free),
            ClaimOutcome::Issued { .. }
        ));
    }

    #[test]
    fn the_name_page_says_who_holds_it_and_when_it_frees() {
        let mut fixture = Fixture::new();
        fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW);
        let page = fixture
            .db
            .with(|connection| status(connection, "alice", NOW + 1))
            .expect("status");
        assert!(page.display.as_deref() == Some("Alice"));
        assert_eq!(page.uuid, Some(Fixture::uuid_of(1).to_hex()));
        assert!(!page.reserved);
        assert!(!page.recovery_pending);
        assert_eq!(page.recovery_notice_until, None);
        assert_eq!(page.free_after, None);

        // A notice running against the name is on its page, with its end.
        let until = NOW + 14 * DAY;
        fixture
            .db
            .with(|connection| {
                connection.execute(
                    "INSERT INTO recoveries
                         (canonical, from_uuid, to_uuid, confirmed_at, notice_until, state)
                     VALUES ('alice', ?1, ?2, ?3, ?4, 'pending')",
                    params![
                        Fixture::uuid_of(1).as_bytes().as_slice(),
                        Fixture::uuid_of(2).as_bytes().as_slice(),
                        NOW,
                        until
                    ],
                )?;
                Ok(())
            })
            .expect("notice");
        let page = fixture
            .db
            .with(|connection| status(connection, "alice", NOW + 2))
            .expect("status");
        assert!(page.recovery_pending);
        assert_eq!(page.recovery_notice_until, Some(until));
        fixture
            .db
            .with(|connection| {
                connection.execute("DELETE FROM recoveries", [])?;
                Ok(())
            })
            .expect("clear");

        fixture.release("alice", 1, NOW + 10);
        let page = fixture
            .db
            .with(|connection| status(connection, "alice", NOW + 11))
            .expect("status");
        assert!(
            page.display.is_none(),
            "a released name does not name its holder"
        );
        assert_eq!(page.free_after, Some(NOW + 10 + HOLD_AFTER_RELEASE));

        let free = fixture
            .db
            .with(|connection| status(connection, "alice", NOW + 10 + HOLD_AFTER_RELEASE))
            .expect("status");
        assert_eq!(free.free_after, None);
    }

    fn rows(fixture: &Fixture, table: &str) -> i64 {
        fixture
            .db
            .with(|connection| {
                connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
            })
            .expect("count")
    }

    #[test]
    fn a_lookalike_of_a_reserved_name_is_refused_with_the_reserved_reason() {
        let mut fixture = Fixture::new();
        // A host's reserved id, as `REGISTRY_RESERVED` seeds it.
        fixture
            .db
            .with(|connection| {
                connection.execute(
                    "INSERT INTO reserved (canonical) VALUES ('core_testkit')",
                    [],
                )?;
                Ok(())
            })
            .expect("reserve");
        // Core's own list and the host's, folded: `l` and `1` read as `i`, `0`
        // as `o`.
        for (seed, lookalike) in [
            (1u8, "lridesium"),
            (2, "ir1desium"),
            (3, "c0re_testkit"),
            (4, "core_testk1t"),
            (5, "c0nsole"),
        ] {
            let ip = format!("198.51.{seed}.1");
            assert!(
                matches!(
                    fixture.claim(lookalike, lookalike, seed, &ip, NOW),
                    ClaimOutcome::Reserved
                ),
                "{lookalike} was not refused"
            );
            let page = fixture
                .db
                .with(|connection| status(connection, lookalike, NOW))
                .expect("status");
            assert!(page.reserved, "{lookalike}'s page does not say reserved");
        }
        assert_eq!(rows(&fixture, "names"), 0);
    }

    #[test]
    fn players_names_are_never_folded_against_each_other() {
        // The owner's decision: the fold is for the reserved list only.
        let mut fixture = Fixture::new();
        for (seed, name) in [(1u8, "alice"), (2, "a1ice"), (3, "bob0"), (4, "boboo")] {
            let ip = format!("198.51.{seed}.1");
            assert!(
                matches!(
                    fixture.claim(name, name, seed, &ip, NOW),
                    ClaimOutcome::Issued { .. }
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn casing_changes_outside_a_mint_are_three_a_month() {
        let mut fixture = Fixture::new();
        let first = Fixture::certificate_of(fixture.claim("alice", "alice", 1, "203.0.113.1", NOW));
        let thirty = limits::window_start(NOW, limits::THIRTY_DAYS);
        let mut at = NOW;
        for display in ["Alice", "ALICE", "aLICE"] {
            at += 60;
            match fixture.renew("alice", display, 1, at) {
                RenewOutcome::Renewed { certificate, .. } => {
                    assert_eq!(certificate.serial, first.serial);
                    assert_eq!(certificate.display, display);
                }
                other => panic!("{display}: {other:?}"),
            }
        }
        let audited = rows(&fixture, "audit");
        // The fourth change in the window is refused and writes nothing.
        assert!(matches!(
            fixture.renew("alice", "AlIcE", 1, at + 60),
            RenewOutcome::Limited(Limit::Display)
        ));
        assert_eq!(rows(&fixture, "audit"), audited);
        // The casing it has is still renewable.
        assert!(matches!(
            fixture.renew("alice", "aLICE", 1, at + 120),
            RenewOutcome::Renewed { .. }
        ));
        // A new window, a new allowance — while the certificate is still not
        // due, so this is the cheap path again.
        let next = thirty + limits::THIRTY_DAYS;
        assert!(
            next - NOW < 60 * DAY,
            "the next window starts before the renewal is due"
        );
        assert!(matches!(
            fixture.renew("alice", "AlIcE", 1, next),
            RenewOutcome::Renewed { .. }
        ));
    }

    #[test]
    fn work_that_reaches_its_commit_late_writes_nothing() {
        let mut fixture = Fixture::new();
        let late = Deadline::starting(std::time::Instant::now(), std::time::Duration::ZERO);
        let identity = SigningKey::from_bytes(&[1; 32]);
        let verified = VerifiedClaim {
            name: Name::from_parts("alice", "Alice").expect("name"),
            uuid: PlayerUuid::from_root_key(&identity.verifying_key()),
            public_key: identity.verifying_key(),
            timestamp: NOW,
            lineage: KeyLineage::of_root(identity.verifying_key()),
        };
        let ip: IpAddr = "203.0.113.1".parse().expect("ip");
        let outcome = fixture
            .db
            .with(|connection| claim(connection, &fixture.keys, &verified, NOW, ip, &late))
            .expect("claim");
        assert!(matches!(outcome, ClaimOutcome::Late), "{outcome:?}");
        for table in ["names", "issued", "limits", "audit", "meta"] {
            assert_eq!(
                rows(&fixture, table),
                0,
                "a late claim left a row in {table}"
            );
        }
        // The retry the client makes is a first claim, not `already_named`.
        let retry = Fixture::certificate_of(fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW));
        assert_eq!(retry.serial, 1, "the late claim spent a serial");

        // A late renewal and a late release change nothing either.
        let renewed = fixture
            .db
            .with(|connection| renew(connection, &fixture.keys, &verified, NOW + 61 * DAY, &late))
            .expect("renew");
        assert!(matches!(renewed, RenewOutcome::Late));
        assert_eq!(rows(&fixture, "issued"), 1);
        let released = fixture
            .db
            .with(|connection| {
                release(
                    connection,
                    &VerifiedRelease {
                        name: "alice".to_owned(),
                        uuid: verified.uuid,
                        public_key: verified.public_key,
                        timestamp: NOW + 1,
                        lineage: verified.lineage.clone(),
                    },
                    NOW + 1,
                    &late,
                )
            })
            .expect("release");
        assert_eq!(released, ReleaseOutcome::Late);
        let page = fixture
            .db
            .with(|connection| status(connection, "alice", NOW + 2))
            .expect("status");
        assert!(page.uuid.is_some(), "a late release released the name");
    }
}
