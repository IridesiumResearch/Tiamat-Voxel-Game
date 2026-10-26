// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Rate limits, and the proof of work that rises with a subnet's claim rate.
//!
//! The limits are the design's (§7): two claims a day per address, six per
//! `/24`; one claim and three renames per identity in any thirty days; and a
//! proof of work on every claim, whose difficulty goes up while a subnet is
//! busy. The point is not to stop a determined squatter — nothing here does —
//! but to make a thousand names cost a thousand seconds and a burst from one
//! place pay more for each name.
//!
//! # Fixed windows, and why that is enough
//!
//! A counter is `(scope, window_start, count)`, where the window is a calendar
//! day or a thirty-day block. A sliding window would be fairer at the boundary
//! and would need two rows and interpolation per check; the operator's remedy
//! for a burst is `admin revoke --subnet`, which is the design's answer too.
//!
//! # The cap is enforced, not the error
//!
//! Every check runs with the database mutex held, so "read the count, then
//! write it" has no race here: the service serialises its own writes.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use rusqlite::{Connection, OptionalExtension};

use tiamat_core::identity::registry::MAX_WORK_BITS;

/// An hour, in seconds: the window the mail allowances use.
pub const HOUR: i64 = 60 * 60;

/// A day, in seconds.
pub const DAY: i64 = 24 * 60 * 60;

/// The thirty-day window the per-identity rules use.
pub const THIRTY_DAYS: i64 = 30 * DAY;

/// How long a limit row is kept **after its window ends**, before the sweep
/// deletes it.
///
/// Seven days, from the design. Measured from the window's end, not its start:
/// a thirty-day window that started eight days ago is still the current one,
/// and deleting it would hand every identity a fresh claim allowance in the
/// middle of the month.
pub const LIMIT_RETENTION: i64 = 7 * DAY;

/// How often the service sweeps, at most: an hour.
///
/// The sweep rides [`crate::service::Service::advance`], which runs before most
/// requests; once an hour is often enough that the table stays small and rare
/// enough that no request pays for it twice.
pub const SWEEP_INTERVAL: i64 = 60 * 60;

/// Claims allowed per address per day.
pub const CLAIMS_PER_IP: i64 = 2;

/// Claims allowed per `/24` per day.
pub const CLAIMS_PER_SUBNET: i64 = 6;

/// Claims allowed per identity per thirty days.
pub const CLAIMS_PER_IDENTITY: i64 = 1;

/// Renames (a release, and the claim that follows it) per identity per thirty
/// days.
pub const RENAMES_PER_IDENTITY: i64 = 3;

/// Casing changes per identity per thirty days, outside a renewal that mints.
///
/// A renewal that is not yet due answers the current certificate in the casing
/// asked for, and records the change in the audit table. Without a cap, a client
/// looping on renew with alternating casings would write an audit row per
/// request for ever — the one table §8 keeps indefinitely. Three, like renames:
/// a casing is the holder's to change, not to churn.
pub const DISPLAY_CHANGES_PER_IDENTITY: i64 = 3;

/// The extra proof-of-work bits a subnet pays once it is busy.
pub const BUSY_SUBNET_BITS: u32 = 4;

/// The extra bits once a subnet is near its daily cap.
pub const HECTIC_SUBNET_BITS: u32 = 8;

/// Where a window starts.
///
/// `now` is Unix seconds and never negative in practice; a negative clock is
/// clamped to its own window rather than panicking.
#[must_use]
pub fn window_start(now: i64, length: i64) -> i64 {
    if length <= 0 {
        return now;
    }
    now.div_euclid(length) * length
}

/// The scope key for one address.
#[must_use]
pub fn ip_scope(ip: IpAddr) -> String {
    format!("ip:{ip}")
}

/// The scope key for an address's neighbourhood.
///
/// `/24` for IPv4, as the design says; `/48` for IPv6, which is the same idea
/// at the granularity one subscriber usually holds.
#[must_use]
pub fn subnet_scope(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            let network = Ipv4Addr::new(octets[0], octets[1], octets[2], 0);
            format!("net:{network}/24")
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            let network = Ipv6Addr::new(segments[0], segments[1], segments[2], 0, 0, 0, 0, 0);
            format!("net:{network}/48")
        }
    }
}

/// One identity's claim allowance.
#[must_use]
pub fn claim_scope(uuid_hex: &str) -> String {
    format!("claim:{uuid_hex}")
}

/// One identity's rename allowance.
#[must_use]
pub fn rename_scope(uuid_hex: &str) -> String {
    format!("rename:{uuid_hex}")
}

/// One identity's allowance of casing changes.
#[must_use]
pub fn display_scope(uuid_hex: &str) -> String {
    format!("display:{uuid_hex}")
}

/// Takes one unit from a window, or reports that there is none left.
///
/// Returns `Ok(true)` when the unit was taken and `Ok(false)` when the window
/// is already at `limit`.
///
/// # Errors
///
/// Any SQL failure.
pub fn take(
    connection: &Connection,
    scope: &str,
    window: i64,
    limit: i64,
) -> rusqlite::Result<bool> {
    let used: i64 = connection
        .query_row(
            "SELECT count FROM limits WHERE scope = ?1 AND window = ?2",
            rusqlite::params![scope, window],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if used >= limit {
        return Ok(false);
    }
    connection.execute(
        "INSERT INTO limits (scope, window, count) VALUES (?1, ?2, 1)
         ON CONFLICT(scope, window) DO UPDATE SET count = count + 1",
        rusqlite::params![scope, window],
    )?;
    Ok(true)
}

/// How many units a window has used so far.
///
/// # Errors
///
/// Any SQL failure.
pub fn used(connection: &Connection, scope: &str, window: i64) -> rusqlite::Result<i64> {
    Ok(connection
        .query_row(
            "SELECT count FROM limits WHERE scope = ?1 AND window = ?2",
            rusqlite::params![scope, window],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// The leading zero bits a claim from this address must carry right now.
///
/// [`DEFAULT_WORK_BITS`] normally; four more once the address's subnet has
/// taken three claims today, eight more at five. The service raises the price
/// because a burst from one place is exactly what the cap cannot stop on its
/// own: six names a day is still six names a day, but the fifth costs sixteen
/// times what the first did.
///
/// The client learns the number from the refusal (`required_bits` in the error)
/// and re-mines; at the default that is about a second, and the raised values
/// are seconds more, on a day someone is abusing the service.
///
/// # Errors
///
/// Any SQL failure.
pub fn required_work_bits(
    connection: &Connection,
    ip: IpAddr,
    now: i64,
    base: u32,
) -> rusqlite::Result<u32> {
    let scope = subnet_scope(ip);
    let window = window_start(now, DAY);
    let claims = used(connection, &scope, window)?;
    let extra = if claims >= CLAIMS_PER_SUBNET - 1 {
        HECTIC_SUBNET_BITS
    } else if claims >= CLAIMS_PER_SUBNET - 3 {
        BUSY_SUBNET_BITS
    } else {
        0
    };
    Ok((base + extra).min(MAX_WORK_BITS))
}

/// Deletes limit rows whose window **ended** more than [`LIMIT_RETENTION`] ago.
///
/// The window's end is its start plus the length of its scope's window, known
/// from the scope's prefix — the scopes are this crate's own strings: `ip:`,
/// `net:` and `mailid:` (in `mailquota`) count by the day; `mail:`, `mailnet:`
/// and `mailall:` by the hour; and the claim, rename and casing allowances by
/// thirty days. So a thirty-day
/// allowance lives as long as it counts, and a day's address count is gone a
/// week after its day. An unknown prefix is given the **longest** window: a
/// sweep that keeps a row too long costs a few bytes, and one that deletes a
/// live row resets somebody's limit.
///
/// # Errors
///
/// Any SQL failure.
pub fn sweep(connection: &Connection, now: i64) -> rusqlite::Result<usize> {
    // One statement over the `limits_window` index rather than a row-by-row
    // loop; the `ELSE` is the unknown-prefix default.
    connection.execute(
        "DELETE FROM limits WHERE window + CASE
             WHEN scope LIKE 'ip:%' OR scope LIKE 'net:%' OR scope LIKE 'mailid:%' THEN ?2
             WHEN scope LIKE 'mail:%' OR scope LIKE 'mailnet:%' OR scope LIKE 'mailall:%' THEN ?3
             ELSE ?4
         END < ?1",
        rusqlite::params![now - LIMIT_RETENTION, DAY, HOUR, THIRTY_DAYS],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use tiamat_core::identity::registry::DEFAULT_WORK_BITS;

    const NOW: i64 = 1_800_000_000;

    fn connection() -> Db {
        Db::in_memory().expect("open")
    }

    #[test]
    fn windows_start_at_their_boundary() {
        assert_eq!(window_start(0, DAY), 0);
        assert_eq!(window_start(DAY - 1, DAY), 0);
        assert_eq!(window_start(DAY, DAY), DAY);
        assert_eq!(window_start(NOW, DAY) % DAY, 0);
        assert_eq!(window_start(NOW, THIRTY_DAYS) % THIRTY_DAYS, 0);
    }

    #[test]
    fn scopes_group_an_address_and_its_neighbourhood() {
        let ip: IpAddr = "203.0.113.42".parse().expect("ip");
        assert_eq!(ip_scope(ip), "ip:203.0.113.42");
        assert_eq!(subnet_scope(ip), "net:203.0.113.0/24");
        let v6: IpAddr = "2001:db8:1234:5678::1".parse().expect("ip");
        assert_eq!(subnet_scope(v6), "net:2001:db8:1234::/48");
    }

    #[test]
    fn a_window_stops_at_its_limit() {
        let db = connection();
        let window = window_start(NOW, DAY);
        db.with(|connection| {
            assert!(take(connection, "ip:x", window, 2)?);
            assert!(take(connection, "ip:x", window, 2)?);
            assert!(!take(connection, "ip:x", window, 2)?);
            assert_eq!(used(connection, "ip:x", window)?, 2);
            Ok(())
        })
        .expect("limits");
    }

    #[test]
    fn a_new_window_starts_empty() {
        let db = connection();
        db.with(|connection| {
            let first = window_start(NOW, DAY);
            assert!(take(connection, "ip:x", first, 1)?);
            assert!(!take(connection, "ip:x", first, 1)?);
            let next = window_start(NOW + DAY, DAY);
            assert!(take(connection, "ip:x", next, 1)?);
            Ok(())
        })
        .expect("limits");
    }

    #[test]
    fn the_proof_of_work_rises_with_a_busy_subnet() {
        let db = connection();
        let ip: IpAddr = "203.0.113.42".parse().expect("ip");
        let window = window_start(NOW, DAY);
        let subnet = subnet_scope(ip);
        db.with(|connection| {
            assert_eq!(
                required_work_bits(connection, ip, NOW, DEFAULT_WORK_BITS)?,
                DEFAULT_WORK_BITS
            );
            // Three claims from the subnet: four more bits.
            for _ in 0..3 {
                take(connection, &subnet, window, CLAIMS_PER_SUBNET)?;
            }
            assert_eq!(
                required_work_bits(connection, ip, NOW, DEFAULT_WORK_BITS)?,
                DEFAULT_WORK_BITS + BUSY_SUBNET_BITS
            );
            // Two more: eight more bits.
            for _ in 0..2 {
                take(connection, &subnet, window, CLAIMS_PER_SUBNET)?;
            }
            assert_eq!(
                required_work_bits(connection, ip, NOW, DEFAULT_WORK_BITS)?,
                DEFAULT_WORK_BITS + HECTIC_SUBNET_BITS
            );
            Ok(())
        })
        .expect("work");
    }

    #[test]
    fn the_sweep_keeps_a_thirty_day_window_for_all_thirty_days() {
        let db = connection();
        db.with(|connection| {
            let start = window_start(NOW, THIRTY_DAYS);
            let scope = claim_scope("abcd");
            assert!(take(connection, &scope, start, CLAIMS_PER_IDENTITY)?);
            // Day 8 through day 30 of the window: the allowance is still spent.
            for day in [8, 15, 29] {
                sweep(connection, start + day * DAY)?;
                assert_eq!(used(connection, &scope, start)?, 1, "day {day}");
                assert!(
                    !take(connection, &scope, start, CLAIMS_PER_IDENTITY)?,
                    "day {day}: a second claim in the same thirty days"
                );
            }
            // A week after the window ends, the row goes.
            sweep(connection, start + THIRTY_DAYS + LIMIT_RETENTION + 1)?;
            assert_eq!(used(connection, &scope, start)?, 0);
            Ok(())
        })
        .expect("sweep");
        // The hour windows go a week after their hour, and a scope this sweep
        // does not know is kept as long as the longest window.
        db.with(|connection| {
            let hour = window_start(NOW, HOUR);
            for scope in ["mailnet:net:203.0.113.0/24", "mailall:", "mail:abcd"] {
                take(connection, scope, hour, 10)?;
            }
            take(connection, "something-new:x", hour, 10)?;
            sweep(connection, hour + HOUR + LIMIT_RETENTION + 1)?;
            for scope in ["mailnet:net:203.0.113.0/24", "mailall:", "mail:abcd"] {
                assert_eq!(used(connection, scope, hour)?, 0, "{scope}");
            }
            assert_eq!(used(connection, "something-new:x", hour)?, 1);
            Ok(())
        })
        .expect("sweep");
        // An identity's daily mail count is a day's window: kept through its
        // day and the week after, not swept as an hour.
        db.with(|connection| {
            let day = window_start(NOW, DAY);
            take(connection, "mailid:abcd", day, 3)?;
            sweep(connection, day + HOUR + LIMIT_RETENTION + 1)?;
            assert_eq!(used(connection, "mailid:abcd", day)?, 1);
            sweep(connection, day + DAY + LIMIT_RETENTION + 1)?;
            assert_eq!(used(connection, "mailid:abcd", day)?, 0);
            Ok(())
        })
        .expect("sweep");
    }

    #[test]
    fn the_sweep_drops_old_rows_and_keeps_current_ones() {
        let db = connection();
        db.with(|connection| {
            let old = window_start(NOW - 30 * DAY, DAY);
            let current = window_start(NOW, DAY);
            take(connection, "ip:old", old, 1)?;
            take(connection, "ip:current", current, 1)?;
            let removed = sweep(connection, NOW)?;
            assert_eq!(removed, 1);
            assert_eq!(used(connection, "ip:old", old)?, 0);
            assert_eq!(used(connection, "ip:current", current)?, 1);
            Ok(())
        })
        .expect("sweep");
    }
}
