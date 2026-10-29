// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The registry's memory of the keys a rotation retired (review A3).
//!
//! # Why the registry has to remember
//!
//! The registry keeps no key store. A request's key is shown to be its
//! identity's by the request itself: the root, whose `BLAKE3` is the UUID, or a
//! key-set proof that reaches the key from the root
//! (`core::identity::registry`). A proof can carry a rotation — "this key
//! handed the identity to its committed successor" — but a proof speaks only
//! for itself. Without a memory, a root key its owner rotated away from
//! because it was stolen would keep full authority here: the thief signs with
//! it alone, or presents a proof that does not mention the rotation, and the
//! pre-committed successor's rotation would protect nothing. The game server
//! refuses exactly this (`session`, "re-registering would hand the identity
//! straight back to the key its owner deliberately retired"); this module is
//! the registry matching it.
//!
//! So the service remembers, per identity, every key a presented rotation
//! retired and the successor it went to (`retired_keys`), and the first
//! successor commitment the root declared (`root_commitments`), and refuses:
//!
//! - a request whose **signing key**, or any key that **authorised a link** of
//!   its proof, is one it knows a rotation retired — the root-by-hash shortcut
//!   included, and so any proof older than the rotation: [`Refusal::RetiredKey`];
//! - a proof that rotates a retired key to a **different successor** than the
//!   one it went to: [`Refusal::RetiredKey`];
//! - a proof whose root declares a **different commitment** than the one it
//!   declared first, which is the thief holding the root trying to rotate it to
//!   a key of their own: [`Refusal::Commitment`].
//!
//! Each refusal is `403` with its own reason and an audit row.
//!
//! # A device does not survive its authoriser's rotation
//!
//! A device key that a since-retired key authorised is refused on that
//! authorisation: the retired key is in its proof's chain of authority. It
//! works again once the successor re-authorises it, with a proof that runs
//! through the rotation (`root → rotation → successor → add-key → device`).
//! An add-key signature carries no time, and the registry has no store that
//! saw the device added, so it cannot tell a device the owner added before a
//! theft from one the thief added after it with the same key. The new
//! controlling key re-authorising what it keeps is the KERI answer, and it is
//! also the only way a stolen device key is ever cut off here.
//!
//! # When it is written
//!
//! [`admit`] runs before a request's own work, in its own transaction, and
//! records what the request's proof showed **when the registry keeps anything
//! of that identity** — so a request refused for any other reason (a taken
//! name, a spent allowance, a late commit) still shuts out the keys it retired.
//! For an identity it keeps nothing of, nothing is written until a request of
//! theirs is accepted: the claim, the address bind and the recovery start call
//! [`record`] inside their own transactions. A stranger's request has nothing
//! here to protect, and a memory any fresh key could write to would be a free
//! table for anyone with a key.
//!
//! # Bounded
//!
//! Every proof starts at the root and carries at most `MAX_KEY_SET_LINKS`
//! links, so an identity can retire at most seven controlling keys in its
//! life, and a retired key has exactly one successor. Devices can be added and
//! rotated without end, so a device rotation is refused once an identity's
//! memory holds [`MAX_RETIRED_KEYS`] keys; a refusal is audited once a day per
//! identity ([`REFUSAL_AUDITS_PER_DAY`]), however often a thief retries.
//!
//! # Kept
//!
//! As long as the identity has any other row the service keeps, because that
//! row is what the memory protects; the retention sweep removes it with the
//! identity's last ([`sweep`]).

use std::collections::BTreeMap;
use std::net::IpAddr;

use rusqlite::{Connection, OptionalExtension, params};
use tiamat_core::identity::PlayerUuid;
use tiamat_core::identity::registry::KeyLineage;

use crate::db::write_transaction;
use crate::deadline::{Deadline, commit};
use crate::limits::{self, DAY};
use crate::names::audit;

/// Keys one identity's memory may hold before it learns no more device
/// rotations.
///
/// Sixty-four is a lifetime of replaced phones for a game identity many times
/// over. A controlling key's rotation is always learned whatever the count:
/// there can be at most seven of those, and refusing one would leave a stolen
/// root with its authority.
pub const MAX_RETIRED_KEYS: i64 = 64;

/// Refusals audited per identity per day.
///
/// The audit table is kept for ever (§8), so a thief retrying with a retired
/// key must not grow it with every attempt; the first refusal of the day is
/// the operator's signal, with the address it came from.
pub const REFUSAL_AUDITS_PER_DAY: i64 = 1;

/// Why the memory refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A key the request relies on was retired by a rotation the registry
    /// knows, or the request rotates a retired key to another successor.
    RetiredKey,
    /// The root declares a successor commitment other than the one it declared
    /// first.
    Commitment,
    /// A device rotation, and the identity's memory is full.
    TooManyKeys,
}

impl Refusal {
    /// The refusal's machine-readable reason, as the error body's `error`.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::RetiredKey => "retired_key",
            Self::Commitment => "commitment",
            Self::TooManyKeys => "too_many_keys",
        }
    }

    /// The sentence a player is shown.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::RetiredKey => {
                "a key this request relies on was retired by a rotation this registry knows; \
                 sign with its successor and carry the rotation in the key-set proof"
            }
            Self::Commitment => {
                "this identity's root committed to a different successor; a stolen key cannot \
                 rotate an identity away from its owner"
            }
            Self::TooManyKeys => {
                "this identity has retired as many keys as the registry keeps; ask the \
                 registry's operator"
            }
        }
    }

    /// What the audit row says.
    const fn audited(self) -> &'static str {
        match self {
            Self::RetiredKey => "refused a retired key",
            Self::Commitment => "refused a second root commitment",
            Self::TooManyKeys => "refused a device rotation over the memory's cap",
        }
    }
}

/// What [`admit`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Nothing the registry knows stands against the request; whatever it
    /// newly showed is recorded.
    Admitted,
    /// Refused, and audited.
    Refused(Refusal),
    /// The deadline passed before the commit; nothing was written.
    Late,
}

/// One identity's memory, as read.
struct Memory {
    /// Retired key → the successor it went to.
    retired: BTreeMap<[u8; 32], [u8; 32]>,
    /// The root's first declared commitment.
    commitment: Option<[u8; 32]>,
}

/// Checks a verified request's lineage against what the registry knows of
/// `uuid`, refuses or records, and commits on its own.
///
/// `signed_at` is the request's own (verified) timestamp, kept as the time a
/// newly learned key was retired: the rotation link itself carries none, and
/// the request is the nearest signed statement of when.
///
/// The writes here are the registry learning what the request proved, not the
/// change the request asked for, so they commit in a transaction of their
/// own: a request refused afterwards for another reason has still shut out
/// the keys it retired, and its retry finds them recorded and records nothing.
///
/// # Errors
///
/// Any SQL failure.
pub fn admit(
    connection: &mut Connection,
    uuid: &PlayerUuid,
    lineage: &KeyLineage,
    signed_at: i64,
    now: i64,
    client: Option<IpAddr>,
    deadline: &Deadline,
) -> rusqlite::Result<Admission> {
    let transaction = write_transaction(connection)?;
    let memory = load(&transaction, uuid)?;
    if let Some(refusal) = check(&memory, lineage) {
        if limits::take(
            &transaction,
            &refusal_scope(uuid),
            limits::window_start(now, DAY),
            REFUSAL_AUDITS_PER_DAY,
        )? {
            audit(
                &transaction,
                &uuid.to_hex(),
                refusal.audited(),
                now,
                "key memory",
                client,
            )?;
        }
        if !commit(transaction, deadline)? {
            return Ok(Admission::Late);
        }
        return Ok(Admission::Refused(refusal));
    }
    if keeps_anything_of(&transaction, uuid)?
        && record(&transaction, uuid, lineage, signed_at, now, client)? > 0
        && !commit(transaction, deadline)?
    {
        return Ok(Admission::Late);
    }
    Ok(Admission::Admitted)
}

/// Records what `lineage` showed of `uuid`: each retired key with its
/// successor, and the root's commitment, where they are new. Returns how many
/// rows it wrote.
///
/// Call it **inside** the transaction of an accepted request that gives the
/// identity a row (a claim, an address bind, a recovery start), after
/// [`admit`] has passed the request: that is how a stranger's first accepted
/// request is remembered, as the module documentation explains. Idempotent:
/// a row already held is left as it is, so its first `retired_at` stands.
///
/// # Errors
///
/// Any SQL failure.
pub fn record(
    connection: &Connection,
    uuid: &PlayerUuid,
    lineage: &KeyLineage,
    signed_at: i64,
    now: i64,
    client: Option<IpAddr>,
) -> rusqlite::Result<usize> {
    let who = uuid.to_hex();
    let mut written = 0;
    for retired in lineage.retired() {
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO retired_keys (uuid, key, successor, retired_at, learned_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid.as_bytes().as_slice(),
                retired.key.as_bytes().as_slice(),
                retired.successor.as_bytes().as_slice(),
                signed_at,
                now
            ],
        )?;
        if inserted > 0 {
            written += inserted;
            audit(
                connection,
                &who,
                &format!(
                    "retire key {} for {}",
                    short(retired.key.as_bytes()),
                    short(retired.successor.as_bytes())
                ),
                now,
                "rotation",
                client,
            )?;
        }
    }
    if let Some(commitment) = lineage.root_commitment() {
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO root_commitments (uuid, commitment, learned_at)
             VALUES (?1, ?2, ?3)",
            params![uuid.as_bytes().as_slice(), commitment.as_slice(), now],
        )?;
        if inserted > 0 {
            written += inserted;
            audit(
                connection,
                &who,
                &format!("root commits to {}", short(&commitment)),
                now,
                "rotation",
                client,
            )?;
        }
    }
    Ok(written)
}

/// Whether the registry keeps any row of `uuid`'s: a name (held, released or
/// lapsed), an address, a link in flight, a recovery from or to it, an
/// operator's revocation of it, a certificate serial, or its own memory.
///
/// Audit rows and limit counters do not count: they are the operator's and
/// the rate limits' records, not the identity's.
///
/// # Errors
///
/// Any SQL failure.
pub fn keeps_anything_of(connection: &Connection, uuid: &PlayerUuid) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM names WHERE uuid = ?1)
             OR EXISTS (SELECT 1 FROM emails WHERE uuid = ?1)
             OR EXISTS (SELECT 1 FROM tokens WHERE uuid = ?1)
             OR EXISTS (SELECT 1 FROM recoveries WHERE from_uuid = ?1 OR to_uuid = ?1)
             OR EXISTS (SELECT 1 FROM revoked_holders WHERE uuid = ?1)
             OR EXISTS (SELECT 1 FROM issued WHERE uuid = ?1)
             OR EXISTS (SELECT 1 FROM retired_keys WHERE uuid = ?1)
             OR EXISTS (SELECT 1 FROM root_commitments WHERE uuid = ?1)",
        [uuid.as_bytes().as_slice()],
        |row| row.get(0),
    )
}

/// Whether the registry knows `key` was retired from `uuid`: what the tests
/// and the stress harness ask before they expect a refusal.
///
/// # Errors
///
/// Any SQL failure.
#[cfg(test)]
pub fn is_retired(
    connection: &Connection,
    uuid: &PlayerUuid,
    key: &[u8; 32],
) -> rusqlite::Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM retired_keys WHERE uuid = ?1 AND key = ?2",
            params![uuid.as_bytes().as_slice(), key.as_slice()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Deletes the memory of every identity the registry keeps nothing else of,
/// and says how many rows of each table went.
///
/// Run by the retention sweep **after** its other deletions, in the same
/// transaction, so "the identity's last row" means what is left once this
/// sweep is done.
///
/// # Errors
///
/// Any SQL failure.
pub fn sweep(connection: &Connection) -> rusqlite::Result<(usize, usize)> {
    let keys = connection.execute(&delete_orphans("retired_keys"), [])?;
    let commitments = connection.execute(&delete_orphans("root_commitments"), [])?;
    Ok((keys, commitments))
}

/// The statement that deletes `table`'s rows whose identity the registry keeps
/// nothing else of. `table` is one of this module's two, never input.
fn delete_orphans(table: &str) -> String {
    format!(
        "DELETE FROM {table} WHERE
             NOT EXISTS (SELECT 1 FROM names n WHERE n.uuid = {table}.uuid)
         AND NOT EXISTS (SELECT 1 FROM emails e WHERE e.uuid = {table}.uuid)
         AND NOT EXISTS (SELECT 1 FROM tokens k WHERE k.uuid = {table}.uuid)
         AND NOT EXISTS (SELECT 1 FROM recoveries r
                         WHERE r.from_uuid = {table}.uuid OR r.to_uuid = {table}.uuid)
         AND NOT EXISTS (SELECT 1 FROM revoked_holders h WHERE h.uuid = {table}.uuid)
         AND NOT EXISTS (SELECT 1 FROM issued i WHERE i.uuid = {table}.uuid)"
    )
}

/// Reads `uuid`'s memory.
fn load(connection: &Connection, uuid: &PlayerUuid) -> rusqlite::Result<Memory> {
    let mut statement =
        connection.prepare_cached("SELECT key, successor FROM retired_keys WHERE uuid = ?1")?;
    let rows = statement.query_map([uuid.as_bytes().as_slice()], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut retired = BTreeMap::new();
    for row in rows {
        let (key, successor) = row?;
        // Written by this service, always 32 bytes. A row that is not is kept
        // as a key with no successor any rotation could match, which refuses
        // more rather than less.
        if let Ok(key) = <[u8; 32]>::try_from(key.as_slice()) {
            retired.insert(
                key,
                <[u8; 32]>::try_from(successor.as_slice()).unwrap_or([0; 32]),
            );
        }
    }
    let commitment = connection
        .query_row(
            "SELECT commitment FROM root_commitments WHERE uuid = ?1",
            [uuid.as_bytes().as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .map(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).unwrap_or([0; 32]));
    Ok(Memory {
        retired,
        commitment,
    })
}

/// The first rule the lineage breaks against the memory, if any.
fn check(memory: &Memory, lineage: &KeyLineage) -> Option<Refusal> {
    if lineage
        .authorities()
        .iter()
        .any(|key| memory.retired.contains_key(key.as_bytes()))
    {
        return Some(Refusal::RetiredKey);
    }
    // Before the second-rotation rule, which a thief's rotation of the root
    // also breaks once the owner's is known: this one names the reason.
    if let (Some(declared), Some(first)) = (lineage.root_commitment(), memory.commitment)
        && declared != first
    {
        return Some(Refusal::Commitment);
    }
    if lineage.retired().iter().any(|retired| {
        memory
            .retired
            .get(retired.key.as_bytes())
            .is_some_and(|successor| successor != retired.successor.as_bytes())
    }) {
        return Some(Refusal::RetiredKey);
    }
    let new_devices = lineage
        .retired()
        .iter()
        .filter(|retired| {
            !retired.controlling && !memory.retired.contains_key(retired.key.as_bytes())
        })
        .count();
    if new_devices > 0
        && i64::try_from(memory.retired.len() + new_devices).unwrap_or(i64::MAX) > MAX_RETIRED_KEYS
    {
        return Some(Refusal::TooManyKeys);
    }
    None
}

/// The limit scope that throttles one identity's refusal audits.
fn refusal_scope(uuid: &PlayerUuid) -> String {
    format!("retired:{}", uuid.to_hex())
}

/// The first eight bytes of a key or hash, as hex, for an audit line.
fn short(bytes: &[u8; 32]) -> String {
    crate::hex::encode(&bytes[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use tiamat_core::identity::Identity;
    use tiamat_core::identity::keyset::{
        add_key_payload, commit_to, root_commitment_payload, rotate_key_payload,
    };
    use tiamat_core::identity::registry::{
        KEY_SET_PROOF_VERSION, KeyAuthorisation, KeySetProof, LinkKind, Signature64,
        verify_key_set_proof,
    };

    const NOW: i64 = 1_800_000_000;

    fn player(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32])
    }

    /// The root declares `successor` and rotates to it, which commits to `next`.
    fn rotated(root: &Identity, successor: &Identity, next: Option<&Identity>) -> KeyLineage {
        let uuid = root.uuid_as_root();
        let commitment = commit_to(&successor.public_key());
        let next = next.map(|next| commit_to(&next.public_key()));
        let proof = KeySetProof {
            version: KEY_SET_PROOF_VERSION,
            root: root.public_key().to_bytes(),
            chain: vec![
                KeyAuthorisation {
                    kind: LinkKind::RootCommitment,
                    key: root.public_key().to_bytes(),
                    next_key_hash: Some(commitment),
                    added_by: root.public_key().to_bytes(),
                    signature: Signature64(
                        root.sign(&root_commitment_payload(&uuid, &commitment))
                            .to_bytes(),
                    ),
                },
                KeyAuthorisation {
                    kind: LinkKind::Rotation,
                    key: successor.public_key().to_bytes(),
                    next_key_hash: next,
                    added_by: root.public_key().to_bytes(),
                    signature: Signature64(
                        root.sign(&rotate_key_payload(
                            &uuid,
                            &successor.public_key(),
                            next.as_ref(),
                        ))
                        .to_bytes(),
                    ),
                },
            ],
        };
        verify_key_set_proof(&proof, &uuid, &successor.public_key()).expect("a rotation")
    }

    /// `root` added `device` (committed to `device_next`), and `device` rotated.
    fn device_rotated(root: &Identity, device: &Identity, device_next: &Identity) -> KeyLineage {
        let uuid = root.uuid_as_root();
        let next = commit_to(&device_next.public_key());
        let proof = KeySetProof {
            version: KEY_SET_PROOF_VERSION,
            root: root.public_key().to_bytes(),
            chain: vec![
                KeyAuthorisation {
                    kind: LinkKind::AddKey,
                    key: device.public_key().to_bytes(),
                    next_key_hash: Some(next),
                    added_by: root.public_key().to_bytes(),
                    signature: Signature64(
                        root.sign(&add_key_payload(&uuid, &device.public_key(), Some(&next)))
                            .to_bytes(),
                    ),
                },
                KeyAuthorisation {
                    kind: LinkKind::Rotation,
                    key: device_next.public_key().to_bytes(),
                    next_key_hash: None,
                    added_by: device.public_key().to_bytes(),
                    signature: Signature64(
                        device
                            .sign(&rotate_key_payload(&uuid, &device_next.public_key(), None))
                            .to_bytes(),
                    ),
                },
            ],
        };
        verify_key_set_proof(&proof, &uuid, &device_next.public_key()).expect("a device rotation")
    }

    /// Gives `uuid` a row the registry keeps, so its memory is written.
    fn known(db: &Db, uuid: &PlayerUuid) {
        db.with(|connection| {
            connection.execute(
                "INSERT INTO emails (uuid, address, verified_at) VALUES (?1, 'a@example.com', 1)",
                [uuid.as_bytes().as_slice()],
            )?;
            Ok(())
        })
        .expect("seed");
    }

    fn admit_at(db: &Db, uuid: &PlayerUuid, lineage: &KeyLineage) -> Admission {
        db.with(|connection| admit(connection, uuid, lineage, NOW, NOW, None, &Deadline::none()))
            .expect("admit")
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|connection| connection.query_row(sql, [], |row| row.get(0)))
            .expect("count")
    }

    #[test]
    fn a_known_identity_s_rotation_is_remembered_and_its_old_key_refused() {
        let db = Db::in_memory().expect("db");
        let root = player(1);
        let successor = player(2);
        let uuid = root.uuid_as_root();
        known(&db, &uuid);

        assert_eq!(
            admit_at(&db, &uuid, &rotated(&root, &successor, None)),
            Admission::Admitted
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM retired_keys"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM root_commitments"), 1);
        db.with(|connection| is_retired(connection, &uuid, root.public_key().as_bytes()))
            .map(|retired| assert!(retired))
            .expect("read");

        // The root alone — the root-by-hash shortcut — is refused now.
        assert_eq!(
            admit_at(&db, &uuid, &KeyLineage::of_root(root.public_key())),
            Admission::Refused(Refusal::RetiredKey)
        );
        // The rotation re-presented is fine, and records nothing new.
        assert_eq!(
            admit_at(&db, &uuid, &rotated(&root, &successor, None)),
            Admission::Admitted
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM retired_keys"), 1);
        // A thief's second rotation of the root, to their own key, with their
        // own declaration: the declaration is not the first.
        let mallory = player(3);
        assert_eq!(
            admit_at(&db, &uuid, &rotated(&root, &mallory, None)),
            Admission::Refused(Refusal::Commitment)
        );
    }

    #[test]
    fn a_stranger_s_request_writes_nothing_until_it_is_accepted() {
        let db = Db::in_memory().expect("db");
        let root = player(1);
        let successor = player(2);
        let uuid = root.uuid_as_root();
        let lineage = rotated(&root, &successor, None);
        assert_eq!(admit_at(&db, &uuid, &lineage), Admission::Admitted);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM retired_keys"), 0);
        // The accepted request records inside its own transaction.
        db.with(|connection| record(connection, &uuid, &lineage, NOW, NOW, None))
            .expect("record");
        assert_eq!(count(&db, "SELECT COUNT(*) FROM retired_keys"), 1);
        assert_eq!(
            admit_at(&db, &uuid, &KeyLineage::of_root(root.public_key())),
            Admission::Refused(Refusal::RetiredKey)
        );
    }

    #[test]
    fn a_refusal_is_audited_once_a_day_however_often_it_is_tried() {
        let db = Db::in_memory().expect("db");
        let root = player(1);
        let uuid = root.uuid_as_root();
        known(&db, &uuid);
        admit_at(&db, &uuid, &rotated(&root, &player(2), None));
        let before = count(&db, "SELECT COUNT(*) FROM audit");
        for _ in 0..20 {
            assert_eq!(
                admit_at(&db, &uuid, &KeyLineage::of_root(root.public_key())),
                Admission::Refused(Refusal::RetiredKey)
            );
        }
        assert_eq!(count(&db, "SELECT COUNT(*) FROM audit"), before + 1);
    }

    #[test]
    fn device_rotations_stop_at_the_cap_and_controlling_ones_never_do() {
        let db = Db::in_memory().expect("db");
        let root = player(1);
        let uuid = root.uuid_as_root();
        known(&db, &uuid);
        // Fill the memory with device rotations, written directly: the cap is
        // a count, and minting sixty-four proofs proves nothing more.
        db.with(|connection| {
            for n in 0..MAX_RETIRED_KEYS {
                connection.execute(
                    "INSERT INTO retired_keys (uuid, key, successor, retired_at, learned_at)
                     VALUES (?1, ?2, ?3, 1, 1)",
                    params![
                        uuid.as_bytes().as_slice(),
                        blake3::hash(&n.to_le_bytes()).as_bytes().as_slice(),
                        [0u8; 32].as_slice()
                    ],
                )?;
            }
            Ok(())
        })
        .expect("fill");
        assert_eq!(
            admit_at(&db, &uuid, &device_rotated(&root, &player(4), &player(5))),
            Admission::Refused(Refusal::TooManyKeys)
        );
        // The root's own rotation is still learned.
        assert_eq!(
            admit_at(&db, &uuid, &rotated(&root, &player(2), None)),
            Admission::Admitted
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM retired_keys"),
            MAX_RETIRED_KEYS + 1
        );
    }

    #[test]
    fn the_memory_goes_with_the_identity_s_last_row() {
        let db = Db::in_memory().expect("db");
        let root = player(1);
        let uuid = root.uuid_as_root();
        known(&db, &uuid);
        admit_at(&db, &uuid, &rotated(&root, &player(2), None));
        assert_eq!(
            db.with(|connection| sweep(connection)).expect("sweep"),
            (0, 0)
        );
        db.with(|connection| {
            connection.execute("DELETE FROM emails", [])?;
            Ok(())
        })
        .expect("forget");
        assert_eq!(
            db.with(|connection| sweep(connection)).expect("sweep"),
            (1, 1)
        );
        assert!(
            !db.with(|connection| keeps_anything_of(connection, &uuid))
                .expect("read")
        );
    }
}
