// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Serving content to clients: chunked, compressed, and rate-limited.
//!
//! # Every request is adversarial
//!
//! Charter rule 14 is usually read from the client's side — a client decodes
//! assets from servers it does not trust. The mirror applies here: a *client*
//! can ask for anything, repeatedly, from an unauthenticated-adjacent position,
//! and the server has to stay up.
//!
//! Four limits, each for a different attack:
//!
//! - **Unknown hashes are refused silently.** Answering "no such content"
//!   differently from "here it is" turns the server into an oracle for probing
//!   what a private mod pack contains.
//! - **A per-client byte quota** bounds what one connection can pull. Without
//!   it, a client asking for the whole index on a loop is an amplifier: cheap
//!   to request, expensive to serve.
//! - **A per-item send cap** bounds how often one connection is sent the same
//!   file, so asking again — which a client must be able to do when taking an
//!   item failed — costs a session a few times its pack rather than the whole
//!   quota spent on one small file.
//! - **A per-tick send budget** bounds how much work a client can cause *at
//!   once*, so a big transfer is slow rather than a stall for everyone else.
//!
//! # Compression is per slice, not per file
//!
//! A client can decode a slice as it arrives rather than buffering the whole
//! file first. That matters for the memory cap on the client side — charter
//! rule 14 requires pre-decode bounds, and a bound you can only check after
//! buffering 16 MB is not much of a bound.
//!
//! # And once per slice, not once per client
//!
//! What a slice compresses to depends only on the item and where the slice
//! sits in it, and content is addressed by the hash of its bytes, so every
//! client asking for a slice can be handed the same compressed bytes. The
//! [`SliceCache`] compresses each slice the first time anyone asks and shares
//! the result with every connection after; fifty players joining at once cost
//! one compression of the pack, not fifty (perf audit N5).

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use tiamat_core::content::{ContentIndex, ContentItem};
use tiamat_core::proto::{ContentHash, MAX_CONTENT_CHUNK_BYTES, ServerMessage};

/// Uncompressed bytes per slice.
///
/// Sized so a compressed slice stays comfortably under
/// [`MAX_CONTENT_CHUNK_BYTES`] even for incompressible data, where zstd adds a
/// small framing overhead rather than shrinking anything. Picking the limit
/// itself would put every incompressible file one byte over.
pub const SLICE_BYTES: usize = 192 * 1024;

/// zstd level for content slices.
///
/// 3 is zstd's default and the knee of its curve: most of the ratio for a small
/// fraction of the time. A slice is compressed while the first player to ask
/// for it waits, so spending 10× the CPU for a few percent would be the wrong
/// trade even though the result is then shared.
pub const COMPRESSION_LEVEL: i32 = 3;

/// Total bytes one connection may pull per session.
///
/// Generous next to any real mod pack, and a hard stop on a client looping
/// requests to burn server CPU. A client that legitimately needs more than this
/// is downloading more than [`tiamat_core::content::MAX_MOD_BYTES`] allows a
/// server to hold.
pub const QUOTA_BYTES: u64 = 512 * 1024 * 1024;

/// Slices sent to one client per pass.
///
/// Bounds how much work a single request can cause at once. A large transfer
/// becomes slow rather than a stall for everyone else on the server.
pub const SLICES_PER_PASS: usize = 2;

/// Times one connection may be sent the same item in a session.
///
/// The first delivery and two more. A client whose taking of an item failed
/// asks for it again, and that has to work without a reconnect (perf audit
/// B9) — but a client that has failed the same item three times has a fault a
/// fourth send will not fix. Without a cap, a client asking on a loop is
/// bounded only by [`QUOTA_BYTES`]: 512 MiB of one small file per connection,
/// whether it is an attacker or an honest client re-asking for a picture its
/// full disk can never store every time a dialog opens. With it, a session
/// costs at most three times the pack, and the quota stays the ceiling over
/// everything. A reconnect starts a fresh count, as it always started a fresh
/// quota.
pub const MAX_SENDS_PER_ITEM: u8 = 3;

/// Compressed bytes the shared [`SliceCache`] holds before it evicts.
///
/// [`tiamat_core::content::MAX_MOD_BYTES`], the most one mod may ship: a
/// server whose whole pack fits one mod's limit — every reference pack, many
/// times over — is served from the cache entirely. A bigger pack still works;
/// its least recently sent slices are dropped and compressed again if asked
/// for, which is what every slice cost before there was a cache.
///
/// A ceiling, not an allocation. The cache holds only what has been sent, so a
/// server with a 20 MB pack holds at most about 20 MB here — less, by whatever
/// compresses — beside the pack its index already holds uncompressed. And it
/// lives only while a connection does ([`SliceCache::shared`]), so a
/// singleplayer world that has closed leaves nothing behind in the client's
/// process.
///
/// The cast is lossless: the supported targets are x86_64 and aarch64 only
/// (charter rule 4), where `usize` is 64 bits.
pub const CACHE_BYTES: usize = tiamat_core::content::MAX_MOD_BYTES as usize;

/// One client's outstanding content transfers.
#[derive(Debug)]
pub struct Transfers {
    /// Hashes queued to send, in request order; the front one is being sent.
    ///
    /// A deque because items leave from the front, where `Vec::remove(0)`
    /// shifted every hash behind it once per finished item.
    queue: VecDeque<ContentHash>,
    /// The hashes in `queue`, so a repeated request for something still on its
    /// way is a no-op without walking a queue a client can make as long as the
    /// index.
    queued: BTreeSet<ContentHash>,
    /// How many times each hash has been queued this session, for
    /// [`MAX_SENDS_PER_ITEM`].
    ///
    /// Only hashes the server holds reach it, so it is bounded by the index
    /// and the uploads rather than by what a client asks for.
    sends: BTreeMap<ContentHash, u8>,
    /// Which slice of the item at the front of `queue` goes next.
    slice: usize,
    /// Bytes sent to this client so far.
    sent_bytes: u64,
    /// Compressed slices, shared with every other connection.
    slices: Arc<SliceCache>,
}

impl Default for Transfers {
    fn default() -> Self {
        Self::with_cache(SliceCache::shared())
    }
}

impl Transfers {
    /// A fresh transfer state, compressing through the process's shared
    /// [`SliceCache`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh transfer state compressing through `slices`.
    ///
    /// For a caller that wants its own cache rather than the process's: a test
    /// counting compressions, or a server that would rather tie the cache's
    /// lifetime to itself than to its connections.
    #[must_use]
    pub fn with_cache(slices: Arc<SliceCache>) -> Self {
        Self {
            queue: VecDeque::new(),
            queued: BTreeSet::new(),
            sends: BTreeMap::new(),
            slice: 0,
            sent_bytes: 0,
            slices,
        }
    }

    /// How many items are still queued.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Bytes sent to this client.
    #[must_use]
    pub const fn sent_bytes(&self) -> u64 {
        self.sent_bytes
    }

    /// Whether this client has exhausted its quota.
    #[must_use]
    pub const fn over_quota(&self) -> bool {
        self.sent_bytes >= QUOTA_BYTES
    }

    /// Queues the hashes a client asked for that the server actually has.
    ///
    /// Returns how many were accepted. Unknown hashes are **dropped without
    /// comment**: distinguishing "no such content" from "here it is" turns the
    /// server into an oracle for probing what a private mod pack contains.
    ///
    /// A hash still queued — the one being sent included — is ignored, so a
    /// client repeating its request cannot make the server send the same file
    /// twice over. A hash that has been *sent* is accepted again: the client
    /// asking again is how it recovers when taking an item failed on its side,
    /// and a recovery that needed a reconnect would stall the join instead
    /// (perf audit B9). Up to [`MAX_SENDS_PER_ITEM`] times a session; past
    /// that the hash is ignored as silently as an unknown one, and
    /// [`QUOTA_BYTES`] bounds all of it.
    pub fn request(
        &mut self,
        hashes: &[ContentHash],
        index: &ContentIndex,
        uploads: &ContentIndex,
    ) -> usize {
        // **Past the quota there is nothing left to accept.** Queuing an item
        // here only to clear it on the next pass left the connection's beat
        // arm awake every 50 ms for a transfer that could never happen.
        if self.over_quota() {
            return 0;
        }
        let mut accepted = 0;
        for hash in hashes {
            if self.queued.contains(hash)
                || (index.get(hash).is_none() && uploads.get(hash).is_none())
            {
                continue;
            }
            let sends = self.sends.entry(*hash).or_insert(0);
            if *sends >= MAX_SENDS_PER_ITEM {
                continue;
            }
            *sends += 1;
            self.queued.insert(*hash);
            self.queue.push_back(*hash);
            accepted += 1;
        }
        accepted
    }

    /// Produces up to [`SLICES_PER_PASS`] messages of content to send.
    ///
    /// Returns an empty vector when there is nothing queued or the quota is
    /// spent.
    pub fn next_slices(
        &mut self,
        index: &ContentIndex,
        uploads: &ContentIndex,
    ) -> Vec<ServerMessage> {
        let mut out = Vec::new();

        while out.len() < SLICES_PER_PASS {
            if self.over_quota() {
                // Stop sending rather than disconnecting. A client that hit the
                // quota has almost certainly finished anyway, and dropping a
                // connection over a byte count would punish a legitimately
                // large mod pack the same as an attack.
                //
                // **And drop what is left.** Nothing queued can ever be sent
                // now; keeping it made the endpoint's beat condition true for
                // ever, waking the connection every 50 ms to produce nothing.
                self.queue.clear();
                self.queued.clear();
                self.slice = 0;
                break;
            }
            let Some(hash) = self.queue.front().copied() else {
                break;
            };
            let Some(item) = index.get(&hash).or_else(|| uploads.get(&hash)) else {
                // Vanished between request and send: a player's upload,
                // forgotten when they left. The mods' index is frozen, but
                // dropping the entry is the right response either way.
                self.finish_item();
                continue;
            };
            let Some(compressed) = self.slices.slice(item, self.slice) else {
                // Compression failing is a bug rather than a condition, and
                // the cache has logged it; dropping the item beats stalling
                // the client forever on an item that will never encode.
                self.finish_item();
                continue;
            };

            debug_assert!(
                compressed.len() <= MAX_CONTENT_CHUNK_BYTES,
                "a compressed slice must fit the protocol's chunk limit"
            );

            let total_len = item.bytes.len();
            let offset = self.slice * SLICE_BYTES;
            let end = (offset + SLICE_BYTES).min(total_len);
            out.push(ServerMessage::ContentChunk {
                hash,
                offset: offset as u64,
                total_len: total_len as u64,
                data: compressed.to_vec(),
            });

            self.sent_bytes = self.sent_bytes.saturating_add((end - offset) as u64);
            self.slice += 1;

            // A zero-length file sends exactly one empty slice, then finishes.
            // `end` and `total_len` are both 0 for it, so this has to be `>=`:
            // a strict `>` would never fire, and the item would never leave
            // the queue, sending empty slices for ever.
            if end >= total_len {
                self.finish_item();
            }
        }

        // A slice sent on this pass may have crossed the quota with items still
        // queued; clear those too, for the reason in the loop above.
        if self.over_quota() {
            self.queue.clear();
            self.queued.clear();
            self.slice = 0;
        }
        out
    }

    /// Takes the item at the front of the queue off it, sent or abandoned.
    fn finish_item(&mut self) {
        if let Some(hash) = self.queue.pop_front() {
            self.queued.remove(&hash);
        }
        self.slice = 0;
    }
}

/// A compressed slice, once someone has made it: `None` inside means
/// compressing it failed.
type Compressed = Arc<OnceLock<Option<Arc<[u8]>>>>;

/// Which slice of which item: the item's hash and the slice's index in it.
type SliceKey = (ContentHash, usize);

/// Compressed content slices, shared by every connection that serves them.
///
/// # Why one result serves everyone
///
/// Compressing is the expensive half of serving content, and every connection
/// used to compress every slice for itself: fifty players joining at once
/// compressed the same pack fifty times (perf audit N5). The key is
/// `(hash, slice index)`, and content is addressed by `BLAKE3` of its bytes,
/// so the key alone fixes the bytes compressed — whichever index the item came
/// from, mods' files or players' uploads, and whichever server in the process
/// is serving it.
///
/// # Once, even when everyone asks at once
///
/// Joiners arrive together and step through a pack at one pace, so they ask
/// for the same slice on the same beat. Each slice has a once-cell: the first
/// to ask compresses, and anyone asking meanwhile waits for that result rather
/// than compressing a copy, which costs them no more than compressing would
/// have. The map's lock is never held while compressing, so a slice being
/// compressed holds up only those who want that slice.
///
/// # Bounded: least recently asked for goes first
///
/// The cache holds at most its capacity in compressed bytes ([`CACHE_BYTES`]
/// for the shared one) and evicts the slice asked for longest ago to stay
/// under it. A compression that fails is not kept: the clients waiting on it
/// drop the item, as they always did, and the next to ask tries again.
pub struct SliceCache {
    /// Every slice held, behind the one lock.
    entries: Mutex<Entries>,
    /// Compressed bytes held before evicting.
    capacity: usize,
    /// Slices compressed over the cache's life, served ones not counted.
    compressions: AtomicU64,
}

/// The cache's bookkeeping.
///
/// `slots` is a `HashMap` for lookup only and is never iterated; eviction
/// order comes from `recency`, so nothing here depends on hash order.
#[derive(Default)]
struct Entries {
    /// Every slice held or being compressed.
    slots: HashMap<SliceKey, Slot>,
    /// The same keys by when they were last asked for, oldest first: what
    /// eviction pops.
    recency: BTreeMap<u64, SliceKey>,
    /// The next stamp for `recency`; one per lookup.
    clock: u64,
    /// Compressed bytes held: the sum of every slot's `charged`.
    bytes: usize,
}

/// One slice in the cache.
struct Slot {
    /// The compressed bytes, once made.
    cell: Compressed,
    /// Its key in [`Entries::recency`].
    stamp: u64,
    /// Bytes counted against the capacity, `None` while still compressing.
    charged: Option<usize>,
}

impl std::fmt::Debug for SliceCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not the slices: a derived `Debug` would print up to the capacity in
        // bytes, and taking the lock here could deadlock a caller that holds it.
        f.debug_struct("SliceCache")
            .field("capacity", &self.capacity)
            .field("compressions", &self.compressions())
            .finish_non_exhaustive()
    }
}

impl SliceCache {
    /// An empty cache that holds at most `capacity` compressed bytes.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            capacity,
            compressions: AtomicU64::new(0),
        }
    }

    /// The cache this process's connections share, of [`CACHE_BYTES`].
    ///
    /// Made by the first connection to want it and freed when the last one
    /// holding it closes: the process keeps only a weak reference. A server
    /// with nobody connected holds no compressed slices, and an embedded
    /// singleplayer server leaves none behind in the client after the world
    /// closes. Every server in the process shares it, which is safe because a
    /// slice's key fixes its bytes (see the type's documentation).
    #[must_use]
    pub fn shared() -> Arc<Self> {
        static SHARED: Mutex<Weak<SliceCache>> = Mutex::new(Weak::new());
        let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cache) = shared.upgrade() {
            return cache;
        }
        let cache = Arc::new(Self::new(CACHE_BYTES));
        *shared = Arc::downgrade(&cache);
        cache
    }

    /// Slice `index` of `item`, compressed: from the cache, or compressed now
    /// and kept for whoever asks next.
    ///
    /// `None` when the item has no such slice or it would not compress. An
    /// empty item has one slice, the empty one, which is how an empty file is
    /// still sent.
    pub fn slice(&self, item: &ContentItem, index: usize) -> Option<Arc<[u8]>> {
        let start = index.checked_mul(SLICE_BYTES)?;
        let end = start.saturating_add(SLICE_BYTES).min(item.bytes.len());
        let plain = item.bytes.get(start..end)?;

        let key = (item.hash, index);
        // The guard is a temporary of this statement, so the lock is released
        // before anything is compressed.
        let cell = self.entries().touch(key);
        let mut made = false;
        let compressed = cell
            .get_or_init(|| {
                made = true;
                self.compress(plain)
            })
            .clone();

        // Whoever compressed it accounts for it; a hit takes the lock once.
        if made {
            let mut entries = self.entries();
            match &compressed {
                Some(bytes) => entries.charge(&key, &cell, bytes.len(), self.capacity),
                None => entries.forget(&key, &cell),
            }
        }
        compressed
    }

    /// How many slices this cache has compressed. A slice served from the
    /// cache is not one, so this is the figure that shows the cache working.
    #[must_use]
    pub fn compressions(&self) -> u64 {
        self.compressions.load(Ordering::Relaxed)
    }

    /// Compressed bytes held now, never more than the capacity.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries().bytes
    }

    /// The bookkeeping, locked.
    fn entries(&self) -> MutexGuard<'_, Entries> {
        // A panic elsewhere while the lock was held can leave the byte count a
        // slice out, never a slice under the wrong key, so carrying on beats
        // refusing every client its content.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Compresses one slice, counting it.
    fn compress(&self, plain: &[u8]) -> Option<Arc<[u8]>> {
        self.compressions.fetch_add(1, Ordering::Relaxed);
        match zstd::encode_all(plain, COMPRESSION_LEVEL) {
            Ok(compressed) => Some(compressed.into()),
            Err(err) => {
                tracing::error!("could not compress content slice: {err}");
                None
            }
        }
    }
}

impl Entries {
    /// The cell for `key`, made if new, and marked as asked for just now.
    fn touch(&mut self, key: SliceKey) -> Compressed {
        let stamp = self.clock;
        self.clock += 1;
        self.recency.insert(stamp, key);
        match self.slots.entry(key) {
            Entry::Occupied(mut occupied) => {
                let slot = occupied.get_mut();
                self.recency.remove(&slot.stamp);
                slot.stamp = stamp;
                Arc::clone(&slot.cell)
            }
            Entry::Vacant(vacant) => {
                let cell = Compressed::default();
                vacant.insert(Slot {
                    cell: Arc::clone(&cell),
                    stamp,
                    charged: None,
                });
                cell
            }
        }
    }

    /// Counts a freshly compressed slice against `capacity`, then evicts the
    /// least recently asked for until the cache fits.
    ///
    /// Counted once, and only if the slot is still that cell's: a slice
    /// evicted while it was being compressed is handed to whoever was waiting
    /// for it, but not put back.
    fn charge(&mut self, key: &SliceKey, cell: &Compressed, len: usize, capacity: usize) {
        if let Some(slot) = self.slots.get_mut(key)
            && Arc::ptr_eq(&slot.cell, cell)
            && slot.charged.is_none()
        {
            slot.charged = Some(len);
            self.bytes = self.bytes.saturating_add(len);
        }
        while self.bytes > capacity
            && let Some((_, oldest)) = self.recency.pop_first()
        {
            if let Some(slot) = self.slots.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(slot.charged.unwrap_or(0));
            }
        }
    }

    /// Drops a slice that failed to compress, so the next ask tries again.
    fn forget(&mut self, key: &SliceKey, cell: &Compressed) {
        if self
            .slots
            .get(key)
            .is_some_and(|slot| Arc::ptr_eq(&slot.cell, cell))
            && let Some(slot) = self.slots.remove(key)
        {
            self.recency.remove(&slot.stamp);
            self.bytes = self.bytes.saturating_sub(slot.charged.unwrap_or(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tiamat_core::content::hash_bytes;

    /// One content chunk as it goes on the wire: hash, offset, compressed bytes.
    type Wire = (ContentHash, u64, Vec<u8>);

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("tiamat-content-serve-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write(dir: &Path, relative: &str, bytes: &[u8]) {
        std::fs::write(dir.join(relative), bytes).expect("write");
    }

    /// An index holding one file of `len` bytes of compressible data.
    fn index_with(name: &str, len: usize) -> (ContentIndex, ContentHash, Vec<u8>) {
        let dir = scratch(name);
        let bytes: Vec<u8> = (0..len)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        write(&dir, "asset.png", &bytes);
        let mut index = ContentIndex::new();
        index.add_mod("x", &dir).expect("index");
        let hash = hash_bytes(&bytes);
        (index, hash, bytes)
    }

    /// Reassembles every slice a transfer produces, decompressing as it goes.
    fn drain(transfers: &mut Transfers, index: &ContentIndex) -> Vec<(ContentHash, Vec<u8>)> {
        let mut assembled: Vec<(ContentHash, Vec<u8>)> = Vec::new();
        loop {
            let slices = transfers.next_slices(index, &ContentIndex::new());
            if slices.is_empty() {
                return assembled;
            }
            for message in slices {
                let ServerMessage::ContentChunk {
                    hash,
                    offset,
                    total_len,
                    data,
                } = message
                else {
                    panic!("expected a ContentChunk, got {message:?}");
                };
                let plain = zstd::decode_all(data.as_slice()).expect("decompress");
                let entry = assembled.iter_mut().find(|(existing, _)| *existing == hash);
                match entry {
                    Some((_, buffer)) => {
                        assert_eq!(
                            buffer.len() as u64,
                            offset,
                            "slices must arrive in order with no gaps"
                        );
                        buffer.extend_from_slice(&plain);
                    }
                    None => {
                        assert_eq!(offset, 0, "the first slice of an item must be at offset 0");
                        assert!(total_len > 0 || plain.is_empty());
                        assembled.push((hash, plain));
                    }
                }
            }
        }
    }

    /// Every message a transfer produces until it stops, exactly as sent.
    fn wire(transfers: &mut Transfers, index: &ContentIndex) -> Vec<Wire> {
        let mut sent = Vec::new();
        loop {
            let slices = transfers.next_slices(index, &ContentIndex::new());
            if slices.is_empty() {
                return sent;
            }
            for message in slices {
                let ServerMessage::ContentChunk {
                    hash, offset, data, ..
                } = message
                else {
                    panic!("expected a ContentChunk, got {message:?}");
                };
                sent.push((hash, offset, data));
            }
        }
    }

    /// Bytes zstd cannot shrink. A fixed sequence rather than randomness, so a
    /// failure reproduces.
    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                u8::try_from(state & 0xFF).unwrap_or(0)
            })
            .collect()
    }

    /// An item outside any index, under a made-up hash: the cache takes the
    /// key it is given, so equal bytes under different hashes are different
    /// slices of equal size.
    fn item(tag: u8, bytes: &[u8]) -> ContentItem {
        ContentItem {
            hash: [tag; 32],
            relative_path: String::new(),
            mod_id: String::new(),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn a_small_file_transfers_in_one_slice_and_round_trips() {
        let (index, hash, bytes) = index_with("small", 1024);
        let mut transfers = Transfers::new();
        assert_eq!(transfers.request(&[hash], &index, &ContentIndex::new()), 1);

        let assembled = drain(&mut transfers, &index);
        assert_eq!(assembled.len(), 1);
        assert_eq!(assembled[0].0, hash);
        assert_eq!(
            assembled[0].1, bytes,
            "the bytes must survive the round trip"
        );
        assert_eq!(transfers.queued(), 0);
    }

    #[test]
    fn a_large_file_transfers_in_ordered_slices_and_round_trips() {
        // The reason for slicing at all. `drain` asserts the offsets line up
        // with no gaps, so a mis-stepped offset fails here rather than
        // producing a corrupt file on the client.
        let (index, hash, bytes) = index_with("large", SLICE_BYTES * 3 + 12_345);
        let mut transfers = Transfers::new();
        transfers.request(&[hash], &index, &ContentIndex::new());

        let assembled = drain(&mut transfers, &index);
        assert_eq!(assembled.len(), 1);
        assert_eq!(assembled[0].1.len(), bytes.len());
        assert_eq!(assembled[0].1, bytes);
    }

    #[test]
    fn an_empty_file_sends_one_slice_and_finishes() {
        // The infinite-loop case: offset and total_len are both zero, so a
        // naive `offset < total_len` check never fires and the item never
        // leaves the queue.
        let (index, hash, _) = index_with("empty", 0);
        let mut transfers = Transfers::new();
        transfers.request(&[hash], &index, &ContentIndex::new());

        let first = transfers.next_slices(&index, &ContentIndex::new());
        assert_eq!(first.len(), 1, "an empty file still sends one slice");
        assert_eq!(transfers.queued(), 0, "and then finishes");
        assert!(
            transfers
                .next_slices(&index, &ContentIndex::new())
                .is_empty()
        );
    }

    #[test]
    fn an_unknown_hash_is_refused_without_a_distinguishable_answer() {
        // Answering "no such content" differently from "here it is" turns the
        // server into an oracle for probing what a private mod pack holds.
        let (index, _, _) = index_with("unknown", 64);
        let mut transfers = Transfers::new();

        assert_eq!(
            transfers.request(&[[0xAB; 32]], &index, &ContentIndex::new()),
            0
        );
        assert_eq!(transfers.queued(), 0);
        assert!(
            transfers
                .next_slices(&index, &ContentIndex::new())
                .is_empty(),
            "an unknown hash must produce no message at all"
        );
    }

    #[test]
    fn a_repeated_request_while_queued_does_not_send_the_file_twice() {
        // Otherwise a client looping its request list is an amplifier: cheap to
        // send, expensive to serve.
        let (index, hash, _) = index_with("repeat", 4096);
        let mut transfers = Transfers::new();

        assert_eq!(transfers.request(&[hash], &index, &ContentIndex::new()), 1);
        assert_eq!(
            transfers.request(&[hash, hash, hash], &index, &ContentIndex::new()),
            0,
            "a hash already queued must be ignored"
        );

        let assembled = drain(&mut transfers, &index);
        assert_eq!(assembled.len(), 1, "and it is sent once");
    }

    #[test]
    fn a_request_for_an_item_part_way_through_sending_is_ignored() {
        // Restarting it would send the first slices twice; queueing it again
        // would send the whole file twice. The client has its answer coming.
        let (index, hash, _) = index_with("in-flight", SLICE_BYTES * 4);
        let mut transfers = Transfers::new();
        transfers.request(&[hash], &index, &ContentIndex::new());

        let first = transfers.next_slices(&index, &ContentIndex::new());
        assert_eq!(first.len(), SLICES_PER_PASS, "half the item is out");
        assert_eq!(
            transfers.request(&[hash], &index, &ContentIndex::new()),
            0,
            "the item being sent must not be queued again"
        );
        assert_eq!(transfers.queued(), 1);

        let rest = wire(&mut transfers, &index);
        let offsets: Vec<u64> = rest.iter().map(|(_, offset, _)| *offset).collect();
        let slice = SLICE_BYTES as u64;
        assert_eq!(
            offsets,
            vec![slice * 2, slice * 3],
            "the rest of the item, once, and nothing after it"
        );
    }

    #[test]
    fn a_repeated_request_after_a_failed_delivery_is_accepted_again() {
        // Perf audit B9. The server used to remember every hash it had ever
        // sent and ignore it for the rest of the session, so a client whose
        // accept failed asked again and waited for ever: its join stalled on
        // an asset that would never come.
        let (index, hash, bytes) = index_with("retry", SLICE_BYTES + 100);
        let mut transfers = Transfers::new();
        assert_eq!(transfers.request(&[hash], &index, &ContentIndex::new()), 1);

        // Sent in full — and lost: the client failed to take it.
        let lost = drain(&mut transfers, &index);
        assert_eq!(lost.len(), 1);
        assert_eq!(transfers.queued(), 0);

        assert_eq!(
            transfers.request(&[hash], &index, &ContentIndex::new()),
            1,
            "a client asking again for what it failed to take must be served"
        );
        let again = drain(&mut transfers, &index);
        assert_eq!(
            again,
            vec![(hash, bytes)],
            "the whole item again, from the start"
        );
    }

    #[test]
    fn asking_for_one_item_on_a_loop_stops_at_the_send_cap() {
        // A client that fails the same item every time — or one asking for it
        // on purpose — is served it a few times, not until the quota.
        let (index, hash, _) = index_with("cap", 512);
        let mut transfers = Transfers::new();

        for round in 0..MAX_SENDS_PER_ITEM {
            assert_eq!(
                transfers.request(&[hash], &index, &ContentIndex::new()),
                1,
                "send {round} of the item is within the cap"
            );
            assert_eq!(wire(&mut transfers, &index).len(), 1);
        }
        assert_eq!(
            transfers.request(&[hash], &index, &ContentIndex::new()),
            0,
            "past the cap the item is refused like an unknown one"
        );
        assert!(
            transfers
                .next_slices(&index, &ContentIndex::new())
                .is_empty()
        );
        assert_eq!(transfers.sent_bytes(), 512 * u64::from(MAX_SENDS_PER_ITEM));
    }

    #[test]
    fn the_quota_still_stops_a_retry_loop() {
        // Asking again must not be a way round the quota: with room for one
        // slice left, a client asking on every beat gets that slice and no
        // more, however often it asks.
        let (index, hash, _) = index_with("quota-loop", SLICE_BYTES);
        let mut transfers = Transfers::new();
        transfers.sent_bytes = QUOTA_BYTES - SLICE_BYTES as u64;

        let mut served = 0;
        for _ in 0..100 {
            transfers.request(&[hash], &index, &ContentIndex::new());
            served += wire(&mut transfers, &index).len();
        }
        assert_eq!(
            served, 1,
            "one slice fitted the quota, and nothing else was sent"
        );
        assert!(transfers.over_quota());
        assert_eq!(
            transfers.queued(),
            0,
            "nothing is left to wake the connection's beat for"
        );
    }

    #[test]
    fn a_request_past_the_quota_is_not_even_queued() {
        let (index, hash, _) = index_with("quota-request", SLICE_BYTES);
        let mut transfers = Transfers::new();
        transfers.sent_bytes = QUOTA_BYTES;
        assert_eq!(
            transfers.request(&[hash], &index, &ContentIndex::new()),
            0,
            "a request past the quota is refused rather than queued"
        );
        assert_eq!(transfers.queued(), 0);
        assert!(wire(&mut transfers, &index).is_empty());
    }

    #[test]
    fn crossing_the_quota_mid_pass_drops_the_rest_of_the_queue() {
        let (index, hash, _) = index_with("quota-mid-pass", SLICE_BYTES * 4);
        let mut transfers = Transfers::new();
        transfers.sent_bytes = QUOTA_BYTES - 1;
        transfers.request(&[hash], &index, &ContentIndex::new());
        let slices = transfers.next_slices(&index, &ContentIndex::new());
        assert_eq!(slices.len(), 1, "one slice crossed the quota");
        assert!(transfers.over_quota());
        assert_eq!(transfers.queued(), 0, "the remainder was abandoned");
    }

    #[test]
    fn one_pass_sends_at_most_the_per_pass_budget() {
        // Bounds how much work a single request can cause at once, so a big
        // transfer is slow rather than a stall for everyone else.
        let (index, hash, _) = index_with("budget", SLICE_BYTES * 10);
        let mut transfers = Transfers::new();
        transfers.request(&[hash], &index, &ContentIndex::new());

        let slices = transfers.next_slices(&index, &ContentIndex::new());
        assert_eq!(slices.len(), SLICES_PER_PASS);
        assert!(transfers.queued() > 0, "the transfer should still be going");
    }

    #[test]
    fn a_compressed_slice_fits_the_protocol_chunk_limit() {
        // Incompressible data is the case that matters: zstd adds framing
        // rather than shrinking, so a slice sized at the protocol limit would
        // come out over it.
        let dir = scratch("incompressible");
        let bytes = noise(SLICE_BYTES * 2);
        write(&dir, "noise.png", &bytes);

        let mut index = ContentIndex::new();
        index.add_mod("x", &dir).expect("index");
        let hash = hash_bytes(&bytes);

        let mut transfers = Transfers::new();
        transfers.request(&[hash], &index, &ContentIndex::new());

        loop {
            let slices = transfers.next_slices(&index, &ContentIndex::new());
            if slices.is_empty() {
                break;
            }
            for message in slices {
                let ServerMessage::ContentChunk { data, .. } = message else {
                    panic!("expected a ContentChunk");
                };
                assert!(
                    data.len() <= MAX_CONTENT_CHUNK_BYTES,
                    "a compressed slice of incompressible data was {} bytes, over the \
                     {MAX_CONTENT_CHUNK_BYTES}-byte protocol limit",
                    data.len()
                );
            }
        }
    }

    #[test]
    fn several_files_transfer_in_request_order() {
        let dir = scratch("several");
        write(&dir, "a.png", b"alpha");
        write(&dir, "b.png", b"beta");
        write(&dir, "c.png", b"gamma");
        let mut index = ContentIndex::new();
        index.add_mod("x", &dir).expect("index");

        let order = [
            hash_bytes(b"gamma"),
            hash_bytes(b"alpha"),
            hash_bytes(b"beta"),
        ];
        let mut transfers = Transfers::new();
        assert_eq!(transfers.request(&order, &index, &ContentIndex::new()), 3);

        let assembled = drain(&mut transfers, &index);
        let hashes: Vec<ContentHash> = assembled.iter().map(|(hash, _)| *hash).collect();
        assert_eq!(hashes, order, "items must be served in the order requested");
    }

    #[test]
    fn the_quota_stops_a_client_pulling_without_bound() {
        let (index, hash, _) = index_with("quota", 4096);
        let mut transfers = Transfers::new();
        transfers.request(&[hash], &index, &ContentIndex::new());

        assert!(!transfers.over_quota());
        drain(&mut transfers, &index);
        assert_eq!(transfers.sent_bytes(), 4096);

        // Force the quota to its limit and confirm sending stops, even for an
        // item the client may ask for again. The request is refused rather
        // than queued: there is no later pass in which it could be served, and
        // a queued entry would keep the connection's beat arm awake for ever.
        transfers.sent_bytes = QUOTA_BYTES;
        assert_eq!(transfers.request(&[hash], &index, &ContentIndex::new()), 0);
        assert!(transfers.over_quota());
        assert!(
            transfers
                .next_slices(&index, &ContentIndex::new())
                .is_empty(),
            "a client over quota must not be served more"
        );
    }

    #[test]
    fn a_fresh_transfer_has_nothing_to_send() {
        let (index, _, _) = index_with("fresh", 16);
        let mut transfers = Transfers::new();
        assert!(
            transfers
                .next_slices(&index, &ContentIndex::new())
                .is_empty()
        );
        assert_eq!(transfers.sent_bytes(), 0);
    }

    #[test]
    fn two_clients_get_byte_identical_slices_compressed_once() {
        // Perf audit N5: every connection compressed every slice for itself,
        // so a crowd joining at once compressed the same pack once each.
        let (index, hash, bytes) = index_with("shared", SLICE_BYTES * 2 + 777);
        let cache = Arc::new(SliceCache::new(CACHE_BYTES));
        let mut first = Transfers::with_cache(Arc::clone(&cache));
        let mut second = Transfers::with_cache(Arc::clone(&cache));
        first.request(&[hash], &index, &ContentIndex::new());
        second.request(&[hash], &index, &ContentIndex::new());

        let one = wire(&mut first, &index);
        assert_eq!(one.len(), 3);
        assert_eq!(cache.compressions(), 3, "the first client's three slices");

        let two = wire(&mut second, &index);
        assert_eq!(one, two, "both clients must be sent the same bytes");
        assert_eq!(
            cache.compressions(),
            3,
            "the second client was served from the cache, compressing nothing"
        );

        let joined: Vec<u8> = two
            .iter()
            .flat_map(|(_, _, data)| zstd::decode_all(data.as_slice()).expect("decompress"))
            .collect();
        assert_eq!(joined, bytes, "and the shared slices still round trip");
    }

    #[test]
    fn a_slice_asked_for_twice_is_one_shared_allocation() {
        let cache = SliceCache::new(CACHE_BYTES);
        let asset = item(1, &noise(1000));

        let first = cache.slice(&asset, 0).expect("compressed");
        let second = cache.slice(&asset, 0).expect("cached");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the second ask must be handed the first ask's bytes, not a copy"
        );
        assert_eq!(cache.compressions(), 1);
        assert_eq!(cache.bytes(), first.len());
    }

    #[test]
    fn joiners_asking_for_one_slice_at_once_compress_it_once() {
        // The join storm: clients step through a pack at one pace, so they ask
        // for the same slice on the same beat. The second asker must wait for
        // the first's result rather than compress a copy of its own.
        let cache = SliceCache::new(CACHE_BYTES);
        let asset = item(4, &noise(SLICE_BYTES));
        let start = std::sync::Barrier::new(8);
        let slices: Vec<Arc<[u8]>> = std::thread::scope(|scope| {
            let joiners: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        cache.slice(&asset, 0).expect("compressed")
                    })
                })
                .collect();
            joiners
                .into_iter()
                .map(|joiner| joiner.join().expect("joiner"))
                .collect()
        });
        assert_eq!(
            cache.compressions(),
            1,
            "eight asked at once, one compressed"
        );
        assert!(
            slices.iter().all(|slice| Arc::ptr_eq(slice, &slices[0])),
            "and all eight were handed the same bytes"
        );
        assert_eq!(cache.bytes(), slices[0].len(), "counted once");
    }

    #[test]
    fn a_slice_past_the_end_of_an_item_is_none_and_costs_nothing() {
        let cache = SliceCache::new(CACHE_BYTES);
        let asset = item(2, &noise(1000));
        assert!(cache.slice(&asset, 1).is_none());
        assert!(
            cache.slice(&asset, usize::MAX).is_none(),
            "no overflow either"
        );
        assert_eq!(cache.compressions(), 0);
        assert_eq!(cache.bytes(), 0);

        // An empty item has exactly one slice, the empty one.
        let empty = item(3, &[]);
        let slice = cache.slice(&empty, 0).expect("the empty slice");
        assert!(zstd::decode_all(&slice[..]).expect("decompress").is_empty());
        assert!(cache.slice(&empty, 1).is_none());
    }

    #[test]
    fn the_cache_evicts_the_least_recently_asked_for_slice_past_its_bound() {
        // Equal bytes under three hashes: three slices of one size, room for
        // two and a half.
        let bytes = noise(4096);
        let size = SliceCache::new(CACHE_BYTES)
            .slice(&item(9, &bytes), 0)
            .expect("probe")
            .len();
        let cache = SliceCache::new(size * 5 / 2);
        let (a, b, c) = (item(10, &bytes), item(11, &bytes), item(12, &bytes));

        cache.slice(&a, 0).expect("a");
        cache.slice(&b, 0).expect("b");
        cache.slice(&a, 0).expect("a again, now the most recent");
        assert_eq!(cache.compressions(), 2);

        cache.slice(&c, 0).expect("c");
        assert_eq!(cache.compressions(), 3);
        assert_eq!(cache.bytes(), size * 2, "one slice was evicted to fit");

        cache.slice(&a, 0).expect("a");
        assert_eq!(
            cache.compressions(),
            3,
            "a was asked for more recently than b"
        );
        cache.slice(&b, 0).expect("b");
        assert_eq!(cache.compressions(), 4, "b was the one evicted");
        assert!(cache.bytes() <= size * 5 / 2, "never over the bound");
    }

    #[test]
    fn the_shared_cache_is_one_cache_while_anyone_holds_it() {
        let one = SliceCache::shared();
        let two = SliceCache::shared();
        assert!(Arc::ptr_eq(&one, &two));
        assert!(Arc::ptr_eq(&one, &Transfers::new().slices));
    }
}
