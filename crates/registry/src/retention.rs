// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The retention sweep: what §8 of the design promises to forget, forgotten.
//!
//! §8 is a promise to players about how long the registry keeps what they
//! send. Before this module it was a table in a document (review B9): IP
//! addresses sat in the audit log for ever, expired links were never deleted,
//! the mail quota keyed raw addresses, and released names and finished
//! recoveries were never pruned. The sweep runs at startup and then at most
//! hourly from [`crate::service::Service::advance`], beside the limit sweep,
//! in one transaction:
//!
//! | what | kept | constant |
//! |---|---|---|
//! | a link (verification or recovery token) | until it expires (a day) | [`crate::tokens::TOKEN_LIFETIME_SECONDS`] |
//! | the IP address on an audit row | seven days | [`IP_RETENTION`] |
//! | a released (or lapsed) name's row | thirty days after its hold ends | [`RELEASED_RETENTION`] |
//! | a recovery record | ninety days after it ends | [`RECOVERY_RETENTION`] |
//! | a recovery address whose identity holds no name | thirty days | [`NAMELESS_ADDRESS_GRACE`] |
//! | a certificate serial | while the certificate can verify | [`crate::names::CERTIFICATE_LIVE_SECONDS`] |
//! | a limit counter | a week after its window | [`crate::limits::LIMIT_RETENTION`] |
//!
//! The audit row itself is kept, as §8 says, without its address. Everything
//! is measured from the row's own time, so a sweep that runs late deletes more
//! and one that runs early deletes nothing it should keep.

use rusqlite::{Connection, params};

use crate::db::write_transaction;
use crate::limits::DAY;
use crate::names::{
    CERTIFICATE_LIVE_SECONDS, EXPIRY_AFTER_RENEWAL, EXPIRY_NEVER_RENEWED, HOLD_AFTER_RELEASE,
};

/// How long an audit row keeps the client address it was written with.
pub const IP_RETENTION: i64 = 7 * DAY;

/// How long a released name's row outlives its thirty-day hold.
///
/// The hold is the old holder's (a release is undoable for thirty days); the
/// thirty after it are the operator's, for a "who had this name" question
/// asked soon after it changed hands. After that the row goes and the name
/// keeps no memory of its old holder.
pub const RELEASED_RETENTION: i64 = 30 * DAY;

/// How long a recovery record outlives its end (moved or cancelled).
pub const RECOVERY_RETENTION: i64 = 90 * DAY;

/// How long a verified address is kept for an identity that holds no name.
///
/// §8 keeps an address "while bound; removed … with the identity's last
/// name". An address is bound before the name is claimed often enough (a
/// client that sets up recovery first) that deleting it the moment no name row
/// exists would undo the player's own order of steps; thirty days is the grace.
pub const NAMELESS_ADDRESS_GRACE: i64 = 30 * DAY;

/// What one sweep removed, by table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    /// Expired links deleted.
    pub tokens: usize,
    /// Audit rows whose address was cleared.
    pub addresses: usize,
    /// Released or lapsed name rows deleted.
    pub names: usize,
    /// Recovery records deleted.
    pub recoveries: usize,
    /// Recovery addresses deleted with their identity's last name.
    pub emails: usize,
    /// Certificate serials that can no longer verify, deleted.
    pub issued: usize,
    /// Revocations of certificates that can no longer verify, deleted.
    pub revocations: usize,
    /// Spent limit counters deleted.
    pub limits: usize,
}

/// Runs every retention rule at `now`, in one transaction.
///
/// # Errors
///
/// Any SQL failure; the transaction is then rolled back whole.
pub fn sweep(connection: &mut Connection, now: i64) -> rusqlite::Result<Swept> {
    let transaction = write_transaction(connection)?;
    let mut swept = Swept {
        tokens: transaction.execute("DELETE FROM tokens WHERE expires <= ?1", [now])?,
        addresses: transaction.execute(
            "UPDATE audit SET ip = NULL WHERE ip IS NOT NULL AND when_at <= ?1",
            [now - IP_RETENTION],
        )?,
        ..Swept::default()
    };

    // A name's row is deleted thirty days after it became anyone's to claim:
    // a released name's hold end, or a lapsed name's expiry plus the same
    // hold. `free_at` in `names` is the same arithmetic in Rust.
    swept.names = transaction.execute(
        "DELETE FROM names WHERE
             CASE
                 WHEN released IS NOT NULL THEN COALESCE(hold_until, released + ?2)
                 WHEN renewals > 0 THEN renewed + ?3 + ?2
                 ELSE claimed + ?4 + ?2
             END + ?5 <= ?1",
        params![
            now,
            HOLD_AFTER_RELEASE,
            EXPIRY_AFTER_RENEWAL,
            EXPIRY_NEVER_RENEWED,
            RELEASED_RETENTION
        ],
    )?;

    // `ended_at` is written when a recovery is moved or cancelled; a record
    // from before it existed (schema 3) falls back to the notice's end, which
    // is never earlier than the real end, so such a record is kept a little
    // longer rather than deleted early.
    swept.recoveries = transaction.execute(
        "DELETE FROM recoveries
         WHERE state != 'pending' AND COALESCE(ended_at, notice_until) + ?2 <= ?1",
        params![now, RECOVERY_RETENTION],
    )?;

    swept.emails = transaction.execute(
        "DELETE FROM emails
         WHERE verified_at + ?2 <= ?1
           AND NOT EXISTS (SELECT 1 FROM names n WHERE n.uuid = emails.uuid)",
        params![now, NAMELESS_ADDRESS_GRACE],
    )?;

    // Revocations first: `prune_revocations` finds the dead ones through their
    // `issued` row, and a revocation whose row is gone is treated as one whose
    // expiry is unknown and published for ever.
    swept.revocations = crate::names::prune_revocations(&transaction, now)?;
    swept.issued = transaction.execute(
        "DELETE FROM issued WHERE issued <= ?1",
        [now - CERTIFICATE_LIVE_SECONDS],
    )?;

    swept.limits = crate::limits::sweep(&transaction, now)?;
    transaction.commit()?;
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::limits::HOUR;

    const NOW: i64 = 1_800_000_000;

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|connection| connection.query_row(sql, [], |row| row.get(0)))
            .expect("count")
    }

    fn sweep_at(db: &Db, now: i64) -> Swept {
        db.with(|connection| sweep(connection, now)).expect("sweep")
    }

    #[test]
    fn an_expired_link_is_deleted_and_a_live_one_kept() {
        let db = Db::in_memory().expect("db");
        db.with(|connection| {
            for (hash, expires) in [(1u8, NOW - 1), (2, NOW + HOUR)] {
                connection.execute(
                    "INSERT INTO tokens (hash, purpose, uuid, address, expires, created)
                     VALUES (?1, 'email', X'00', 'a@example.com', ?2, ?3)",
                    params![vec![hash; 32], expires, expires - DAY],
                )?;
            }
            Ok(())
        })
        .expect("seed");
        assert_eq!(sweep_at(&db, NOW).tokens, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM tokens"), 1);
    }

    #[test]
    fn an_audit_row_keeps_its_address_for_seven_days_and_itself_for_ever() {
        let db = Db::in_memory().expect("db");
        db.with(|connection| {
            for (at, ip) in [
                (NOW - IP_RETENTION - 1, "203.0.113.1"),
                (NOW - DAY, "203.0.113.2"),
            ] {
                connection.execute(
                    "INSERT INTO audit (who, what, when_at, why, ip)
                     VALUES ('x', 'claim alice', ?1, 'claim', ?2)",
                    params![at, ip],
                )?;
            }
            Ok(())
        })
        .expect("seed");
        assert_eq!(sweep_at(&db, NOW).addresses, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM audit"), 2);
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM audit WHERE ip IS NOT NULL"),
            1
        );
        // Nothing in the row's other columns is an address either.
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM audit WHERE why LIKE '%203.0.113%'"
            ),
            0
        );
    }

    #[test]
    fn released_and_lapsed_names_go_thirty_days_after_their_hold() {
        let db = Db::in_memory().expect("db");
        let held_released = NOW - HOLD_AFTER_RELEASE - RELEASED_RETENTION;
        db.with(|connection| {
            let insert = |canonical: &str,
                          uuid: u8,
                          claimed: i64,
                          renewed: i64,
                          renewals: i64,
                          released: Option<i64>|
             -> rusqlite::Result<usize> {
                connection.execute(
                    "INSERT INTO names
                         (canonical, display, uuid, claimed, renewed, released, hold_until, renewals)
                     VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        canonical,
                        vec![uuid; 32],
                        claimed,
                        renewed,
                        released,
                        released.map(|at| at + HOLD_AFTER_RELEASE),
                        renewals
                    ],
                )
            };
            // Released long enough ago: its hold and the thirty after are over.
            insert("gone", 1, NOW - 400 * DAY, NOW - 400 * DAY, 0, Some(held_released))?;
            // Released a second later: one second of retention left.
            insert("kept", 2, NOW - 400 * DAY, NOW - 400 * DAY, 0, Some(held_released + 1))?;
            // Never renewed, claimed long ago: lapsed, held, and past retention.
            let lapsed = NOW - EXPIRY_NEVER_RENEWED - HOLD_AFTER_RELEASE - RELEASED_RETENTION;
            insert("lapsed", 3, lapsed, lapsed, 0, None)?;
            // Renewed a year ago: still the holder's, whatever its claim date.
            insert("renewed", 4, NOW - 900 * DAY, NOW - 300 * DAY, 3, None)?;
            // Held today.
            insert("held", 5, NOW - DAY, NOW - DAY, 0, None)?;
            Ok(())
        })
        .expect("seed");
        assert_eq!(sweep_at(&db, NOW).names, 2);
        let left: Vec<String> = db
            .with(|connection| {
                let mut statement =
                    connection.prepare("SELECT canonical FROM names ORDER BY canonical")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            })
            .expect("names");
        assert_eq!(left, vec!["held", "kept", "renewed"]);
    }

    #[test]
    fn a_finished_recovery_is_kept_ninety_days_and_a_running_one_always() {
        let db = Db::in_memory().expect("db");
        db.with(|connection| {
            for (canonical, state, ended, notice) in [
                ("moved", "moved", Some(NOW - RECOVERY_RETENTION), NOW - RECOVERY_RETENTION),
                ("cancelled", "cancelled", Some(NOW - DAY), NOW + 13 * DAY),
                // A schema-3 record with no end time: judged by its notice.
                ("legacy", "cancelled", None, NOW - RECOVERY_RETENTION - 1),
                ("pending", "pending", None, NOW - 500 * DAY),
            ] {
                connection.execute(
                    "INSERT INTO recoveries
                         (canonical, from_uuid, to_uuid, confirmed_at, notice_until, state, ended_at)
                     VALUES (?1, X'01', X'02', ?2, ?3, ?4, ?5)",
                    params![canonical, notice - 14 * DAY, notice, state, ended],
                )?;
            }
            Ok(())
        })
        .expect("seed");
        assert_eq!(sweep_at(&db, NOW).recoveries, 2);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM recoveries"), 2);
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM recoveries WHERE canonical IN ('cancelled', 'pending')"
            ),
            2
        );
    }

    #[test]
    fn an_address_goes_with_its_identity_s_last_name_after_a_grace() {
        let db = Db::in_memory().expect("db");
        db.with(|connection| {
            connection.execute(
                "INSERT INTO names (canonical, display, uuid, claimed, renewed, renewals)
                 VALUES ('alice', 'alice', ?1, ?2, ?2, 0)",
                params![vec![1u8; 32], NOW - DAY],
            )?;
            for (uuid, verified) in [
                (1u8, NOW - 400 * DAY),                     // holds a name: kept
                (2, NOW - NAMELESS_ADDRESS_GRACE),          // no name, grace over
                (3, NOW - NAMELESS_ADDRESS_GRACE + 1),      // no name, in its grace
            ] {
                connection.execute(
                    "INSERT INTO emails (uuid, address, verified_at) VALUES (?1, 'x@example.com', ?2)",
                    params![vec![uuid; 32], verified],
                )?;
            }
            Ok(())
        })
        .expect("seed");
        assert_eq!(sweep_at(&db, NOW).emails, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM emails"), 2);
    }

    #[test]
    fn a_dead_serial_and_its_revocation_go_together() {
        let db = Db::in_memory().expect("db");
        let key = [9u8; 32];
        db.with(|connection| {
            for (serial, issued) in [(1_i64, NOW - CERTIFICATE_LIVE_SECONDS), (2, NOW - DAY)] {
                connection.execute(
                    "INSERT INTO issued (serial, registry_key, uuid, canonical, issued)
                     VALUES (?1, ?2, X'01', 'alice', ?3)",
                    params![serial, key.as_slice(), issued],
                )?;
                connection.execute(
                    "INSERT INTO revocations (registry_key, serial, when_at, reason)
                     VALUES (?1, ?2, ?3, 'test')",
                    params![key.as_slice(), serial, NOW - DAY],
                )?;
            }
            Ok(())
        })
        .expect("seed");
        let swept = sweep_at(&db, NOW);
        assert_eq!((swept.issued, swept.revocations), (1, 1));
        // The live one is still published: its `issued` row is what says so.
        let published = db
            .with(|connection| crate::names::revocations_in_force(connection, NOW))
            .expect("list");
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].serial, 2);
    }
}
