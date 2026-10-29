// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The two mail flows: binding a recovery address, and recovering a lost name.
//!
//! # The address
//!
//! A bound address is not a login and grants nothing while it stands; it is the
//! door a recovery goes through. So the flow is: sign a request, receive a link
//! at the address, follow it, and only then is the address stored (§4). The
//! token is one-shot and dies in a day, and its hash is all the database keeps.
//!
//! # The recovery
//!
//! A player with no phrase and no device signs a request from their **new**
//! identity. The registry mails the bound address; following the link starts a
//! **fourteen-day public notice**, during which the name's page says so and
//! every renewal the holder makes is answered with the notice. If the holder is
//! still playing, one signed cancellation ends it — that is the defence against
//! a stolen mailbox. When the notice ends uncancelled, [`advance`] moves the
//! binding, the address with it, and revokes every certificate the old identity
//! was issued.
//!
//! # Mail is not a transaction
//!
//! A token is stored before its mail is sent and deleted if the send fails, so
//! a crash leaves either a link that works or no link at all, never a mail
//! pointing at nothing. The transaction is committed **and the database lock
//! released** before the transport is called: holding the one connection for
//! an SMTP round trip would stall every other request behind it. That is why
//! [`bind_email`] and [`start_recovery`] take the [`Db`] rather than a
//! connection — they own the lock discipline, and a caller cannot hand them a
//! lock to hold across the send.
//!
//! # A link is spent when it is followed
//!
//! Following a link takes its token whatever the answer: a refusal commits the
//! take too, so a link that was refused once cannot be tried again later when
//! the state it was refused for has changed.
//!
//! # Who may make the registry mail
//!
//! Every mail is counted against the asking identity, the (folded) address —
//! by the hour and by the day — the asker's network and the registry's hourly
//! ceiling ([`crate::mailquota`]), inside the transaction that issues its link.
//! Binding an address from an identity that holds no name also needs a proof
//! of work, carried in the request itself (`EmailRequest::work`), at the
//! difficulty a claim from the same network would pay.
//!
//! # The memory of retired keys
//!
//! Both flows that a signed request starts — the bind and the recovery start —
//! record what the request's key-set proof showed ([`crate::retired::record`])
//! inside the transaction that issues the link, because that link may be the
//! first row the registry keeps of the identity.

use std::net::IpAddr;

use rusqlite::{Connection, OptionalExtension, params};
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::{VerifiedCancellation, VerifiedEmail, VerifiedRecovery};

use crate::db::{Db, write_transaction};
use crate::deadline::{Deadline, commit};
use crate::limits::DAY;
use crate::mail::{Mail, MailError, Mailer};
use crate::mailquota::{self, MailLimit};
use crate::names::{self, HOLD_AFTER_RELEASE, binding};
use crate::tokens::{self, Purpose, TokenError, TokenRecord};

/// How long the public notice runs: fourteen days.
pub const NOTICE_SECONDS: i64 = 14 * DAY;

/// What the mail flows need from the service besides the database.
///
/// One value rather than four arguments, because every flow that mails takes
/// all of them.
#[derive(Clone, Copy)]
pub struct MailFlow<'a> {
    /// Where the mail goes out.
    pub mailer: &'a dyn Mailer,
    /// The public base URL the link is built on.
    pub base_url: &'a str,
    /// The registry's hourly mail ceiling (`REGISTRY_MAIL_PER_HOUR`).
    pub per_hour: i64,
    /// The asker's address, for the per-network allowance.
    pub client: IpAddr,
}

/// What went wrong in a flow.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// A statement failed.
    #[error(transparent)]
    Db(#[from] rusqlite::Error),

    /// A link could not be made or taken.
    #[error(transparent)]
    Tokens(#[from] TokenError),

    /// The database could not be reached.
    #[error("the registry database is unavailable: {0}")]
    Database(String),
}

impl From<crate::db::DbError> for FlowError {
    fn from(error: crate::db::DbError) -> Self {
        match error {
            crate::db::DbError::Sql(sql) => Self::Db(sql),
            other => Self::Database(other.to_string()),
        }
    }
}

/// Whether a mail was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailOutcome {
    /// It was handed to the transport.
    Sent,
    /// No transport is configured.
    NotConfigured,
    /// The transport failed.
    Failed,
}

/// What binding an address did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailOutcome {
    /// A link is on its way to the address.
    Sent,
    /// The bound address was removed (or there was none to remove).
    Removed,
    /// The signing key is not shown to be the UUID's own (a hand-built value;
    /// `EmailRequest::verify` never makes one).
    NotOwnKey,
    /// The identity holds no name and the request's proof of work is short.
    Work {
        /// The leading zero bits the request carried (zero with no work).
        found: u32,
        /// The bits it needed.
        required: u32,
    },
    /// A mail allowance is spent.
    Limited(MailLimit),
    /// The mail could not be handed off.
    Mail(MailOutcome),
    /// The request's deadline passed before the commit; nothing was written.
    Late,
}

/// What starting a recovery did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// A link is on its way to the bound address.
    Sent,
    /// The signing key is not shown to be the new UUID's own (a hand-built
    /// value; `RecoveryRequest::verify` never makes one).
    NotOwnKey,
    /// No name by that canonical form is held.
    UnknownName,
    /// The name has no verified recovery address.
    NoEmail,
    /// The new identity already holds the name.
    AlreadyHeld,
    /// A notice is already running for this name.
    Pending,
    /// A mail allowance is spent.
    Limited(MailLimit),
    /// The mail could not be handed off.
    Mail(MailOutcome),
    /// The request's deadline passed before the commit; nothing was written.
    Late,
}

/// What following an address-verification link did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The address is bound to the identity.
    Bound {
        /// The identity it belongs to.
        uuid: PlayerUuid,
        /// The address, as it was asked for.
        address: String,
    },
    /// The link is unknown, used or expired.
    Unknown,
    /// The request's deadline passed before the commit; the link is unspent.
    Late,
}

/// What confirming a recovery link did.
#[derive(Debug)]
pub enum ConfirmOutcome {
    /// The public notice has started; this is when it ends.
    Started {
        /// Unix seconds.
        notice_until: i64,
    },
    /// The token is unknown, used or expired.
    UnknownToken,
    /// The name is not held (any more).
    UnknownName,
    /// The name already belongs to the identity that asked.
    AlreadyHeld,
    /// A notice is already running.
    Pending,
    /// The request's deadline passed before the commit; the link is unspent.
    Late,
}

/// What cancelling a recovery did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The notice is over.
    Cancelled,
    /// No notice is running for this name.
    NoRecovery,
    /// The request is not from the current holder.
    NotYours,
    /// The request's deadline passed before the commit; nothing was written.
    Late,
}

/// Binds, changes or removes a recovery address.
///
/// A bind or change from an identity that **holds no name** must carry a proof
/// of work: `work_bits` is what the request's nonce gave
/// (`EmailRequest::work_bits`, `None` without one), checked against the
/// difficulty a claim from the same network would pay. The mail is sent after
/// the database lock is released (see the module documentation), and its
/// token discarded under a fresh lock if it fails.
///
/// A removal sends nothing and needs nothing, and writes (and audits) only
/// when there was an address to remove, so a loop of removals is a loop of
/// no-ops rather than of audit rows.
///
/// # Errors
///
/// [`FlowError`] when the database (or a link's randomness) fails. A rule that
/// refuses the request is an outcome.
pub fn bind_email(
    db: &Db,
    flow: MailFlow<'_>,
    verified: &VerifiedEmail,
    work_bits: Option<u32>,
    base_work_bits: u32,
    now: i64,
    deadline: &Deadline,
) -> Result<EmailOutcome, FlowError> {
    // Defence in depth: `EmailRequest::verify` already showed the key to be the
    // identity's own, and this keeps the rule true for a caller that built the
    // value — the only lineage one can build is the root's.
    if !shown_own(&verified.lineage, &verified.uuid, &verified.public_key) {
        return Ok(EmailOutcome::NotOwnKey);
    }

    let Some(address) = verified.address.clone() else {
        return db.try_with(|connection| -> Result<EmailOutcome, FlowError> {
            let transaction = write_transaction(connection)?;
            let removed = transaction.execute(
                "DELETE FROM emails WHERE uuid = ?1",
                [verified.uuid.as_bytes().as_slice()],
            )?;
            if removed == 0 {
                return Ok(EmailOutcome::Removed);
            }
            names::audit(
                &transaction,
                &verified.uuid.to_hex(),
                "remove recovery address",
                now,
                "player request",
                Some(flow.client),
            )?;
            if !commit(transaction, deadline)? {
                return Ok(EmailOutcome::Late);
            }
            Ok(EmailOutcome::Removed)
        });
    };

    let issued = db.try_with(|connection| -> Result<Result<_, EmailOutcome>, FlowError> {
        let transaction = write_transaction(connection)?;
        if !names::holds_name(&transaction, &verified.uuid, now)? {
            let required =
                mailquota::required_work_bits(&transaction, flow.client, now, base_work_bits)?;
            // No nonce counts as none of the work: refused at any difficulty
            // above zero, which is every difficulty a deployment asks for.
            let found = work_bits.unwrap_or(0);
            if found < required {
                return Ok(Err(EmailOutcome::Work { found, required }));
            }
        }
        if let Some(limit) = mailquota::take(
            &transaction,
            &address,
            &verified.uuid,
            flow.client,
            now,
            flow.per_hour,
        )? {
            // The transaction's drop is the rollback: no counter is spent by a
            // request that was refused.
            return Ok(Err(EmailOutcome::Limited(limit)));
        }
        let issued = tokens::issue(
            &transaction,
            Purpose::Email,
            &verified.uuid,
            None,
            Some(&address),
            now,
        )?;
        // The link is a row the registry keeps of this identity, so from here
        // its memory of retired keys holds what this request showed.
        crate::retired::record(
            &transaction,
            &verified.uuid,
            &verified.lineage,
            verified.timestamp,
            now,
            Some(flow.client),
        )?;
        names::audit(
            &transaction,
            &verified.uuid.to_hex(),
            "ask to verify a recovery address",
            now,
            "player request",
            Some(flow.client),
        )?;
        if !commit(transaction, deadline)? {
            return Ok(Err(EmailOutcome::Late));
        }
        Ok(Ok(issued))
    })?;
    let (token, hash) = match issued {
        Ok(issued) => issued,
        Err(refused) => return Ok(refused),
    };
    let base_url = flow.base_url;

    let mail = Mail {
        to: address,
        subject: "Verify your Tiamat recovery address".to_owned(),
        body: format!(
            "Open this link and press its button to bind this address to your Tiamat \
             identity. It works once, for a day:\n\n{base_url}/v1/email/verify?t={token}\n\n\
             If you did not ask for this, ignore this message: nothing is stored until the \
             button is pressed.\n"
        ),
    };
    match send(flow.mailer, &mail) {
        MailOutcome::Sent => Ok(EmailOutcome::Sent),
        other => {
            // A link whose mail never left would sit in the table until it
            // expired, able to bind an address nobody was told about.
            discard(db, &hash);
            Ok(EmailOutcome::Mail(other))
        }
    }
}

/// Takes a link and, if it is good, binds the address it carries.
///
/// # Errors
///
/// [`FlowError`] when the database fails.
pub fn verify_email(
    connection: &mut Connection,
    token_hex: &str,
    now: i64,
    deadline: &Deadline,
) -> Result<VerifyOutcome, FlowError> {
    let transaction = write_transaction(connection)?;
    let Some(record) = tokens::take(&transaction, token_hex, Purpose::Email, now)? else {
        return Ok(VerifyOutcome::Unknown);
    };
    let Some(address) = record.address else {
        // Spent even though it binds nothing: see the module documentation.
        if !commit(transaction, deadline)? {
            return Ok(VerifyOutcome::Late);
        }
        return Ok(VerifyOutcome::Unknown);
    };
    let uuid = PlayerUuid::from_bytes(record.uuid);
    transaction.execute(
        "INSERT INTO emails (uuid, address, verified_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(uuid) DO UPDATE SET address = excluded.address,
                                         verified_at = excluded.verified_at",
        params![uuid.as_bytes().as_slice(), address, now],
    )?;
    names::audit(
        &transaction,
        &uuid.to_hex(),
        "verify a recovery address",
        now,
        "link",
        None,
    )?;
    if !commit(transaction, deadline)? {
        return Ok(VerifyOutcome::Late);
    }
    Ok(VerifyOutcome::Bound { uuid, address })
}

/// What the database half of starting a recovery decided.
enum Started {
    /// A refusal, answered as it is.
    Refused(StartOutcome),
    /// A link to mail: the token, its hash, and the address.
    Mail {
        token: String,
        hash: [u8; 32],
        address: String,
    },
}

/// Starts a recovery: mails the bound address a confirmation link.
///
/// The mail counts against the **asking** identity's daily allowance and the
/// **holder's** address's hourly one, with the network and the registry's
/// ceiling beside them. The mail is sent after the database lock is released
/// (see the module documentation).
///
/// # Errors
///
/// [`FlowError`] when the database fails.
pub fn start_recovery(
    db: &Db,
    flow: MailFlow<'_>,
    verified: &VerifiedRecovery,
    now: i64,
    deadline: &Deadline,
) -> Result<StartOutcome, FlowError> {
    // Defence in depth, as in `bind_email`: `RecoveryRequest::verify` already
    // showed the key to be the new identity's own.
    if !shown_own(&verified.lineage, &verified.new_uuid, &verified.public_key) {
        return Ok(StartOutcome::NotOwnKey);
    }

    let started = db.try_with(|connection| -> Result<Started, FlowError> {
        let transaction = write_transaction(connection)?;
        let Some(held) = binding(&transaction, &verified.name)? else {
            return Ok(Started::Refused(StartOutcome::UnknownName));
        };
        if held.released.is_some() {
            return Ok(Started::Refused(StartOutcome::UnknownName));
        }
        if held.uuid == *verified.new_uuid.as_bytes() {
            return Ok(Started::Refused(StartOutcome::AlreadyHeld));
        }
        let Some(address) = email_for(&transaction, &held.uuid)? else {
            return Ok(Started::Refused(StartOutcome::NoEmail));
        };
        if pending_notice(&transaction, &verified.name, now)?.is_some() {
            return Ok(Started::Refused(StartOutcome::Pending));
        }
        if let Some(limit) = mailquota::take(
            &transaction,
            &address,
            &verified.new_uuid,
            flow.client,
            now,
            flow.per_hour,
        )? {
            return Ok(Started::Refused(StartOutcome::Limited(limit)));
        }
        let (token, hash) = tokens::issue(
            &transaction,
            Purpose::Recovery,
            &verified.new_uuid,
            Some(&verified.name),
            None,
            now,
        )?;
        crate::retired::record(
            &transaction,
            &verified.new_uuid,
            &verified.lineage,
            verified.timestamp,
            now,
            Some(flow.client),
        )?;
        names::audit(
            &transaction,
            &verified.new_uuid.to_hex(),
            &format!("ask to recover {}", verified.name),
            now,
            "player request",
            Some(flow.client),
        )?;
        if !commit(transaction, deadline)? {
            return Ok(Started::Refused(StartOutcome::Late));
        }
        Ok(Started::Mail {
            token,
            hash,
            address,
        })
    })?;
    let (token, hash, address) = match started {
        Started::Refused(outcome) => return Ok(outcome),
        Started::Mail {
            token,
            hash,
            address,
        } => (token, hash, address),
    };

    let base_url = flow.base_url;
    let mail = Mail {
        to: address,
        subject: "A Tiamat name recovery was requested".to_owned(),
        body: format!(
            "Somebody has asked to recover the Tiamat name `{}` to a new identity. If that is \
             you, open this link and press its button (it works once, for a day):\n\n\
             {base_url}/v1/recover/confirm?t={token}\n\n\
             Pressing the button starts a fourteen-day public notice, and the name's current \
             holder can cancel it in one click from their client. If you did not ask for \
             this, ignore this message: opening the link alone does nothing.\n",
            verified.name
        ),
    };
    match send(flow.mailer, &mail) {
        MailOutcome::Sent => Ok(StartOutcome::Sent),
        other => {
            discard(db, &hash);
            Ok(StartOutcome::Mail(other))
        }
    }
}

/// Whether a verified value's key is shown to be `uuid`'s: its lineage is
/// rooted at the UUID and ends at that key.
fn shown_own(
    lineage: &tiamat_core::identity::registry::KeyLineage,
    uuid: &PlayerUuid,
    key: &ed25519_dalek::VerifyingKey,
) -> bool {
    lineage.uuid() == *uuid && lineage.signing_key() == key
}

/// Deletes a link whose mail could not be sent, under a lock of its own.
///
/// Best effort: if even this fails, the token dies in a day and nobody was
/// mailed it, so the worst case is a row that sits until it expires.
fn discard(db: &Db, hash: &[u8; 32]) {
    if let Err(error) =
        db.try_with(|connection| tokens::discard(connection, hash).map_err(FlowError::from))
    {
        tracing::warn!(error = %error, "an unsent link could not be discarded");
    }
}

/// Follows a recovery link and starts the public notice.
///
/// # Errors
///
/// [`FlowError`] when the database fails.
pub fn confirm_recovery(
    connection: &mut Connection,
    token_hex: &str,
    now: i64,
    deadline: &Deadline,
) -> Result<ConfirmOutcome, FlowError> {
    let transaction = write_transaction(connection)?;
    let Some(record) = tokens::take(&transaction, token_hex, Purpose::Recovery, now)? else {
        return Ok(ConfirmOutcome::UnknownToken);
    };
    // The take is committed on every path from here, refusals included: see
    // the module documentation. Past the deadline nothing is, so a link that
    // was answered "timed out" still works.
    let outcome = confirm_taken(&transaction, record, now)?;
    if !commit(transaction, deadline)? {
        return Ok(ConfirmOutcome::Late);
    }
    Ok(outcome)
}

/// The checks and the write of a confirmation whose token is already taken.
fn confirm_taken(
    transaction: &Connection,
    record: TokenRecord,
    now: i64,
) -> Result<ConfirmOutcome, FlowError> {
    let Some(canonical) = record.canonical else {
        return Ok(ConfirmOutcome::UnknownToken);
    };
    let Some(held) = binding(transaction, &canonical)? else {
        return Ok(ConfirmOutcome::UnknownName);
    };
    if held.released.is_some() {
        return Ok(ConfirmOutcome::UnknownName);
    }
    if held.uuid == record.uuid {
        return Ok(ConfirmOutcome::AlreadyHeld);
    }
    if pending_notice(transaction, &canonical, now)?.is_some() {
        return Ok(ConfirmOutcome::Pending);
    }

    let notice_until = now + NOTICE_SECONDS;
    transaction.execute(
        "INSERT INTO recoveries
             (canonical, from_uuid, to_uuid, confirmed_at, notice_until, state)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending')
         ON CONFLICT(canonical) DO UPDATE SET
             from_uuid = excluded.from_uuid,
             to_uuid = excluded.to_uuid,
             confirmed_at = excluded.confirmed_at,
             notice_until = excluded.notice_until,
             state = 'pending',
             ended_at = NULL",
        params![
            canonical,
            held.uuid.as_slice(),
            record.uuid.as_slice(),
            now,
            notice_until
        ],
    )?;
    names::audit(
        transaction,
        &PlayerUuid::from_bytes(record.uuid).to_hex(),
        &format!("recovery notice started for {canonical}"),
        now,
        "link followed",
        None,
    )?;
    Ok(ConfirmOutcome::Started { notice_until })
}

/// Cancels a recovery, signed by the identity that holds the name.
///
/// Only a notice still **running** can be cancelled: once `notice_until` has
/// passed the move is due, and a cancel that arrives in the gap before
/// [`advance`] runs must not undo fourteen days of silence.
///
/// # Errors
///
/// [`FlowError`] when the database fails.
pub fn cancel_recovery(
    connection: &mut Connection,
    verified: &VerifiedCancellation,
    now: i64,
    deadline: &Deadline,
) -> Result<CancelOutcome, FlowError> {
    let transaction = write_transaction(connection)?;
    let row = transaction
        .query_row(
            "SELECT from_uuid FROM recoveries
             WHERE canonical = ?1 AND state = 'pending' AND notice_until > ?2",
            params![&verified.name, now],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    let Some(from) = row else {
        return Ok(CancelOutcome::NoRecovery);
    };
    // Defence in depth: `CancelRecoveryRequest::verify` already showed the key
    // to be this UUID's; this shows the UUID to be the holder the notice is
    // about.
    if from.as_slice() != verified.uuid.as_bytes() {
        return Ok(CancelOutcome::NotYours);
    }
    transaction.execute(
        "UPDATE recoveries SET state = 'cancelled', ended_at = ?2 WHERE canonical = ?1",
        params![&verified.name, now],
    )?;
    names::audit(
        &transaction,
        &verified.uuid.to_hex(),
        &format!("recovery cancelled for {}", verified.name),
        now,
        "holder's request",
        None,
    )?;
    if !commit(transaction, deadline)? {
        return Ok(CancelOutcome::Late);
    }
    Ok(CancelOutcome::Cancelled)
}

/// The end of a notice, if one is running for this name.
///
/// # Errors
///
/// Any SQL failure.
pub fn pending_notice(
    connection: &Connection,
    canonical: &str,
    now: i64,
) -> rusqlite::Result<Option<i64>> {
    connection
        .query_row(
            "SELECT notice_until FROM recoveries
             WHERE canonical = ?1 AND state = 'pending' AND notice_until > ?2",
            params![canonical, now],
            |row| row.get(0),
        )
        .optional()
}

/// Completes every notice whose fourteen days are up.
///
/// Called at startup and before the handlers that read a name, so a notice ends
/// even if nobody visits the site at the right minute.
///
/// # Errors
///
/// Any SQL failure.
pub fn advance(connection: &mut Connection, now: i64) -> rusqlite::Result<usize> {
    let transaction = write_transaction(connection)?;
    let due: Vec<(String, Vec<u8>, Vec<u8>)> = {
        let mut statement = transaction.prepare(
            "SELECT canonical, from_uuid, to_uuid FROM recoveries
             WHERE state = 'pending' AND notice_until <= ?1",
        )?;
        let rows = statement.query_map([now], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut moved = 0;
    for (canonical, from, to) in due {
        let (Ok(from), Ok(to)) = (
            <[u8; 32]>::try_from(from.as_slice()),
            <[u8; 32]>::try_from(to.as_slice()),
        ) else {
            // Written by this service, always 32 bytes. Refusing rather than
            // guessing is the same rule every parser here follows.
            continue;
        };
        let Some(held) = binding(&transaction, &canonical)? else {
            transaction.execute(
                "UPDATE recoveries SET state = 'cancelled', ended_at = ?2 WHERE canonical = ?1",
                params![&canonical, now],
            )?;
            continue;
        };
        if held.released.is_some() || held.uuid != from {
            // The name moved on (or was released) during the notice. The notice
            // is about a binding that no longer exists.
            transaction.execute(
                "UPDATE recoveries SET state = 'cancelled', ended_at = ?2 WHERE canonical = ?1",
                params![&canonical, now],
            )?;
            continue;
        }

        // One name per identity: anything the new identity holds gives way,
        // with the ordinary thirty-day hold, and is not a rename.
        transaction.execute(
            "UPDATE names SET released = ?1, hold_until = ?2
             WHERE uuid = ?3 AND released IS NULL",
            params![now, now + HOLD_AFTER_RELEASE, to.as_slice()],
        )?;
        transaction.execute(
            "UPDATE names SET uuid = ?1, claimed = ?2, renewed = ?2,
                              released = NULL, hold_until = NULL, renewals = 0
             WHERE canonical = ?3",
            params![to.as_slice(), now, &canonical],
        )?;

        // The recovery address follows the name; there is one per identity.
        transaction.execute("DELETE FROM emails WHERE uuid = ?1", [to.as_slice()])?;
        transaction.execute(
            "UPDATE emails SET uuid = ?1 WHERE uuid = ?2",
            params![to.as_slice(), from.as_slice()],
        )?;

        // Every certificate the old identity holds that could still verify
        // stops being honoured. An expired one needs no revocation, and
        // listing it would crowd the capped list.
        for (registry_key, serial) in names::live_issues(&transaction, from.as_slice(), now)? {
            transaction.execute(
                "INSERT OR IGNORE INTO revocations (registry_key, serial, when_at, reason)
                 VALUES (?1, ?2, ?3, 'recovery')",
                params![registry_key, serial, now],
            )?;
        }

        transaction.execute(
            "UPDATE recoveries SET state = 'moved', ended_at = ?2 WHERE canonical = ?1",
            params![&canonical, now],
        )?;
        names::audit(
            &transaction,
            &PlayerUuid::from_bytes(from).to_hex(),
            &format!(
                "recovery moved {canonical} to {}",
                PlayerUuid::from_bytes(to).to_hex()
            ),
            now,
            "notice ended",
            None,
        )?;
        moved += 1;
    }
    transaction.commit()?;
    Ok(moved)
}

/// The verified recovery address of an identity.
fn email_for(connection: &Connection, uuid: &[u8; 32]) -> rusqlite::Result<Option<String>> {
    connection
        .query_row(
            "SELECT address FROM emails WHERE uuid = ?1",
            [uuid.as_slice()],
            |row| row.get(0),
        )
        .optional()
}

/// Hands a mail to the transport, collapsing its failure into an outcome.
fn send(mailer: &dyn Mailer, mail: &Mail) -> MailOutcome {
    match mailer.send(mail) {
        Ok(()) => MailOutcome::Sent,
        Err(MailError::NotConfigured) => MailOutcome::NotConfigured,
        Err(MailError::Transport(_)) => MailOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::mail::SinkMailer;
    use tiamat_core::identity::registry::KeyLineage;

    const NOW: i64 = 1_800_000_000;

    /// The flow settings every test uses: a quiet network, a generous ceiling.
    fn flow(mailer: &dyn Mailer) -> MailFlow<'_> {
        MailFlow {
            mailer,
            base_url: "https://r.example",
            per_hour: 60,
            client: "203.0.113.1".parse().expect("ip"),
        }
    }

    #[test]
    fn a_recovery_address_is_bound_only_by_following_the_link() {
        let db = Db::in_memory().expect("db");
        let mailer = SinkMailer::default();
        // The verified email must be signed by a root key, so the fixture's
        // public key has to be built from the same seed as the UUID.
        let identity = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let uuid = PlayerUuid::from_root_key(&identity.verifying_key());
        let verified = VerifiedEmail {
            uuid,
            address: Some("alice@example.com".to_owned()),
            public_key: identity.verifying_key(),
            timestamp: NOW,
            lineage: KeyLineage::of_root(identity.verifying_key()),
        };
        let outcome = bind_email(
            &db,
            flow(&mailer),
            &verified,
            None,
            0,
            NOW,
            &Deadline::none(),
        )
        .expect("bind");
        assert_eq!(outcome, EmailOutcome::Sent);
        let mails = mailer.take();
        assert_eq!(mails.len(), 1);
        assert!(mails[0].body.contains("/v1/email/verify?t="));

        // Nothing is stored yet.
        let bound: i64 = db
            .with(|connection| {
                connection.query_row("SELECT COUNT(*) FROM emails", [], |row| row.get(0))
            })
            .expect("count");
        assert_eq!(bound, 0);

        // The link binds it, once.
        let token = mails[0]
            .body
            .split("t=")
            .nth(1)
            .expect("token in the link")
            // The link is followed by a blank line and prose; the token is the
            // first whitespace-delimited run after `t=`.
            .split_whitespace()
            .next()
            .expect("a token")
            .to_owned();
        let bound = db
            .try_with(|connection| verify_email(connection, &token, NOW, &Deadline::none()))
            .expect("verify");
        assert!(
            matches!(&bound, VerifyOutcome::Bound { address, .. } if address == "alice@example.com"),
            "{bound:?}"
        );
        let reused = db
            .try_with(|connection| verify_email(connection, &token, NOW, &Deadline::none()))
            .expect("verify");
        assert_eq!(reused, VerifyOutcome::Unknown);
    }

    #[test]
    fn a_recovery_moves_the_name_revokes_and_ends_at_the_notice() {
        let db = Db::in_memory().expect("db");
        let mailer = SinkMailer::default();
        let registry_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .verifying_key()
            .to_bytes();
        let old = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let new = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let old_uuid = PlayerUuid::from_root_key(&old.verifying_key());
        let new_uuid = PlayerUuid::from_root_key(&new.verifying_key());

        // A held name with a verified address and an issued certificate.
        db.with(|connection| {
            connection.execute(
                "INSERT INTO names (canonical, display, uuid, claimed, renewed, renewals)
                 VALUES ('alice', 'Alice', ?1, ?2, ?2, 0)",
                params![old_uuid.as_bytes().as_slice(), NOW - 1000],
            )?;
            connection.execute(
                "INSERT INTO emails (uuid, address, verified_at) VALUES (?1, 'alice@example.com', ?2)",
                params![old_uuid.as_bytes().as_slice(), NOW],
            )?;
            connection.execute(
                "INSERT INTO issued (serial, registry_key, uuid, canonical, issued)
                 VALUES (5, ?1, ?2, 'alice', ?3)",
                params![
                    registry_key.as_slice(),
                    old_uuid.as_bytes().as_slice(),
                    NOW - 1000
                ],
            )?;
            Ok(())
        })
        .expect("seed");

        let recovery = VerifiedRecovery {
            name: "alice".to_owned(),
            new_uuid,
            public_key: new.verifying_key(),
            timestamp: NOW,
            lineage: KeyLineage::of_root(new.verifying_key()),
        };
        let outcome =
            start_recovery(&db, flow(&mailer), &recovery, NOW, &Deadline::none()).expect("start");
        assert_eq!(outcome, StartOutcome::Sent);
        let token = mailer.take()[0]
            .body
            .split("t=")
            .nth(1)
            .expect("token")
            .split_whitespace()
            .next()
            .expect("a token")
            .to_owned();

        let confirm = db
            .try_with(|connection| confirm_recovery(connection, &token, NOW, &Deadline::none()))
            .expect("confirm");
        let ConfirmOutcome::Started { notice_until } = confirm else {
            panic!("expected a notice, got {confirm:?}");
        };
        assert_eq!(notice_until, NOW + NOTICE_SECONDS);
        // The holder is told by their renewal.
        let notice = db
            .with(|connection| pending_notice(connection, "alice", NOW + 1))
            .expect("notice");
        assert_eq!(notice, Some(notice_until));

        // Nothing has moved yet.
        let before = db
            .with(|connection| binding(connection, "alice"))
            .expect("binding")
            .expect("held");
        assert_eq!(before.uuid, *old_uuid.as_bytes());

        // When the notice ends, the name, the address and the revocation move.
        let moved = db
            .with(|connection| advance(connection, notice_until))
            .expect("advance");
        assert_eq!(moved, 1);
        let after = db
            .with(|connection| binding(connection, "alice"))
            .expect("binding")
            .expect("held");
        assert_eq!(after.uuid, *new_uuid.as_bytes());
        let address: Option<String> = db
            .with(|connection| Ok(email_for(connection, new_uuid.as_bytes()).expect("email")))
            .expect("email");
        assert_eq!(address.as_deref(), Some("alice@example.com"));
        let revoked: i64 = db
            .with(|connection| {
                connection.query_row("SELECT COUNT(*) FROM revocations", [], |row| row.get(0))
            })
            .expect("revocations");
        assert_eq!(revoked, 1);
    }

    /// A transport that records whether the database was held while it ran.
    struct LockProbe {
        db: std::sync::Arc<Db>,
        held: std::sync::Mutex<Vec<bool>>,
    }

    impl Mailer for LockProbe {
        fn send(&self, _mail: &Mail) -> Result<(), MailError> {
            if let Ok(mut held) = self.held.lock() {
                held.push(self.db.is_locked());
            }
            Ok(())
        }
    }

    /// Alice holds `alice` with a verified address; returns her UUID.
    fn seed_holder(db: &Db) -> PlayerUuid {
        let old = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let uuid = PlayerUuid::from_root_key(&old.verifying_key());
        db.with(|connection| {
            connection.execute(
                "INSERT INTO names (canonical, display, uuid, claimed, renewed, renewals)
                 VALUES ('alice', 'Alice', ?1, ?2, ?2, 0)",
                params![uuid.as_bytes().as_slice(), NOW - 1000],
            )?;
            connection.execute(
                "INSERT INTO emails (uuid, address, verified_at) VALUES (?1, 'alice@example.com', ?2)",
                params![uuid.as_bytes().as_slice(), NOW],
            )?;
            Ok(())
        })
        .expect("seed");
        uuid
    }

    fn recovery_by(seed: u8) -> VerifiedRecovery {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        VerifiedRecovery {
            name: "alice".to_owned(),
            new_uuid: PlayerUuid::from_root_key(&key.verifying_key()),
            public_key: key.verifying_key(),
            timestamp: NOW,
            lineage: KeyLineage::of_root(key.verifying_key()),
        }
    }

    #[test]
    fn mail_is_sent_with_the_database_unlocked() {
        let db = std::sync::Arc::new(Db::in_memory().expect("db"));
        let probe = LockProbe {
            db: std::sync::Arc::clone(&db),
            held: std::sync::Mutex::new(Vec::new()),
        };
        let holder = seed_holder(&db);

        let identity = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let email = VerifiedEmail {
            uuid: holder,
            address: Some("alice@example.com".to_owned()),
            public_key: identity.verifying_key(),
            timestamp: NOW,
            lineage: KeyLineage::of_root(identity.verifying_key()),
        };
        assert_eq!(
            bind_email(&db, flow(&probe), &email, None, 0, NOW, &Deadline::none()).expect("bind"),
            EmailOutcome::Sent
        );
        assert_eq!(
            start_recovery(&db, flow(&probe), &recovery_by(2), NOW, &Deadline::none())
                .expect("start"),
            StartOutcome::Sent
        );
        let held = probe.held.lock().expect("probe").clone();
        assert_eq!(
            held,
            vec![false, false],
            "a mail was sent holding the database"
        );
    }

    #[test]
    fn a_refused_recovery_link_is_spent_all_the_same() {
        let db = Db::in_memory().expect("db");
        let mailer = SinkMailer::default();
        seed_holder(&db);
        assert_eq!(
            start_recovery(&db, flow(&mailer), &recovery_by(2), NOW, &Deadline::none())
                .expect("start"),
            StartOutcome::Sent
        );
        let token = mailer.take()[0]
            .body
            .split("t=")
            .nth(1)
            .expect("token")
            .split_whitespace()
            .next()
            .expect("a token")
            .to_owned();

        // The holder releases the name before the link is followed: refused.
        db.with(|connection| {
            connection.execute(
                "UPDATE names SET released = ?1, hold_until = ?2 WHERE canonical = 'alice'",
                params![NOW + 1, NOW + 1 + HOLD_AFTER_RELEASE],
            )?;
            Ok(())
        })
        .expect("release");
        let first = db
            .try_with(|connection| confirm_recovery(connection, &token, NOW + 2, &Deadline::none()))
            .expect("confirm");
        assert!(matches!(first, ConfirmOutcome::UnknownName), "{first:?}");

        // The refusal spent the link: it is gone, not waiting for the name to
        // come back.
        db.with(|connection| {
            connection.execute(
                "UPDATE names SET released = NULL, hold_until = NULL WHERE canonical = 'alice'",
                [],
            )?;
            Ok(())
        })
        .expect("reclaim");
        let second = db
            .try_with(|connection| confirm_recovery(connection, &token, NOW + 3, &Deadline::none()))
            .expect("confirm");
        assert!(matches!(second, ConfirmOutcome::UnknownToken), "{second:?}");
    }

    #[test]
    fn a_notice_that_has_ended_cannot_be_cancelled() {
        let db = Db::in_memory().expect("db");
        let holder = seed_holder(&db);
        let newcomer = recovery_by(2).new_uuid;
        db.with(|connection| {
            connection.execute(
                "INSERT INTO recoveries
                     (canonical, from_uuid, to_uuid, confirmed_at, notice_until, state)
                 VALUES ('alice', ?1, ?2, ?3, ?4, 'pending')",
                params![
                    holder.as_bytes().as_slice(),
                    newcomer.as_bytes().as_slice(),
                    NOW - NOTICE_SECONDS,
                    NOW
                ],
            )?;
            Ok(())
        })
        .expect("notice");
        let key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let cancel = VerifiedCancellation {
            name: "alice".to_owned(),
            uuid: holder,
            public_key: key.verifying_key(),
            timestamp: NOW,
            lineage: KeyLineage::of_root(key.verifying_key()),
        };
        // One second before the end it is still the holder's to stop.
        let running = db
            .try_with(|connection| cancel_recovery(connection, &cancel, NOW - 1, &Deadline::none()))
            .expect("cancel");
        assert_eq!(running, CancelOutcome::Cancelled);

        db.with(|connection| {
            connection.execute("UPDATE recoveries SET state = 'pending'", [])?;
            Ok(())
        })
        .expect("reset");
        let ended = db
            .try_with(|connection| cancel_recovery(connection, &cancel, NOW, &Deadline::none()))
            .expect("cancel");
        assert_eq!(ended, CancelOutcome::NoRecovery);
    }

    #[test]
    fn a_recovery_revokes_only_certificates_that_can_still_verify() {
        let db = Db::in_memory().expect("db");
        let holder = seed_holder(&db);
        let newcomer = recovery_by(2).new_uuid;
        let key = [9u8; 32];
        db.with(|connection| {
            for (serial, issued) in [(1_i64, NOW - 300 * DAY), (2, NOW - 10 * DAY)] {
                connection.execute(
                    "INSERT INTO issued (serial, registry_key, uuid, canonical, issued)
                     VALUES (?1, ?2, ?3, 'alice', ?4)",
                    params![serial, key.as_slice(), holder.as_bytes().as_slice(), issued],
                )?;
            }
            connection.execute(
                "INSERT INTO recoveries
                     (canonical, from_uuid, to_uuid, confirmed_at, notice_until, state)
                 VALUES ('alice', ?1, ?2, ?3, ?4, 'pending')",
                params![
                    holder.as_bytes().as_slice(),
                    newcomer.as_bytes().as_slice(),
                    NOW - NOTICE_SECONDS,
                    NOW
                ],
            )?;
            Ok(())
        })
        .expect("seed");
        assert_eq!(
            db.with(|connection| advance(connection, NOW))
                .expect("advance"),
            1
        );
        let revoked: Vec<i64> = db
            .with(|connection| {
                let mut statement = connection.prepare("SELECT serial FROM revocations")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            })
            .expect("revocations");
        assert_eq!(revoked, vec![2]);
    }

    #[test]
    fn removing_an_address_that_is_not_there_writes_nothing() {
        let db = Db::in_memory().expect("db");
        let mailer = SinkMailer::default();
        let identity = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let removal = VerifiedEmail {
            uuid: PlayerUuid::from_root_key(&identity.verifying_key()),
            address: None,
            public_key: identity.verifying_key(),
            timestamp: NOW,
            lineage: KeyLineage::of_root(identity.verifying_key()),
        };
        // A loop of removals is a loop of no-ops, not of audit rows.
        for _ in 0..50 {
            assert_eq!(
                bind_email(
                    &db,
                    flow(&mailer),
                    &removal,
                    None,
                    0,
                    NOW,
                    &Deadline::none()
                )
                .expect("remove"),
                EmailOutcome::Removed
            );
        }
        let audited: i64 = db
            .with(|connection| {
                connection.query_row("SELECT COUNT(*) FROM audit", [], |row| row.get(0))
            })
            .expect("count");
        assert_eq!(audited, 0);
    }

    #[test]
    fn a_recovery_counts_against_its_asker_and_the_holder_s_address() {
        let db = Db::in_memory().expect("db");
        let mailer = SinkMailer::default();
        seed_holder(&db);
        // Three strangers, three mails: the holder's address is spent for the
        // hour, whoever asks next.
        for seed in 2..5 {
            assert_eq!(
                start_recovery(
                    &db,
                    flow(&mailer),
                    &recovery_by(seed),
                    NOW,
                    &Deadline::none()
                )
                .expect("start"),
                StartOutcome::Sent
            );
        }
        assert_eq!(
            start_recovery(&db, flow(&mailer), &recovery_by(9), NOW, &Deadline::none())
                .expect("start"),
            StartOutcome::Limited(MailLimit::Address)
        );
        // An hour on the address may be mailed again, but a stranger who has
        // asked three times today may not ask again.
        let later = NOW + crate::limits::HOUR;
        for _ in 0..2 {
            assert_eq!(
                start_recovery(
                    &db,
                    flow(&mailer),
                    &recovery_by(2),
                    later,
                    &Deadline::none()
                )
                .expect("start"),
                StartOutcome::Sent
            );
        }
        assert_eq!(
            start_recovery(
                &db,
                flow(&mailer),
                &recovery_by(2),
                later,
                &Deadline::none()
            )
            .expect("start"),
            StartOutcome::Limited(MailLimit::Identity)
        );
    }

    #[test]
    fn a_late_confirmation_leaves_its_link_unspent() {
        let db = Db::in_memory().expect("db");
        let mailer = SinkMailer::default();
        seed_holder(&db);
        assert_eq!(
            start_recovery(&db, flow(&mailer), &recovery_by(2), NOW, &Deadline::none())
                .expect("start"),
            StartOutcome::Sent
        );
        let token = mailer.take()[0]
            .body
            .split("t=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .expect("a token")
            .to_owned();
        let late = Deadline::starting(std::time::Instant::now(), std::time::Duration::ZERO);
        let first = db
            .try_with(|connection| confirm_recovery(connection, &token, NOW, &late))
            .expect("confirm");
        assert!(matches!(first, ConfirmOutcome::Late), "{first:?}");
        // The client was told "timed out, try again", and trying again works.
        let second = db
            .try_with(|connection| confirm_recovery(connection, &token, NOW, &Deadline::none()))
            .expect("confirm");
        assert!(
            matches!(second, ConfirmOutcome::Started { .. }),
            "{second:?}"
        );
    }

    #[test]
    fn a_finished_recovery_records_when_it_ended() {
        let db = Db::in_memory().expect("db");
        let holder = seed_holder(&db);
        let newcomer = recovery_by(2).new_uuid;
        db.with(|connection| {
            connection.execute(
                "INSERT INTO recoveries
                     (canonical, from_uuid, to_uuid, confirmed_at, notice_until, state)
                 VALUES ('alice', ?1, ?2, ?3, ?4, 'pending')",
                params![
                    holder.as_bytes().as_slice(),
                    newcomer.as_bytes().as_slice(),
                    NOW - NOTICE_SECONDS,
                    NOW
                ],
            )?;
            Ok(())
        })
        .expect("notice");
        db.with(|connection| advance(connection, NOW + 5))
            .expect("advance");
        let ended: Option<i64> = db
            .with(|connection| {
                connection.query_row("SELECT ended_at FROM recoveries", [], |row| row.get(0))
            })
            .expect("ended");
        assert_eq!(ended, Some(NOW + 5));
    }
}
