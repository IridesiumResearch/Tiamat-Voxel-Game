// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The server's copy of every mod's persistent storage.
//!
//! # Held in memory, written on the save
//!
//! A mod writes storage from inside a tick, and the tick already holds the
//! world — so a write that reached `SQLite` directly would need the database
//! borrowed in two places at once, and would put a synchronous disk write in
//! the middle of the simulation. The whole set is small (a mod stores facts,
//! not data), so it is loaded at startup and written back on the same debounced
//! save the chunks use.
//!
//! **Dirty is per key.** It was per mod, and the save replaced a mod's whole
//! bag — every row deleted and written again whenever one key changed. Fine
//! for a mod storing a dozen facts; a mod keeping one key per player per fact
//! (Progress ask 2: ten thousand keys at fifty players) would have rewritten
//! ten thousand rows every two seconds for as long as anybody was discovering
//! anything. Now what is handed to the save is each changed key with its new
//! value, or `None` for one deleted, and the save writes exactly those.

use std::collections::{BTreeMap, BTreeSet};

use tiamat_core::storage::{Bag, Value};

/// Every mod's storage, and which of them need writing.
#[derive(Debug, Default)]
pub struct ModStorage {
    bags: BTreeMap<String, Bag>,
    /// The keys changed since the last save, by mod.
    dirty: BTreeMap<String, BTreeSet<String>>,
}

/// One mod's changes since the last save: each key with its new value, or
/// `None` for a key that was deleted.
pub type Changes = Vec<(String, Option<Value>)>;

impl ModStorage {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs what the world database had for a mod.
    pub fn load(&mut self, mod_id: &str, bag: Bag) {
        self.bags.insert(mod_id.to_owned(), bag);
    }

    /// Reads a key.
    #[must_use]
    pub fn get(&self, mod_id: &str, key: &str) -> Option<Value> {
        self.bags.get(mod_id)?.get(key).cloned()
    }

    /// Writes a key, or removes it when `value` is `None`.
    ///
    /// A write that changes nothing does not mark the mod dirty. Mods poll —
    /// a state machine writing "still following" every tick is the normal
    /// case — and without this the debounced save would rewrite every bag
    /// twenty times a second for as long as the server ran.
    pub fn set(&mut self, mod_id: &str, key: &str, value: Option<Value>) {
        let bag = self.bags.entry(mod_id.to_owned()).or_default();
        let changed = match value {
            Some(value) => bag.insert(key.to_owned(), value.clone()) != Some(value),
            None => bag.remove(key).is_some(),
        };
        if changed {
            self.dirty
                .entry(mod_id.to_owned())
                .or_default()
                .insert(key.to_owned());
        }
    }

    /// A mod's keys, in order.
    #[must_use]
    pub fn keys(&self, mod_id: &str, prefix: &str) -> Vec<String> {
        self.bags
            .get(mod_id)
            .map(|bag| {
                // A bounded walk: from the prefix, while it still matches.
                bag.range(prefix.to_owned()..)
                    .map(|(key, _)| key)
                    .take_while(|key| key.starts_with(prefix))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Takes the changes waiting to be written, by mod.
    ///
    /// Each entry is a key with the value it holds now, or `None` for one
    /// that was deleted, so the save can write and delete exactly those rows
    /// and no others. Nothing is handed over twice: a key changed again
    /// after this is dirty again.
    pub fn take_dirty(&mut self) -> Vec<(String, Changes)> {
        std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|(mod_id, keys)| {
                let bag = self.bags.get(&mod_id);
                let changes = keys
                    .into_iter()
                    .map(|key| {
                        let value = bag.and_then(|bag| bag.get(&key).cloned());
                        (key, value)
                    })
                    .collect();
                (mod_id, changes)
            })
            .collect()
    }

    /// How many mods are waiting to be written.
    #[must_use]
    pub fn dirty(&self) -> usize {
        self.dirty.len()
    }

    /// Puts a change back on the list, for a save that failed.
    ///
    /// The value the store holds is still the truth; what is re-marked is
    /// only that the key needs writing. `value` is ignored except that it
    /// keeps the call's shape the same as what [`Self::take_dirty`] handed
    /// out, so a failed batch is put back with one loop.
    pub fn mark_dirty(&mut self, mod_id: &str, key: &str, _value: Option<Value>) {
        self.dirty
            .entry(mod_id.to_owned())
            .or_default()
            .insert(key.to_owned());
    }
}

/// A handle on the store, for the mod API.
///
/// The same arrangement `fluid::Shared` and `ent::Shared` use, and behind a
/// lock for the same reason.
pub struct Shared {
    storage: std::sync::Arc<std::sync::RwLock<ModStorage>>,
}

impl Shared {
    /// Wraps a store the simulation thread owns.
    #[must_use]
    pub const fn new(storage: std::sync::Arc<std::sync::RwLock<ModStorage>>) -> Self {
        Self { storage }
    }
}

impl tiamat_core::storage::Access for Shared {
    fn get(&self, mod_id: &str, key: &str) -> Option<Value> {
        self.storage
            .read()
            .ok()
            .and_then(|storage| storage.get(mod_id, key))
    }

    fn set(&self, mod_id: &str, key: &str, value: Option<Value>) {
        if let Ok(mut storage) = self.storage.write() {
            storage.set(mod_id, key, value);
        }
    }

    fn keys(&self, mod_id: &str, prefix: &str) -> Vec<String> {
        self.storage
            .read()
            .map(|storage| storage.keys(mod_id, prefix))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_mod_cannot_see_anothers_keys() {
        // The isolation the whole design rests on. It is enforced by the API
        // never taking a mod id from Lua, but the store had better honour the
        // one it is given too.
        let mut storage = ModStorage::new();
        storage.set("a", "seen", Some(Value::Flag(true)));
        storage.set("b", "seen", Some(Value::Text("something else".into())));

        assert_eq!(storage.get("a", "seen"), Some(Value::Flag(true)));
        assert_eq!(
            storage.get("b", "seen"),
            Some(Value::Text("something else".into()))
        );
        assert_eq!(storage.get("c", "seen"), None);
        assert_eq!(storage.keys("a", ""), vec!["seen".to_owned()]);
    }

    #[test]
    fn keys_under_a_prefix_are_a_bounded_walk_from_it() {
        let mut storage = ModStorage::new();
        for key in ["ada:1", "ada:2", "adam:1", "bob:1"] {
            storage.set("a", key, Some(Value::Flag(true)));
        }
        assert_eq!(
            storage.keys("a", "ada:"),
            vec!["ada:1".to_owned(), "ada:2".to_owned()]
        );
        assert_eq!(
            storage.keys("a", "ada"),
            vec!["ada:1".to_owned(), "ada:2".to_owned(), "adam:1".to_owned()]
        );
        assert_eq!(storage.keys("a", "zed").len(), 0);
        assert_eq!(storage.keys("a", "").len(), 4);
    }

    #[test]
    fn writing_the_same_value_again_does_not_dirty_the_mod() {
        // **A mod's state machine writes every tick.** Without this the
        // debounced save rewrites every bag twenty times a second for as long
        // as the server runs, for a value that never changed.
        let mut storage = ModStorage::new();
        storage.set("a", "state", Some(Value::Text("following".into())));
        assert_eq!(storage.dirty(), 1);
        let _ = storage.take_dirty();

        storage.set("a", "state", Some(Value::Text("following".into())));
        assert_eq!(storage.dirty(), 0, "an unchanged write dirtied the mod");

        storage.set("a", "state", Some(Value::Text("fleeing".into())));
        assert_eq!(storage.dirty(), 1);
    }

    #[test]
    fn deleting_a_key_dirties_the_mod_and_deleting_a_missing_one_does_not() {
        let mut storage = ModStorage::new();
        storage.set("a", "gone", Some(Value::Flag(true)));
        let _ = storage.take_dirty();

        storage.set("a", "gone", None);
        assert_eq!(storage.dirty(), 1);
        assert_eq!(storage.get("a", "gone"), None);
        let _ = storage.take_dirty();

        storage.set("a", "never-existed", None);
        assert_eq!(storage.dirty(), 0);
    }

    #[test]
    fn take_dirty_hands_over_only_what_changed_with_deletions_as_none() {
        // Progress ask 2: a mod with ten thousand keys pays for the ones it
        // touched. The save writes what it is handed and deletes the `None`s,
        // so the untouched rows stay as they are.
        let mut storage = ModStorage::new();
        storage.set("a", "one", Some(Value::Number(1.0)));
        storage.set("a", "gone", Some(Value::Flag(true)));
        let _ = storage.take_dirty();
        storage.set("a", "two", Some(Value::Number(2.0)));
        storage.set("a", "gone", None);

        let written = storage.take_dirty();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, "a");
        assert_eq!(
            written[0].1,
            vec![
                ("gone".to_owned(), None),
                ("two".to_owned(), Some(Value::Number(2.0))),
            ],
            "the changed keys only, in key order, a deletion as None"
        );
        assert!(
            storage.take_dirty().is_empty(),
            "nothing is handed over twice"
        );
    }
}
