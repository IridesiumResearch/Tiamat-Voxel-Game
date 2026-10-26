// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Who may make the registry send a mail, and how often.
//!
//! The two mails this service sends are fixed text with one link, so a stranger
//! who can make it send them cannot phish anyone — but every mail spends the
//! sending domain's reputation, and a registry that can be made to mail any
//! address on demand is a registry whose mail ends up in spam (review B13).
//! So every mail is counted four ways, inside the same transaction that issues
//! its link, and the first count that is spent refuses it:
//!
//! | scope | window | allowance |
//! |---|---|---|
//! | the identity that asked (`mailid:`) | a day | [`MAILS_PER_IDENTITY`] |
//! | the address mailed (`mail:`) | an hour | [`MAILS_PER_ADDRESS`] |
//! | the asker's `/24` or `/48` (`mailnet:`) | an hour | [`MAILS_PER_SUBNET`] |
//! | the whole registry (`mailall:`) | an hour | `REGISTRY_MAIL_PER_HOUR` ([`DEFAULT_MAILS_PER_HOUR`]) |
//!
//! # The address is folded, then hashed
//!
//! The per-address key is the address with its local part's `+tag` removed and
//! the whole of it lower-cased ([`fold_address`]), so `alice+1@example.com` and
//! `ALICE+2@example.com` share Alice's allowance instead of each having its own.
//! It is then hashed with a domain prefix ([`address_scope`]): the limits table
//! is a rate counter, and §8 of the design keeps an address only while it is
//! bound — a raw address in a counter would be one more copy of it, kept for a
//! week after its hour.

use std::net::IpAddr;

use rusqlite::Connection;
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::MAX_WORK_BITS;

use crate::limits::{self, DAY, HOUR};

/// Mails to one (folded) address per hour.
pub const MAILS_PER_ADDRESS: i64 = 3;

/// Mails asked for from one neighbourhood per hour.
pub const MAILS_PER_SUBNET: i64 = 10;

/// Mails one identity may cause per day, across both flows.
///
/// Three: binding an address, a typo and its correction. A recovery is asked
/// for by a fresh identity, so this cap is the one that bounds a single key,
/// and the address and global caps are the ones that bound a crowd of them.
pub const MAILS_PER_IDENTITY: i64 = 3;

/// The registry's own hourly ceiling, when `REGISTRY_MAIL_PER_HOUR` is unset.
///
/// Sixty is far above what players need (a handful of addresses an hour on a
/// busy day) and far below what would get a transactional sender's domain
/// flagged; it is the cap that holds when every narrower one is being dodged by
/// a crowd of identities and networks.
pub const DEFAULT_MAILS_PER_HOUR: i64 = 60;

/// The domain prefix of the per-address key's hash.
const ADDRESS_DOMAIN: &[u8] = b"tiamat:registry-mail-quota:v1";

/// The global counter's scope: one row per hour.
pub const GLOBAL_SCOPE: &str = "mailall:";

/// Which allowance refused a mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailLimit {
    /// The identity asked for its three mails today.
    Identity,
    /// The address was mailed three times this hour.
    Address,
    /// The neighbourhood asked for ten mails this hour.
    Subnet,
    /// The registry sent its hourly ceiling.
    Global,
}

impl MailLimit {
    /// The sentence a player is shown.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Identity => "this identity has asked for three mails today; try again tomorrow",
            Self::Address => "that address has been mailed three times this hour; try again later",
            Self::Subnet => "this network has asked for ten mails this hour; try again later",
            Self::Global => {
                "the registry has sent all the mail it sends in an hour; try again later"
            }
        }
    }
}

/// An address as its quota sees it: lower-cased, with any `+tag` removed from
/// the local part.
///
/// Only the quota sees this form. The mail still goes to the address as given,
/// because a provider that does not treat `+` as a tag would deliver the folded
/// address to somebody else.
#[must_use]
pub fn fold_address(address: &str) -> String {
    let lowered = address.to_ascii_lowercase();
    match lowered.rsplit_once('@') {
        Some((local, domain)) => {
            let local = local.split_once('+').map_or(local, |(base, _)| base);
            format!("{local}@{domain}")
        }
        None => lowered,
    }
}

/// The per-address scope: a domain-separated BLAKE3 of the folded address.
#[must_use]
pub fn address_scope(address: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ADDRESS_DOMAIN);
    hasher.update(fold_address(address).as_bytes());
    format!("mail:{}", crate::hex::encode(hasher.finalize().as_bytes()))
}

/// The per-identity scope.
#[must_use]
pub fn identity_scope(uuid: &PlayerUuid) -> String {
    format!("mailid:{}", uuid.to_hex())
}

/// The per-neighbourhood scope.
#[must_use]
pub fn subnet_scope(client: IpAddr) -> String {
    format!("mailnet:{}", limits::subnet_scope(client))
}

/// Spends one mail from every allowance it counts against, or names the first
/// that is spent.
///
/// `Ok(None)` when the mail may go. **Call it inside the transaction that
/// issues the link**: a refusal returns early, the transaction's drop rolls
/// back whatever this took before the refusal, and a refused request spends
/// nothing.
///
/// # Errors
///
/// Any SQL failure.
pub fn take(
    connection: &Connection,
    address: &str,
    asker: &PlayerUuid,
    client: IpAddr,
    now: i64,
    per_hour: i64,
) -> rusqlite::Result<Option<MailLimit>> {
    let day = limits::window_start(now, DAY);
    let hour = limits::window_start(now, HOUR);
    let checks = [
        (
            identity_scope(asker),
            day,
            MAILS_PER_IDENTITY,
            MailLimit::Identity,
        ),
        (
            address_scope(address),
            hour,
            MAILS_PER_ADDRESS,
            MailLimit::Address,
        ),
        (
            subnet_scope(client),
            hour,
            MAILS_PER_SUBNET,
            MailLimit::Subnet,
        ),
        (GLOBAL_SCOPE.to_owned(), hour, per_hour, MailLimit::Global),
    ];
    for (scope, window, allowance, refusal) in checks {
        if !limits::take(connection, &scope, window, allowance)? {
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

/// The proof-of-work difficulty an email request from this address must meet.
///
/// The claim's difficulty ([`limits::required_work_bits`], which rises with
/// the subnet's claims today), raised the same way by the subnet's mails this
/// hour: a network that is busy asking for mail pays more for each one, as a
/// network busy claiming names does.
///
/// # Errors
///
/// Any SQL failure.
pub fn required_work_bits(
    connection: &Connection,
    client: IpAddr,
    now: i64,
    base: u32,
) -> rusqlite::Result<u32> {
    let claims = limits::required_work_bits(connection, client, now, base)?;
    let mails = limits::used(
        connection,
        &subnet_scope(client),
        limits::window_start(now, HOUR),
    )?;
    let extra = if mails >= MAILS_PER_SUBNET - 1 {
        limits::HECTIC_SUBNET_BITS
    } else if mails >= MAILS_PER_SUBNET - 3 {
        limits::BUSY_SUBNET_BITS
    } else {
        0
    };
    Ok(claims.max(base + extra).min(MAX_WORK_BITS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    const NOW: i64 = 1_800_000_000;

    fn uuid(seed: u8) -> PlayerUuid {
        PlayerUuid::from_bytes([seed; 32])
    }

    #[test]
    fn tags_and_case_fold_to_one_address() {
        assert_eq!(fold_address("Alice+games@Example.COM"), "alice@example.com");
        assert_eq!(fold_address("alice@example.com"), "alice@example.com");
        assert_eq!(fold_address("a+b+c@example.com"), "a@example.com");
        // Only the local part's tag: a `+` in the domain is not a tag (and not
        // a valid domain either, but the fold must not move the `@`).
        assert_eq!(fold_address("bob@ex+ample.com"), "bob@ex+ample.com");
        assert_eq!(
            address_scope("ALICE+1@example.com"),
            address_scope("alice+2@EXAMPLE.com")
        );
        assert_ne!(
            address_scope("alice@example.com"),
            address_scope("bob@example.com")
        );
    }

    #[test]
    fn no_raw_address_is_stored_in_a_quota_key() {
        let scope = address_scope("alice@example.com");
        assert!(scope.starts_with("mail:"));
        assert!(!scope.contains('@') && !scope.contains("alice"), "{scope}");
        assert_eq!(scope.len(), "mail:".len() + 64);
    }

    #[test]
    fn each_allowance_refuses_in_turn() {
        let db = Db::in_memory().expect("db");
        let client: IpAddr = "203.0.113.7".parse().expect("ip");
        db.with(|connection| {
            // One identity: three a day, whatever the address.
            for n in 0..3 {
                assert_eq!(
                    take(
                        connection,
                        &format!("a{n}@example.com"),
                        &uuid(1),
                        client,
                        NOW,
                        60
                    )?,
                    None
                );
            }
            assert_eq!(
                take(connection, "a9@example.com", &uuid(1), client, NOW, 60)?,
                Some(MailLimit::Identity)
            );

            // One address, tagged three ways, from three identities: three an
            // hour.
            for n in 0..3 {
                assert_eq!(
                    take(
                        connection,
                        &format!("bob+{n}@example.com"),
                        &uuid(10 + n),
                        client,
                        NOW,
                        60
                    )?,
                    None
                );
            }
            assert_eq!(
                take(connection, "BOB@example.com", &uuid(20), client, NOW, 60)?,
                Some(MailLimit::Address)
            );
            Ok(())
        })
        .expect("limits");
    }

    #[test]
    fn the_neighbourhood_and_the_registry_have_ceilings() {
        let db = Db::in_memory().expect("db");
        db.with(|connection| {
            let client: IpAddr = "198.51.100.1".parse().expect("ip");
            for n in 0..MAILS_PER_SUBNET {
                let address = format!("n{n}@example.com");
                assert_eq!(
                    take(connection, &address, &uuid(n as u8), client, NOW, 1000)?,
                    None
                );
            }
            assert_eq!(
                take(connection, "x@example.com", &uuid(99), client, NOW, 1000)?,
                Some(MailLimit::Subnet)
            );

            // The global ceiling holds across networks, in an hour of its own.
            let later = NOW + HOUR;
            for n in 0..2u8 {
                let elsewhere: IpAddr = format!("192.0.2.{n}").parse().expect("ip");
                assert_eq!(
                    take(
                        connection,
                        &format!("g{n}@example.com"),
                        &uuid(100 + n),
                        elsewhere,
                        later,
                        2
                    )?,
                    None
                );
            }
            let elsewhere: IpAddr = "192.0.2.50".parse().expect("ip");
            assert_eq!(
                take(
                    connection,
                    "g9@example.com",
                    &uuid(150),
                    elsewhere,
                    later,
                    2
                )?,
                Some(MailLimit::Global)
            );
            Ok(())
        })
        .expect("limits");
    }

    #[test]
    fn a_network_busy_asking_for_mail_pays_more_for_the_next() {
        let db = Db::in_memory().expect("db");
        let client: IpAddr = "203.0.113.9".parse().expect("ip");
        db.with(|connection| {
            assert_eq!(required_work_bits(connection, client, NOW, 4)?, 4);
            let hour = limits::window_start(NOW, HOUR);
            for _ in 0..(MAILS_PER_SUBNET - 3) {
                limits::take(connection, &subnet_scope(client), hour, MAILS_PER_SUBNET)?;
            }
            assert_eq!(
                required_work_bits(connection, client, NOW, 4)?,
                4 + limits::BUSY_SUBNET_BITS
            );
            for _ in 0..2 {
                limits::take(connection, &subnet_scope(client), hour, MAILS_PER_SUBNET)?;
            }
            assert_eq!(
                required_work_bits(connection, client, NOW, 4)?,
                4 + limits::HECTIC_SUBNET_BITS
            );
            Ok(())
        })
        .expect("work");
    }
}
