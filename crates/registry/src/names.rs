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
//!   operator's table.
//! - **A claim names an identity, not just a key.** `verify_claim` has already
//!   shown the signing key to be the identity's own — the root, or a device a
//!   signed key-set chain reaches from it — so this layer takes the UUID as
//!   given and keeps no key store.

use std::net::IpAddr;

use rusqlite::{Connection, OptionalExtension, params};
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::{
    NameCertificate, VerifiedClaim, VerifiedRelease, is_reserved,
};

use crate::keys::RegistryKeys;
use crate::limits::{self, DAY};

/// How long a released name is held for the UUID that released it.
pub const HOLD_AFTER_RELEASE: i64 = 30 * DAY;

/// When a name never renewed since its claim stops being the holder's.
pub const EXPIRY_NEVER_RENEWED: i64 = 90 * DAY;

/// When a name that has been renewed stops being the holder's.
pub const EXPIRY_AFTER_RENEWAL: i64 = 365 * DAY;

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
    /// A quota refused it.
    Limited(Limit),
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
) -> rusqlite::Result<ClaimOutcome> {
    let canonical = claimed.name.canonical().to_owned();
    let uuid = claimed.uuid.to_hex();
    let transaction = connection.transaction()?;

    if is_reserved(&canonical) || reserved_here(&transaction, &canonical)? {
        return Ok(ClaimOutcome::Reserved);
    }
    if active_binding(&transaction, &claimed.uuid)?.is_some() {
        return Ok(ClaimOutcome::AlreadyNamed);
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
        &client.to_string(),
    )?;
    let certificate = keys.issue(&claimed.name, claimed.uuid, now, serial);
    let notice = crate::recover::pending_notice(&transaction, &canonical, now)?;
    transaction.commit()?;
    Ok(ClaimOutcome::Issued {
        certificate,
        notice,
    })
}

/// Renews the binding `claimed.name` already holds, or says why not.
///
/// # Errors
///
/// Any SQL failure.
pub fn renew(
    connection: &mut Connection,
    keys: &RegistryKeys,
    claimed: &VerifiedClaim,
    now: i64,
) -> rusqlite::Result<RenewOutcome> {
    let canonical = claimed.name.canonical().to_owned();
    let uuid = claimed.uuid.to_hex();
    let transaction = connection.transaction()?;
    let Some(existing) = active_binding(&transaction, &claimed.uuid)? else {
        return Ok(RenewOutcome::Unknown);
    };
    if existing.canonical != canonical {
        return Ok(RenewOutcome::NotYours);
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
    )?;
    let certificate = keys.issue(&claimed.name, claimed.uuid, now, serial);
    let notice = crate::recover::pending_notice(&transaction, &canonical, now)?;
    transaction.commit()?;
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
) -> rusqlite::Result<ReleaseOutcome> {
    let uuid = request.uuid.to_hex();
    let transaction = connection.transaction()?;
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
    )?;
    transaction.commit()?;
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
    /// When a name that is not free becomes claimable, if that is knowable.
    pub free_after: Option<i64>,
}

/// Reads the public state of a name.
///
/// # Errors
///
/// Any SQL failure.
pub fn status(connection: &Connection, canonical: &str, now: i64) -> rusqlite::Result<NameStatus> {
    let reserved = is_reserved(canonical) || reserved_here(connection, canonical)?;
    match binding(connection, canonical)? {
        None => Ok(NameStatus {
            canonical: canonical.to_owned(),
            display: None,
            uuid: None,
            reserved,
            recovery_pending: false,
            free_after: None,
        }),
        Some(binding) => {
            let free_at = free_at(&binding);
            let held = binding.released.is_none() && now < free_at;
            Ok(NameStatus {
                canonical: canonical.to_owned(),
                display: held.then(|| binding.display.clone()),
                uuid: held.then(|| PlayerUuid::from_bytes(binding.uuid).to_hex()),
                reserved,
                recovery_pending: false,
                free_after: (!held && free_at > now).then_some(free_at),
            })
        }
    }
}

/// Whether a name is in the operator's reserved table.
fn reserved_here(connection: &Connection, canonical: &str) -> rusqlite::Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM reserved WHERE canonical = ?1",
        [canonical],
        |row| row.get(0),
    )?;
    Ok(count > 0)
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
/// `who` is a UUID rather than a name: names change, identities do not.
pub(crate) fn audit(
    connection: &Connection,
    who: &str,
    what: &str,
    now: i64,
    why: &str,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO audit (who, what, when_at, why) VALUES (?1, ?2, ?3, ?4)",
        params![who, what, now, why],
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
    use tiamat_core::identity::registry::{Name, verify_certificate};

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
            };
            let ip: IpAddr = ip.parse().expect("ip");
            self.db
                .with(|connection| claim(connection, &self.keys, &verified, now, ip))
                .expect("claim")
        }

        fn renew(&mut self, canonical: &str, display: &str, player: u8, now: i64) -> RenewOutcome {
            let identity = SigningKey::from_bytes(&[player; 32]);
            let verified = VerifiedClaim {
                name: Name::from_parts(canonical, display).expect("name"),
                uuid: PlayerUuid::from_root_key(&identity.verifying_key()),
                public_key: identity.verifying_key(),
                timestamp: now,
            };
            self.db
                .with(|connection| renew(connection, &self.keys, &verified, now))
                .expect("renew")
        }

        fn release(&mut self, canonical: &str, player: u8, now: i64) -> ReleaseOutcome {
            let identity = SigningKey::from_bytes(&[player; 32]);
            let verified = VerifiedRelease {
                name: canonical.to_owned(),
                uuid: PlayerUuid::from_root_key(&identity.verifying_key()),
                public_key: identity.verifying_key(),
            };
            self.db
                .with(|connection| release(connection, &verified, now))
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
        match fixture.renew("alice", "ALICE", 1, NOW + 100) {
            RenewOutcome::Renewed { certificate, .. } => {
                assert_eq!(certificate.serial, first.serial + 1);
                assert_eq!(certificate.display, "ALICE");
                assert_eq!(certificate.expires, NOW + 100 + 90 * DAY);
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
    fn a_renewed_name_expires_a_year_after_its_last_renewal() {
        let mut fixture = Fixture::new();
        fixture.claim("alice", "Alice", 1, "203.0.113.1", NOW);
        fixture.renew("alice", "Alice", 1, NOW + 100);
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
        let free = NOW + 100 + EXPIRY_AFTER_RENEWAL + HOLD_AFTER_RELEASE;
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
        assert_eq!(page.free_after, None);

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
}
