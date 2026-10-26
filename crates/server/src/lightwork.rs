// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only
//! The light thread: relighting off the tick (milestone 0.5.0, P1).
//!
//! # Why
//!
//! The fen soak of 2026-10-12 put nine tenths of a sprinting player's tick
//! in relighting the chunks served to them — 0.2 to 0.5 ms a chunk, thirty
//! or so a tick, on the one thread every player shares. Four players
//! sprinting would have filled the budget with light alone, before a
//! minimum-spec server's factor of three. Light is derived from blocks, is
//! never persisted, and reaches the client as its own message after the
//! chunk; nothing in the tick waits on it. So it moves here.
//!
//! # How it stays the same light
//!
//! The tick does not relight. It records what happened — a chunk arrived, a
//! block changed, a chunk left memory — in the order it happened, and at
//! the end of the tick hands this thread one [`Batch`] per space: the events
//! and a [`crate::world::World::snapshot`] of that space's chunks as they
//! stood when the tick ended. The thread runs the same passes the tick used
//! to run, in the same order, over those blocks. The result is a function
//! of the blocks and the order alone, so it is the light the tick would have
//! computed, computed later — a few milliseconds later, which a client
//! already tolerates, having always received the light after the chunk.
//!
//! Batches from one tick are processed after batches from the tick before,
//! on one thread, so the order across ticks is the tick order too. An edit
//! on tick T+1 to a chunk whose arrival on tick T is still waiting here is
//! relit twice, in order, and the second pass sees the edit.
//!
//! # What the tick still does
//!
//! Loads a served chunk's fluid before the tick ends, so the lava a relight
//! glows by is in the pond store when this thread reads it; decides whether
//! a chunk is sealed from its palette alone ([`crate::light::DarkSolid`]),
//! which needs no light; and answers a mod's `game.get_light` from the
//! store behind its lock, which this thread holds for one pass at a time so
//! a reader waits a relight at most.
use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tiamat_core::proto::ServerMessage;
use tiamat_core::{BlockPos, Chunk, ChunkPos};
use tracing::{debug, warn};

use crate::light::{Dry, Glowing, Lights, Snapshot};
use crate::sim::Control;
use crate::transport::endpoint::Shared;

/// One thing the tick saw happen to a space's chunks, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A chunk came into memory: generated, loaded, or served.
    Arrived(ChunkPos),
    /// A block's content changed.
    Edited(BlockPos),
    /// A chunk left memory, and its light goes with it.
    Forget(ChunkPos),
}

/// Everything that happened to one space's light in one tick, with the
/// blocks it happened to.
#[derive(Debug)]
pub struct Batch {
    /// The tick it is from, for the log.
    pub tick: u64,
    /// The space.
    pub domain: String,
    /// That space's chunks as they stood when the tick ended.
    pub terrain: Arc<HashMap<ChunkPos, Arc<Chunk>>>,
    /// What happened, in order.
    pub events: Vec<Event>,
}

/// The events of one tick, per space, collected as the tick runs.
#[derive(Debug, Default)]
pub struct Events {
    by_domain: std::collections::BTreeMap<String, Vec<Event>>,
}

impl Events {
    /// Records one event in `domain`.
    pub fn push(&mut self, domain: &str, event: Event) {
        self.by_domain
            .entry(domain.to_owned())
            .or_default()
            .push(event);
    }

    /// Whether anything was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_domain.is_empty()
    }

    /// Turns the tick's events into one batch per space, with a snapshot of
    /// the chunks each event can read, and clears them.
    ///
    /// **The neighbourhood, not the space.** A relight reads its own chunk, its
    /// six face neighbours and (for the roof pass) the chunk below; an edit's
    /// light cannot cross more than one chunk because the light radius is
    /// smaller than a chunk. Snapshotting all resident chunks for one edit was
    /// 0.2–0.3 ms at 5.8k and ~10 ms at 100k, every tick anyone moved; this is
    /// at most 27 chunks per edit and 36 per arrival (the extra layer is the
    /// roof pass's chunk below), deduplicated.
    pub fn into_batches(self, tick: u64, world: &crate::world::World) -> Vec<Batch> {
        self.by_domain
            .into_iter()
            .map(|(domain, events)| {
                let mut positions = BTreeSet::new();
                for event in &events {
                    let (at, lowest) = match *event {
                        Event::Arrived(pos) => (pos, -2),
                        Event::Edited(pos) => (pos.chunk(), -1),
                        Event::Forget(pos) => (pos, 0),
                    };
                    // A forget needs no blocks: the layer is dropped by the
                    // event itself. Everything else gets the 3×3 face ring; an
                    // arrival also gets the chunk BELOW (the roof pass centres
                    // there) and therefore one layer below that, since the
                    // roof pass reads its own face ring.
                    if matches!(event, Event::Forget(_)) {
                        continue;
                    }
                    for dx in -1..=1 {
                        for dy in lowest..=1 {
                            for dz in -1..=1 {
                                positions.insert(ChunkPos::new(
                                    at.x.saturating_add(dx),
                                    at.y.saturating_add(dy),
                                    at.z.saturating_add(dz),
                                ));
                            }
                        }
                    }
                }
                Batch {
                    tick,
                    terrain: world.snapshot_of(&domain, &positions),
                    domain,
                    events,
                }
            })
            .collect()
    }
}

/// The thread, and the channel the tick feeds it.
pub struct Worker {
    sender: Option<mpsc::Sender<Batch>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    /// Starts the thread.
    ///
    /// # Errors
    ///
    /// The operating system's, if a thread cannot be made.
    pub fn spawn(
        lighting: Arc<RwLock<Lights>>,
        fluidics: Arc<RwLock<crate::fluid::Ponds>>,
        shared: Arc<Shared>,
        control: Control,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<Batch>();
        let handle = std::thread::Builder::new()
            .name("light".to_owned())
            .spawn(move || {
                // Light is integer arithmetic, but this is a simulation
                // thread and every one of them asserts the mode at spawn
                // (charter rule 4).
                tiamat_core::assert_ieee_mode();
                for batch in receiver {
                    process(&batch, &lighting, &fluidics, &shared, &control);
                }
                debug!("the light thread stopped");
            })?;
        Ok(Self {
            sender: Some(sender),
            handle: Some(handle),
        })
    }

    /// Hands the thread a tick's batch. A thread that has stopped drops it,
    /// which can only happen during shutdown.
    pub fn send(&self, batch: Batch) {
        if let Some(sender) = &self.sender
            && sender.send(batch).is_err()
        {
            warn!("the light thread is gone; a tick's light was dropped");
        }
    }

    /// Stops the thread once it has finished what it was handed.
    pub fn stop(mut self) {
        drop(self.sender.take());
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            warn!("the light thread panicked");
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Runs one tick's events for one space.
///
/// **The lighting lock is taken per event, not per batch**, so a reader on
/// the tick — a mod asking `game.get_light` — waits for one pass and never
/// for a whole tick's worth. The fluid lock is read under it, in the order
/// every lighting site has always used.
fn process(
    batch: &Batch,
    lighting: &RwLock<Lights>,
    fluidics: &RwLock<crate::fluid::Ponds>,
    shared: &Shared,
    control: &Control,
) {
    let started = Instant::now();
    let terrain = Snapshot(&batch.terrain);
    let mut touched: BTreeSet<ChunkPos> = BTreeSet::new();
    let mut relit = 0u64;
    for event in &batch.events {
        let Ok(mut lit) = lighting.write() else {
            return;
        };
        let Ok(ponds) = fluidics.read() else {
            return;
        };
        let dry = Dry;
        let glow: &dyn Glowing = ponds
            .get(&batch.domain)
            .map_or(&dry as &dyn Glowing, |fluid| fluid as &dyn Glowing);
        let light = lit.of(&batch.domain);
        match *event {
            Event::Arrived(pos) => {
                // Already lit — by an earlier arrival of the same chunk for
                // another player, or an edit that reached it first — and a
                // relight would compute the answer it already has.
                if !light.holds(pos) {
                    control.note_full_relight();
                    relit += 1;
                    touched.extend(light.chunk_loaded_in(terrain, pos, glow));
                }
            }
            Event::Edited(pos) => {
                touched.extend(light.edited_in(terrain, pos, glow));
            }
            Event::Forget(pos) => {
                light.forget(pos);
                touched.remove(&pos);
            }
        }
    }
    // The broadcast reads the layers, so it is under the lock too — one
    // more take, after the passes, for every chunk the batch touched.
    if let Ok(lit) = lighting.read() {
        if let Some(light) = lit.get(&batch.domain) {
            for pos in &touched {
                let Some(layer) = light.layer(*pos) else {
                    continue;
                };
                shared.broadcast_in(
                    &batch.domain,
                    ServerMessage::ChunkLight {
                        pos: *pos,
                        light: tiamat_core::light::codec::encode(layer),
                    },
                );
            }
        }
        control.note_lit_chunks(lit.len());
    }
    control.note_light_work(started.elapsed(), relit, batch.events.len() as u64);
    if started.elapsed() > tiamat_core::tick::TICK_DURATION {
        debug!(
            tick = batch.tick,
            domain = %batch.domain,
            events = batch.events.len(),
            relit,
            took_ms = started.elapsed().as_secs_f64() * 1000.0,
            "a tick's light took longer than a tick"
        );
    }
}
