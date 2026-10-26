// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The operator's commands: the actions that are deliberately manual.
//!
//! Everything here is a subcommand of `registry admin`, run on the machine that
//! holds the keys or beside the database. Nothing is reachable over HTTP: the
//! design's §4 makes revocation, reservation and renaming an operator's
//! judgement, and a judgement with an API is a thing an attacker can try to
//! automate.
//!
//! Every function writes an **audit row** with the reason it was given. The
//! reason is required, not defaulted: "why did this name disappear" is the
//! question a player will ask, and an empty answer is the one that costs.

use std::collections::BTreeMap;
use std::net::IpAddr;

use ed25519_dalek::SigningKey;
use rusqlite::{OptionalExtension, params};
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::{Name, RegistryError, RotationRecord, check_canonical};

use crate::config::Config;
use crate::db::{Db, DbError, write_transaction};
use crate::hex;
use crate::keys::{KeyError, RegistryKeys};
use crate::limits;

/// How the admin commands failed.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// An admin command needs the database.
    #[error("admin commands need REGISTRY_DATA set; they read the deployment's database")]
    NoDataDir,

    /// The database failed.
    #[error(transparent)]
    Db(#[from] DbError),

    /// The key chain failed.
    #[error(transparent)]
    Keys(#[from] KeyError),

    /// A name the operator gave is not a name.
    #[error(transparent)]
    Name(#[from] RegistryError),

    /// A hex value the operator gave is not hex.
    #[error("`{what}` is not 64 hex characters")]
    Hex {
        /// Which value.
        what: &'static str,
    },

    /// No active name for a UUID.
    #[error("no identity with an active name has that UUID")]
    UnknownIdentity,

    /// A bulk revocation whose network and window matched no claim.
    ///
    /// An error rather than "revoked 0": the operator typed this because they
    /// saw a burst, so matching nothing means the subnet or the window was
    /// mistyped, and a quiet success would leave the burst standing.
    #[error("no claim from {subnet} in that window; check the subnet and --since")]
    NoMatchingClaims {
        /// The subnet as it was matched.
        subnet: String,
    },

    /// The minute clock is wrong.
    #[error("could not read the system clock")]
    Clock,

    /// A record could not be encoded for storage.
    #[error("a rotation record could not be encoded")]
    Encode,
}

/// What the registry holds, in one screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Names with an active binding.
    pub held: i64,
    /// Bindings released and waiting out their hold.
    pub released: i64,
    /// Verified recovery addresses.
    pub emails: i64,
    /// Revoked certificate serials.
    pub revocations: i64,
    /// Recovery notices running.
    pub pending_recoveries: i64,
    /// Claims in the last day.
    pub claims_today: i64,
}

/// Claims from one network, in a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetClaims {
    /// The scope key, `net:203.0.113.0/24` or `unknown`.
    pub subnet: String,
    /// How many claims.
    pub claims: usize,
}

/// One day's claims and recovery notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyActivity {
    /// The date, `YYYY-MM-DD`, UTC.
    pub date: String,
    /// Claims that day.
    pub claims: i64,
    /// Recovery notices started that day.
    pub recoveries: i64,
}

/// What a bulk revocation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revoked {
    /// Names released.
    pub names: usize,
    /// Certificate serials revoked.
    pub serials: usize,
}

/// The operator's view of a running registry.
pub struct Admin {
    db: Db,
    keys: RegistryKeys,
    now: i64,
}

impl Admin {
    /// Opens the deployment's database and key chain.
    ///
    /// # Errors
    ///
    /// [`AdminError`] when there is no data directory, the database cannot be
    /// opened, or the environment's key does not match the chain.
    pub fn open(config: &Config) -> Result<Self, AdminError> {
        let Some(dir) = &config.data_dir else {
            return Err(AdminError::NoDataDir);
        };
        let db = Db::open(dir.join("registry.sqlite"))?;
        let keys = crate::keys::from_database(&db, config.key_seed, config.next_key_hash)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| AdminError::Clock)?
            .as_secs() as i64;
        Ok(Self { db, keys, now })
    }

    /// Counts what the registry holds.
    ///
    /// # Errors
    ///
    /// [`AdminError::Db`] when a query fails.
    pub fn stats(&self) -> Result<Stats, AdminError> {
        let day_ago = self.now - limits::DAY;
        let stats = self.db.with(|connection| {
            let count = |sql: &str| -> rusqlite::Result<i64> {
                connection.query_row(sql, [], |row| row.get(0))
            };
            Ok(Stats {
                held: count("SELECT COUNT(*) FROM names WHERE released IS NULL")?,
                released: count("SELECT COUNT(*) FROM names WHERE released IS NOT NULL")?,
                emails: count("SELECT COUNT(*) FROM emails")?,
                revocations: count("SELECT COUNT(*) FROM revocations")?,
                pending_recoveries: count(
                    "SELECT COUNT(*) FROM recoveries WHERE state = 'pending'",
                )?,
                claims_today: connection.query_row(
                    "SELECT COUNT(*) FROM audit WHERE what LIKE 'claim %' AND when_at >= ?1",
                    [day_ago],
                    |row| row.get(0),
                )?,
            })
        })?;
        Ok(stats)
    }

    /// Groups the window's claims by network, biggest first.
    ///
    /// The network comes from the audit row's `ip`, which the retention sweep
    /// clears seven days after the claim (§8): a claim older than that is
    /// counted under `unknown`, because the registry no longer knows where it
    /// came from.
    ///
    /// # Errors
    ///
    /// [`AdminError::Db`] when the audit table cannot be read.
    pub fn claims_by_subnet(&self, since: i64) -> Result<Vec<SubnetClaims>, AdminError> {
        let rows: Vec<Option<String>> = self.db.with(|connection| {
            let mut statement = connection
                .prepare("SELECT ip FROM audit WHERE what LIKE 'claim %' AND when_at >= ?1")?;
            let rows = statement.query_map([since], |row| row.get(0))?;
            rows.collect()
        })?;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for ip in rows {
            *counts.entry(subnet_of(ip.as_deref())).or_default() += 1;
        }
        let mut counts: Vec<SubnetClaims> = counts
            .into_iter()
            .map(|(subnet, claims)| SubnetClaims { subnet, claims })
            .collect();
        // Biggest first, ties by name so the output is stable between runs.
        counts.sort_by(|left, right| {
            right
                .claims
                .cmp(&left.claims)
                .then_with(|| left.subnet.cmp(&right.subnet))
        });
        Ok(counts)
    }

    /// Claims and recovery notices per day, most recent first.
    ///
    /// # Errors
    ///
    /// [`AdminError::Db`] when the audit table cannot be read.
    pub fn daily_activity(&self, days: i64) -> Result<Vec<DailyActivity>, AdminError> {
        let since = self.now - days * limits::DAY;
        let rows: Vec<(i64, String)> = self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT when_at, what FROM audit WHERE when_at >= ?1 ORDER BY when_at DESC",
            )?;
            let rows = statement.query_map([since], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect()
        })?;
        let mut by_day: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for (at, what) in rows {
            let day = crate::backup::date_of(at);
            let entry = by_day.entry(day).or_default();
            if what.starts_with("claim ") {
                entry.0 += 1;
            } else if what.starts_with("recovery notice") {
                entry.1 += 1;
            }
        }
        Ok(by_day
            .into_iter()
            .rev()
            .map(|(date, (claims, recoveries))| DailyActivity {
                date,
                claims,
                recoveries,
            })
            .collect())
    }

    /// Revokes every name claimed from a network in a window.
    ///
    /// The burst remedy from §7: one line undoes a squatter's morning. It
    /// reaches as far back as claims still carry their address, which is the
    /// seven days §8 keeps one for. Each
    /// name is released with the ordinary thirty-day hold, its certificates
    /// that can still verify go on the revocation list, the identity is barred
    /// from claiming that name back (`revoked_holders`), and the reason goes
    /// to the audit table.
    ///
    /// `subnet` is accepted as `admin claims` prints it (`net:203.0.113.0/24`)
    /// or bare (`203.0.113.0/24`), so pasting the one into the other works.
    ///
    /// # Errors
    ///
    /// [`AdminError::Db`] when a query fails, and
    /// [`AdminError::NoMatchingClaims`] when nothing matched.
    pub fn revoke_subnet(
        &self,
        subnet: &str,
        since: i64,
        reason: &str,
    ) -> Result<Revoked, AdminError> {
        let bare = subnet.trim();
        let bare = bare.strip_prefix("net:").unwrap_or(bare);
        let wanted = format!("net:{bare}");
        let now = self.now;
        let revoked = self.db.with(|connection| {
            let transaction = write_transaction(connection)?;
            let rows: Vec<(String, Option<String>)> = {
                let mut statement = transaction.prepare(
                    "SELECT who, ip FROM audit WHERE what LIKE 'claim %' AND when_at >= ?1",
                )?;
                let rows = statement.query_map([since], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut identities: Vec<[u8; 32]> = Vec::new();
            for (who, ip) in rows {
                if subnet_of(ip.as_deref()) != wanted {
                    continue;
                }
                if let Some(uuid) = hex::decode::<32>(&who)
                    && !identities.contains(&uuid)
                {
                    identities.push(uuid);
                }
            }

            if identities.is_empty() {
                // Nothing to write; the transaction's drop is the rollback.
                return Ok(None);
            }

            let mut serials = 0;
            let mut names = 0;
            for uuid in &identities {
                let issues = crate::names::live_issues(&transaction, uuid.as_slice(), now)?;
                // The names this identity holds now: each is barred to it
                // after the release, or the release would be undone by the
                // next claim (a released name is its holder's to reclaim).
                let held: Vec<String> = {
                    let mut statement = transaction.prepare(
                        "SELECT canonical FROM names WHERE uuid = ?1 AND released IS NULL",
                    )?;
                    let rows = statement.query_map([uuid.as_slice()], |row| row.get(0))?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                };
                for canonical in &held {
                    transaction.execute(
                        "INSERT OR IGNORE INTO revoked_holders (canonical, uuid, revoked_at)
                         VALUES (?1, ?2, ?3)",
                        params![canonical, uuid.as_slice(), now],
                    )?;
                }
                for (registry_key, serial) in issues {
                    serials += transaction.execute(
                        "INSERT OR IGNORE INTO revocations (registry_key, serial, when_at, reason)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![registry_key, serial, now, reason],
                    )?;
                }
                names += transaction.execute(
                    "UPDATE names SET released = ?1, hold_until = ?2
                     WHERE uuid = ?3 AND released IS NULL",
                    params![now, now + 30 * limits::DAY, uuid.as_slice()],
                )?;
                transaction.execute(
                    "INSERT INTO audit (who, what, when_at, why) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        hex::encode(uuid),
                        format!("operator revoke --subnet {subnet}"),
                        now,
                        reason
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(Some(Revoked { names, serials }))
        })?;
        revoked.ok_or_else(|| AdminError::NoMatchingClaims {
            subnet: wanted.clone(),
        })
    }

    /// Reserves a name, so no claim may take it.
    ///
    /// # Errors
    ///
    /// [`AdminError::Name`] when the canonical form is not a name.
    pub fn reserve(&self, canonical: &str) -> Result<(), AdminError> {
        check_canonical(canonical)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO reserved (canonical) VALUES (?1)",
                [canonical],
            )?;
            connection.execute(
                "INSERT INTO audit (who, what, when_at, why) VALUES ('operator', ?1, ?2, 'reserve')",
                params![format!("reserve {canonical}"), self.now],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Releases a reservation.
    ///
    /// # Errors
    ///
    /// [`AdminError::Name`] when the canonical form is not a name.
    pub fn unreserve(&self, canonical: &str) -> Result<bool, AdminError> {
        check_canonical(canonical)?;
        let removed = self.db.with(|connection| {
            let removed =
                connection.execute("DELETE FROM reserved WHERE canonical = ?1", [canonical])?;
            if removed > 0 {
                connection.execute(
                    "INSERT INTO audit (who, what, when_at, why)
                     VALUES ('operator', ?1, ?2, 'unreserve')",
                    params![format!("unreserve {canonical}"), self.now],
                )?;
            }
            Ok(removed > 0)
        })?;
        Ok(removed)
    }

    /// Changes the display casing of a held name.
    ///
    /// The canonical form cannot move: it is what uniqueness is decided on.
    ///
    /// # Errors
    ///
    /// [`AdminError::Hex`] when the UUID is not hex, [`AdminError::Name`] when
    /// the display casing is not the canonical form in another casing, and
    /// [`AdminError::UnknownIdentity`] when that UUID holds no name.
    pub fn rename(&self, uuid_hex: &str, display: &str) -> Result<Name, AdminError> {
        let uuid = hex::decode::<32>(uuid_hex).ok_or(AdminError::Hex { what: "uuid" })?;
        let canonical: Option<String> = self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT canonical FROM names WHERE uuid = ?1 AND released IS NULL",
                    [uuid.as_slice()],
                    |row| row.get(0),
                )
                .optional()
        })?;
        let Some(canonical) = canonical else {
            return Err(AdminError::UnknownIdentity);
        };
        // Checked outside the write so a bad casing is a sentence, not a
        // `rusqlite::Error` with the reason lost.
        let name = Name::from_parts(&canonical, display)?;
        let now = self.now;
        let who = hex::encode(&uuid);
        self.db.with(|connection| {
            connection.execute(
                "UPDATE names SET display = ?1 WHERE canonical = ?2",
                params![name.display(), canonical],
            )?;
            connection.execute(
                "INSERT INTO audit (who, what, when_at, why) VALUES (?1, ?2, ?3, 'rename')",
                params![who, format!("rename {canonical}"), now],
            )?;
            Ok(())
        })?;
        Ok(name)
    }

    /// Publishes a rotation to the successor whose seed is in `successor_seed`.
    ///
    /// Run while the environment still holds the **current** key: the record is
    /// signed by the key being retired. Afterwards the operator sets
    /// `REGISTRY_KEY` to the successor and `REGISTRY_NEXT_KEY_HASH` to `next`.
    ///
    /// # Errors
    ///
    /// [`AdminError::Keys`] when there is no commitment, the successor does not
    /// match it, or the database cannot be read.
    pub fn rotate(
        &self,
        successor_seed: [u8; 32],
        next: [u8; 32],
    ) -> Result<RotationRecord, AdminError> {
        let successor = SigningKey::from_bytes(&successor_seed).verifying_key();
        let record = self.keys.sign_rotation(&successor, Some(next), self.now)?;
        let encoded = postcard::to_allocvec(&record).map_err(|_| AdminError::Encode)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO rotations (seq, record)
                 VALUES ((SELECT COALESCE(MAX(seq), -1) + 1 FROM rotations), ?1)",
                [encoded],
            )?;
            connection.execute(
                "INSERT INTO audit (who, what, when_at, why)
                 VALUES ('operator', 'rotate the registry key', ?1, ?2)",
                params![
                    self.now,
                    format!("successor {}", hex::encode(successor.as_bytes()))
                ],
            )?;
            Ok(())
        })?;
        Ok(record)
    }

    /// The key chain this admin opened, for a command that prints it.
    #[must_use]
    pub fn keys(&self) -> &RegistryKeys {
        &self.keys
    }

    /// The database, for a command that needs a direct query.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// When the command started, Unix seconds.
    #[must_use]
    pub fn now(&self) -> i64 {
        self.now
    }
}

impl std::fmt::Debug for Admin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admin")
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

/// The `net:` scope of an audit row's address, or `unknown` when it has none
/// (cleared by the retention sweep) or it does not parse.
fn subnet_of(ip: Option<&str>) -> String {
    ip.and_then(|ip| ip.trim().parse::<IpAddr>().ok())
        .map_or_else(|| "unknown".to_owned(), limits::subnet_scope)
}

/// Parses a window like `48h` or `30m` into Unix seconds before now.
///
/// # Errors
///
/// A sentence when the text is not a window.
pub fn parse_window(text: &str, now: i64) -> Result<i64, String> {
    let trimmed = text.trim();
    // By character, not byte: `split_at` on a byte index panics inside a
    // multi-byte character, and an operator's typo is not a reason to panic.
    let mut characters = trimmed.chars();
    let Some(unit) = characters.next_back() else {
        return Err(format!("`{text}` is not a window like 48h, 30m or 7d"));
    };
    let number = characters.as_str();
    let count: i64 = number
        .parse()
        .map_err(|_| format!("`{text}` is not a window like 48h, 30m or 7d"))?;
    if count < 0 {
        return Err(format!("`{text}` is a negative window"));
    }
    let seconds: i64 = match unit {
        'm' => 60,
        'h' => 60 * 60,
        'd' => 24 * 60 * 60,
        'w' => 7 * 24 * 60 * 60,
        _ => return Err(format!("`{text}` has no unit; use m, h, d or w")),
    };
    count
        .checked_mul(seconds)
        .and_then(|span| now.checked_sub(span))
        .ok_or_else(|| format!("`{text}` is longer than any window this registry has"))
}

/// A player's UUID from hex, for a command that prints one.
#[must_use]
pub fn uuid_from_hex(text: &str) -> Option<PlayerUuid> {
    hex::decode::<32>(text).map(PlayerUuid::from_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::RegistryKeys;
    use ed25519_dalek::SigningKey;

    const NOW: i64 = 1_800_000_000;

    fn admin() -> Admin {
        let keys = RegistryKeys::open([7; 32], None, Vec::new()).expect("keys");
        Admin {
            db: Db::in_memory().expect("db"),
            keys,
            now: NOW,
        }
    }

    fn hold_name(admin: &Admin, canonical: &str, player_seed: u8, ip: &str) {
        let uuid =
            PlayerUuid::from_root_key(&SigningKey::from_bytes(&[player_seed; 32]).verifying_key());
        admin
            .db
            .with(|connection| {
                connection.execute(
                    "INSERT INTO names (canonical, display, uuid, claimed, renewed, renewals)
                     VALUES (?1, ?1, ?2, ?3, ?3, 0)",
                    params![canonical, uuid.as_bytes().as_slice(), NOW - 1000],
                )?;
                connection.execute(
                    "INSERT INTO issued (serial, registry_key, uuid, canonical, issued)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        i64::from(player_seed) + 1,
                        admin.keys.public_key().to_bytes().as_slice(),
                        uuid.as_bytes().as_slice(),
                        canonical,
                        NOW - 1000
                    ],
                )?;
                connection.execute(
                    "INSERT INTO audit (who, what, when_at, why, ip)
                     VALUES (?1, ?2, ?3, 'claim', ?4)",
                    params![uuid.to_hex(), format!("claim {canonical}"), NOW - 100, ip],
                )?;
                Ok(())
            })
            .expect("seed");
    }

    #[test]
    fn a_window_is_a_number_and_a_unit() {
        assert_eq!(parse_window("48h", NOW), Ok(NOW - 48 * 3600));
        assert_eq!(parse_window("30m", NOW), Ok(NOW - 30 * 60));
        assert_eq!(parse_window("2d", NOW), Ok(NOW - 2 * 86_400));
        assert_eq!(parse_window("1w", NOW), Ok(NOW - 7 * 86_400));
        assert!(parse_window("48", NOW).is_err());
        assert!(parse_window("h", NOW).is_err());
        assert!(parse_window("x h", NOW).is_err());
        // A trailing multi-byte character, an empty string, and a window so
        // long it overflows: refusals, not panics.
        assert!(parse_window("48é", NOW).is_err());
        assert!(parse_window("é", NOW).is_err());
        assert!(parse_window("", NOW).is_err());
        assert!(parse_window("9223372036854775807w", NOW).is_err());
    }

    #[test]
    fn claims_are_grouped_by_network_biggest_first() {
        let admin = admin();
        hold_name(&admin, "alice", 1, "203.0.113.5");
        hold_name(&admin, "bobby", 2, "203.0.113.9");
        hold_name(&admin, "carol", 3, "198.51.100.5");
        let groups = admin.claims_by_subnet(NOW - 3600).expect("groups");
        assert_eq!(groups[0].subnet, "net:203.0.113.0/24");
        assert_eq!(groups[0].claims, 2);
        assert_eq!(groups[1].subnet, "net:198.51.100.0/24");
    }

    #[test]
    fn the_daily_breakdown_counts_claims_and_recoveries() {
        let admin = admin();
        hold_name(&admin, "alice", 1, "203.0.113.5");
        admin
            .db
            .with(|connection| {
                connection.execute(
                    "INSERT INTO audit (who, what, when_at, why)
                     VALUES ('operator', 'recovery notice started for alice', ?1, 'link')",
                    [NOW - 50],
                )?;
                Ok(())
            })
            .expect("audit");
        let days = admin.daily_activity(7).expect("activity");
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].date, crate::backup::date_of(NOW - 100));
        assert_eq!(days[0].claims, 1);
        assert_eq!(days[0].recoveries, 1);
    }

    #[test]
    fn a_bulk_revocation_releases_and_revokes_the_burst() {
        let admin = admin();
        hold_name(&admin, "alice", 1, "203.0.113.5");
        hold_name(&admin, "bobby", 2, "203.0.113.9");
        hold_name(&admin, "carol", 3, "198.51.100.5");
        let revoked = admin
            .revoke_subnet("203.0.113.0/24", NOW - 3600, "burst")
            .expect("revoke");
        assert_eq!(revoked.names, 2);
        assert_eq!(revoked.serials, 2);

        let released: i64 = admin
            .db
            .with(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM names WHERE released IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("count");
        assert_eq!(released, 2);
        // Carol's network was not touched.
        let held: i64 = admin
            .db
            .with(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM names WHERE canonical = 'carol' AND released IS NULL",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("count");
        assert_eq!(held, 1);
        let rows: i64 = admin
            .db
            .with(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM audit WHERE what LIKE 'operator revoke%'",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("audit");
        assert_eq!(rows, 2);
    }

    #[test]
    fn a_revoked_squatter_cannot_claim_the_name_back() {
        let admin = admin();
        hold_name(&admin, "alice", 1, "203.0.113.5");
        admin
            .revoke_subnet("203.0.113.0/24", NOW - 3600, "burst")
            .expect("revoke");

        // The same identity, at once, from another network: before the mark,
        // `is_free`'s "a released name is its holder's" let it straight back.
        let squatter = SigningKey::from_bytes(&[1; 32]);
        let verified = tiamat_core::identity::registry::VerifiedClaim {
            name: Name::from_parts("alice", "alice").expect("name"),
            uuid: PlayerUuid::from_root_key(&squatter.verifying_key()),
            public_key: squatter.verifying_key(),
            timestamp: NOW + 10,
        };
        let ip: IpAddr = "198.51.100.9".parse().expect("ip");
        let outcome = admin
            .db
            .with(|connection| {
                crate::names::claim(
                    connection,
                    &admin.keys,
                    &verified,
                    NOW + 10,
                    ip,
                    &crate::deadline::Deadline::none(),
                )
            })
            .expect("claim");
        assert!(
            matches!(outcome, crate::names::ClaimOutcome::Revoked),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_subnet_pasted_from_the_claims_view_matches_and_a_miss_is_an_error() {
        let admin = admin();
        hold_name(&admin, "alice", 1, "203.0.113.5");
        let shown = admin.claims_by_subnet(NOW - 3600).expect("groups")[0]
            .subnet
            .clone();
        assert_eq!(shown, "net:203.0.113.0/24");
        let revoked = admin
            .revoke_subnet(&shown, NOW - 3600, "burst")
            .expect("the printed form is accepted");
        assert_eq!(revoked.names, 1);

        assert!(matches!(
            admin.revoke_subnet("192.0.2.0/24", NOW - 3600, "typo"),
            Err(AdminError::NoMatchingClaims { .. })
        ));
    }

    #[test]
    fn a_claim_whose_address_has_expired_is_grouped_as_unknown_and_not_revoked() {
        let admin = admin();
        hold_name(&admin, "alice", 1, "203.0.113.5");
        hold_name(&admin, "bobby", 2, "203.0.113.9");
        // A week on, the retention sweep has cleared Bobby's address (here by
        // hand, as the sweep would).
        admin
            .db
            .with(|connection| {
                connection.execute("UPDATE audit SET ip = NULL WHERE what = 'claim bobby'", [])?;
                Ok(())
            })
            .expect("expire");
        let groups = admin.claims_by_subnet(NOW - 3600).expect("groups");
        assert_eq!(
            groups,
            vec![
                SubnetClaims {
                    subnet: "net:203.0.113.0/24".to_owned(),
                    claims: 1
                },
                SubnetClaims {
                    subnet: "unknown".to_owned(),
                    claims: 1
                },
            ]
        );
        let revoked = admin
            .revoke_subnet("203.0.113.0/24", NOW - 3600, "burst")
            .expect("revoke");
        assert_eq!(
            revoked.names, 1,
            "only the claim that still has its address"
        );
    }

    #[test]
    fn a_reservation_keeps_a_name_out_of_claims() {
        let admin = admin();
        admin.reserve("secretname").expect("reserve");
        let reserved: i64 = admin
            .db
            .with(|connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM reserved WHERE canonical = 'secretname'",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("count");
        assert_eq!(reserved, 1);
        assert!(admin.unreserve("secretname").expect("unreserve"));
        assert!(!admin.unreserve("secretname").expect("unreserve"));
    }

    #[test]
    fn a_rename_changes_the_display_and_not_the_canonical_form() {
        let admin = admin();
        hold_name(&admin, "alice", 1, "203.0.113.5");
        let uuid = PlayerUuid::from_root_key(&SigningKey::from_bytes(&[1; 32]).verifying_key());
        let name = admin.rename(&uuid.to_hex(), "ALICE").expect("rename");
        assert_eq!(name.canonical(), "alice");
        assert_eq!(name.display(), "ALICE");
        // A display that is not the canonical form in other casing is refused.
        assert!(admin.rename(&uuid.to_hex(), "bob").is_err());
        // A UUID that holds no name is refused.
        let other = PlayerUuid::from_root_key(&SigningKey::from_bytes(&[9; 32]).verifying_key());
        assert!(matches!(
            admin.rename(&other.to_hex(), "Other"),
            Err(AdminError::UnknownIdentity)
        ));
    }

    #[test]
    fn a_rotation_is_signed_and_published_only_against_its_commitment() {
        let successor = SigningKey::from_bytes(&[2; 32]);
        let after = SigningKey::from_bytes(&[3; 32]);
        let committed = tiamat_core::identity::keyset::commit_to(&successor.verifying_key());
        let next_commitment = tiamat_core::identity::keyset::commit_to(&after.verifying_key());
        let keys = RegistryKeys::open([1; 32], Some(committed), Vec::new()).expect("keys");
        let admin = Admin {
            db: Db::in_memory().expect("db"),
            keys,
            now: NOW,
        };
        let record = admin
            .rotate([2; 32], next_commitment)
            .expect("a committed successor");
        assert_eq!(
            record.retired,
            SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes()
        );
        assert_eq!(record.successor, successor.verifying_key().to_bytes());
        assert_eq!(record.next_key_hash, Some(next_commitment));
        let stored: i64 = admin
            .db
            .with(|connection| {
                connection.query_row("SELECT COUNT(*) FROM rotations", [], |row| row.get(0))
            })
            .expect("rotations");
        assert_eq!(stored, 1);

        // The wrong successor is refused.
        let bad = Admin {
            db: Db::in_memory().expect("db"),
            keys: RegistryKeys::open([1; 32], Some(committed), Vec::new()).expect("keys"),
            now: NOW,
        };
        assert!(matches!(
            bad.rotate([4; 32], next_commitment),
            Err(AdminError::Keys(KeyError::SuccessorMismatch))
        ));
    }
}
