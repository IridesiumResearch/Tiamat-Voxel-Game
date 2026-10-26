// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The server's view of who exists: key sets and name bindings.
//!
//! Backed by `player_keys` and `player_names` from Task 03, which is why those
//! tables were reserved then — this is the shape they were reserved for.
//!
//! # Names are display strings; UUIDs are identity
//!
//! Charter rule 13, and it is worth being explicit about what that costs. The
//! binding is first-come and one holder per name, so the *name* is a scarce
//! resource, but nothing keys on it. Inventory, ownership, bans, mod storage —
//! all UUID. A name can be released, reassigned by an admin, or changed, and
//! nothing else in the engine notices.

use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signature, VerifyingKey};

use crate::identity::{AuthorisedKey, KeyLookup, KeySet, KeySetError, PlayerUuid, RotationProof};

/// A display name bound to an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameBinding {
    /// The name, as claimed.
    pub name: String,
    /// Who holds it.
    pub uuid: PlayerUuid,
}

/// What [`IdentityRegistry::bind_certified`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertifiedBinding {
    /// The name bound — the certificate's display casing.
    pub name: String,
    /// The identity that wore the canonical name and was moved to a variant,
    /// with the variant it now wears.
    pub displaced: Option<(PlayerUuid, String)>,
}

/// Something went wrong updating the registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The name belongs to a different identity.
    #[error("the name `{name}` is held by another player")]
    NameTaken {
        /// The contested name.
        name: String,
    },

    /// No such identity.
    #[error("unknown identity")]
    UnknownIdentity,

    /// A key-set operation failed.
    #[error(transparent)]
    KeySet(#[from] KeySetError),
}

/// Every identity the server knows, and every bound name.
///
/// In-memory; the transport layer is responsible for loading it from and
/// flushing it to the world database. Kept separate from `WorldDb` so the join
/// flow can be tested without a filesystem, which is most of why the identity
/// suite runs in microseconds.
#[derive(Debug, Default)]
pub struct IdentityRegistry {
    identities: BTreeMap<PlayerUuid, KeySet>,
    /// Every authorised key, mapped to its identity.
    ///
    /// Denormalised from the key sets because it is looked up on every join and
    /// a scan over all identities would be O(players) per handshake. Rebuilt,
    /// never edited independently — the key sets are the source of truth.
    by_key: BTreeMap<[u8; 32], PlayerUuid>,
    names: BTreeMap<String, PlayerUuid>,

    /// Identities whose key sets have changed since the last flush.
    ///
    /// Tracked so a save writes what moved rather than every identity. On a
    /// long-lived server the registry holds every player who has ever joined,
    /// while a tick typically changes none of them.
    dirty_identities: BTreeSet<PlayerUuid>,
    /// Names whose binding changed since the last flush.
    ///
    /// A name in here may have been bound, rebound, or released — the flush
    /// resolves which by looking it up, so a bind-then-release between flushes
    /// collapses to one write instead of two.
    dirty_names: BTreeSet<String>,

    /// Bound names that a registry certificate issued (design §5).
    ///
    /// **Kept with the binding, not with the connection.** The online set the
    /// server keeps answers "is this player, here now, certified"; this answers
    /// "will this binding come back from a certificate at the next join", which
    /// is the question an operator's RCON `rename` has to ask of a player who
    /// is not here — renaming them would be undone when they return, and would
    /// push out whoever had taken the name meanwhile. Every name in here is
    /// bound: releasing or moving a binding takes its mark with it.
    certified: BTreeSet<String>,
    /// Names whose certified mark changed since the last flush, resolved the
    /// way `dirty_names` is: by looking the mark up at flush time.
    dirty_certified: BTreeSet<String>,
}

impl IdentityRegistry {
    /// Adds or replaces an identity, marking it for the next flush.
    pub fn insert(&mut self, keys: KeySet) {
        let uuid = keys.uuid();
        self.dirty_identities.insert(uuid);
        self.insert_clean(keys);
    }

    /// Adds or replaces an identity **without** marking it dirty.
    ///
    /// For loading from the database, where writing back what was just read
    /// would be pointless work on every startup.
    fn insert_clean(&mut self, keys: KeySet) {
        let uuid = keys.uuid();
        // Drop any stale key mappings for this identity before reindexing, so a
        // revoked key cannot linger in the lookup after a reload.
        self.by_key.retain(|_, owner| *owner != uuid);
        for entry in keys.all_keys() {
            if entry.is_active() {
                self.by_key.insert(*entry.key.as_bytes(), uuid);
            }
        }
        self.identities.insert(uuid, keys);
    }

    /// Rebuilds an identity from persisted key rows.
    ///
    /// Does not mark the identity dirty — these rows came *from* the database.
    ///
    /// # Errors
    ///
    /// [`RegistryError::KeySet`] if the rows do not describe a valid set.
    pub fn insert_stored(&mut self, keys: Vec<AuthorisedKey>) -> Result<(), RegistryError> {
        self.insert_clean(KeySet::from_stored(keys)?);
        Ok(())
    }

    /// Binds a name **without** marking it dirty, for loading.
    pub fn bind_name_clean(&mut self, name: &str, uuid: PlayerUuid) {
        self.names.insert(name.to_owned(), uuid);
    }

    /// Marks a bound name as certified **without** marking it dirty, for
    /// loading. A name nobody holds is not marked: the mark lives with a
    /// binding, and there is none.
    pub fn mark_certified_clean(&mut self, name: &str) {
        if self.names.contains_key(name) {
            self.certified.insert(name.to_owned());
        }
    }

    /// Whether the binding of `name` is one a registry certificate issued, so
    /// the identity that holds it gets it back from their certificate at the
    /// next join whatever this server does to it in the meantime.
    ///
    /// Read by RCON `rename`, which refuses such a name whether the player is
    /// here or not. Not the same question as the server's online set, which
    /// is about this join and is what `game.player_certified` reads.
    #[must_use]
    pub fn is_certified_name(&self, name: &str) -> bool {
        self.certified.contains(name)
    }

    /// Drops the certified mark from whatever name `uuid` holds: the identity
    /// joined without a certificate this server honours, so the binding it
    /// wears now is this server's, and an operator may move it.
    pub fn clear_certified(&mut self, uuid: &PlayerUuid) {
        if let Some(name) = self.name_of(uuid).map(ToOwned::to_owned) {
            self.unmark_certified(&name);
        }
    }

    /// Certified marks changed since the last [`clear_dirty`](Self::clear_dirty).
    #[must_use]
    pub fn dirty_certified(&self) -> Vec<String> {
        self.dirty_certified.iter().cloned().collect()
    }

    /// Removes a name's certified mark, recording the change for the flush.
    fn unmark_certified(&mut self, name: &str) {
        if self.certified.remove(name) {
            self.dirty_certified.insert(name.to_owned());
        }
    }

    /// Identities changed since the last [`clear_dirty`](Self::clear_dirty).
    #[must_use]
    pub fn dirty_identities(&self) -> Vec<PlayerUuid> {
        self.dirty_identities.iter().copied().collect()
    }

    /// Names changed since the last [`clear_dirty`](Self::clear_dirty).
    #[must_use]
    pub fn dirty_names(&self) -> Vec<String> {
        self.dirty_names.iter().cloned().collect()
    }

    /// Whether anything is waiting to be written.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        !self.dirty_identities.is_empty()
            || !self.dirty_names.is_empty()
            || !self.dirty_certified.is_empty()
    }

    /// Marks everything flushed.
    ///
    /// Call only after the write has actually succeeded. Clearing first and
    /// then failing to write would lose the change silently — the registry
    /// would believe the database already had it.
    pub fn clear_dirty(&mut self) {
        self.dirty_identities.clear();
        self.dirty_names.clear();
        self.dirty_certified.clear();
    }

    /// The identity a key belongs to, if it is currently authorised.
    #[must_use]
    pub fn identity_of_key(&self, key: &VerifyingKey) -> Option<PlayerUuid> {
        self.by_key.get(key.as_bytes()).copied()
    }

    /// An identity's key set.
    #[must_use]
    pub fn key_set(&self, uuid: &PlayerUuid) -> Option<&KeySet> {
        self.identities.get(uuid)
    }

    /// Whether an identity is known.
    #[must_use]
    pub fn contains(&self, uuid: &PlayerUuid) -> bool {
        self.identities.contains_key(uuid)
    }

    /// How many identities are known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.identities.len()
    }

    /// Whether no identities are known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }

    /// Who holds a display name.
    #[must_use]
    pub fn name_holder(&self, name: &str) -> Option<PlayerUuid> {
        self.names.get(name).copied()
    }

    /// The name an identity holds.
    #[must_use]
    pub fn name_of(&self, uuid: &PlayerUuid) -> Option<&str> {
        self.names
            .iter()
            .find(|(_, holder)| *holder == uuid)
            .map(|(name, _)| name.as_str())
    }

    /// Binds a name to an identity, first-come.
    ///
    /// Re-binding the same name to the same identity is a no-op, so a
    /// reconnecting player is not refused their own name.
    ///
    /// # Errors
    ///
    /// [`RegistryError::NameTaken`] if another identity holds it.
    pub fn bind_name(&mut self, name: &str, uuid: PlayerUuid) -> Result<(), RegistryError> {
        match self.names.get(name) {
            Some(holder) if *holder == uuid => Ok(()),
            Some(_) => Err(RegistryError::NameTaken {
                name: name.to_owned(),
            }),
            None => {
                // One name per identity: taking a new one releases the old,
                // otherwise a player could hoard names by reconnecting.
                if let Some(previous) = self.name_of(&uuid).map(ToOwned::to_owned) {
                    self.names.remove(&previous);
                    // A mark lives with its binding, so it goes with the name.
                    self.unmark_certified(&previous);
                    // The release is a change too. Without this the old name
                    // would stay bound in the database after a rename, and a
                    // restart would hand the player back a name they no longer
                    // hold — while also blocking anyone else from taking it.
                    self.dirty_names.insert(previous);
                }
                self.names.insert(name.to_owned(), uuid);
                self.dirty_names.insert(name.to_owned());
                Ok(())
            }
        }
    }

    /// Binds `name`, or the next free numbered variant of it — `name 2`,
    /// `name 3`, … up to `most` — and answers the name that was bound.
    ///
    /// **Why a variant rather than a refusal.** Every fresh install calls
    /// its player `player`, so two of them on one LAN were a host and a
    /// guest who could not join: the guest was refused for wearing the
    /// host's name, with nothing on the screen saying what to change
    /// (reported from the window as a crash on joining). A name is still
    /// bound to one identity first-come (charter rule 13), the identity
    /// that got the variant keeps it on every reconnect, and the caller
    /// tells the player which name they wear here.
    ///
    /// # Errors
    ///
    /// [`RegistryError::NameTaken`] if every variant up to `most` is held by
    /// somebody else, or would not fit [`crate::proto::MAX_NAME_BYTES`].
    pub fn bind_name_or_next(
        &mut self,
        name: &str,
        uuid: PlayerUuid,
        most: usize,
    ) -> Result<String, RegistryError> {
        // The identity's own earlier variant first: a guest who was
        // `player 2` yesterday is `player 2` today, not `player 3`.
        if let Some(own) = self.name_of(&uuid)
            && (own == name || is_variant_of(own, name))
        {
            let own = own.to_owned();
            self.bind_name(&own, uuid)?;
            return Ok(own);
        }
        match self.bind_name(name, uuid) {
            Ok(()) => return Ok(name.to_owned()),
            Err(RegistryError::NameTaken { .. }) => {}
            Err(err) => return Err(err),
        }
        for n in 2..=most {
            let variant = format!("{name} {n}");
            if variant.len() > crate::proto::MAX_NAME_BYTES {
                break;
            }
            match self.bind_name(&variant, uuid) {
                Ok(()) => return Ok(variant),
                Err(RegistryError::NameTaken { .. }) => {}
                Err(err) => return Err(err),
            }
        }
        Err(RegistryError::NameTaken {
            name: name.to_owned(),
        })
    }

    /// Binds a name the registry has certified to `uuid`, displacing an
    /// uncertified holder first (design §5).
    ///
    /// The **canonical** form decides whose the name is; `display` — the
    /// certificate's casing — is the string this server shows. A holder of the
    /// canonical name with another UUID is moved to a numbered variant of what
    /// they wore, and answered in [`CertifiedBinding::displaced`] so the caller
    /// can tell them. The certified identity's previous name, if any, is
    /// released: a certified player's name follows the certificate.
    ///
    /// The bound name is marked certified ([`Self::is_certified_name`]); the
    /// displaced holder's mark, if a stale certificate had given them one, goes
    /// with the name they lost, and their variant is unmarked.
    ///
    /// # Errors
    ///
    /// [`RegistryError::NameTaken`] if every variant up to `most` is held and
    /// the displaced holder could not be moved — in which case nothing has
    /// changed: the holder keeps the name and the certified identity is bound
    /// to nothing new.
    pub fn bind_certified(
        &mut self,
        display: &str,
        canonical: &str,
        uuid: PlayerUuid,
        most: usize,
    ) -> Result<CertifiedBinding, RegistryError> {
        // Whose name is this, by the rule the registry uses? A server's own
        // names have no canonical form, so the comparison is case-insensitive:
        // an uncertified `alice` wears a certified `Alice`'s name.
        let worn = self
            .names
            .iter()
            .find(|(name, holder)| **holder != uuid && name.eq_ignore_ascii_case(canonical))
            .map(|(name, holder)| (name.clone(), *holder));
        // **Nothing moves until everything can.** The displaced holder's
        // variant is chosen before a single release, so a server with every
        // variant taken refuses the certified join and leaves the holder as
        // they were — not nameless beside a bound certified name on a join
        // that is refused all the same.
        let variant = match &worn {
            Some((name, _)) => Some(self.free_variant(name, most)?),
            None => None,
        };
        if let Some((name, _)) = &worn {
            self.release_name(name);
        }
        if let Some(previous) = self.name_of(&uuid).map(ToOwned::to_owned)
            && previous != display
        {
            self.release_name(&previous);
        }
        self.bind_name(display, uuid)?;
        if self.certified.insert(display.to_owned()) {
            self.dirty_certified.insert(display.to_owned());
        }
        let displaced = match (worn, variant) {
            (Some((_, holder)), Some(variant)) => {
                self.bind_name(&variant, holder)?;
                Some((holder, variant))
            }
            _ => None,
        };
        Ok(CertifiedBinding {
            name: display.to_owned(),
            displaced,
        })
    }

    /// The first free `<name> 2`, `<name> 3`, … up to `most`, chosen without
    /// binding it.
    ///
    /// **Variants only**, unlike [`Self::bind_name_or_next`], which tries the
    /// name itself first: a displaced holder must not get the name back that a
    /// certificate just took. Chosen before anything is released so that a
    /// refusal leaves the registry untouched; a variant free now is still free
    /// after the releases that follow.
    fn free_variant(&self, name: &str, most: usize) -> Result<String, RegistryError> {
        for n in 2..=most {
            let variant = format!("{name} {n}");
            if variant.len() > crate::proto::MAX_NAME_BYTES {
                break;
            }
            if !self.names.contains_key(&variant) {
                return Ok(variant);
            }
        }
        Err(RegistryError::NameTaken {
            name: name.to_owned(),
        })
    }

    /// Releases a name, and its certified mark with it. **Admin operation**
    /// (RCON `rename`).
    pub fn release_name(&mut self, name: &str) {
        if self.names.remove(name).is_some() {
            self.dirty_names.insert(name.to_owned());
        }
        self.unmark_certified(name);
    }

    /// Every binding, in a stable order.
    #[must_use]
    pub fn bindings(&self) -> Vec<NameBinding> {
        self.names
            .iter()
            .map(|(name, uuid)| NameBinding {
                name: name.clone(),
                uuid: *uuid,
            })
            .collect()
    }

    /// Adds a key to an identity's set, authorised by an existing key.
    ///
    /// # Errors
    ///
    /// [`RegistryError`] if the identity is unknown or the key set refuses.
    pub fn add_key(
        &mut self,
        uuid: &PlayerUuid,
        signer: &VerifyingKey,
        new_key: VerifyingKey,
        next_key_hash: Option<[u8; 32]>,
        signature: &Signature,
        at: i64,
    ) -> Result<(), RegistryError> {
        let keys = self
            .identities
            .get_mut(uuid)
            .ok_or(RegistryError::UnknownIdentity)?;
        keys.add_key(signer, new_key, next_key_hash, signature, at)?;
        // Reindex from the key set rather than inserting directly — the set is
        // the source of truth and the index is derived.
        let updated = keys.clone();
        self.insert(updated);
        Ok(())
    }

    /// Rotates a key to its pre-committed successor.
    ///
    /// # Errors
    ///
    /// [`RegistryError`] if the identity is unknown or the rotation is refused.
    pub fn rotate_key(
        &mut self,
        uuid: &PlayerUuid,
        current: &VerifyingKey,
        new_key: VerifyingKey,
        new_next_key_hash: Option<[u8; 32]>,
        signature: &Signature,
        at: i64,
    ) -> Result<RotationProof, RegistryError> {
        let keys = self
            .identities
            .get_mut(uuid)
            .ok_or(RegistryError::UnknownIdentity)?;
        let proof = keys.rotate_key(current, new_key, new_next_key_hash, signature, at)?;
        let updated = keys.clone();
        self.insert(updated);
        Ok(proof)
    }

    /// Replaces an identity's root key. **Admin operation** (RCON `rebind`).
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownIdentity`] if there is no such identity.
    pub fn admin_rebind(
        &mut self,
        uuid: &PlayerUuid,
        new_root: VerifyingKey,
        at: i64,
    ) -> Result<(), RegistryError> {
        let keys = self
            .identities
            .get_mut(uuid)
            .ok_or(RegistryError::UnknownIdentity)?;
        keys.admin_rebind(new_root, at);
        let updated = keys.clone();
        self.insert(updated);
        Ok(())
    }
}

impl KeyLookup for IdentityRegistry {
    fn identity_of(&self, key: &VerifyingKey) -> Option<PlayerUuid> {
        self.identity_of_key(key)
    }
}

/// Whether `candidate` is `name` followed by a space and a number — the
/// shape [`IdentityRegistry::bind_name_or_next`] hands out.
pub(super) fn is_variant_of(candidate: &str, name: &str) -> bool {
    candidate
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Identity, keyset::add_key_payload};

    fn identity() -> Identity {
        Identity::generate().expect("generate")
    }

    #[test]
    fn a_key_resolves_to_its_identity() {
        let alice = identity();
        let mut registry = IdentityRegistry::default();
        registry.insert(KeySet::new(alice.public_key(), None, 0));

        assert_eq!(
            registry.identity_of_key(&alice.public_key()),
            Some(alice.uuid_as_root())
        );
        assert_eq!(registry.identity_of_key(&identity().public_key()), None);
    }

    #[test]
    fn an_added_key_resolves_to_the_same_identity() {
        let alice = identity();
        let laptop = identity();
        let mut registry = IdentityRegistry::default();
        registry.insert(KeySet::new(alice.public_key(), None, 0));

        let uuid = alice.uuid_as_root();
        let payload = add_key_payload(&uuid, &laptop.public_key(), None);
        registry
            .add_key(
                &uuid,
                &alice.public_key(),
                laptop.public_key(),
                None,
                &alice.sign(&payload),
                1,
            )
            .expect("add key");

        assert_eq!(registry.identity_of_key(&laptop.public_key()), Some(uuid));
        assert_eq!(registry.identity_of_key(&alice.public_key()), Some(uuid));
        assert_eq!(registry.len(), 1, "a device is not a new player");
    }

    #[test]
    fn a_rotated_away_key_stops_resolving() {
        // The index is derived from the key set, so a revocation must remove it
        // rather than leaving a stale mapping that would let the old key join.
        let alice = identity();
        let successor = identity();
        let mut registry = IdentityRegistry::default();
        registry.insert(KeySet::new(
            alice.public_key(),
            Some(crate::identity::keyset::commit_to(&successor.public_key())),
            0,
        ));

        let uuid = alice.uuid_as_root();
        let payload =
            crate::identity::keyset::rotate_key_payload(&uuid, &successor.public_key(), None);
        registry
            .rotate_key(
                &uuid,
                &alice.public_key(),
                successor.public_key(),
                None,
                &alice.sign(&payload),
                1,
            )
            .expect("rotate");

        assert_eq!(
            registry.identity_of_key(&alice.public_key()),
            None,
            "the retired key must stop resolving"
        );
        assert_eq!(
            registry.identity_of_key(&successor.public_key()),
            Some(uuid)
        );
    }

    #[test]
    fn a_rebind_swaps_every_key_but_keeps_the_identity() {
        let alice = identity();
        let new_root = identity();
        let mut registry = IdentityRegistry::default();
        registry.insert(KeySet::new(alice.public_key(), None, 0));
        let uuid = alice.uuid_as_root();
        registry.bind_name("Alice", uuid).expect("bind");

        registry
            .admin_rebind(&uuid, new_root.public_key(), 5)
            .expect("rebind");

        assert_eq!(registry.identity_of_key(&new_root.public_key()), Some(uuid));
        assert_eq!(registry.identity_of_key(&alice.public_key()), None);
        assert_eq!(
            registry.name_holder("Alice"),
            Some(uuid),
            "a rebind must not cost the player their name"
        );
    }

    #[test]
    fn a_name_is_first_come() {
        let alice = identity().uuid_as_root();
        let bob = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();

        registry.bind_name("Shared", alice).expect("first come");
        assert!(matches!(
            registry.bind_name("Shared", bob),
            Err(RegistryError::NameTaken { .. })
        ));
        assert_eq!(registry.name_holder("Shared"), Some(alice));
    }

    #[test]
    fn a_taken_name_binds_as_its_next_free_variant_and_sticks() {
        // Two fresh installs called `player`: the second is `player 2`, a
        // third `player 3`; each keeps its variant on reconnect, and the
        // first keeps the plain name. Past the limit, refused as before.
        let host = identity().uuid_as_root();
        let guest = identity().uuid_as_root();
        let third = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();

        assert_eq!(
            registry
                .bind_name_or_next("player", host, 99)
                .expect("host"),
            "player"
        );
        assert_eq!(
            registry
                .bind_name_or_next("player", guest, 99)
                .expect("guest"),
            "player 2"
        );
        assert_eq!(
            registry
                .bind_name_or_next("player", third, 99)
                .expect("third"),
            "player 3"
        );
        // Reconnecting with the plain name again: the same variant.
        assert_eq!(
            registry
                .bind_name_or_next("player", guest, 99)
                .expect("guest again"),
            "player 2"
        );
        assert_eq!(
            registry
                .bind_name_or_next("player", host, 99)
                .expect("host again"),
            "player"
        );
        assert_eq!(registry.name_holder("player 2"), Some(guest));
        // Nothing left to try: a refusal, as it always was.
        let fourth = identity().uuid_as_root();
        assert!(matches!(
            registry.bind_name_or_next("player", fourth, 3),
            Err(RegistryError::NameTaken { .. })
        ));
        // A new name releases the variant, as any rename does.
        assert_eq!(
            registry
                .bind_name_or_next("Guest", guest, 99)
                .expect("renamed"),
            "Guest"
        );
        assert_eq!(registry.name_holder("player 2"), None);
    }

    #[test]
    fn rebinding_your_own_name_is_a_no_op() {
        // A reconnecting player must not be refused their own name.
        let alice = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();
        registry.bind_name("Alice", alice).expect("bind");
        registry.bind_name("Alice", alice).expect("rebind own name");
        assert_eq!(registry.name_holder("Alice"), Some(alice));
    }

    #[test]
    fn taking_a_new_name_releases_the_old_one() {
        // Otherwise a player could hoard names by reconnecting under new ones.
        let alice = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();
        registry.bind_name("First", alice).expect("bind");
        registry.bind_name("Second", alice).expect("rename");

        assert_eq!(registry.name_holder("Second"), Some(alice));
        assert_eq!(
            registry.name_holder("First"),
            None,
            "the old name is released"
        );
        assert_eq!(registry.bindings().len(), 1);
    }

    #[test]
    fn releasing_a_name_frees_it_for_someone_else() {
        let alice = identity().uuid_as_root();
        let bob = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();
        registry.bind_name("Contested", alice).expect("bind");
        registry.release_name("Contested");
        registry.bind_name("Contested", bob).expect("now free");
        assert_eq!(registry.name_holder("Contested"), Some(bob));
    }

    #[test]
    fn an_unknown_identity_cannot_be_operated_on() {
        let mut registry = IdentityRegistry::default();
        let stranger = identity();
        assert!(matches!(
            registry.admin_rebind(&stranger.uuid_as_root(), stranger.public_key(), 1),
            Err(RegistryError::UnknownIdentity)
        ));
    }

    #[test]
    fn reinserting_an_identity_does_not_leave_stale_key_mappings() {
        // The index is denormalised, so the one bug it can have is going stale.
        let alice = identity();
        let successor = identity();
        let mut registry = IdentityRegistry::default();

        let mut keys = KeySet::new(
            alice.public_key(),
            Some(crate::identity::keyset::commit_to(&successor.public_key())),
            0,
        );
        registry.insert(keys.clone());
        assert!(registry.identity_of_key(&alice.public_key()).is_some());

        let payload = crate::identity::keyset::rotate_key_payload(
            &alice.uuid_as_root(),
            &successor.public_key(),
            None,
        );
        keys.rotate_key(
            &alice.public_key(),
            successor.public_key(),
            None,
            &alice.sign(&payload),
            1,
        )
        .expect("rotate");

        registry.insert(keys);
        assert_eq!(
            registry.identity_of_key(&alice.public_key()),
            None,
            "reinsert must drop the retired key from the index"
        );
    }

    #[test]
    fn a_certified_name_binds_its_own_casing() {
        let alice = identity();
        let mut registry = IdentityRegistry::default();
        let bound = registry
            .bind_certified("Alice", "alice", alice.uuid_as_root(), 99)
            .expect("bind");
        assert_eq!(bound.name, "Alice");
        assert_eq!(bound.displaced, None);
        assert_eq!(registry.name_of(&alice.uuid_as_root()), Some("Alice"));
    }

    #[test]
    fn a_certified_name_displaces_an_uncertified_holder() {
        let alice = identity();
        let bob = identity();
        let mut registry = IdentityRegistry::default();
        registry
            .bind_name("alice", bob.uuid_as_root())
            .expect("bob was here first");

        let bound = registry
            .bind_certified("Alice", "alice", alice.uuid_as_root(), 99)
            .expect("bind");
        assert_eq!(bound.name, "Alice");
        assert_eq!(
            bound.displaced,
            Some((bob.uuid_as_root(), "alice 2".to_owned()))
        );
        assert_eq!(registry.name_of(&alice.uuid_as_root()), Some("Alice"));
        assert_eq!(registry.name_of(&bob.uuid_as_root()), Some("alice 2"));
        assert_eq!(registry.name_holder("alice"), None);
    }

    #[test]
    fn a_certified_name_releases_what_its_identity_wore_before() {
        let alice = identity();
        let mut registry = IdentityRegistry::default();
        registry
            .bind_name("Alicia", alice.uuid_as_root())
            .expect("earlier name");

        let bound = registry
            .bind_certified("Alice", "alice", alice.uuid_as_root(), 99)
            .expect("bind");
        assert_eq!(bound.name, "Alice");
        assert_eq!(registry.name_holder("Alicia"), None);
        assert_eq!(registry.name_of(&alice.uuid_as_root()), Some("Alice"));
    }

    #[test]
    fn a_certified_join_with_no_variant_free_changes_nothing() {
        // Every variant up to `most` is held, so the holder cannot be moved.
        // The join is refused — and the refusal must be the only effect: the
        // holder keeps the name, and the certified identity is bound to nothing,
        // rather than the holder left nameless beside a name whose owner was
        // turned away.
        let alice = identity();
        let bob = identity();
        let carol = identity();
        let dave = identity();
        let mut registry = IdentityRegistry::default();
        registry
            .bind_name("alice", bob.uuid_as_root())
            .expect("bob");
        registry
            .bind_name("alice 2", carol.uuid_as_root())
            .expect("carol");
        registry
            .bind_name("alice 3", dave.uuid_as_root())
            .expect("dave");

        let refused = registry.bind_certified("Alice", "alice", alice.uuid_as_root(), 3);
        assert!(
            matches!(refused, Err(RegistryError::NameTaken { .. })),
            "got {refused:?}"
        );
        assert_eq!(registry.name_of(&bob.uuid_as_root()), Some("alice"));
        assert_eq!(registry.name_holder("Alice"), None);
        assert_eq!(registry.name_of(&alice.uuid_as_root()), None);
        assert_eq!(registry.name_of(&carol.uuid_as_root()), Some("alice 2"));
        assert_eq!(registry.name_of(&dave.uuid_as_root()), Some("alice 3"));
    }

    #[test]
    fn a_certified_binding_is_marked_and_the_mark_goes_with_the_name() {
        // The mark RCON reads for a player who is not here: set where the
        // certificate is applied, and gone the moment the binding moves.
        let alice = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();
        registry
            .bind_certified("Alice", "alice", alice, 99)
            .expect("bind");
        assert!(registry.is_certified_name("Alice"));
        assert_eq!(registry.dirty_certified(), vec!["Alice".to_owned()]);
        registry.clear_dirty();

        // The same certificate again changes nothing that needs writing.
        registry
            .bind_certified("Alice", "alice", alice, 99)
            .expect("rebind");
        assert!(registry.is_certified_name("Alice"));
        assert!(!registry.is_dirty());

        // A new name — the identity's own, uncertified — releases the old one
        // and its mark; the new binding is unmarked.
        registry.bind_name("Alicia", alice).expect("rename");
        assert!(!registry.is_certified_name("Alice"));
        assert!(!registry.is_certified_name("Alicia"));
        assert_eq!(registry.dirty_certified(), vec!["Alice".to_owned()]);

        // A release takes the mark too.
        registry
            .bind_certified("Alice", "alice", alice, 99)
            .expect("bind again");
        registry.release_name("Alice");
        assert!(!registry.is_certified_name("Alice"));
        assert_eq!(registry.name_holder("Alice"), None);
    }

    #[test]
    fn a_displaced_certified_holder_loses_the_mark_with_the_name() {
        // Two certificates for one name (a stale one, after a rename at the
        // registry): the newer holder takes it, and the mark is theirs — the
        // displaced holder's variant is this server's, not the registry's.
        let alice = identity().uuid_as_root();
        let bob = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();
        registry
            .bind_certified("Alice", "alice", bob, 99)
            .expect("bob's stale certificate");
        assert!(registry.is_certified_name("Alice"));

        let bound = registry
            .bind_certified("Alice", "alice", alice, 99)
            .expect("alice's");
        assert_eq!(bound.displaced, Some((bob, "Alice 2".to_owned())));
        assert_eq!(registry.name_holder("Alice"), Some(alice));
        assert!(registry.is_certified_name("Alice"), "alice's mark now");
        assert!(!registry.is_certified_name("Alice 2"));
    }

    #[test]
    fn clearing_an_identitys_mark_leaves_its_binding_and_everyone_elses() {
        let alice = identity().uuid_as_root();
        let bob = identity().uuid_as_root();
        let mut registry = IdentityRegistry::default();
        registry
            .bind_certified("Alice", "alice", alice, 99)
            .expect("alice");
        registry.bind_certified("Bob", "bob", bob, 99).expect("bob");

        registry.clear_certified(&alice);
        assert_eq!(registry.name_holder("Alice"), Some(alice));
        assert!(!registry.is_certified_name("Alice"));
        assert!(registry.is_certified_name("Bob"));

        // Nothing to clear for an identity with no name, or no mark.
        registry.clear_certified(&identity().uuid_as_root());
        registry.clear_certified(&alice);
        assert!(registry.is_certified_name("Bob"));

        // A load-time mark only lands on a name somebody holds.
        registry.mark_certified_clean("Nobody");
        assert!(!registry.is_certified_name("Nobody"));
    }
}
