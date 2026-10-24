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
//! pointing at nothing. The transaction is committed before the transport is
//! called: holding the database lock for an SMTP round trip would stall every
//! other request behind it.

use std::net::IpAddr;

use rusqlite::{Connection, OptionalExtension, params};
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::{VerifiedCancellation, VerifiedEmail, VerifiedRecovery};

use crate::limits::{self, DAY};
use crate::mail::{Mail, MailError, Mailer};
use crate::names::{self, HOLD_AFTER_RELEASE, binding};
use crate::tokens::{self, Purpose, TokenError};

/// How long the public notice runs: fourteen days.
pub const NOTICE_SECONDS: i64 = 14 * DAY;

/// The window mail limits use.
pub const HOUR: i64 = 60 * 60;

/// Mails to one address per hour.
pub const MAILS_PER_ADDRESS: i64 = 3;

/// Mails to one neighbourhood per hour.
pub const MAILS_PER_SUBNET: i64 = 10;

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
    /// The bound address was removed.
    Removed,
    /// The signing key is not the UUID's root key.
    NotRoot,
    /// The mail allowance for this address or network is spent.
    Limited,
    /// The mail could not be handed off.
    Mail(MailOutcome),
}

/// What starting a recovery did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// A link is on its way to the bound address.
    Sent,
    /// The signing key is not the new UUID's root key.
    NotRoot,
    /// No name by that canonical form is held.
    UnknownName,
    /// The name has no verified recovery address.
    NoEmail,
    /// The new identity already holds the name.
    AlreadyHeld,
    /// A notice is already running for this name.
    Pending,
    /// The mail allowance is spent.
    Limited,
    /// The mail could not be handed off.
    Mail(MailOutcome),
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
}

/// Binds, changes or removes a recovery address.
///
/// # Errors
///
/// [`FlowError`] when the database (or a link's randomness) fails. A rule that
/// refuses the request is an outcome.
pub fn bind_email(
    connection: &mut Connection,
    mailer: &dyn Mailer,
    base_url: &str,
    verified: &VerifiedEmail,
    now: i64,
    client: IpAddr,
) -> Result<EmailOutcome, FlowError> {
    if PlayerUuid::from_root_key(&verified.public_key) != verified.uuid {
        return Ok(EmailOutcome::NotRoot);
    }

    let Some(address) = verified.address.clone() else {
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM emails WHERE uuid = ?1",
            [verified.uuid.as_bytes().as_slice()],
        )?;
        names::audit(
            &transaction,
            &verified.uuid.to_hex(),
            "remove recovery address",
            now,
            &client.to_string(),
        )?;
        transaction.commit()?;
        return Ok(EmailOutcome::Removed);
    };

    let transaction = connection.transaction()?;
    if !take_mail_quota(&transaction, &address, client, now)? {
        // The transaction's drop is the rollback: no counter is spent by a
        // request that was refused.
        return Ok(EmailOutcome::Limited);
    }
    let (token, hash) = tokens::issue(
        &transaction,
        Purpose::Email,
        &verified.uuid,
        None,
        Some(&address),
        now,
    )?;
    names::audit(
        &transaction,
        &verified.uuid.to_hex(),
        "ask to verify a recovery address",
        now,
        &client.to_string(),
    )?;
    transaction.commit()?;

    let mail = Mail {
        to: address,
        subject: "Verify your Tiamat recovery address".to_owned(),
        body: format!(
            "Follow this link to bind this address to your Tiamat identity. It works once, \
             for a day:\n\n{base_url}/v1/email/verify?t={token}\n\nIf you did not ask for this, \
             ignore this message: nothing is stored until the link is followed.\n"
        ),
    };
    match send(mailer, &mail) {
        MailOutcome::Sent => Ok(EmailOutcome::Sent),
        other => {
            // A link whose mail never left would sit in the table until it
            // expired, able to bind an address nobody was told about.
            let _ = tokens::discard(connection, &hash);
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
) -> Result<Option<(PlayerUuid, String)>, FlowError> {
    let transaction = connection.transaction()?;
    let Some(record) = tokens::take(&transaction, token_hex, Purpose::Email, now)? else {
        return Ok(None);
    };
    let Some(address) = record.address else {
        return Ok(None);
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
    )?;
    transaction.commit()?;
    Ok(Some((uuid, address)))
}

/// Starts a recovery: mails the bound address a confirmation link.
///
/// # Errors
///
/// [`FlowError`] when the database fails.
pub fn start_recovery(
    connection: &mut Connection,
    mailer: &dyn Mailer,
    base_url: &str,
    verified: &VerifiedRecovery,
    now: i64,
    client: IpAddr,
) -> Result<StartOutcome, FlowError> {
    if PlayerUuid::from_root_key(&verified.public_key) != verified.new_uuid {
        return Ok(StartOutcome::NotRoot);
    }

    let transaction = connection.transaction()?;
    let Some(held) = binding(&transaction, &verified.name)? else {
        return Ok(StartOutcome::UnknownName);
    };
    if held.released.is_some() {
        return Ok(StartOutcome::UnknownName);
    }
    if held.uuid == *verified.new_uuid.as_bytes() {
        return Ok(StartOutcome::AlreadyHeld);
    }
    let Some(address) = email_for(&transaction, &held.uuid)? else {
        return Ok(StartOutcome::NoEmail);
    };
    if pending_notice(&transaction, &verified.name, now)?.is_some() {
        return Ok(StartOutcome::Pending);
    }
    if !take_mail_quota(&transaction, &address, client, now)? {
        return Ok(StartOutcome::Limited);
    }
    let (token, hash) = tokens::issue(
        &transaction,
        Purpose::Recovery,
        &verified.new_uuid,
        Some(&verified.name),
        None,
        now,
    )?;
    names::audit(
        &transaction,
        &verified.new_uuid.to_hex(),
        &format!("ask to recover {}", verified.name),
        now,
        &client.to_string(),
    )?;
    transaction.commit()?;

    let mail = Mail {
        to: address,
        subject: "A Tiamat name recovery was requested".to_owned(),
        body: format!(
            "Somebody has asked to recover the Tiamat name `{}` to a new identity. If that is \
             you, follow this link (it works once, for a day):\n\n\
             {base_url}/v1/recover/confirm?t={token}\n\n\
             Following the link starts a fourteen-day public notice, and the name's current \
             holder can cancel it in one click from their client.\n",
            verified.name
        ),
    };
    match send(mailer, &mail) {
        MailOutcome::Sent => Ok(StartOutcome::Sent),
        other => {
            let _ = tokens::discard(connection, &hash);
            Ok(StartOutcome::Mail(other))
        }
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
) -> Result<ConfirmOutcome, FlowError> {
    let transaction = connection.transaction()?;
    let Some(record) = tokens::take(&transaction, token_hex, Purpose::Recovery, now)? else {
        return Ok(ConfirmOutcome::UnknownToken);
    };
    let Some(canonical) = record.canonical else {
        return Ok(ConfirmOutcome::UnknownToken);
    };
    let Some(held) = binding(&transaction, &canonical)? else {
        return Ok(ConfirmOutcome::UnknownName);
    };
    if held.released.is_some() {
        return Ok(ConfirmOutcome::UnknownName);
    }
    if held.uuid == record.uuid {
        return Ok(ConfirmOutcome::AlreadyHeld);
    }
    if pending_notice(&transaction, &canonical, now)?.is_some() {
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
             state = 'pending'",
        params![
            canonical,
            held.uuid.as_slice(),
            record.uuid.as_slice(),
            now,
            notice_until
        ],
    )?;
    names::audit(
        &transaction,
        &PlayerUuid::from_bytes(record.uuid).to_hex(),
        &format!("recovery notice started for {canonical}"),
        now,
        "link followed",
    )?;
    transaction.commit()?;
    Ok(ConfirmOutcome::Started { notice_until })
}

/// Cancels a recovery, signed by the identity that holds the name.
///
/// # Errors
///
/// [`FlowError`] when the database fails.
pub fn cancel_recovery(
    connection: &mut Connection,
    verified: &VerifiedCancellation,
    now: i64,
) -> Result<CancelOutcome, FlowError> {
    let transaction = connection.transaction()?;
    let row = transaction
        .query_row(
            "SELECT from_uuid FROM recoveries WHERE canonical = ?1 AND state = 'pending'",
            [&verified.name],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    let Some(from) = row else {
        return Ok(CancelOutcome::NoRecovery);
    };
    if from.as_slice() != verified.uuid.as_bytes() {
        return Ok(CancelOutcome::NotYours);
    }
    transaction.execute(
        "UPDATE recoveries SET state = 'cancelled' WHERE canonical = ?1",
        [&verified.name],
    )?;
    names::audit(
        &transaction,
        &verified.uuid.to_hex(),
        &format!("recovery cancelled for {}", verified.name),
        now,
        "holder's request",
    )?;
    transaction.commit()?;
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
    let transaction = connection.transaction()?;
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
                "UPDATE recoveries SET state = 'cancelled' WHERE canonical = ?1",
                [&canonical],
            )?;
            continue;
        };
        if held.released.is_some() || held.uuid != from {
            // The name moved on (or was released) during the notice. The notice
            // is about a binding that no longer exists.
            transaction.execute(
                "UPDATE recoveries SET state = 'cancelled' WHERE canonical = ?1",
                [&canonical],
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

        // Every certificate the old identity was issued stops being honoured.
        let issued: Vec<(Vec<u8>, i64)> = {
            let mut statement =
                transaction.prepare("SELECT registry_key, serial FROM issued WHERE uuid = ?1")?;
            let rows =
                statement.query_map([from.as_slice()], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (registry_key, serial) in issued {
            transaction.execute(
                "INSERT OR IGNORE INTO revocations (registry_key, serial, when_at, reason)
                 VALUES (?1, ?2, ?3, 'recovery')",
                params![registry_key, serial, now],
            )?;
        }

        transaction.execute(
            "UPDATE recoveries SET state = 'moved' WHERE canonical = ?1",
            [&canonical],
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

/// Spends one mail from this address's and this network's hourly allowance.
fn take_mail_quota(
    connection: &Connection,
    address: &str,
    client: IpAddr,
    now: i64,
) -> rusqlite::Result<bool> {
    let window = limits::window_start(now, HOUR);
    if !limits::take(
        connection,
        &format!("mail:{}", address.to_ascii_lowercase()),
        window,
        MAILS_PER_ADDRESS,
    )? {
        return Ok(false);
    }
    if !limits::take(
        connection,
        &format!("mailnet:{}", limits::subnet_scope(client)),
        window,
        MAILS_PER_SUBNET,
    )? {
        return Ok(false);
    }
    Ok(true)
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

    const NOW: i64 = 1_800_000_000;

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
        };
        let client: IpAddr = "203.0.113.1".parse().expect("ip");
        let outcome = db
            .try_with(|connection| {
                bind_email(
                    connection,
                    &mailer,
                    "https://r.example",
                    &verified,
                    NOW,
                    client,
                )
            })
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
            .try_with(|connection| verify_email(connection, &token, NOW))
            .expect("verify")
            .expect("a good link");
        assert_eq!(bound.1, "alice@example.com");
        let reused = db
            .try_with(|connection| verify_email(connection, &token, NOW))
            .expect("verify");
        assert!(reused.is_none());
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

        let client: IpAddr = "203.0.113.1".parse().expect("ip");
        let recovery = VerifiedRecovery {
            name: "alice".to_owned(),
            new_uuid,
            public_key: new.verifying_key(),
            timestamp: NOW,
        };
        let outcome = db
            .try_with(|connection| {
                start_recovery(
                    connection,
                    &mailer,
                    "https://r.example",
                    &recovery,
                    NOW,
                    client,
                )
            })
            .expect("start");
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
            .try_with(|connection| confirm_recovery(connection, &token, NOW))
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
}
