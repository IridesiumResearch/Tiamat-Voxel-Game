// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The update rule.
//!
//! # What it is
//!
//! **Conserved flow**, stated as four rules applied in a fixed order to one
//! block at a time. Sub-Node Contract §4.2 is authoritative; this implements it.
//!
//! 1. **Down first.** Move as much volume as the block below will accept.
//! 2. **Sideways.** For each horizontal neighbour holding less, lowest first,
//!    move half the difference. A difference of one moves nothing, so a pond
//!    **settles without a separate stability test** — that is what makes this
//!    terminate.
//! 3. **Stuck droplets.** One or two cells cannot split, so on a slope they
//!    would streak forever. They move whole, or not at all.
//! 4. **Sinks.** Absorption into ground a mod declared absorbent, and
//!    evaporation from a block open to the air.
//!
//! # Conservation, and why the sinks are counted
//!
//! Volume is never created. It leaves only through a sink, and [`Sinks`] records
//! how much left through each — charter rule 15's conservation proptest is
//! unwritable otherwise, because "volume in equals volume out" is false by
//! design and "volume in equals volume out plus what was absorbed plus what
//! evaporated" is the invariant that actually holds.
//!
//! # Determinism
//!
//! Charter rule 4. The active set is a `BTreeSet`, so blocks are visited in
//! coordinate order rather than insertion order; neighbours are visited in a
//! fixed order rotated by **the block's own coordinates**; every arithmetic
//! operation here is on integers.
//!
//! **The rotation is coordinate-derived and not tick-derived**, which matters
//! twice. A tick counter has to be persisted or a reloaded world diverges from a
//! fresh one, and a tick-derived rotation makes every block in the world favour
//! the same side on the same tick — visible as a pulse crossing a large pond.
//!
//! **A `HashSet` here would be a bug that CI catches on the third platform.**
//! Rust's default hasher is randomly seeded per process, so an active set built
//! on one would not even be stable between two runs on one machine.
//!
//! # Why blocks are queued rather than scanned
//!
//! A settled world costs nothing. Only blocks that an edit touched, or that a
//! flow is currently moving through, are in the active set; a pond that has
//! finished spreading has an empty one, and the whole system drops out of the
//! tick. That is the property the perf criterion asserts, and it is why the
//! solver is written around a work queue instead of a per-chunk sweep.

use std::collections::BTreeSet;

use crate::coords::BlockPos;
use crate::detgen::rng::SplitMix64;

use super::{Fluid, FluidId, capacity};

/// The four lateral directions, in the order they are visited before rotation.
///
/// Down is handled separately and first, because falling beats spreading. Up is
/// never a flow direction — this fluid does not climb — and is absent.
const LATERAL: [[i32; 3]; 4] = [[-1, 0, 0], [1, 0, 0], [0, 0, -1], [0, 0, 1]];
/// The block under, for the one case [`record_blocked`] looks down.
const BELOW: [i32; 3] = [0, -1, 0];

/// The world, as the fluid solver needs to see it.
///
/// Deliberately the same shape as [`crate::light::propagate::Neighbourhood`] and
/// for the same reason: the server has a world database behind a cache and the
/// client has a map of streamed chunks, and one update rule has to run over
/// both or the client cannot predict what the server will do.
pub trait Neighbourhood {
    /// How full a block is of terrain, in cells of 27.
    ///
    /// **Sub-Node Contract §4.** The world reports a fact and the fluid decides
    /// what it means: [`capacity`] turns this and the fluid's own `waterlogs_at`
    /// into how much will fit. Two fluids in one world may disagree about what
    /// counts as floor, which is why the threshold lives with the fluid and this
    /// does not.
    ///
    /// `None` for anything not loaded — NOT zero, and the difference matters.
    /// Zero would let a flood run off the edge of the loaded world and a pond
    /// drain silently into a chunk that has not arrived.
    fn occupancy(&self, pos: BlockPos) -> Option<u32>;

    /// How many cells of `fluid` this block soaks up per fluid tick, or zero.
    ///
    /// **A fact about the block, not a policy.** Which materials are absorbent,
    /// how much they take, which fluid they take it of and what they turn into
    /// when they have had it are the mod's (charter rule 1); this module knows
    /// only the number, exactly as it knows occupancy and not what the block
    /// is made of. The fluid is passed because a mod may say "this ground
    /// drinks rainwater and not the sea", and the answer is then a number for
    /// one and zero for the other. The material swap is applied by whoever is
    /// holding the registry, from the [`Sinks::absorbed`] events this
    /// produces.
    ///
    /// Zero for anything not loaded, because the caller has already established
    /// loadedness through [`Neighbourhood::occupancy`] before asking.
    fn absorbency(&self, pos: BlockPos, fluid: FluidId) -> u32 {
        let _ = (pos, fluid);
        0
    }

    /// Whether fluid entering this block sweeps its terrain away: a plant.
    ///
    /// **A fact about the block, like [`Self::absorbency`]**, and this module
    /// knows nothing about what it is made of. The world mod's grass, ferns
    /// and flowers are `passable`, so a flood runs THROUGH them and leaves
    /// them standing in the water — the mod could not see it happen, because
    /// a flow into a block nothing blocked is not a blocked flow (World ask
    /// 37). The clearing is applied by whoever holds the registry, from the
    /// positions this produces ([`Solver::take_washed`]), exactly as the
    /// absorbed material swap is.
    ///
    /// Answered for a block whose occupied cells are ALL such a material, so
    /// clearing it cannot take a wall's cells with the fern growing on it.
    fn washes_away(&self, pos: BlockPos) -> bool {
        let _ = pos;
        false
    }

    /// What a block holds now.
    fn fluid(&self, pos: BlockPos) -> Fluid;

    /// Records what a block holds.
    ///
    /// Positions outside what the implementation holds are dropped rather than
    /// being an error, exactly as light does: a flow reaching the edge of the
    /// loaded region has nowhere to write.
    fn set_fluid(&mut self, pos: BlockPos, value: Fluid);
}

/// How one fluid behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuning {
    /// How full a block has to be before this fluid treats it as floor, in
    /// cells of 27.
    ///
    /// **The number that makes fluid work on smoothed terrain.** Before it,
    /// §4 read "accepts fluid iff empty", which made a single chiselled cell
    /// waterproof — defensible on blocky terrain and wrong on the terrain this
    /// engine is for. A sub-node-smoothed hillside is a ramp INSIDE blocks, so
    /// every column's top block is `Partial`, nothing below any of them was a
    /// drop, and the solver saw a perfectly flat floor: milk spread as a disc
    /// across a hillside, floating above ground that was smooth beneath it.
    ///
    /// A mod that wants the old behaviour registers `waterlogs_at = 1`.
    pub waterlogs_at: u32,
    /// Fluid ticks between updates of this fluid.
    ///
    /// One is every fluid tick. Larger is slower and thicker — and it is the
    /// only knob that changes how fast a pour is SEEN to run, which is the
    /// first thing anybody watching a fluid has an opinion about.
    pub tick_rate: u8,
    /// One in how many fluid ticks an exposed block loses a cell, or zero.
    ///
    /// **A declared sink** (Sub-Node Contract §4.3). Only a block with air
    /// directly above it evaporates, so a wide shallow pool goes before a deep
    /// narrow one. Zero never evaporates.
    pub evaporates: u32,
    /// Whether this fluid sweeps away a block that declares `washes_away`.
    ///
    /// **True, because that is what ask 37 was for**: water running into a
    /// tuft of grass clears it. But a fluid is not only rivers — the weather
    /// mod's rainwater is a fluid, its puddles spread a few cells on open
    /// ground, and every shower would strip the meadow it fell on. So the
    /// fluid's author says whether theirs is the washing kind; the plant's
    /// author cannot be expected to list every fluid in the world (World ask
    /// 38, Weather ask W14 — the same ask, filed by two mods a day apart).
    pub washes: bool,
}

impl Tuning {
    /// What milk uses, and a sensible default for a fluid that behaves like it.
    pub const DEFAULT: Self = Self {
        // Fourteen of 27 — over half. Under it the block is more air than
        // anything and fluid runs through; at or above it, it is more solid
        // than not and holds the fluid up.
        waterlogs_at: 14,
        tick_rate: 1,
        // Off. Destroying matter is a mod's call, exactly as creating it was.
        evaporates: 0,
        // A fluid sweeps a plant away unless its author says otherwise, which
        // is what `washes_away` on the block means by itself.
        washes: true,
    };
}

/// Every fluid's tuning, by id.
///
/// **One set of settings per FLUID, which for a long time it was not.** The
/// solver took one `Tuning` for the whole tick, read from whichever fluid had
/// registered first — alphabetically by qualified id, across every loaded mod
/// — so `core_milk:milk`'s `tick_rate = 4` governed a world's water and lava
/// too, and "rainwater evaporates, the sea does not" was not expressible. Now
/// every question the solver asks about a block is asked of the tuning of the
/// fluid IN that block, which is the only reading that makes a registration
/// mean something.
///
/// Indexed by [`FluidId`], with [`Tuning::DEFAULT`] for the ids nobody has
/// registered — a placeholder's rules left with the mod that knew them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunings {
    by_id: Vec<Tuning>,
    /// Which ids a mod actually registered. The gate in [`Tunings::any_due`]
    /// looks only at these: the defaulted slots run at rate one and would
    /// otherwise make every tick "due" for a world whose only fluid is slow.
    registered: Vec<bool>,
}

impl Default for Tunings {
    /// Every fluid at [`Tuning::DEFAULT`]: a store built before any registry.
    fn default() -> Self {
        Self::uniform(Tuning::DEFAULT)
    }
}

impl Tunings {
    /// The same tuning for every fluid: a one-fluid world, or a test.
    #[must_use]
    pub fn uniform(tuning: Tuning) -> Self {
        Self {
            by_id: vec![tuning; super::MAX_FLUIDS + 1],
            registered: vec![true; super::MAX_FLUIDS + 1],
        }
    }

    /// Builds the table from `(id, tuning)` pairs; anything unnamed is the
    /// default.
    #[must_use]
    pub fn from_pairs(entries: impl IntoIterator<Item = (FluidId, Tuning)>) -> Self {
        let mut by_id = vec![Tuning::DEFAULT; super::MAX_FLUIDS + 1];
        let mut registered = vec![false; super::MAX_FLUIDS + 1];
        for (id, tuning) in entries {
            if let Some(slot) = by_id.get_mut(usize::from(id.0)) {
                *slot = tuning;
                registered[usize::from(id.0)] = true;
            }
        }
        Self { by_id, registered }
    }

    /// One fluid's tuning.
    #[must_use]
    pub fn of(&self, fluid: FluidId) -> Tuning {
        self.by_id
            .get(usize::from(fluid.0))
            .copied()
            .unwrap_or(Tuning::DEFAULT)
    }

    /// Whether any fluid is due to move on this fluid tick.
    ///
    /// The gate the server keeps in front of the whole pass: a world whose
    /// every fluid runs at rate four does nothing on three ticks in four, and
    /// should not pay for the queue walk to find that out.
    #[must_use]
    pub fn any_due(&self, fluid_tick: u64) -> bool {
        let mut any_registered = false;
        for (tuning, registered) in self.by_id.iter().zip(&self.registered) {
            if !registered {
                continue;
            }
            any_registered = true;
            if fluid_tick.is_multiple_of(u64::from(tuning.tick_rate.max(1))) {
                return true;
            }
        }
        // Nothing registered at all: a world with no fluid, or a placeholder's
        // orphaned milk. Let the pass run; it has nothing to do and says so.
        !any_registered
    }

    /// Whether every registered fluid is due on this fluid tick — a *full*
    /// tick, on which the queue is worked exactly as a one-fluid world works
    /// it. See the empty-block rule in [`Solver::tick`].
    #[must_use]
    pub fn all_due(&self, fluid_tick: u64) -> bool {
        self.by_id
            .iter()
            .zip(&self.registered)
            .filter(|(_, registered)| **registered)
            .all(|(tuning, _)| fluid_tick.is_multiple_of(u64::from(tuning.tick_rate.max(1))))
    }
}

/// One block's worth of change, for whoever needs to hear about it.
///
/// The server broadcasts these and the client applies them; both also use them
/// to decide which chunks need re-meshing. Carrying the previous value as well
/// as the new one means a listener can tell "a puddle appeared" from "a puddle
/// got deeper" without holding its own copy of the world.
///
/// **A conserved transfer produces two of these**, one for each end. The old
/// model changed one block at a time; this one cannot, and a listener that
/// assumed otherwise would draw half of every flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flow {
    /// Where it happened.
    pub pos: BlockPos,
    /// What was there.
    pub was: Fluid,
    /// What is there now.
    pub now: Fluid,
}

/// A flow that did not happen, and where it was stopped.
///
/// # What this is for
///
/// The interesting thing about a fluid is not only where it went but where it
/// *tried* to go. A mod that wants waterlogging — a block that changes when milk
/// reaches it — has no way to find out that milk is pressing against something
/// unless the engine says so: the fluid layer records where milk IS, and a block
/// milk cannot enter is by definition somewhere it is not.
///
/// Reported per lateral direction rather than per block, because "which side is
/// wet" is the question a mod is actually asking.
///
/// The blocking block's material is deliberately NOT here. This module knows
/// occupancy and nothing about materials — [`Neighbourhood`] is the whole of its
/// view of the world — and a `MaterialId` is only meaningful next to the
/// registry that issued it (charter rule 8). The server looks it up when it
/// hands the event to a mod, where the right registry is in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocked {
    /// The block holding the fluid.
    pub from: BlockPos,
    /// The block it could not get into.
    pub into: BlockPos,
    /// Which fluid was pressing.
    pub fluid: FluidId,
    /// How much it was pressing with, in cells.
    pub volume: u32,
    /// The OTHER fluid in `into`, when that is what stopped it; `None` for
    /// terrain. Two fluids never share a block, so this is a meeting.
    pub meets: Option<FluidId>,
}

/// One block soaking up fluid.
///
/// The block's material is not here for the same reason [`Blocked`]'s is not:
/// the solver has no registry. Whoever holds one turns this into "dirt becomes
/// damp dirt".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Absorbed {
    /// The block that soaked it up.
    pub pos: BlockPos,
    /// Which fluid it took.
    pub fluid: FluidId,
    /// How many cells it took.
    pub cells: u32,
}

/// Where volume went when it left the world.
///
/// **Every cell destroyed is counted here.** Sub-Node Contract §4.3: the
/// conservation invariant is `in == still present + absorbed + evaporated +
/// displaced`, and a solver that quietly dropped a cell would make that
/// unwritable rather than merely false.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sinks {
    /// Blocks that soaked fluid up, and how much each took.
    pub absorbed: Vec<Absorbed>,
    /// Cells lost to the air.
    pub evaporated: u32,
    /// Cells that had nowhere to go when terrain took their space.
    ///
    /// Somebody filling a flooded block with stone displaces what was in it.
    /// The engine pushes it out first and only destroys what nothing would
    /// accept — but destroy it it must, and an uncounted loss here is a
    /// conservation test that fails for the wrong reason.
    pub displaced: u32,
}

impl Sinks {
    /// Total cells destroyed.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.absorbed.iter().map(|entry| entry.cells).sum::<u32>()
            + self.evaporated
            + self.displaced
    }

    /// Whether nothing was destroyed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.absorbed.is_empty() && self.evaporated == 0 && self.displaced == 0
    }
}

/// The active set, and the rule that drains it.
///
/// Holds no world of its own — [`Neighbourhood`] is the world — so a server and
/// a client can each keep one over their own storage.
#[derive(Debug, Default, Clone)]
pub struct Solver {
    /// Blocks whose state might need to change, in coordinate order.
    active: BTreeSet<BlockPos>,
    /// Blocks left over from a tick that hit its cap.
    ///
    /// Carried rather than dropped: a pour that overruns its budget finishes
    /// next tick instead of leaving milk half-spread forever.
    carried: BTreeSet<BlockPos>,
    /// Flows that could not happen, for the `on_fluid_flow` hook.
    ///
    /// Drained by the caller with [`Solver::take_blocked`] rather than returned
    /// from `tick`: it is an observation channel and not part of what the solver
    /// did, and a caller with no mods listening should be able to ignore it
    /// without the signature saying otherwise.
    ///
    /// **Capped at [`BLOCKED_PER_TICK`] and dropped rather than carried**, which
    /// is the opposite of what `carried` does and is deliberate. An unfinished
    /// flow must be finished or the world is wrong; an unreported block is a
    /// notification nobody got, and the shoreline it describes will still be
    /// there next time the pond is examined. Carrying them would let a mod that
    /// is slow to handle them grow an unbounded queue inside the tick.
    blocked: Vec<Blocked>,
    /// Blocks whose terrain a flow swept away this tick — World ask 37.
    ///
    /// Positions rather than edits, for the reason `Sinks::absorbed` is: this
    /// module cannot name a material, and clearing a block is the caller's to
    /// do. Bounded by construction, because a position only lands here when a
    /// transfer happens and transfers are capped per tick.
    washed: Vec<BlockPos>,
    /// Where volume went when it left the world, since this was last drained.
    ///
    /// Accumulated rather than returned per tick because the caller that
    /// applies material swaps and the caller that checks conservation are the
    /// same caller, and both want it after the tick rather than during.
    sinks: Sinks,
}

/// How many blocked flows one tick will report.
///
/// A cap on the HOOK's cost, not on the solver's — the visit budget already
/// bounds that. Task 11 asks for `on_fluid_flow` to be budgeted, and this is
/// where: a mod's callback runs once per entry, so an ocean meeting a continent
/// must not be able to hand the script VM ten thousand events in one tick.
///
/// Sixty-four is roughly the perimeter of an eight-block pond, so an ordinary
/// pool reports its whole shoreline in a tick and only something enormous is
/// sampled rather than enumerated.
const BLOCKED_PER_TICK: usize = 64;

/// The most cells a single block can hold and still be a droplet.
///
/// **Sub-Node Contract §4.2 rule 3.** Half of anything at or below this rounds
/// down to nothing, so a droplet cannot split and would streak down a slope for
/// ever. Two cells rather than one because `(2 - 0) / 2` is one, which leaves a
/// single cell behind — the streak, one tick later.
const DROPLET: u32 = 2;

impl Solver {
    /// An empty solver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a block and everything that could be affected by it changing.
    ///
    /// Called for every edit — a block broken, a block placed, milk poured —
    /// and for the fluid's own neighbours when it moves. The six-neighbour halo
    /// is what makes a wall being knocked out wake the pond behind it.
    pub fn touch(&mut self, pos: BlockPos) {
        self.active.insert(pos);
        self.active.insert(BlockPos::new(pos.x, pos.y - 1, pos.z));
        self.active.insert(BlockPos::new(pos.x, pos.y + 1, pos.z));
        for offset in LATERAL {
            self.active.insert(BlockPos::new(
                pos.x + offset[0],
                pos.y + offset[1],
                pos.z + offset[2],
            ));
        }
    }

    /// How many blocks are waiting.
    ///
    /// Zero for a settled world, which is the assertion the perf criterion
    /// makes: milk that has finished moving costs nothing at all.
    #[must_use]
    pub fn active(&self) -> usize {
        self.active.len() + self.carried.len()
    }

    /// Whether there is nothing to do.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.active.is_empty() && self.carried.is_empty()
    }

    /// Takes the flows that could not happen since this was last called.
    pub fn take_blocked(&mut self) -> Vec<Blocked> {
        std::mem::take(&mut self.blocked)
    }

    /// Takes the blocks a flow has swept the terrain out of — World ask 37.
    ///
    /// The caller clears each one as a dig would. Deduplicated: two flows into
    /// one plant are one clearing.
    pub fn take_washed(&mut self) -> Vec<BlockPos> {
        std::mem::take(&mut self.washed)
    }

    /// Takes what has been destroyed since this was last called.
    ///
    /// The caller applies the material swaps [`Sinks::absorbed`] describes, and
    /// a conservation test adds [`Sinks::total`] to what is still in the world.
    pub fn take_sinks(&mut self) -> Sinks {
        std::mem::take(&mut self.sinks)
    }

    /// Runs one fluid tick, visiting at most `budget` blocks.
    ///
    /// Returns every change made, in the order it was made, for broadcasting and
    /// re-meshing. Blocks not reached within the budget are carried to the next
    /// tick rather than dropped.
    ///
    /// `seed` is the world seed and `fluid_tick` the tick number; together with
    /// a block's position they are the whole of the randomness evaporation uses
    /// (charter rule 4). A process RNG here would fail the cross-platform hash
    /// gate — or worse, would not, and two servers would drift.
    ///
    /// # The budget is a cap on VISITS, not on changes
    ///
    /// A block that is examined and left alone still costs a lookup, and the
    /// pathological case — a settled pond re-queued by an edit — is all
    /// examinations and no changes. Counting changes would let that case run
    /// unbounded, which is exactly the tick overrun the cap is there to prevent.
    pub fn tick(
        &mut self,
        world: &mut impl Neighbourhood,
        tunings: &Tunings,
        budget: usize,
        seed: u64,
        fluid_tick: u64,
    ) -> Vec<Flow> {
        let mut changes = Vec::new();
        // Last tick's leftovers first, so a block cannot be starved forever by
        // a pour that keeps re-queueing its own neighbourhood.
        let mut pending = std::mem::take(&mut self.carried);
        pending.extend(std::mem::take(&mut self.active));

        let full_tick = tunings.all_due(fluid_tick);
        let mut visited = 0;
        let mut woken = BTreeSet::new();
        for pos in pending {
            if visited >= budget {
                self.carried.insert(pos);
                continue;
            }
            // **A fluid's own rate.** A block holding a fluid whose tick this
            // is not is put back for the tick that is, still active — a
            // viscous fluid is slow, not stopped.
            //
            // **Before the visit is counted, and that is not tidiness.** A
            // deferral is one lookup, and counting it as a visit starved a
            // puddle: on a slow fluid's off-ticks the pending set does not
            // shrink, so the same blocks spent the same budget every tick and
            // a block past the budget line was never looked at on the tick
            // that mattered. One cell of milk stood on open ground for ever.
            //
            // **An empty block waits for a full tick.** A block woken empty —
            // a neighbour of something that moved — is kept in the queue until
            // every fluid is due, because on that tick it may be FILLED by a
            // neighbour settled before it and then settled itself in the same
            // pass. That same-tick cascade is what a one-fluid world always
            // had, since its off-ticks ran nothing at all; a mixed world that
            // looked at the empties on a fast fluid's ticks dropped them, lost
            // the cascade, and left one cell of a slow fluid standing on
            // level soaked ground where nothing would ever wake it again.
            let held = world.fluid(pos);
            let due = held.is_empty()
                || fluid_tick.is_multiple_of(u64::from(tunings.of(held.fluid()).tick_rate.max(1)));
            if !due || (held.is_empty() && !full_tick) {
                self.carried.insert(pos);
                continue;
            }
            visited += 1;
            // Where this block's milk is pressing against something that will
            // not take it. Recorded whether or not the block itself changed: a
            // settled pond against a wall changes nothing every tick and is
            // exactly the case a waterlogging mod cares about.
            if self.blocked.len() < BLOCKED_PER_TICK {
                record_blocked(world, tunings, pos, &mut self.blocked);
            }
            let before = changes.len();
            settle_one(
                world,
                tunings,
                pos,
                seed,
                fluid_tick,
                &mut Record {
                    flows: &mut changes,
                    sinks: &mut self.sinks,
                    washed: &mut self.washed,
                },
            );
            // Everything that changed wakes its own neighbourhood, including
            // the block above: milk drained from under a column is what lets
            // the column fall.
            for change in &changes[before..] {
                woken.insert(change.pos);
                woken.insert(BlockPos::new(change.pos.x, change.pos.y + 1, change.pos.z));
            }
        }
        for pos in woken {
            self.touch(pos);
        }
        changes
    }
}

/// How many brimming blocks have to touch before they are a body.
///
/// **Three, because two is a spilled bucket.** It is the smallest number that
/// tells a body somebody meant — a river, a pond, a sea — from water in
/// motion, and the difference decides what survives a chunk load (§4.5).
pub const BODY_BLOCKS: u32 = 3;

/// The six face neighbours, in a fixed order.
///
/// All six, unlike [`LATERAL`]: a body is a three-dimensional thing and a sea
/// is connected through its own depth. This is not a flow direction list —
/// fluid still never climbs — it is a connectivity one.
const FACES: [[i32; 3]; 6] = [
    [-1, 0, 0],
    [1, 0, 0],
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
];

/// Whether a block holds every cell it has room for.
///
/// **Sub-Node Contract §4.5's "brims".** Capacity rather than 27, because the
/// bed of a river is rarely a whole block of air: a block one third full of
/// gravel brims at eighteen cells and is as much a part of the river as the
/// clear water beside it. A block that cannot hold fluid at all never brims —
/// there is nothing there to be part of anything.
///
/// `false` for a block that is not loaded, which is the conservative answer:
/// what cannot be read cannot be counted on.
fn brims<N: Neighbourhood + ?Sized>(world: &N, tunings: &Tunings, at: BlockPos) -> bool {
    let Some(occupancy) = world.occupancy(at) else {
        return false;
    };
    let held = world.fluid(at);
    if held.is_empty() {
        return false;
    }
    let room = capacity(occupancy, tunings.of(held.fluid()).waterlogs_at);
    room > 0 && held.volume() >= room
}

/// Whether `at` is part of a body: [`BODY_BLOCKS`] brimming blocks that touch.
///
/// **Sub-Node Contract §4.5, and what loading a chunk asks before it wakes
/// anything.** A body is water that has come to rest in the shape somebody
/// meant — worldgen's river, a sea, a pond a player filled — and re-running the
/// physics over it every time it streams in is how a river whose banks the
/// terrain never built empties itself.
///
/// # Connectivity, not a count of neighbours
///
/// The end block of a one-wide river has exactly one brimming neighbour, and
/// that neighbour has another: counting neighbours would wake both ends of
/// every river and let them unravel from the tips. So this is reachability —
/// and it needs no component tracking, because **two steps reach every
/// 3-connected set containing `at`**. The worst case is a straight line with
/// `at` at one end.
///
/// Nothing is allocated and the answer does not depend on the order the
/// neighbours are visited, which charter rule 4 requires of anything the
/// simulation acts on.
pub fn in_a_body<N: Neighbourhood + ?Sized>(world: &N, tunings: &Tunings, at: BlockPos) -> bool {
    if !brims(world, tunings, at) {
        return false;
    }
    let step = |from: BlockPos, [dx, dy, dz]: [i32; 3]| {
        BlockPos::new(
            from.x.saturating_add(dx),
            from.y.saturating_add(dy),
            from.z.saturating_add(dz),
        )
    };
    let mut found = 1;
    let mut only = None;
    for offset in FACES {
        let next = step(at, offset);
        if brims(world, tunings, next) {
            found += 1;
            if found >= BODY_BLOCKS {
                return true;
            }
            only = Some(next);
        }
    }
    // One neighbour, so the third block — if there is one — is out past it.
    let Some(only) = only else {
        return false;
    };
    FACES
        .into_iter()
        .map(|offset| step(only, offset))
        .any(|next| next != at && brims(world, tunings, next))
}

/// How much more fluid a block will take of `fluid`, in cells.
///
/// Zero for a block already holding a different fluid: two fluids do not mix,
/// and the one that got there first keeps the space.
fn accepts(world: &impl Neighbourhood, tunings: &Tunings, pos: BlockPos, fluid: FluidId) -> u32 {
    let Some(occupancy) = world.occupancy(pos) else {
        // Not loaded. Sub-Node Contract §4.2: unloaded is solid, or a flood
        // runs off the edge of the world.
        return 0;
    };
    let here = world.fluid(pos);
    if !here.is_empty() && here.fluid() != fluid {
        return 0;
    }
    capacity(occupancy, tunings.of(fluid).waterlogs_at).saturating_sub(here.volume())
}

/// Moves `cells` of `fluid` from one block to another, recording both ends.
///
/// Both ends, because a conserved transfer changes two blocks and a listener
/// that heard about one would draw half a flow.
fn transfer(
    world: &mut impl Neighbourhood,
    from: BlockPos,
    into: BlockPos,
    fluid: FluidId,
    cells: u32,
    washes: bool,
    record: &mut Record<'_>,
) {
    if cells == 0 {
        return;
    }
    // **Every way fluid enters a block runs through here**, which is why the
    // question is asked here and not at the four call sites — World ask 37,
    // and a fifth way added later would otherwise wash nothing.
    //
    // `washes` is the FLUID's answer (World 38, Weather W14): rain spreading
    // into the grass beside its puddle must leave it standing.
    if washes && world.washes_away(into) && !record.washed.contains(&into) {
        record.washed.push(into);
    }
    let was_from = world.fluid(from);
    let was_into = world.fluid(into);
    let now_from = was_from.with_volume(was_from.volume() - cells);
    let now_into = Fluid::new(fluid, was_into.volume() + cells);
    world.set_fluid(from, now_from);
    world.set_fluid(into, now_into);
    record.flows.push(Flow {
        pos: from,
        was: was_from,
        now: now_from,
    });
    record.flows.push(Flow {
        pos: into,
        was: was_into,
        now: now_into,
    });
}

/// The four lateral directions, rotated by the block's own coordinates.
///
/// **Sub-Node Contract §4.2.** Without a rotation the same side is always
/// served first and a pour spreads lopsidedly. Rotating by the tick counter
/// instead would have to be persisted — a reloaded world would diverge from a
/// fresh one — and would make the whole world favour one side at once, which
/// reads as a pulse crossing a pond rather than as water finding its level.
const fn rotation(pos: BlockPos) -> usize {
    // `rem_euclid` rather than `%`: the world has negative coordinates and a
    // negative index is not a direction.
    (pos.x.rem_euclid(4) + pos.y.rem_euclid(4) + pos.z.rem_euclid(4)) as usize % 4
}

/// Whether this block loses a cell to the air on this tick.
///
/// Stateless: the seed, the position and the tick number are the whole input,
/// so no counter has to survive a save and two servers agree without talking.
fn evaporates(tuning: Tuning, pos: BlockPos, seed: u64, fluid_tick: u64) -> bool {
    if tuning.evaporates == 0 {
        return false;
    }
    // Mixed rather than added so that moving one block does not simply shift
    // which tick it happens on.
    let mixed = seed
        ^ (i64::from(pos.x) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (i64::from(pos.y) as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (i64::from(pos.z) as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ fluid_tick.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    SplitMix64::new(mixed)
        .next_u64()
        .is_multiple_of(u64::from(tuning.evaporates))
}

/// Records the lateral directions this block's fluid is pressing into and
/// cannot enter.
///
/// **Only where the fluid would actually have gone.** A block with a single cell
/// has nothing to give — half of one is nothing — so it presses against nothing
/// and reports nothing, and a dry block obviously does not either. Without that,
/// every solid block adjacent to any milk anywhere would generate an event every
/// time the pond was examined.
///
/// The block BELOW is not considered unless it holds a DIFFERENT fluid. Fluid
/// stopped by a floor is not blocked, it is resting; that is the ordinary case
/// and a mod hearing about it would hear about every pond in the world having a
/// bottom. But lava come down onto a lake has met water, not a floor, and that
/// meeting is the one thing a mod cannot see any other way.
///
/// **Another fluid is reported** wherever it is met (2026-09-16: "lava needs to
/// create blocks that are a mix of lava rock, stone, obsidian and metal when
/// they come into contact"). Two fluids never mix — [`accepts`] gives the space
/// to whichever got there first — so until now a flow into a block of another
/// fluid was neither a flow nor a blocked one, and lava could lie against a sea
/// for ever with nothing told. The event's `into` holds the other fluid; the
/// same fluid in `into` is still a meeting of one body, not a blocked flow.
fn record_blocked(
    world: &impl Neighbourhood,
    tunings: &Tunings,
    pos: BlockPos,
    out: &mut Vec<Blocked>,
) {
    let here = world.fluid(pos);
    if here.is_empty() || here.volume() <= 1 {
        return;
    }

    for offset in LATERAL.iter().chain(std::iter::once(&BELOW)) {
        let into = BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2]);
        // Unloaded is not blocked. A flow reaching the edge of the loaded world
        // is a flow nobody can answer for yet, and reporting it would tell a mod
        // that a chunk which has not arrived is a wall.
        if world.occupancy(into).is_none() {
            continue;
        }
        let there = world.fluid(into);
        let meets_another = !there.is_empty() && there.fluid() != here.fluid();
        if !meets_another {
            // Below, anything but another fluid is the floor it rests on.
            if offset == &BELOW {
                continue;
            }
            // Somewhere it could go is not somewhere it was stopped.
            if accepts(world, tunings, into, here.fluid()) > 0 {
                continue;
            }
            // Already holding this fluid means it is not blocked, it is met.
            if !there.is_empty() {
                continue;
            }
        }
        out.push(Blocked {
            from: pos,
            into,
            fluid: here.fluid(),
            volume: here.volume(),
            meets: meets_another.then(|| there.fluid()),
        });
    }
}

/// Applies the whole rule to one block, appending what changed.
/// Everything one settled block has to report.
///
/// **One borrow rather than three.** A flow, a sink and a washed plant are all
/// "what this tick did", they are all written by the same few functions, and
/// carrying them separately put `settle_one` over clippy's argument limit —
/// which was the honest signal that they belong together.
struct Record<'a> {
    /// Fluid that moved, both ends of every transfer.
    flows: &'a mut Vec<Flow>,
    /// Fluid that was destroyed, and by what.
    sinks: &'a mut Sinks,
    /// Blocks whose terrain a flow swept away — World ask 37.
    washed: &'a mut Vec<BlockPos>,
}

fn settle_one(
    world: &mut impl Neighbourhood,
    tunings: &Tunings,
    pos: BlockPos,
    seed: u64,
    fluid_tick: u64,
    record: &mut Record<'_>,
) {
    let here = world.fluid(pos);
    if here.is_empty() {
        return;
    }
    let fluid = here.fluid();
    // The fluid's own answer to "does this sweep a plant away", read once for
    // the whole settle rather than at each of the four transfers.
    let washes = tunings.of(fluid).washes;

    // **Terrain arriving in a flooded block.** Somebody placed stone where milk
    // was, so the block now holds more than fits. Pushed out below and sideways
    // first; only what nothing will take is destroyed, and it is counted.
    let room = world.occupancy(pos).map_or(0, |occupancy| {
        capacity(occupancy, tunings.of(fluid).waterlogs_at)
    });
    if here.volume() > room {
        let excess = here.volume() - room;
        let spilled = spill(world, tunings, pos, fluid, excess, record);
        if spilled < excess {
            let lost = excess - spilled;
            let now = world.fluid(pos);
            let was = now;
            let now = now.with_volume(now.volume().saturating_sub(lost));
            world.set_fluid(pos, now);
            record.flows.push(Flow { pos, was, now });
            record.sinks.displaced += lost;
        }
        if world.fluid(pos).is_empty() {
            return;
        }
    }

    // Rule 1 — down first.
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let mine = world.fluid(pos).volume();
    let falling = accepts(world, tunings, below, fluid).min(mine);
    if falling > 0 {
        transfer(world, pos, below, fluid, falling, washes, record);
        if world.fluid(pos).is_empty() {
            return;
        }
    }

    // Rule 2 — sideways, lowest-holding first, half the difference each.
    //
    // Recomputed after every transfer so a block can never give away more than
    // it has, and sorted so the emptiest neighbour is served first: filling the
    // lowest first is what makes a pond level rather than terraced.
    let turn = rotation(pos);
    let mut neighbours: Vec<(u32, usize, BlockPos)> = (0..LATERAL.len())
        .map(|index| {
            let offset = LATERAL[(index + turn) % LATERAL.len()];
            let at = BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2]);
            (world.fluid(at).volume(), index, at)
        })
        .collect();
    // Ties break on the rotated direction index, which is a fixed order for a
    // given block — never on the address of anything.
    neighbours.sort_by_key(|(volume, index, _)| (*volume, *index));

    for (_, _, at) in neighbours {
        let mine = world.fluid(pos).volume();
        let theirs = world.fluid(at).volume();
        if theirs >= mine {
            continue;
        }
        let half = (mine - theirs) / 2;
        let moved = half.min(accepts(world, tunings, at, fluid));
        if moved > 0 {
            transfer(world, pos, at, fluid, moved, washes, record);
        }
    }

    // Rule 3 — stuck droplets move whole or not at all.
    let mine = world.fluid(pos).volume();
    if mine > 0 && mine <= DROPLET {
        let turn = rotation(pos);
        for index in 0..LATERAL.len() {
            let offset = LATERAL[(index + turn) % LATERAL.len()];
            let at = BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2]);
            if !world.fluid(at).is_empty() || accepts(world, tunings, at, fluid) < mine {
                continue;
            }
            // Only downhill. A droplet that moved sideways onto level ground
            // would wander for ever, and two of them would swap places.
            let under = BlockPos::new(at.x, at.y - 1, at.z);
            if accepts(world, tunings, under, fluid) == 0 {
                continue;
            }
            transfer(world, pos, at, fluid, mine, washes, record);
            return;
        }
    }

    // Rule 4 — the sinks.
    absorb(world, pos, fluid, record);
    if world.fluid(pos).is_empty() {
        return;
    }
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    if world.fluid(above).is_empty()
        && world
            .occupancy(above)
            .is_some_and(|occupancy| occupancy == 0)
        && evaporates(tunings.of(fluid), pos, seed, fluid_tick)
    {
        let was = world.fluid(pos);
        let now = was.with_volume(was.volume() - 1);
        world.set_fluid(pos, now);
        record.flows.push(Flow { pos, was, now });
        record.sinks.evaporated += 1;
    }
}

/// Pushes `cells` out of a block that no longer has room, returning how many
/// found somewhere to go.
fn spill(
    world: &mut impl Neighbourhood,
    tunings: &Tunings,
    pos: BlockPos,
    fluid: FluidId,
    cells: u32,
    record: &mut Record<'_>,
) -> u32 {
    let washes = tunings.of(fluid).washes;
    let mut left = cells;
    let turn = rotation(pos);
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let sideways = (0..LATERAL.len()).map(|index| {
        let offset = LATERAL[(index + turn) % LATERAL.len()];
        BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2])
    });
    for at in std::iter::once(below).chain(sideways) {
        if left == 0 {
            break;
        }
        let moved = accepts(world, tunings, at, fluid).min(left);
        if moved > 0 {
            transfer(world, pos, at, fluid, moved, washes, record);
            left -= moved;
        }
    }
    cells - left
}

/// Lets the ground either side of and beneath a block soak fluid out of it.
///
/// One absorption per neighbour per tick, and a neighbour takes at most what is
/// there: a single cell over ground that would drink three is one cell absorbed,
/// not a debt.
fn absorb(world: &mut impl Neighbourhood, pos: BlockPos, fluid: FluidId, record: &mut Record<'_>) {
    let turn = rotation(pos);
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let sideways: Vec<BlockPos> = (0..LATERAL.len())
        .map(|index| {
            let offset = LATERAL[(index + turn) % LATERAL.len()];
            BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2])
        })
        .collect();
    for at in std::iter::once(below).chain(sideways) {
        let mine = world.fluid(pos).volume();
        if mine == 0 {
            return;
        }
        if world.occupancy(at).is_none() {
            continue;
        }
        let cells = world.absorbency(at, fluid).min(mine);
        if cells == 0 {
            continue;
        }
        let was = world.fluid(pos);
        let now = was.with_volume(mine - cells);
        world.set_fluid(pos, now);
        record.flows.push(Flow { pos, was, now });
        record.sinks.absorbed.push(Absorbed {
            pos: at,
            fluid,
            cells,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::fluid::MAX_VOLUME;

    /// The fluid every scene here pours.
    pub(super) const MILK: FluidId = FluidId(1);

    /// A world seed. Only evaporation reads it, and most scenes have it off.
    const SEED: u64 = 0x2E5A_11C0_77BD_9134;

    /// A world made of a set of solid blocks and a map of fluid.
    ///
    /// Everything outside `loaded` is unloaded, which is NOT the same as empty:
    /// the solver must treat it as solid, and a scene that answered zero
    /// everywhere could not test that.
    #[derive(Default)]
    pub(super) struct Scene {
        solid: BTreeSet<(i32, i32, i32)>,
        absorbent: BTreeMap<(i32, i32, i32), u32>,
        /// Absorbent blocks that drink ONE fluid; the rest drink any.
        thirst: BTreeMap<(i32, i32, i32), FluidId>,
        /// Blocks part full of terrain, in cells. A river bed is rarely a whole
        /// block of air, and §4.5's "brims" is about capacity rather than 27.
        partial: BTreeMap<(i32, i32, i32), u32>,
        fluid: BTreeMap<(i32, i32, i32), Fluid>,
        /// Blocks a flow sweeps the terrain out of: a plant (World ask 37).
        washes: BTreeSet<(i32, i32, i32)>,
        loaded: Option<BTreeSet<(i32, i32, i32)>>,
    }

    impl Scene {
        /// A floor spanning `xs` by `zs` at `y`, with everything above it open.
        pub(super) fn floored(
            xs: std::ops::RangeInclusive<i32>,
            zs: std::ops::RangeInclusive<i32>,
        ) -> Self {
            let mut scene = Self::default();
            for x in xs {
                for z in zs.clone() {
                    scene.solid.insert((x, 0, z));
                }
            }
            scene
        }

        /// A sealed box of side `2 * radius`, air inside and solid all round.
        ///
        /// **The property tests need this and the unit tests mostly do not.**
        /// An open scene reports everything outside its floor as empty and
        /// loaded, so milk poured near an edge falls out of the world for ever
        /// — which makes a conservation property fail on the fixture rather
        /// than on the rule.
        pub(super) fn sealed(radius: i32) -> Self {
            let mut scene = Self::default();
            let mut loaded = BTreeSet::new();
            for x in -radius..=radius {
                for y in -radius..=radius {
                    for z in -radius..=radius {
                        loaded.insert((x, y, z));
                        if x.abs() == radius || y.abs() == radius || z.abs() == radius {
                            scene.solid.insert((x, y, z));
                        }
                    }
                }
            }
            scene.loaded = Some(loaded);
            scene
        }

        pub(super) fn pour(&mut self, pos: BlockPos, volume: u32) {
            self.fluid
                .insert((pos.x, pos.y, pos.z), Fluid::new(MILK, volume));
        }

        /// Makes a block solid, and takes any milk in it with it.
        pub(super) fn make_solid(&mut self, x: i32, y: i32, z: i32) {
            self.solid.insert((x, y, z));
        }

        /// Puts `cells` of terrain in a block without making it solid.
        pub(super) fn occupy(&mut self, x: i32, y: i32, z: i32, cells: u32) {
            self.partial.insert((x, y, z), cells);
        }

        /// A plant: a tuft water sweeps away.
        ///
        /// **No occupancy**, which is what the real thing reports: the world
        /// mod's plants are `passable`, so the server counts their cells as
        /// blocking nothing and a flood runs straight through them (World ask
        /// 37). That is exactly why the mod could not see it happen.
        pub(super) fn plant(&mut self, x: i32, y: i32, z: i32) {
            self.washes.insert((x, y, z));
        }

        pub(super) fn make_absorbent(&mut self, x: i32, y: i32, z: i32, rate: u32) {
            self.absorbent.insert((x, y, z), rate);
        }

        fn at(&self, pos: BlockPos) -> u32 {
            self.volume_at(pos)
        }

        pub(super) fn volume_at(&self, pos: BlockPos) -> u32 {
            self.fluid
                .get(&(pos.x, pos.y, pos.z))
                .map_or(0, |value| value.volume())
        }

        /// Every block holding milk, for a property to walk.
        pub(super) fn contents(&self) -> impl Iterator<Item = (&(i32, i32, i32), &Fluid)> {
            self.fluid.iter()
        }

        /// Every cell of fluid in the scene.
        pub(super) fn total(&self) -> u32 {
            self.fluid.values().map(|value| value.volume()).sum()
        }
    }

    impl Scene {
        fn washes_at(&self, pos: BlockPos) -> bool {
            self.washes.contains(&(pos.x, pos.y, pos.z))
        }
    }

    impl Neighbourhood for Scene {
        fn occupancy(&self, pos: BlockPos) -> Option<u32> {
            if let Some(loaded) = &self.loaded
                && !loaded.contains(&(pos.x, pos.y, pos.z))
            {
                return None;
            }
            if self.solid.contains(&(pos.x, pos.y, pos.z)) {
                return Some(MAX_VOLUME);
            }
            Some(
                self.partial
                    .get(&(pos.x, pos.y, pos.z))
                    .copied()
                    .unwrap_or(0),
            )
        }

        fn washes_away(&self, pos: BlockPos) -> bool {
            self.washes_at(pos)
        }

        fn absorbency(&self, pos: BlockPos, fluid: FluidId) -> u32 {
            let at = (pos.x, pos.y, pos.z);
            if self.thirst.get(&at).is_some_and(|only| *only != fluid) {
                return 0;
            }
            self.absorbent.get(&at).copied().unwrap_or(0)
        }

        fn fluid(&self, pos: BlockPos) -> Fluid {
            self.fluid
                .get(&(pos.x, pos.y, pos.z))
                .copied()
                .unwrap_or(Fluid::EMPTY)
        }

        fn set_fluid(&mut self, pos: BlockPos, value: Fluid) {
            if value.is_empty() {
                self.fluid.remove(&(pos.x, pos.y, pos.z));
            } else {
                self.fluid.insert((pos.x, pos.y, pos.z), value);
            }
        }
    }

    /// Settles a scene under [`SEED`], returning what was destroyed on the way.
    fn settle(scene: &mut Scene, solver: &mut Solver, tuning: Tuning, ticks: u64) -> Sinks {
        settle_seeded(scene, solver, tuning, ticks, SEED)
    }

    /// The same, under a named seed.
    ///
    ///
    /// **Separate because the seed has to be reachable.** The first version of
    /// the determinism test below took a seed and then called a helper that
    /// used the constant, so both halves ran identically and it asserted
    /// nothing at all.
    pub(super) fn settle_seeded(
        scene: &mut Scene,
        solver: &mut Solver,
        tuning: Tuning,
        ticks: u64,
        seed: u64,
    ) -> Sinks {
        let tunings = Tunings::uniform(tuning);
        for tick in 0..ticks {
            solver.tick(scene, &tunings, usize::MAX, seed, tick);
        }
        solver.take_sinks()
    }

    #[test]
    fn a_fluid_against_another_is_reported_beside_and_below_but_its_own_is_not() {
        // Two fluids never mix, so lava beside or on top of water is stuck
        // there for good — and a mod turning the meeting into rock can only
        // learn of it here. A floor under it, and more of itself beside it,
        // are still not blocked flows.
        const LAVA: FluidId = FluidId(2);
        let mut scene = Scene::sealed(4);
        for x in -3..=3 {
            for z in -3..=3 {
                scene.make_solid(x, -3, z);
            }
        }
        // Water at the bottom; lava in the block beside it and the one above.
        scene.pour(BlockPos::new(0, -2, 0), MAX_VOLUME);
        scene.pour(BlockPos::new(1, -2, 0), MAX_VOLUME);
        scene.fluid.insert((0, -1, 0), Fluid::new(LAVA, MAX_VOLUME));
        scene
            .fluid
            .insert((-1, -2, 0), Fluid::new(LAVA, MAX_VOLUME));
        let mut out = Vec::new();
        record_blocked(
            &scene,
            &Tunings::uniform(Tuning::DEFAULT),
            BlockPos::new(0, -1, 0),
            &mut out,
        );
        assert!(
            out.iter().any(|b| b.into == BlockPos::new(0, -2, 0)
                && b.fluid == LAVA
                && b.meets == Some(MILK)),
            "lava resting on water was not reported: {out:?}"
        );
        let mut beside = Vec::new();
        record_blocked(
            &scene,
            &Tunings::uniform(Tuning::DEFAULT),
            BlockPos::new(-1, -2, 0),
            &mut beside,
        );
        assert!(
            beside.iter().any(|b| b.into == BlockPos::new(0, -2, 0)),
            "lava beside water was not reported: {beside:?}"
        );
        let mut own = Vec::new();
        record_blocked(
            &scene,
            &Tunings::uniform(Tuning::DEFAULT),
            BlockPos::new(1, -2, 0),
            &mut own,
        );
        assert!(
            own.iter()
                .all(|b| b.into != BlockPos::new(0, -2, 0) && b.into != BlockPos::new(1, -3, 0)),
            "water beside water, or on its floor, was reported: {own:?}"
        );
    }

    #[test]
    fn a_pour_keeps_every_cell_it_started_with() {
        // **The invariant the whole model exists for.** Nothing here absorbs
        // and nothing evaporates, so the only honest answer after any number of
        // ticks is the number that went in.
        let mut scene = Scene::floored(-4..=4, -4..=4);
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
        solver.touch(BlockPos::new(0, 1, 0));

        let sinks = settle(&mut scene, &mut solver, Tuning::DEFAULT, 60);

        assert!(
            sinks.is_empty(),
            "a scene with no sink in it destroyed milk"
        );
        assert_eq!(
            scene.total(),
            MAX_VOLUME,
            "milk was created or destroyed by flowing"
        );
    }

    #[test]
    fn milk_falls_before_it_spreads() {
        // Rule 1. A block over a hole empties downward rather than sideways,
        // which is what makes a waterfall a waterfall.
        let mut scene = Scene::floored(-4..=4, -4..=4);
        // A shaft: no floor directly under the pour, walled all the way down so
        // the milk has exactly one way to go, and capped at the bottom.
        //
        // **The walls are not decoration.** `Scene` reports every block outside
        // `solid` as empty and loaded, so an unwalled shaft opens onto an
        // infinite void and the milk falls out of the world — which is a fact
        // about this fixture, not about the rule.
        scene.solid.remove(&(0, 0, 0));
        for y in -3..=0 {
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                scene.solid.insert((dx, y, dz));
            }
        }
        scene.solid.insert((0, -3, 0));
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
        solver.touch(BlockPos::new(0, 1, 0));

        settle(&mut scene, &mut solver, Tuning::DEFAULT, 60);

        assert_eq!(
            scene.at(BlockPos::new(0, 1, 0)),
            0,
            "milk stayed up top instead of falling down the shaft"
        );
        assert!(
            scene.at(BlockPos::new(0, -2, 0)) > 0,
            "nothing reached the bottom of the shaft"
        );
        assert_eq!(scene.total(), MAX_VOLUME);
    }

    #[test]
    fn a_pond_levels_itself_and_then_stops() {
        // Rule 2, and the property that makes it terminate: a difference of one
        // transfers nothing, so settling needs no separate stability test.
        let mut scene = Scene::floored(-2..=2, -2..=2);
        // Walls, so the pond has somewhere to level INTO and nowhere to escape.
        for x in -3i32..=3 {
            for z in -3i32..=3 {
                if x.abs() == 3 || z.abs() == 3 {
                    scene.solid.insert((x, 1, z));
                }
            }
        }
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
        solver.touch(BlockPos::new(0, 1, 0));

        settle(&mut scene, &mut solver, Tuning::DEFAULT, 200);

        assert!(
            solver.is_settled(),
            "the pond never stopped moving, so a settled world would cost a tick forever"
        );
        let volumes: Vec<u32> = scene.fluid.values().map(|value| value.volume()).collect();
        let high = volumes.iter().copied().max().unwrap_or(0);
        let low = volumes.iter().copied().min().unwrap_or(0);
        assert!(
            high - low <= 1,
            "the pond settled uneven: {low} in one block and {high} in another"
        );
        assert_eq!(scene.total(), MAX_VOLUME);
    }

    #[test]
    fn a_settled_pond_costs_nothing() {
        // The perf property Task 11 asserts, restated for the new rule: milk
        // that has finished moving leaves the active set entirely.
        let mut scene = Scene::floored(-3..=3, -3..=3);
        for x in -4i32..=4 {
            for z in -4i32..=4 {
                if x.abs() == 4 || z.abs() == 4 {
                    scene.solid.insert((x, 1, z));
                }
            }
        }
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
        solver.touch(BlockPos::new(0, 1, 0));
        settle(&mut scene, &mut solver, Tuning::DEFAULT, 300);

        assert!(solver.is_settled());
        assert_eq!(solver.active(), 0, "a settled pond is still being visited");
    }

    #[test]
    fn a_droplet_moves_whole_or_not_at_all() {
        // Rule 3. Half of one cell is nothing, so without this a droplet on a
        // slope leaves a permanent streak behind it.
        let mut scene = Scene::default();
        // A step down: floor at x <= 0, then a drop.
        for x in -3..=0 {
            scene.solid.insert((x, 0, 0));
        }
        scene.solid.insert((1, -2, 0));
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), 1);
        solver.touch(BlockPos::new(0, 1, 0));

        settle(&mut scene, &mut solver, Tuning::DEFAULT, 60);

        assert_eq!(
            scene.at(BlockPos::new(0, 1, 0)),
            0,
            "a single cell stayed put beside a drop, which is the streak this rule prevents"
        );
        assert_eq!(
            scene.total(),
            1,
            "the droplet was destroyed rather than moved"
        );
    }

    #[test]
    fn a_droplet_on_level_ground_stays_where_it_is() {
        // The other half of rule 3, and the reason it is only downhill: a
        // droplet free to move sideways on the flat would wander for ever, and
        // two of them would swap places every tick.
        let mut scene = Scene::floored(-3..=3, -3..=3);
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), 1);
        solver.touch(BlockPos::new(0, 1, 0));

        settle(&mut scene, &mut solver, Tuning::DEFAULT, 60);

        assert_eq!(scene.at(BlockPos::new(0, 1, 0)), 1, "the droplet wandered");
        assert!(solver.is_settled());
    }

    #[test]
    fn unloaded_neighbours_are_solid_rather_than_empty() {
        // A flood must not run off the edge of the loaded world. `None` from
        // `occupancy` means "not loaded", and treating it as zero would drain
        // a pond into a chunk that has not arrived.
        let mut scene = Scene::floored(-4..=4, -4..=4);
        let mut loaded = BTreeSet::new();
        for x in -1..=1 {
            for y in -1..=2 {
                for z in -1..=1 {
                    loaded.insert((x, y, z));
                }
            }
        }
        scene.loaded = Some(loaded);
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
        solver.touch(BlockPos::new(0, 1, 0));

        settle(&mut scene, &mut solver, Tuning::DEFAULT, 60);

        assert_eq!(
            scene.total(),
            MAX_VOLUME,
            "milk leaked into unloaded space, so a pond drains into chunks nobody has yet"
        );
        for at in scene.fluid.keys() {
            assert!(
                at.0.abs() <= 1 && at.2.abs() <= 1,
                "milk reached {at:?}, which is outside the loaded region"
            );
        }
    }

    #[test]
    fn terrain_takes_the_space_it_needs_and_the_rest_is_counted() {
        // Sub-Node Contract §4.1: capacity is `27 − occupancy`. A block that
        // suddenly holds more than fits pushes the excess out, and only what
        // nothing will accept is destroyed — counted as `displaced`, so the
        // conservation invariant still closes.
        let mut scene = Scene::default();
        // One sealed block: floor, ceiling and four walls, holding a full load.
        scene.solid.insert((0, 0, 0));
        scene.solid.insert((0, 2, 0));
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            scene.solid.insert((dx, 1, dz));
        }
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
        // Now fill it with terrain, which leaves the milk nowhere to be.
        scene.solid.insert((0, 1, 0));
        solver.touch(BlockPos::new(0, 1, 0));

        let sinks = settle(&mut scene, &mut solver, Tuning::DEFAULT, 20);

        assert_eq!(scene.total(), 0, "milk survived inside solid terrain");
        assert_eq!(
            sinks.displaced, MAX_VOLUME,
            "displaced milk was destroyed without being counted, so conservation cannot be checked"
        );
    }

    #[test]
    fn a_gentle_fluid_runs_into_a_plant_and_leaves_it_standing() {
        // World ask 38 and Weather W14, filed a day apart by two mods that hit
        // the same wall: `washes_away` is the plant's declaration and names no
        // fluid, so rain — which is a fluid, and whose puddles spread a few
        // cells on open ground — would strip every meadow it fell on.
        let mut scene = Scene::floored(-4..=4, -4..=4);
        scene.plant(0, 1, 0);
        scene.pour(BlockPos::new(0, 2, 0), MAX_VOLUME);

        let mut solver = Solver::new();
        let gentle = Tunings::uniform(Tuning {
            washes: false,
            ..Tuning::DEFAULT
        });
        solver.touch(BlockPos::new(0, 2, 0));
        for tick in 0..12 {
            solver.tick(&mut scene, &gentle, 64, 7, tick);
        }
        assert!(
            solver.take_washed().is_empty(),
            "rain swept the grass it fell on"
        );

        // The fluid still FLOWS into it — the plant is passable, so the water
        // stands in the same block, which is what ask 29 made true. What
        // changes is only whether the plant survives it.
        assert!(
            scene.fluid(BlockPos::new(0, 1, 0)).volume() > 0,
            "a gentle fluid should still run into the plant's block"
        );

        // And the same scene with the ordinary fluid washes it, so the
        // difference is the flag rather than the fixture.
        let mut washing = Scene::floored(-4..=4, -4..=4);
        washing.plant(0, 1, 0);
        washing.pour(BlockPos::new(0, 2, 0), MAX_VOLUME);
        let mut solver = Solver::new();
        let tunings = Tunings::uniform(Tuning::DEFAULT);
        solver.touch(BlockPos::new(0, 2, 0));
        for tick in 0..12 {
            solver.tick(&mut washing, &tunings, 64, 7, tick);
        }
        assert!(
            solver.take_washed().contains(&BlockPos::new(0, 1, 0)),
            "the ordinary fluid stopped washing"
        );
    }

    #[test]
    fn a_flow_into_a_plant_sweeps_it_away_once() {
        // World ask 37. A tuft is `passable`, so a flood runs through it and
        // stands in the same block — the mod's own report of a meadow flooded
        // was 1,208 blocked flows, none of them from a plant's block, because
        // a flow into a block nothing blocks is not blocked.
        let mut scene = Scene::floored(-4..=4, -4..=4);
        scene.plant(0, 1, 0);
        scene.plant(1, 1, 0);
        scene.pour(BlockPos::new(0, 2, 0), MAX_VOLUME);
        let mut solver = Solver::new();
        let tunings = Tunings::uniform(Tuning::DEFAULT);
        solver.touch(BlockPos::new(0, 2, 0));
        for tick in 0..12 {
            solver.tick(&mut scene, &tunings, 64, 7, tick);
        }
        let washed = solver.take_washed();
        assert!(
            washed.contains(&BlockPos::new(0, 1, 0)),
            "the water fell through the plant and left it standing: {washed:?}"
        );
        assert_eq!(
            washed
                .iter()
                .filter(|pos| **pos == BlockPos::new(0, 1, 0))
                .count(),
            1,
            "one plant, one clearing: {washed:?}"
        );
        // Taken, so the caller is told once and the next tick starts empty.
        assert!(solver.take_washed().is_empty());

        // And a block nobody called a plant is never reported, however much
        // water runs through it.
        assert!(
            !washed.contains(&BlockPos::new(0, 2, 0)),
            "an ordinary block was washed: {washed:?}"
        );
    }

    #[test]
    fn absorbent_ground_drinks_and_says_how_much() {
        // Sub-Node Contract §4.3. The solver knows a number and nothing about
        // materials; turning "three cells went into this block" into "dirt
        // becomes damp dirt" belongs to whoever holds the registry.
        let mut scene = Scene::floored(-2..=2, -2..=2);
        scene.absorbent.insert((0, 0, 0), 3);
        // Walled, because absorption is rule 4 and spreading is rule 2: an open
        // block has already given most of its milk to its neighbours by the
        // time the ground gets a turn, so an unwalled scene would measure the
        // ORDER of the rules while claiming to measure the rate.
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            scene.solid.insert((dx, 1, dz));
        }
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), 6);
        solver.touch(BlockPos::new(0, 1, 0));

        let sinks = settle(&mut scene, &mut solver, Tuning::DEFAULT, 1);

        assert_eq!(
            sinks.absorbed.len(),
            1,
            "exactly one block should have absorbed, once"
        );
        let taken = sinks.absorbed[0];
        assert_eq!(taken.pos, BlockPos::new(0, 0, 0));
        assert_eq!(taken.cells, 3);
        assert_eq!(taken.fluid, MILK);
        assert_eq!(
            scene.total() + sinks.total(),
            6,
            "what was absorbed plus what is left is not what was poured"
        );
    }

    #[test]
    fn ground_that_drinks_one_fluid_refuses_another() {
        // **Weather ask W7.** Rain-wet dirt beside a river: ground that names
        // the fluid it drinks takes that one at its rate and the other not at
        // all — the same walled scene as above, poured with milk, under
        // ground thirsty first for something else and then for milk.
        const RAIN: FluidId = FluidId(2);
        let scene_for = |thirst: FluidId| {
            let mut scene = Scene::floored(-2..=2, -2..=2);
            scene.absorbent.insert((0, 0, 0), 3);
            scene.thirst.insert((0, 0, 0), thirst);
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                scene.solid.insert((dx, 1, dz));
            }
            scene.pour(BlockPos::new(0, 1, 0), 6);
            scene
        };

        let mut scene = scene_for(RAIN);
        let mut solver = Solver::new();
        solver.touch(BlockPos::new(0, 1, 0));
        let sinks = settle(&mut scene, &mut solver, Tuning::DEFAULT, 4);
        assert!(
            sinks.absorbed.is_empty(),
            "ground thirsty for rain drank milk: {:?}",
            sinks.absorbed
        );
        assert_eq!(scene.total(), 6, "and the milk is all still there");

        let mut scene = scene_for(MILK);
        let mut solver = Solver::new();
        solver.touch(BlockPos::new(0, 1, 0));
        let sinks = settle(&mut scene, &mut solver, Tuning::DEFAULT, 1);
        assert_eq!(sinks.absorbed.len(), 1, "ground thirsty for milk drinks it");
        assert_eq!(sinks.absorbed[0].cells, 3);
    }

    #[test]
    fn ground_never_drinks_more_than_is_there() {
        // A single cell over ground that would take three is one cell absorbed,
        // not a debt against a block that has nothing left.
        let mut scene = Scene::floored(-2..=2, -2..=2);
        scene.absorbent.insert((0, 0, 0), 9);
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            scene.solid.insert((dx, 1, dz));
        }
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), 1);
        solver.touch(BlockPos::new(0, 1, 0));

        let sinks = settle(&mut scene, &mut solver, Tuning::DEFAULT, 4);

        assert_eq!(sinks.total(), 1, "absorbed more than was there to absorb");
        assert_eq!(scene.total(), 0);
    }

    #[test]
    fn each_fluid_runs_on_its_own_settings() {
        // **The defect a weather mod's asks turned up.** The solver took one
        // `Tuning` for the whole tick, from whichever fluid registered first —
        // alphabetically — so a puddle that dries beside a sea that must not,
        // or a slow lava beside a quick river, could not be expressed, and
        // `core_milk:milk`'s rate governed a world's water. Two fluids, two
        // settings, one scene.
        const LAVA: FluidId = FluidId(2);
        let mut scene = Scene::default();
        // Two open-topped cups on a floor, walled from each other.
        for x in -1..=5 {
            scene.solid.insert((x, 0, 0));
            for dz in [-1, 1] {
                scene.solid.insert((x, 1, dz));
            }
        }
        for x in [-1, 1, 3, 5] {
            scene.solid.insert((x, 1, 0));
        }
        scene.pour(BlockPos::new(0, 1, 0), 10);
        scene.fluid.insert((4, 1, 0), Fluid::new(LAVA, 10));
        let mut solver = Solver::new();
        solver.touch(BlockPos::new(0, 1, 0));
        solver.touch(BlockPos::new(4, 1, 0));

        // Milk dries; lava does not.
        let tunings = Tunings::from_pairs([
            (
                MILK,
                Tuning {
                    evaporates: 1,
                    ..Tuning::DEFAULT
                },
            ),
            (LAVA, Tuning::DEFAULT),
        ]);
        for tick in 0..8 {
            solver.tick(&mut scene, &tunings, usize::MAX, SEED, tick);
        }
        assert!(
            scene.at(BlockPos::new(0, 1, 0)) < 10,
            "milk with `evaporates = 1` lost nothing over eight ticks"
        );
        assert_eq!(
            scene.at(BlockPos::new(4, 1, 0)),
            10,
            "lava with `evaporates = 0` evaporated, so the milk's setting leaked onto it"
        );

        // And a rate of its own: lava at one tick in four moves on a quarter
        // of the ticks milk does. Two open shafts, a full block dropped into
        // each; count the flows per fluid over four ticks.
        let mut scene = Scene::default();
        for x in [0, 4] {
            for y in -8..=0 {
                for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    scene.solid.insert((x + dx, y, dz));
                }
            }
            scene.solid.insert((x, -9, 0));
        }
        scene.pour(BlockPos::new(0, 0, 0), MAX_VOLUME);
        scene.fluid.insert((4, 0, 0), Fluid::new(LAVA, MAX_VOLUME));
        let mut solver = Solver::new();
        solver.touch(BlockPos::new(0, 0, 0));
        solver.touch(BlockPos::new(4, 0, 0));
        let tunings = Tunings::from_pairs([
            (MILK, Tuning::DEFAULT),
            (
                LAVA,
                Tuning {
                    tick_rate: 4,
                    washes: true,
                    ..Tuning::DEFAULT
                },
            ),
        ]);
        let (mut milk_flows, mut lava_flows) = (0, 0);
        for tick in 0..4 {
            for flow in solver.tick(&mut scene, &tunings, usize::MAX, SEED, tick) {
                let fluid = if flow.now.is_empty() {
                    flow.was
                } else {
                    flow.now
                };
                if fluid.fluid() == LAVA {
                    lava_flows += 1;
                } else {
                    milk_flows += 1;
                }
            }
        }
        assert!(
            milk_flows > lava_flows && lava_flows > 0,
            "milk at rate 1 flowed {milk_flows} times and lava at rate 4 flowed {lava_flows}: \
             the slow fluid is not slower, or is stopped"
        );
    }

    #[test]
    fn a_last_cell_on_thirsty_ground_is_taken_at_a_slow_fluids_own_rate() {
        // A one-cell droplet on absorbent ground, for a fluid that moves one
        // tick in four: it must still be drunk within a few of its own ticks.
        let mut scene = Scene::default();
        scene.solid.insert((0, 0, 0));
        scene.make_absorbent(0, 0, 0, 9);
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            scene.solid.insert((dx, 1, dz));
        }
        scene.pour(BlockPos::new(0, 1, 0), 1);
        let mut solver = Solver::new();
        solver.touch(BlockPos::new(0, 1, 0));
        let slow = Tunings::uniform(Tuning {
            tick_rate: 4,
            washes: true,
            ..Tuning::DEFAULT
        });
        for tick in 0..16 {
            solver.tick(&mut scene, &slow, usize::MAX, SEED, tick);
        }
        assert_eq!(
            scene.at(BlockPos::new(0, 1, 0)),
            0,
            "a droplet at rate 4 was never absorbed over sixteen ticks"
        );
    }

    #[test]
    fn evaporation_only_takes_from_blocks_open_to_the_air() {
        // A wide shallow pool goes before a deep narrow one, because more of it
        // is exposed — which only holds if a covered block is exempt.
        let mut scene = Scene::default();
        scene.solid.insert((0, 0, 0));
        // A lid directly over the milk.
        scene.solid.insert((0, 2, 0));
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            scene.solid.insert((dx, 1, dz));
        }
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), 10);
        solver.touch(BlockPos::new(0, 1, 0));

        let thirsty = Tuning {
            evaporates: 1,
            ..Tuning::DEFAULT
        };
        let sinks = settle(&mut scene, &mut solver, thirsty, 40);

        assert_eq!(
            sinks.evaporated, 0,
            "milk under a lid evaporated, so depth would not protect a pool"
        );
        assert_eq!(scene.at(BlockPos::new(0, 1, 0)), 10);
    }

    #[test]
    fn evaporation_is_the_same_everywhere_for_the_same_seed() {
        // Charter rule 4. The randomness is the seed, the position and the tick
        // and nothing else — no stored counter, so two servers agree without
        // talking and a reloaded world matches a fresh one.
        let run = |seed: u64| {
            let mut scene = Scene::floored(-3..=3, -3..=3);
            let mut solver = Solver::new();
            for x in -2..=2 {
                for z in -2..=2 {
                    scene.pour(BlockPos::new(x, 1, z), 9);
                    solver.touch(BlockPos::new(x, 1, z));
                }
            }
            let thirsty = Tuning {
                evaporates: 2,
                ..Tuning::DEFAULT
            };
            let sinks = settle_seeded(&mut scene, &mut solver, thirsty, 30, seed);
            (scene.total(), sinks.evaporated)
        };

        let (held, gone) = run(SEED);
        assert_eq!(
            run(SEED),
            (held, gone),
            "the same seed gave a different world"
        );
        assert!(gone > 0, "nothing evaporated, so this test asserts nothing");
        // A different seed is allowed to agree by chance on the total, but the
        // scene should not be identical — otherwise the seed is being ignored.
        let other = run(SEED ^ 0xFFFF_FFFF);
        assert_ne!(
            other,
            (held, gone),
            "changing the world seed changed nothing, so evaporation is not seeded by it"
        );
    }

    #[test]
    fn the_direction_order_is_the_blocks_own_and_not_the_ticks() {
        // Sub-Node Contract §4.2. A tick-derived rotation has to be persisted
        // or a reloaded world diverges; a coordinate-derived one is stateless.
        // Two neighbouring blocks must not agree on which side to serve first,
        // or a pour crawls in one direction.
        let mut seen = BTreeSet::new();
        for x in 0..4 {
            seen.insert(rotation(BlockPos::new(x, 0, 0)));
        }
        assert_eq!(
            seen.len(),
            4,
            "neighbouring blocks all favour the same side"
        );

        // And it does not depend on the sign of a coordinate: a negative
        // remainder is not a direction.
        for x in -8..8 {
            let turn = rotation(BlockPos::new(x, -3, 5));
            assert!(turn < LATERAL.len(), "rotation {turn} is not a direction");
        }
    }

    #[test]
    fn the_budget_carries_what_it_could_not_reach() {
        // An unfinished flow must be finished or the world is wrong, so blocks
        // the budget did not reach are carried rather than dropped.
        let mut scene = Scene::floored(-6..=6, -6..=6);
        let mut solver = Solver::new();
        for x in -5..=5 {
            for z in -5..=5 {
                scene.pour(BlockPos::new(x, 1, z), MAX_VOLUME);
                solver.touch(BlockPos::new(x, 1, z));
            }
        }
        let queued = solver.active();
        solver.tick(&mut scene, &Tunings::uniform(Tuning::DEFAULT), 4, SEED, 0);
        assert!(
            solver.active() > 0,
            "a tick with a budget of four retired all {queued} blocks"
        );
        assert_eq!(scene.total(), MAX_VOLUME * 121, "a capped tick lost milk");
    }

    #[test]
    fn a_transfer_is_reported_at_both_ends() {
        // A conserved move changes two blocks. A listener told about only one
        // would draw half of every flow, and would re-mesh the wrong chunk at
        // a chunk boundary.
        let mut scene = Scene::floored(-2..=2, -2..=2);
        let mut solver = Solver::new();
        scene.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
        solver.touch(BlockPos::new(0, 1, 0));

        let changes = solver.tick(
            &mut scene,
            &Tunings::uniform(Tuning::DEFAULT),
            usize::MAX,
            SEED,
            0,
        );

        let gave: Vec<&Flow> = changes
            .iter()
            .filter(|flow| flow.now.volume() < flow.was.volume())
            .collect();
        let took: Vec<&Flow> = changes
            .iter()
            .filter(|flow| flow.now.volume() > flow.was.volume())
            .collect();
        assert!(!gave.is_empty(), "nothing moved at all");
        assert_eq!(
            gave.len(),
            took.len(),
            "every cell that left a block should have arrived in another"
        );
    }

    /// Sub-Node Contract §4.5. What a chunk load is allowed to wake.
    mod bodies {
        use super::*;

        /// A floored scene with milk brimming in each of `blocks`.
        fn body(blocks: &[(i32, i32, i32)]) -> Scene {
            let mut scene = Scene::floored(-8..=8, -8..=8);
            for (x, y, z) in blocks {
                scene.pour(BlockPos::new(*x, *y, *z), MAX_VOLUME);
            }
            scene
        }

        fn is_body(scene: &Scene, at: (i32, i32, i32)) -> bool {
            in_a_body(
                scene,
                &Tunings::uniform(Tuning::DEFAULT),
                BlockPos::new(at.0, at.1, at.2),
            )
        }

        #[test]
        fn one_block_is_not_a_body_and_neither_are_two() {
            // Two touching blocks is a spilled bucket, and a bucket is water in
            // motion: it must still be woken and still settle.
            let lone = body(&[(0, 1, 0)]);
            assert!(!is_body(&lone, (0, 1, 0)));

            let pair = body(&[(0, 1, 0), (1, 1, 0)]);
            assert!(!is_body(&pair, (0, 1, 0)));
            assert!(!is_body(&pair, (1, 1, 0)));
        }

        #[test]
        fn three_in_a_line_are_a_body_from_either_end() {
            // **The case a count of neighbours gets wrong.** The end of a
            // one-wide river has exactly one brimming neighbour, so a rule that
            // counted them would wake both ends of every river and let it
            // unravel from the tips. The middle block is the easy case; the
            // ends are the test.
            let line = body(&[(0, 1, 0), (1, 1, 0), (2, 1, 0)]);
            assert!(is_body(&line, (1, 1, 0)), "the middle of a line");
            assert!(is_body(&line, (0, 1, 0)), "one end of a line");
            assert!(is_body(&line, (2, 1, 0)), "the other end");
        }

        #[test]
        fn a_body_is_connected_in_three_dimensions() {
            // A sea is joined through its own depth, so the six faces are the
            // neighbours rather than the four a flow may take.
            let column = body(&[(0, 1, 0), (0, 2, 0), (0, 3, 0)]);
            assert!(is_body(&column, (0, 1, 0)));
            assert!(is_body(&column, (0, 3, 0)));
        }

        #[test]
        fn blocks_that_only_meet_at_a_corner_are_not_touching() {
            // Diagonals are not faces. Two pairs that share only an edge are two
            // spills, not one pond, and both stay awake.
            let corner = body(&[(0, 1, 0), (1, 2, 0)]);
            assert!(!is_body(&corner, (0, 1, 0)));
            assert!(!is_body(&corner, (1, 2, 0)));
        }

        #[test]
        fn water_that_does_not_brim_is_never_part_of_a_body() {
            // A film on its way somewhere is exactly what loading must still
            // wake, however much brimming water it is beside.
            let mut scene = body(&[(0, 1, 0), (1, 1, 0), (2, 1, 0)]);
            scene.pour(BlockPos::new(3, 1, 0), MAX_VOLUME - 1);
            assert!(!is_body(&scene, (3, 1, 0)), "a block one cell short");
            assert!(
                is_body(&scene, (2, 1, 0)),
                "and its neighbours are still a body"
            );
        }

        #[test]
        fn a_block_brims_at_the_capacity_its_terrain_leaves_it() {
            // **The river bed.** A block a third full of gravel holds eighteen
            // cells and is as much a part of the river as the clear water
            // beside it; measuring against 27 would wake the whole bed of every
            // stream in the world.
            let mut scene = body(&[(0, 1, 0), (1, 1, 0)]);
            scene.occupy(2, 1, 0, 9);
            scene.pour(BlockPos::new(2, 1, 0), MAX_VOLUME - 9);
            assert!(
                is_body(&scene, (2, 1, 0)),
                "a bed block full to its capacity"
            );
            assert!(
                is_body(&scene, (0, 1, 0)),
                "so the three of them are a body"
            );
        }

        #[test]
        fn a_block_with_no_room_at_all_is_not_water() {
            // Fluid-solid: there is nothing there to be part of anything, and a
            // body made of three of them would pin water that does not exist.
            let mut scene = body(&[(0, 1, 0), (1, 1, 0)]);
            scene.occupy(2, 1, 0, MAX_VOLUME);
            assert!(!is_body(&scene, (2, 1, 0)));
        }

        #[test]
        fn what_cannot_be_read_does_not_count() {
            // §4.2 already says an unloaded neighbour is not readable. The
            // conservative answer here is the same one: the two blocks this
            // scene can see are not a body on their own, so they are woken and
            // settle against the solid edge — which is where they already are.
            let mut scene = Scene::sealed(4);
            for x in 0..3 {
                scene.pour(BlockPos::new(x, 1, 0), MAX_VOLUME);
            }
            assert!(is_body(&scene, (1, 1, 0)), "all three are loaded here");

            let mut cut = Scene::sealed(4);
            cut.pour(BlockPos::new(0, 1, 0), MAX_VOLUME);
            cut.pour(BlockPos::new(1, 1, 0), MAX_VOLUME);
            // The third block of the line is outside the loaded world.
            cut.pour(BlockPos::new(5, 1, 0), MAX_VOLUME);
            assert!(!is_body(&cut, (0, 1, 0)));
        }
    }
}

/// Charter rule 15: the conservation invariant, over arbitrary worlds.
///
/// **This is the test the whole model was reshaped to make writable.** Under the
/// old rule a source created milk and a drain destroyed it, so "what went in
/// came out" had no meaning; the property below is only expressible because
/// [`Sinks`] counts every cell that leaves.
#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::tests::{MILK, Scene, settle_seeded};
    use super::*;
    use crate::fluid::MAX_VOLUME;

    /// How far from the origin a generated world reaches.
    const SPAN: i32 = 3;

    /// One thing a generated world contains.
    #[derive(Debug, Clone)]
    enum Feature {
        /// Solid terrain at a block.
        Solid(i32, i32, i32),
        /// Ground that drinks, at a rate.
        Absorbent(i32, i32, i32, u32),
        /// Milk, poured.
        Pour(i32, i32, i32, u32),
    }

    fn coordinate() -> impl Strategy<Value = i32> {
        -SPAN..=SPAN
    }

    fn feature() -> impl Strategy<Value = Feature> {
        prop_oneof![
            (coordinate(), -SPAN..=SPAN, coordinate())
                .prop_map(|(x, y, z)| Feature::Solid(x, y, z)),
            (coordinate(), -SPAN..=SPAN, coordinate(), 1u32..=6)
                .prop_map(|(x, y, z, rate)| Feature::Absorbent(x, y, z, rate)),
            (coordinate(), -SPAN..=SPAN, coordinate(), 1u32..=MAX_VOLUME)
                .prop_map(|(x, y, z, volume)| Feature::Pour(x, y, z, volume)),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        /// Every cell poured in is either still in the world or in a sink.
        #[test]
        fn nothing_is_created_and_every_loss_is_accounted_for(
            features in prop::collection::vec(feature(), 1..24),
            evaporates in prop::option::of(1u32..=4),
            ticks in 1u64..=40,
        ) {
            let mut scene = Scene::sealed(SPAN + 1);
            let mut solver = Solver::new();
            let mut poured = 0;

            for feature in &features {
                match *feature {
                    Feature::Solid(x, y, z) => {
                        scene.make_solid(x, y, z);
                    }
                    Feature::Absorbent(x, y, z, rate) => {
                        scene.make_absorbent(x, y, z, rate);
                    }
                    Feature::Pour(x, y, z, volume) => {
                        // Only into somewhere that can hold it, and only what
                        // fits: a pour into solid rock is not a pour, and
                        // counting it as one would make the property fail for
                        // the fixture's arithmetic rather than the solver's.
                        let at = BlockPos::new(x, y, z);
                        let room = accepts(&scene, &Tunings::uniform(Tuning::DEFAULT), at, MILK);
                        let took = volume.min(room);
                        if took > 0 {
                            let now = scene.volume_at(at) + took;
                            scene.pour(at, now);
                            solver.touch(at);
                            poured += took;
                        }
                    }
                }
            }

            let tuning = Tuning {
                evaporates: evaporates.unwrap_or(0),
                ..Tuning::DEFAULT
            };
            let sinks = settle_seeded(&mut scene, &mut solver, tuning, ticks, 0x9E37_79B9);

            prop_assert_eq!(
                scene.total() + sinks.total(),
                poured,
                "{} cells poured, {} still in the world, {} accounted for as sinks",
                poured,
                scene.total(),
                sinks.total()
            );
        }

        /// No block ever holds more than its terrain leaves room for.
        #[test]
        fn no_block_holds_more_than_it_has_room_for(
            features in prop::collection::vec(feature(), 1..24),
            ticks in 1u64..=40,
        ) {
            let mut scene = Scene::sealed(SPAN + 1);
            let mut solver = Solver::new();

            for feature in &features {
                match *feature {
                    Feature::Solid(x, y, z) => scene.make_solid(x, y, z),
                    Feature::Absorbent(x, y, z, rate) => scene.make_absorbent(x, y, z, rate),
                    Feature::Pour(x, y, z, volume) => {
                        let at = BlockPos::new(x, y, z);
                        scene.pour(at, volume);
                        solver.touch(at);
                    }
                }
            }

            settle_seeded(&mut scene, &mut solver, Tuning::DEFAULT, ticks, 0x9E37_79B9);

            for (at, value) in scene.contents() {
                let pos = BlockPos::new(at.0, at.1, at.2);
                let room = scene
                    .occupancy(pos)
                    .map_or(0, |occupancy| capacity(occupancy, Tuning::DEFAULT.waterlogs_at));
                prop_assert!(
                    value.volume() <= room,
                    "{:?} holds {} cells in room for {}",
                    at,
                    value.volume(),
                    room
                );
            }
        }
    }
}
