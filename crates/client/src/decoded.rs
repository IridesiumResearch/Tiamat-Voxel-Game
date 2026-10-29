// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Decoded assets kept across joins (perf audit N4).
//!
//! # Why
//!
//! Every join decoded every texture the material table named, and every
//! model, picture and skin as it was named, and a new `App` per join dropped
//! the lot on the way out. Leaving a world and coming back — the LAN host
//! restarting, a dropped connection — paid the whole decode again, on the
//! network task, before `JoinWorld` could go out: half a second to two on a
//! large mod set. The bytes were already on disk and already verified; what
//! was lost was only the decode.
//!
//! # What keys a decode, and why that is safe
//!
//! A decode is keyed by the **content hash of the bytes it was made from**
//! and the **version of the decoder that made it**. The hash is the identity
//! the content pipeline already verifies — `ContentCache::get` re-hashes
//! what it reads, and a slice off the wire is hashed before it is kept — so
//! a decode is only ever made from bytes that are what the server said they
//! were, and a server that names the same hash again names the same bytes.
//! The callers still read and verify the bytes first (charter rule 14's
//! gate is the hash check, and it stays where it was); what they skip is the
//! decode. A file tampered with on disk fails that check, is fetched afresh,
//! and never reaches here; a file missing from disk is fetched as before.
//!
//! A decode that FAILED is not kept: the warning it carries is per server
//! and per join, and a cache that swallowed it would make the second join
//! silently draw the placeholder the first one explained.
//!
//! The decoder version is in the key so that a cache which one day outlives
//! the process — on disk — cannot serve a decode an older decoder made. In
//! memory it is a constant and costs a word.
//!
//! # Bounded
//!
//! By decoded bytes, least recently used out first, and an item that would
//! not fit on its own is not kept at all. A texture pack is small — a few
//! hundred 16² tiles is a megabyte — and interface art, skins and models are
//! what fill it; the budget is what two large mod sets decode to, so
//! switching between two worlds keeps both.
//!
//! # Process-wide
//!
//! One cache for the process, through [`DecodedCache::shared`], because the
//! join that wants it is the next one, in a new `App` over a new connection.
//! Threading it through `Client` and every `App` would say the same thing
//! in more places; this is the wiring a later change can add without
//! touching what is here.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tiamat_core::model::{Limits, Model, ModelError};
use tiamat_core::proto::ContentHash;

use crate::texture::{Image, TextureError, decode_or_missing};

/// The version of the texture decode a cached [`Image`] was made by.
///
/// Bump it when `decode_png`, or what follows it, would give different
/// pixels for the same bytes.
pub const IMAGE_DECODER: u32 = 1;

/// The same, for a model's load.
pub const MODEL_DECODER: u32 = 1;

/// How many decoded bytes the process-wide cache holds.
///
/// A 2048-wide interface picture is 16 MiB decoded, the largest one thing a
/// texture decode may be; a few of those, a few hundred tiles and skins, and
/// a mod set's models fit several times over.
pub const SHARED_BUDGET: usize = 192 * 1024 * 1024;

/// What kind of decode a key names, so a hash's image and its model — the
/// same bytes could not be both, but the same KEY space must not pretend —
/// are two entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Image,
    Model,
}

/// What a decode is filed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    hash: ContentHash,
    kind: Kind,
    decoder: u32,
}

impl Key {
    const fn image(hash: ContentHash) -> Self {
        Self {
            hash,
            kind: Kind::Image,
            decoder: IMAGE_DECODER,
        }
    }

    const fn model(hash: ContentHash) -> Self {
        Self {
            hash,
            kind: Kind::Model,
            decoder: MODEL_DECODER,
        }
    }
}

/// A decode, shared with whoever asked.
#[derive(Clone)]
enum Decoded {
    Image(Arc<Image>),
    Model(Arc<Model>),
}

/// One cached decode and what it costs.
struct Slot {
    value: Decoded,
    bytes: usize,
    /// When it was last asked for, on [`Entries::clock`].
    used: u64,
}

/// The entries and their order of use.
#[derive(Default)]
struct Entries {
    slots: BTreeMap<Key, Slot>,
    /// Least recently used first: the key under each `used` stamp.
    recency: BTreeMap<u64, Key>,
    clock: u64,
    bytes: usize,
}

impl Entries {
    /// The entry under `key`, now the most recently used.
    fn touch(&mut self, key: Key) -> Option<Decoded> {
        let slot = self.slots.get_mut(&key)?;
        self.recency.remove(&slot.used);
        self.clock += 1;
        slot.used = self.clock;
        self.recency.insert(self.clock, key);
        Some(slot.value.clone())
    }

    /// Files `value` under `key`, replacing what was there.
    fn insert(&mut self, key: Key, value: Decoded, bytes: usize) {
        if let Some(old) = self.slots.remove(&key) {
            self.recency.remove(&old.used);
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.clock += 1;
        self.recency.insert(self.clock, key);
        self.bytes += bytes;
        self.slots.insert(
            key,
            Slot {
                value,
                bytes,
                used: self.clock,
            },
        );
    }

    /// Drops the least recently used entry. False when there is none.
    fn evict_oldest(&mut self) -> bool {
        let Some((&used, &key)) = self.recency.iter().next() else {
            return false;
        };
        self.recency.remove(&used);
        if let Some(slot) = self.slots.remove(&key) {
            self.bytes = self.bytes.saturating_sub(slot.bytes);
        }
        true
    }
}

/// Decoded images and models by content hash, bounded in bytes.
///
/// See the module docs for what keys an entry and why that is safe.
pub struct DecodedCache {
    entries: Mutex<Entries>,
    budget: usize,
    /// How many decodes ran through here: the misses. For the tests and the
    /// overlay; a rejoin that decodes nothing shows as this standing still.
    decodes: AtomicUsize,
}

impl DecodedCache {
    /// A cache holding at most `budget` decoded bytes.
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            budget,
            decodes: AtomicUsize::new(0),
        }
    }

    /// The process's one cache, at [`SHARED_BUDGET`].
    #[must_use]
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<DecodedCache>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(Self::new(SHARED_BUDGET))))
    }

    /// The image decoded from the bytes with this hash, if it is held.
    #[must_use]
    pub fn image(&self, hash: &ContentHash) -> Option<Arc<Image>> {
        match self.touch(Key::image(*hash))? {
            Decoded::Image(image) => Some(image),
            Decoded::Model(_) => None,
        }
    }

    /// The model loaded from the bytes with this hash, if it is held.
    #[must_use]
    pub fn model(&self, hash: &ContentHash) -> Option<Arc<Model>> {
        match self.touch(Key::model(*hash))? {
            Decoded::Model(model) => Some(model),
            Decoded::Image(_) => None,
        }
    }

    /// The image for `hash`: held, or decoded now from `bytes` — the bytes
    /// whose hash it is, which the caller has verified — and held if the
    /// decode succeeded.
    ///
    /// The same answer as `texture::decode_or_missing`: the placeholder and
    /// the reason when the bytes will not decode, and that failure is
    /// reported on every call rather than remembered.
    pub fn image_or_decode(
        &self,
        hash: &ContentHash,
        bytes: &[u8],
    ) -> (Arc<Image>, Option<TextureError>) {
        if let Some(image) = self.image(hash) {
            return (image, None);
        }
        self.decodes.fetch_add(1, Ordering::Relaxed);
        let (image, failure) = decode_or_missing(bytes);
        let image = Arc::new(image);
        if failure.is_none() {
            self.remember(Key::image(*hash), Decoded::Image(Arc::clone(&image)));
        }
        (image, failure)
    }

    /// The model for `hash`: held, or loaded now from `bytes` under `limits`
    /// and held if the load succeeded.
    ///
    /// # Errors
    ///
    /// What `model::load_isolated` refuses, every time it is asked.
    pub fn model_or_load(
        &self,
        hash: &ContentHash,
        bytes: &[u8],
        limits: &Limits,
    ) -> Result<Arc<Model>, ModelError> {
        if let Some(model) = self.model(hash) {
            return Ok(model);
        }
        self.decodes.fetch_add(1, Ordering::Relaxed);
        let model = Arc::new(tiamat_core::model::load_isolated(bytes, limits)?);
        self.remember(Key::model(*hash), Decoded::Model(Arc::clone(&model)));
        Ok(model)
    }

    /// Holds `image` as the decode of the bytes with this hash.
    pub fn remember_image(&self, hash: &ContentHash, image: Arc<Image>) {
        self.remember(Key::image(*hash), Decoded::Image(image));
    }

    /// Holds `model` as the load of the bytes with this hash.
    pub fn remember_model(&self, hash: &ContentHash, model: Arc<Model>) {
        self.remember(Key::model(*hash), Decoded::Model(model));
    }

    /// Decoded bytes held now.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.bytes)
    }

    /// Entries held now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.slots.len())
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many decodes have run through this cache: its misses.
    #[must_use]
    pub fn decodes(&self) -> usize {
        self.decodes.load(Ordering::Relaxed)
    }

    /// Drops everything held.
    pub fn forget_all(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            *entries = Entries::default();
        }
    }

    fn touch(&self, key: Key) -> Option<Decoded> {
        self.entries.lock().ok()?.touch(key)
    }

    /// Files a decode, and forgets the least recently used until the budget
    /// holds. A decode that would not fit alone is not filed at all.
    fn remember(&self, key: Key, value: Decoded) {
        let bytes = match &value {
            Decoded::Image(image) => image_bytes(image),
            Decoded::Model(model) => model_bytes(model),
        };
        if bytes > self.budget {
            return;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.insert(key, value, bytes);
        while entries.bytes > self.budget {
            if !entries.evict_oldest() {
                break;
            }
        }
    }
}

impl std::fmt::Debug for DecodedCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecodedCache")
            .field("entries", &self.len())
            .field("bytes", &self.bytes())
            .field("budget", &self.budget)
            .field("decodes", &self.decodes())
            .finish()
    }
}

/// What an image costs to hold: its pixels and its header.
fn image_bytes(image: &Image) -> usize {
    size_of::<Image>() + image.rgba.len()
}

/// What a model costs to hold: its buffers, its joints and its clips.
fn model_bytes(model: &Model) -> usize {
    use tiamat_core::model::{Channel, Clip, Joint, Vertex};
    let channel = |channel: &Channel| {
        size_of::<Channel>() + (channel.times.len() + channel.values.len()) * size_of::<f32>()
    };
    size_of::<Model>()
        + model.vertices.len() * size_of::<Vertex>()
        + model.indices.len() * size_of::<u32>()
        + model
            .skin
            .joints
            .iter()
            .map(|joint| size_of::<Joint>() + joint.name.len())
            .sum::<usize>()
        + model
            .clips
            .iter()
            .map(|clip| {
                size_of::<Clip>()
                    + clip.name.len()
                    + clip.channels.iter().map(channel).sum::<usize>()
            })
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width`×`height` RGBA PNG whose pixels follow `seed`.
    fn png_bytes(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            let pixels: Vec<u8> = (0..width * height * 4)
                .map(|index| index.to_le_bytes()[0].wrapping_add(seed))
                .collect();
            writer.write_image_data(&pixels).expect("data");
        }
        out
    }

    fn hash_of(bytes: &[u8]) -> ContentHash {
        tiamat_core::content::hash_bytes(bytes)
    }

    #[test]
    fn the_second_decode_of_the_same_bytes_is_the_first_one() {
        let cache = DecodedCache::new(1 << 20);
        let bytes = png_bytes(8, 8, 1);
        let hash = hash_of(&bytes);
        let (first, failure) = cache.image_or_decode(&hash, &bytes);
        assert!(failure.is_none());
        let (second, failure) = cache.image_or_decode(&hash, &bytes);
        assert!(failure.is_none());
        assert!(Arc::ptr_eq(&first, &second), "served, not decoded again");
        assert_eq!(cache.decodes(), 1);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), image_bytes(&first));
    }

    #[test]
    fn different_bytes_are_different_decodes() {
        let cache = DecodedCache::new(1 << 20);
        let a = png_bytes(8, 8, 1);
        let b = png_bytes(8, 8, 2);
        let (first, _) = cache.image_or_decode(&hash_of(&a), &a);
        let (second, _) = cache.image_or_decode(&hash_of(&b), &b);
        assert_ne!(first.rgba, second.rgba);
        assert_eq!(cache.decodes(), 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_decode_that_failed_is_not_kept_and_fails_again_out_loud() {
        // The warning a bad texture earns is per join; a cache that kept the
        // placeholder would make the second join draw it silently.
        let cache = DecodedCache::new(1 << 20);
        let bytes = b"not a png at all";
        let hash = hash_of(bytes);
        let (_, failure) = cache.image_or_decode(&hash, bytes);
        assert!(failure.is_some());
        let (_, again) = cache.image_or_decode(&hash, bytes);
        assert!(again.is_some(), "reported every time");
        assert_eq!(cache.decodes(), 2);
        assert!(cache.is_empty());
    }

    #[test]
    fn the_budget_holds_by_forgetting_the_least_recently_used() {
        let a = png_bytes(8, 8, 1);
        let b = png_bytes(8, 8, 2);
        let c = png_bytes(8, 8, 3);
        let one = image_bytes(&Image::solid(8, 8, [0; 4]));
        let cache = DecodedCache::new(one * 2);
        cache.image_or_decode(&hash_of(&a), &a);
        cache.image_or_decode(&hash_of(&b), &b);
        assert_eq!(cache.len(), 2);
        assert!(cache.image(&hash_of(&a)).is_some(), "a is the newer now");
        cache.image_or_decode(&hash_of(&c), &c);
        assert_eq!(cache.len(), 2);
        assert!(
            cache.image(&hash_of(&b)).is_none(),
            "b had gone longest unasked for"
        );
        assert!(cache.image(&hash_of(&a)).is_some());
        assert!(cache.image(&hash_of(&c)).is_some());
        assert!(cache.bytes() <= one * 2);
    }

    #[test]
    fn one_item_over_the_whole_budget_is_not_kept() {
        let cache = DecodedCache::new(16);
        let bytes = png_bytes(8, 8, 1);
        let (image, failure) = cache.image_or_decode(&hash_of(&bytes), &bytes);
        assert!(
            failure.is_none() && image.width == 8,
            "decoded all the same"
        );
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn remembering_a_hash_again_replaces_what_it_held() {
        let cache = DecodedCache::new(1 << 20);
        let hash = [3u8; 32];
        cache.remember_image(&hash, Arc::new(Image::solid(4, 4, [1; 4])));
        cache.remember_image(&hash, Arc::new(Image::solid(2, 2, [2; 4])));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), image_bytes(&Image::solid(2, 2, [2; 4])));
        assert_eq!(cache.image(&hash).map(|image| image.width), Some(2));
    }

    #[test]
    fn an_image_and_a_model_under_one_hash_do_not_collide() {
        let cache = DecodedCache::new(1 << 20);
        let hash = [7u8; 32];
        cache.remember_image(&hash, Arc::new(Image::solid(2, 2, [1, 2, 3, 4])));
        cache.remember_model(&hash, Arc::new(Model::default()));
        assert!(cache.image(&hash).is_some());
        assert!(cache.model(&hash).is_some());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_held_model_is_served_without_a_load_and_garbage_is_refused_every_time() {
        let cache = DecodedCache::new(1 << 20);
        let hash = [9u8; 32];
        cache.remember_model(&hash, Arc::new(Model::default()));
        let served = cache
            .model_or_load(&hash, b"never read", &Limits::default())
            .expect("held, so the bytes are not looked at");
        assert!(served.vertices.is_empty());
        assert_eq!(cache.decodes(), 0);

        let garbage = b"not a gltf";
        let refused = hash_of(garbage);
        assert!(
            cache
                .model_or_load(&refused, garbage, &Limits::default())
                .is_err()
        );
        assert!(
            cache
                .model_or_load(&refused, garbage, &Limits::default())
                .is_err()
        );
        assert_eq!(cache.decodes(), 2);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn forgetting_everything_leaves_nothing() {
        let cache = DecodedCache::new(1 << 20);
        let bytes = png_bytes(8, 8, 1);
        cache.image_or_decode(&hash_of(&bytes), &bytes);
        cache.forget_all();
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn the_shared_cache_is_one_for_the_process() {
        assert!(Arc::ptr_eq(
            &DecodedCache::shared(),
            &DecodedCache::shared()
        ));
    }
}
