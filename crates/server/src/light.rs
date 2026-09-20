// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The server's light, and the adapter that runs core's propagation over it.
//!
//! # Light is derived, not stored
//!
//! Nothing here is written to the world file. Light is a pure function of block
//! content and the mods' emission table, so persisting it would be storing a
//! cache that can disagree with what it was derived from — and a world file
//! whose light said "cave" where its blocks said "hillside" is a bug nobody
//! would think to look for in the file format. A chunk is relit when it enters
//! memory.
//!
//! # What that costs, measured
//!
//! **1.47 ms** for the case that dominates a join — a chunk of air under open
//! sky with its neighbours resident — on the reference machine, down from
//! **2.14 ms** before the terrain reads were cached (see `Lit::blocks`). Task
//! 02b's spike said 30 µs, which was the number the tick's cap was sized on
//! until this was written down; see `handle::RELIGHT_TIME_BUDGET`. Charter rule
//! 18 wants the share of a 50 ms tick, and one relight is 2.9% of one.
//!
//! **Re-measure rather than trusting this line**: `tests::measure_chunk_loaded`
//! prints it, and the 30 µs above is what happens when a number in a doc
//! comment outlives the code it described. The 2.14 → 1.47 figures are an A/B
//! on one machine in one sitting, which is the only comparison worth making.
//!
//! Where it goes, measured the same way: **the flood is ~76% of it** and the
//! seeding passes are the rest. Nothing else is close, so an optimisation that
//! does not make the flood cheaper is not worth the risk.
//!
//! Relighting a chunk that already has light is therefore never free and never
//! useful: the tick skips it, and `bot`'s
//! `a_chunk_is_lit_once_however_many_players_ask_for_it` holds that line.
//!
//! # Relighting a chunk needs its neighbours
//!
//! Light crosses chunk boundaries, so a chunk relit alone is dark down its
//! edges until its neighbours arrive. The region is the chunk exactly, and the
//! ring of blocks around it acts as a boundary condition: those keep their
//! light and flood inward. [`Lighting::chunk_loaded`] reports every other chunk
//! it touched on the way, so the caller can remesh and re-send them too.

use std::collections::{BTreeSet, HashMap};

use tiamot_core::coords::{BlockPos, ChunkPos};
use tiamot_core::light::propagate::{Neighbourhood, Region};
use tiamot_core::light::{Emissions, Faces, Light, LightLayer, propagate};
use tiamot_core::{CHUNK_BLOCKS, MaterialId};

use crate::world::World;

/// The light store, as a mod may read it.
///
/// This is the whole of `game.get_light`'s implementation: the VM lives in
/// core and cannot know about [`Lighting`] (charter rule 3), so it asks through
/// [`tiamot_core::light::LightSource`] and this is what answers.
///
/// **Read-only, deliberately.** A mod that could write light would be writing a
/// derived value — the next relight would overwrite it, and the disagreement in
/// between would be invisible everywhere except in whatever the mod did next.
/// If a mod wants somewhere to be brighter, the way to say so is a block that
/// emits.
#[derive(Debug)]
pub struct Shared {
    lighting: std::sync::Arc<std::sync::RwLock<Lights>>,
}

impl Shared {
    /// Wraps a store the simulation thread owns.
    #[must_use]
    pub const fn new(lighting: std::sync::Arc<std::sync::RwLock<Lights>>) -> Self {
        Self { lighting }
    }
}

/// One [`Lighting`] per domain, made on first use.
///
/// **Light is per-space, like everything else about a place.** A layer is keyed
/// by chunk position, and the same position is different terrain in another
/// domain — so one store shared between them would light a ship's floor with
/// the sky above the overworld at those coordinates, and put the overworld's
/// caves inside somebody's hull.
///
/// A store per domain rather than a domain inside the store: the flood fill
/// reaches for neighbouring layers constantly and wants a map it can index by
/// position alone, and nothing about lighting is cross-domain.
#[derive(Debug)]
pub struct Lights {
    emissions: tiamot_core::light::Emissions,
    /// Which materials light passes through, shared by every domain: a world's
    /// mod set is one set, whatever domains it grows.
    see_through: tiamot_core::light::SeeThrough,
    /// How much each material dims what passes through it: a canopy.
    dimming: tiamot_core::light::Dimming,
    domains: std::collections::BTreeMap<String, Lighting>,
}

impl Lights {
    /// A set of stores for a world whose mods emit these levels.
    #[must_use]
    pub fn new(
        emissions: tiamot_core::light::Emissions,
        see_through: tiamot_core::light::SeeThrough,
        dimming: tiamot_core::light::Dimming,
    ) -> Self {
        Self {
            emissions,
            see_through,
            dimming,
            domains: std::collections::BTreeMap::new(),
        }
    }

    /// One domain's light, created on first use.
    pub fn of(&mut self, domain: &str) -> &mut Lighting {
        self.domains.entry(domain.to_owned()).or_insert_with(|| {
            Lighting::new(
                self.emissions.clone(),
                self.see_through.clone(),
                self.dimming.clone(),
            )
        })
    }

    /// One domain's light, if anything has lit it.
    #[must_use]
    pub fn get(&self, domain: &str) -> Option<&Lighting> {
        self.domains.get(domain)
    }

    /// How many chunks are lit across every domain.
    #[must_use]
    pub fn len(&self) -> usize {
        self.domains.values().map(Lighting::len).sum()
    }

    /// Whether nothing is lit anywhere.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl tiamot_core::light::LightSource for Shared {
    fn light_at(&self, domain: &str, pos: BlockPos) -> Light {
        // A poisoned lock means the simulation thread panicked, in which case
        // there is no light and no world; darkness is the honest answer and
        // panicking inside a mod callback would blame the mod.
        // A space nothing has lit is dark, which is an answer.
        self.lighting.read().map_or(Light::DARK, |lighting| {
            lighting.get(domain).map_or(Light::DARK, |lit| lit.at(pos))
        })
    }
}

/// Every loaded chunk's light, and what the mods said glows.
#[derive(Debug, Default)]
pub struct Lighting {
    layers: HashMap<ChunkPos, LightLayer>,
    /// Chunks answered as dark from their palette alone, without a relight.
    /// See `chunk_loaded`; read by the tick's serve report.
    dark_shortcuts: usize,
    emissions: Emissions,
    /// Which materials light passes straight through: glass. Contract §8.1.
    see_through: tiamot_core::light::SeeThrough,
    /// How much each material dims what it passes: foliage. Contract §8.2.
    ///
    /// **Beside `see_through` rather than inside it.** Leaves are permeable and
    /// they dim; a material that stopped light would be neither, and folding
    /// the two would make a uniform chunk of leaves read as dark solid and be
    /// short-circuited to black without a relight.
    dimming: tiamot_core::light::Dimming,
}

impl Lighting {
    /// A store for a world whose mods emit these levels.
    #[must_use]
    pub fn new(
        emissions: Emissions,
        see_through: tiamot_core::light::SeeThrough,
        dimming: tiamot_core::light::Dimming,
    ) -> Self {
        Self {
            layers: HashMap::new(),
            dark_shortcuts: 0,
            emissions,
            see_through,
            dimming,
        }
    }

    /// What the mods said glows.
    #[must_use]
    pub const fn emissions(&self) -> &Emissions {
        &self.emissions
    }

    /// The light at a block, or [`Light::DARK`] if its chunk is not loaded.
    ///
    /// Dark rather than an error: a mod asking about somewhere nobody is gets
    /// the honest answer that there is no light there to speak of, and an
    /// `Option` would push that judgement onto every caller.
    #[must_use]
    pub fn at(&self, pos: BlockPos) -> Light {
        self.layers
            .get(&pos.chunk())
            .map_or(Light::DARK, |layer| layer.get(pos.local()))
    }

    /// How many chunks have light.
    #[must_use]
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether any chunk has light.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Whether a chunk has light at all.
    ///
    /// The question the tick's catch-up pass asks: a chunk with blocks and no
    /// light renders black, so anything resident and unlit is work to do.
    #[must_use]
    pub fn holds(&self, pos: ChunkPos) -> bool {
        self.layers.contains_key(&pos)
    }

    /// A chunk's levels, for sending to a client.
    #[must_use]
    pub fn layer(&self, pos: ChunkPos) -> Option<&LightLayer> {
        self.layers.get(&pos)
    }

    /// Whether a chunk is one opaque, unlit material through and through — dark
    /// without a relight, and a wall light cannot pass.
    ///
    /// One predicate for two callers: `chunk_loaded` answers such a chunk as
    /// dark, and the streamer treats it as sealed — nothing behind it can be
    /// seen until something digs into it. Kept together so the two can never
    /// disagree about what "solid" means; both need the same two tables.
    #[must_use]
    pub fn is_dark_solid(&self, chunk: &tiamot_core::Chunk) -> bool {
        chunk.is_uniform().is_some_and(|material| {
            !material.is_air()
                && !self.see_through.is(material)
                && self.emissions.of(material).is_dark()
        })
    }

    /// How many loaded chunks were answered as dark without a relight.
    #[must_use]
    pub const fn dark_shortcuts(&self) -> usize {
        self.dark_shortcuts
    }

    /// Forgets a chunk's light.
    ///
    /// Called when a chunk leaves memory. Keeping it would be a slow leak of
    /// 8 KiB per chunk for a world nobody is looking at.
    pub fn forget(&mut self, pos: ChunkPos) {
        self.layers.remove(&pos);
    }

    /// Relights a chunk that has just entered memory.
    ///
    /// Returns every chunk whose light changed, including this one — the caller
    /// needs that set to know what to remesh and what to re-send.
    ///
    /// Costly, and never worth doing twice: see the module docs for what it
    /// measures and for the guard the tick keeps in front of it.
    pub fn chunk_loaded(
        &mut self,
        domain: &str,
        world: &World,
        pos: ChunkPos,
    ) -> BTreeSet<ChunkPos> {
        self.chunk_loaded_with_fluid(domain, world, pos, &Dry)
    }

    /// The same, with the fluid of that space in view.
    ///
    /// **Which is how a lava lake lights a cave.** A caller that has the fluid
    /// store to hand passes it; one that has not — a test, or a space with no
    /// fluid in it — takes [`Dry`] through the signature above and lights
    /// exactly as it always did. See [`Glowing`].
    pub fn chunk_loaded_with_fluid(
        &mut self,
        domain: &str,
        world: &World,
        pos: ChunkPos,
        fluid: &dyn Glowing,
    ) -> BTreeSet<ChunkPos> {
        self.layers.entry(pos).or_insert_with(LightLayer::dark);
        // **A chunk of one opaque, unlit material is dark, and knowing that
        // costs nothing.** Relighting it would darken all 4,096 blocks, scan
        // the border, seed emissions and test the sky over every block — four
        // passes to compute what its palette already says, measured at 68 µs a
        // chunk on the bench's harness against 217 for open air. A player at
        // the default view asks for roughly 2,500 such chunks below their feet,
        // so this was the single largest share of a join's serve cost spent on
        // rock nobody can see. The answer is identical: opaque blocks pass no
        // light in, and a material that emits none puts none there.
        //
        // Three conditions, and all three matter. Uniform, so the palette
        // holds one entry; not air, since air under sky is the expensive case
        // and the bright one; not see-through, because a chunk of glass is lit
        // straight through; and not emissive, because a chunk of lamps is lit
        // from within. The last two are the tables lighting already consults.
        if world
            .resident(domain, pos)
            .is_some_and(|chunk| self.is_dark_solid(chunk))
        {
            self.dark_shortcuts += 1;
            let mut touched = Touched::default();
            touched.chunks.insert(pos);
            // Dark for free, but still a roof: rock arriving over a lit chunk
            // is the commonest way the sky under it goes away.
            self.roof_over_below(domain, world, pos, &mut touched, fluid);
            self.compact(&touched.chunks);
            return touched.chunks;
        }

        // Exactly the chunk. The blocks around it are handled as a boundary
        // condition by `relight` — they keep their light and flood inward —
        // rather than being relit as part of the region. Widening the region
        // instead would clear the neighbours' light and then fail to re-seed
        // it, because the sky that lit them is further away still.
        let corner = BlockPos::from_chunk_corner(pos);
        let span = CHUNK_BLOCKS as i32 - 1;
        let region = Region {
            min: corner,
            max: BlockPos::new(corner.x + span, corner.y + span, corner.z + span),
        };

        let mut touched = Touched::default();
        // The chunk itself, always — even when relighting changed nothing.
        // "Nothing changed" is measured against the dark layer this function
        // just inserted, but a client has no layer at all, so a chunk that is
        // genuinely pitch black still has to be told to it. Without this a
        // client cannot tell "dark" from "not arrived yet", and every
        // underground chunk goes unreported.
        touched.chunks.insert(pos);
        self.with_centre(domain, world, pos, &mut touched, fluid, |lit| {
            propagate::relight(lit, region);
        });
        self.roof_over_below(domain, world, pos, &mut touched, fluid);
        self.compact(&touched.chunks);
        touched.chunks
    }

    /// Takes away the daylight a chunk that has just arrived now stands in
    /// front of, in the lit chunk under it.
    ///
    /// **The sky is wherever the loaded world ends**, so the chunk below took
    /// full sun through a gap that was only "nothing loaded yet", and a relight
    /// of the arrival clears the arrival and nothing else. A floor loaded
    /// before its canopy — it is nearer the player, so it always is — stayed
    /// lit like a meadow. See [`propagate::roofed`].
    ///
    /// Nothing to do when the chunk below holds no light: it has not been lit
    /// yet, and when it is, the arrival will already be over it.
    fn roof_over_below(
        &mut self,
        domain: &str,
        world: &World,
        pos: ChunkPos,
        touched: &mut Touched,
        fluid: &dyn Glowing,
    ) {
        let below = ChunkPos::new(pos.x, pos.y - 1, pos.z);
        if !self.layers.contains_key(&below) {
            return;
        }
        let corner = BlockPos::from_chunk_corner(pos);
        let span = CHUNK_BLOCKS as i32 - 1;
        let region = Region {
            min: corner,
            max: BlockPos::new(corner.x + span, corner.y + span, corner.z + span),
        };
        self.with_centre(domain, world, below, touched, fluid, |lit| {
            propagate::roofed(lit, region);
        });
    }

    /// Re-lights around a block whose content just changed.
    ///
    /// Returns every chunk whose light changed.
    pub fn edited(&mut self, domain: &str, world: &World, pos: BlockPos) -> BTreeSet<ChunkPos> {
        self.edited_with_fluid(domain, world, pos, &Dry)
    }

    /// The same, with the fluid of that space in view — see [`Glowing`].
    pub fn edited_with_fluid(
        &mut self,
        domain: &str,
        world: &World,
        pos: BlockPos,
        fluid: &dyn Glowing,
    ) -> BTreeSet<ChunkPos> {
        let mut touched = Touched::default();
        self.with_centre(domain, world, pos.chunk(), &mut touched, fluid, |lit| {
            propagate::edited(lit, pos);
        });
        self.compact(&touched.chunks);
        touched.chunks
    }

    /// Runs a propagation pass with `centre`'s layer held out of the map.
    ///
    /// Taking it out and putting it back is what lets [`Lit`] reach the layer
    /// the flood spends its time in without a lookup per visit.
    ///
    /// A chunk that arrived here **without** a layer keeps none: writes to it go
    /// nowhere and nothing is inserted, exactly as when the layer was looked up
    /// and found missing. Creating one instead would quietly mark an unlit chunk
    /// as done — [`Lighting::holds`] is what the tick's catch-up pass asks
    /// before relighting, so the chunk would stay black for as long as it stayed
    /// loaded.
    fn with_centre(
        &mut self,
        domain: &str,
        world: &World,
        centre: ChunkPos,
        touched: &mut Touched,
        fluid: &dyn Glowing,
        pass: impl FnOnce(&mut Lit<'_>),
    ) {
        let held = self.layers.remove(&centre);
        let lit_here = held.is_some();
        let terrain = world.solid(domain);
        // Resolved BEFORE the struct takes ownership of the accessor: the
        // reference outlives it either way (`Solid::resident` is tied to the
        // world), but the borrow checker needs the read to happen first.
        let centre_blocks = terrain.resident(centre);
        let mut lit = Lit {
            terrain,
            lighting: self,
            touched,
            centre,
            lit_here,
            layer: held.unwrap_or_else(LightLayer::dark),
            centre_blocks,
            memo: std::cell::Cell::new(None),
            fluid,
            fluid_memo: std::cell::Cell::new(None),
            any_falloff: fluid.any_falloff(),
        };
        pass(&mut lit);
        let layer = lit.layer;
        if lit_here {
            self.layers.insert(centre, layer);
        }
    }

    /// Collapses layers that ended up uniform.
    ///
    /// Once per relight rather than per write — see [`LightLayer::compact`],
    /// which would otherwise turn a linear relight quadratic.
    fn compact(&mut self, chunks: &BTreeSet<ChunkPos>) {
        for pos in chunks {
            if let Some(layer) = self.layers.get_mut(pos) {
                layer.compact();
            }
        }
    }
}

/// Chunks whose light a propagation pass changed.
///
/// A `BTreeSet` rather than a `HashSet`: this set decides what gets remeshed and
/// re-sent, so its iteration order is observable and charter rule 4 wants it
/// fixed.
#[derive(Debug, Default)]
struct Touched {
    chunks: BTreeSet<ChunkPos>,
}

/// What glowing fluid a lighting pass can see.
///
/// **Lava is a light source, and lava is not a block.** A block holds terrain
/// and fluid independently (Sub-Node Contract §4), so a block full of lava is
/// AIR as far as the block store is concerned and emits nothing — which is how
/// a lava lake left a cave pitch black. The rule is that a fluid glows with
/// whatever the material it is drawn as emits: a mod says `light_emit` on the
/// block its fluid looks like, once, and the fluid and the block agree by
/// construction rather than by a second field somebody has to keep in step.
///
/// A layer at a time rather than a level at a block, so the pass can memo the
/// chunk it is standing in exactly as it memos the terrain — an emission query
/// happens for every block of a region and a map probe per block is what the
/// rest of [`Lit`] exists to avoid.
pub trait Glowing {
    /// A chunk's fluid, if anything has pooled there.
    fn layer(&self, pos: ChunkPos) -> Option<&tiamot_core::fluid::FluidLayer>;

    /// What a full block of a fluid is drawn as, for [`Emissions::of`].
    fn material(&self, fluid: tiamot_core::fluid::FluidId) -> Option<MaterialId>;

    /// Levels a block of this fluid takes out of the light reaching it.
    ///
    /// World ask 25. `0` is "like air", which is what every fluid was until a
    /// mod said otherwise, and is what keeps a world with no falloff in it
    /// lighting exactly as it did — see
    /// [`propagate::Neighbourhood::falloff`](tiamot_core::light::Neighbourhood::falloff).
    ///
    /// Defaulted, so a test fixture and [`Dry`] say nothing about it.
    fn falloff(&self, fluid: tiamot_core::fluid::FluidId) -> u8 {
        let _ = fluid;
        0
    }

    /// Whether ANY registered fluid takes light out of what passes through it.
    ///
    /// Read once when a pass starts, not per block. A flood asks `falloff` once
    /// a visit and a relight visits a chunk's blocks eighteen times over, so
    /// this is what keeps a world whose water is ordinary water from reaching
    /// into the fluid layer seventy-six thousand times to be told nothing —
    /// exactly the gate `Emissions::any` is for the question beside it.
    fn any_falloff(&self) -> bool {
        false
    }
}

/// A world with no fluid in it at all.
///
/// The honest answer for every caller that has no fluid store to hand — the
/// tests in this module, and anything lighting a space before a pond exists.
pub struct Dry;

impl Glowing for Dry {
    fn layer(&self, _pos: ChunkPos) -> Option<&tiamot_core::fluid::FluidLayer> {
        None
    }

    fn material(&self, _fluid: tiamot_core::fluid::FluidId) -> Option<MaterialId> {
        None
    }
}

/// The world and its light, as [`Neighbourhood`] wants to see them.
///
/// # Why the chunk being relit is held apart from the others
///
/// A flood spends nearly all of its visits inside one chunk, and every one of
/// them used to hash a [`ChunkPos`] to find that chunk's layer — twice, once to
/// read the level and once to write it. Holding the centre layer as a field
/// turns those into a field access and leaves the map for the boundary, where
/// the flood goes rarely.
///
/// Measured on the reference machine, relighting a chunk of air under open sky
/// with its neighbours resident: **1.56 ms before, 1.38 ms after**. Worth
/// keeping and not the win it looks like it should be — the lookups are 12% of
/// this, and the propagation itself is the rest.
///
/// # The terrain memo was rejected once, on a measurement that was right
///
/// This comment used to end "memoising the *world* chunk the same way was
/// measured first and bought 3%, so it was not kept." That was true when it
/// was written, and it is not true now: measured 2026-09-05, the same memo is
/// **1.98 ms → 1.44 ms, about 28%** (three runs either side, no overlap).
///
/// **The order the two were tried in is the whole explanation.** The terrain
/// memo was measured while the light-layer lookups above were still there and
/// dominating; with those gone, the terrain lookups became the top cost and
/// the same change is worth ten times what it was. An optimisation correctly
/// rejected on a measurement can become the right one once something else is
/// removed — so re-measure a rejected idea after changing what it competed
/// with, rather than trusting the note that rejected it. This one cost a year.
struct Lit<'a> {
    terrain: crate::world::Solid<'a>,
    lighting: &'a mut Lighting,
    touched: &'a mut Touched,
    /// The chunk this pass is centred on, taken out of the map for the
    /// duration and put back by [`Lighting::with_centre`].
    centre: ChunkPos,
    /// Whether the centre chunk had light at all when the pass started.
    ///
    /// A chunk that had none keeps none — writes to it go nowhere, exactly as
    /// they did when the layer was looked up and found missing.
    lit_here: bool,
    /// Its light. Owned here, so reaching it costs nothing.
    layer: LightLayer,
    /// The centre's BLOCKS, resolved once.
    ///
    /// **The same trick as `layer`, for the other half of what a pass reads.**
    /// A relight asks `faces` about 76,000 times for one chunk — eighteen times
    /// per block, because a flood revisits — and every one of those was a
    /// `ChunkPos` division and a `HashMap` probe before reaching the blocks.
    /// The overwhelming majority are inside the centre, so resolving it once
    /// turns the probe into a comparison.
    centre_blocks: Option<&'a tiamot_core::chunk::Chunk>,
    /// The last non-centre chunk looked up, and it.
    ///
    /// One entry, because the rest of what a pass touches is the ring of
    /// neighbours it floods into and it works along one at a time. A bigger
    /// cache would be a second copy of the map it is standing in front of.
    memo: std::cell::Cell<Option<(ChunkPos, &'a tiamot_core::chunk::Chunk)>>,
    /// The fluid this pass can see glowing. See [`Glowing`].
    fluid: &'a dyn Glowing,
    /// The last fluid layer looked up, by the same argument as `memo`.
    ///
    /// `Some(None)` is "asked, and that chunk is dry" — worth remembering,
    /// because a dry chunk is the overwhelmingly common case and re-asking is
    /// the probe this exists to skip.
    fluid_memo: std::cell::Cell<Option<(ChunkPos, Option<&'a tiamot_core::fluid::FluidLayer>)>>,
    /// Whether any registered fluid dims what passes through it, resolved once
    /// when the pass starts. See [`Glowing::any_falloff`].
    any_falloff: bool,
}

impl<'a> Lit<'a> {
    /// The blocks of whatever chunk `pos` is in, if it is resident.
    fn blocks(&self, pos: ChunkPos) -> Option<&'a tiamot_core::chunk::Chunk> {
        if pos == self.centre {
            return self.centre_blocks;
        }
        if let Some((at, chunk)) = self.memo.get()
            && at == pos
        {
            return Some(chunk);
        }
        let chunk = self.terrain.resident(pos)?;
        self.memo.set(Some((pos, chunk)));
        Some(chunk)
    }
}

impl Lit<'_> {
    /// What the fluid standing in a block glows, if any of it does.
    ///
    /// **Gated on there being any emitting material at all**, which is one bool
    /// and is false in every world whose mods registered no lamp and no lava.
    /// Any of the fluid, at full strength: a block holding one cell of lava
    /// glows like a block holding twenty-seven, for the reason
    /// [`Emissions::block`] gives about a chiselled lamp — dimming by how much
    /// is there would make the solver a dimmer switch, and the engine has no
    /// business deciding that.
    fn fluid_emission(&self, pos: BlockPos) -> Light {
        if !self.lighting.emissions.any() {
            return Light::DARK;
        }
        let chunk = pos.chunk();
        let layer = match self.fluid_memo.get() {
            Some((at, layer)) if at == chunk => layer,
            _ => {
                let layer = self.fluid.layer(chunk);
                self.fluid_memo.set(Some((chunk, layer)));
                layer
            }
        };
        let Some(layer) = layer else {
            return Light::DARK;
        };
        let held = layer.get(pos.local());
        if held.is_empty() {
            return Light::DARK;
        }
        self.fluid
            .material(held.fluid())
            .map_or(Light::DARK, |material| self.lighting.emissions.of(material))
    }

    /// What the fluid at a block takes out of the light arriving in it.
    ///
    /// The same memo `fluid_emission` uses, and the same shape: one probe per
    /// chunk rather than per block, because a flood asks this once per visit
    /// and a relight visits a chunk's blocks eighteen times over.
    ///
    /// **Gated on any fluid declaring a falloff at all**, so a world whose
    /// water is ordinary water pays one bool per call and never reaches for the
    /// layer — which is every world until somebody asks for a dark sea.
    fn fluid_falloff(&self, pos: BlockPos) -> u8 {
        if !self.any_falloff {
            return 0;
        }
        let chunk = pos.chunk();
        let layer = match self.fluid_memo.get() {
            Some((at, layer)) if at == chunk => layer,
            _ => {
                let layer = self.fluid.layer(chunk);
                self.fluid_memo.set(Some((chunk, layer)));
                layer
            }
        };
        let Some(layer) = layer else {
            return 0;
        };
        let held = layer.get(pos.local());
        if held.is_empty() {
            return 0;
        }
        self.fluid.falloff(held.fluid())
    }

    /// How much the terrain in this block dims what passes through it.
    ///
    /// Contract §8.2, World ask 24. **Gated on any material dimming at all**,
    /// like `fluid_falloff` above it: a world with no canopy pays one bool per
    /// call and never reaches for a chunk.
    ///
    /// `Uniform` only, exactly as `faces` reads `see_through`: a block that is
    /// partly leaves is a block whose cells the ordinary rule already answers,
    /// and inventing a per-cell dimming would be a second lighting resolution
    /// (contract §8.1 records the same limit for glass).
    fn block_falloff(&self, pos: BlockPos) -> u8 {
        if !self.lighting.dimming.any() {
            return 0;
        }
        let Some(chunk) = self.blocks(pos.chunk()) else {
            return 0;
        };
        match chunk.get_block_local(pos.local()) {
            tiamot_core::block::BlockView::Uniform(material) => self.lighting.dimming.of(material),
            _ => 0,
        }
    }

    /// The light at a block, from the centre layer where it lives there.
    fn level(&self, pos: BlockPos) -> Light {
        if pos.chunk() == self.centre {
            if !self.lit_here {
                return Light::DARK;
            }
            return self.layer.get(pos.local());
        }
        self.lighting.at(pos)
    }
}

impl Neighbourhood for Lit<'_> {
    fn faces(&self, pos: BlockPos) -> Option<Faces> {
        // `resident` rather than `chunk`: propagation must never generate a
        // chunk. A flood reaching unexplored terrain would otherwise turn a
        // lamp into unbounded worldgen inside the tick, which is the same trap
        // collision documents at `World::resident`. `blocks` preserves that —
        // it caches what `resident` returned and never asks for more.
        let chunk = self.blocks(pos.chunk())?;
        // **Glass, applied where the cached answer is READ.** Contract §8.1: a
        // whole block of one transparent material is permeable on all six
        // faces, or a glass roof makes a dark room. The permeability CACHE is
        // untouched and rule 19 still holds — this is a table lookup, not the
        // 3x3 cell test — and `Chunk` stays ignorant of the material registry,
        // which it must, being built in ninety-four places.
        //
        // Gated on `any()` first, so a world with no glass in it pays one bool
        // per call and never reaches for the block. That is every world until a
        // mod registers a window.
        //
        // `Uniform` only. A chiselled or mixed block holding glass falls back
        // to the cell rule: "how much light does a half-glass block pass" has
        // no obviously right answer and no caller, and §8.1 records that as a
        // limit rather than guessing.
        if self.lighting.see_through.any()
            && let tiamot_core::block::BlockView::Uniform(material) =
                chunk.get_block_local(pos.local())
            && self.lighting.see_through.is(material)
        {
            return Some(Faces::OPEN);
        }
        Some(chunk.faces(pos.local()))
    }

    fn emission(&self, pos: BlockPos) -> Light {
        let Some(chunk) = self.blocks(pos.chunk()) else {
            return Light::DARK;
        };
        let block = self
            .lighting
            .emissions
            .block(&chunk.get_block_local(pos.local()));
        // **And what is POURED here, which is not in the block store at all.**
        // See [`Glowing`]: a block of lava is air with a fluid in it, so
        // without this a lake of it lights nothing. The brighter of the two per
        // channel, exactly as a block of several materials takes the brightest
        // of them — a lamp under water is still a lamp.
        block.max(self.fluid_emission(pos))
    }

    fn light(&self, pos: BlockPos) -> Light {
        self.level(pos)
    }

    /// **The most either says, not the sum.** A block of leaves standing in
    /// water dims by whichever takes more — "levels lost per block of it" is a
    /// property of the block, and adding two of them would make a flooded
    /// canopy darker than either the water or the canopy ever asked for.
    fn falloff(&self, pos: BlockPos) -> u8 {
        self.fluid_falloff(pos).max(self.block_falloff(pos))
    }

    fn set_light(&mut self, pos: BlockPos, level: Light) {
        let chunk = pos.chunk();
        let local = pos.local();
        if chunk == self.centre {
            if !self.lit_here || self.layer.get(local) == level {
                return;
            }
            self.layer.set(local, level);
            self.touched.chunks.insert(chunk);
            return;
        }
        // Only where a chunk is actually loaded. A flood reaching the edge of
        // the loaded region has nowhere to write, and that is the bound working.
        let Some(layer) = self.lighting.layers.get_mut(&chunk) else {
            return;
        };
        if layer.get(local) == level {
            return;
        }
        layer.set(local, level);
        self.touched.chunks.insert(chunk);
    }
}

/// Builds a transparency table from what the mods registered.
///
/// Keyed by WORLD id for the same reason emissions are: a world that has seen a
/// different mod set numbers its materials differently, and a table of this
/// session's runtime ids would name every window one number out (charter rule
/// 8).
/// Builds a dimming table from what the mods registered.
///
/// World ask 24, Sub-Node Contract §8.2: a canopy that shades. Keyed by WORLD
/// id for the reason the two tables beside it are — a world that has seen a
/// different mod set numbers its materials differently.
///
/// Only the materials that dim, so [`tiamot_core::light::Dimming::any`] is
/// false for every world until a mod asks, and the lighting hot path skips the
/// question with one bool.
#[must_use]
pub fn dimming_from_rules(
    rules: &[tiamot_core::script::BlockRules],
    id_of: impl Fn(&str) -> Option<MaterialId>,
) -> tiamot_core::light::Dimming {
    tiamot_core::light::Dimming::new(
        rules
            .iter()
            .filter(|rule| rule.light_falloff > 0)
            .filter_map(|rule| Some((id_of(&rule.block)?, rule.light_falloff))),
    )
}

#[must_use]
pub fn see_through_from_rules(
    rules: &[tiamot_core::script::BlockRules],
    id_of: impl Fn(&str) -> Option<MaterialId>,
) -> tiamot_core::light::SeeThrough {
    tiamot_core::light::SeeThrough::new(
        rules
            .iter()
            // Contract §8.2: foliage passes light the way glass does. Leaves
            // that stopped it would put a forest floor in total darkness, and
            // "some light" is not expressible — permeability is yes or no.
            .filter(|rule| rule.transparent || rule.cutout)
            .filter_map(|rule| id_of(&rule.block)),
    )
}

/// Builds an emission table from what the mods registered.
#[must_use]
pub fn emissions_from_rules(
    rules: &[tiamot_core::script::BlockRules],
    id_of: impl Fn(&str) -> Option<MaterialId>,
) -> Emissions {
    Emissions::new(rules.iter().filter_map(|rule| {
        let level = rule.emission();
        if level.is_dark() {
            return None;
        }
        Some((id_of(&rule.block)?, level))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamot_core::block::BlockValue;
    use tiamot_core::light::MAX_LEVEL;
    use tiamot_core::persist::WorldDb;

    const STONE: MaterialId = MaterialId(2);
    const LAMP: MaterialId = MaterialId(3);

    /// A generator that produces nothing but air, so tests decide the contents.
    struct Empty;

    impl crate::world::ChunkSource for Empty {
        fn generate(
            &mut self,
            _domain: &str,
            pos: ChunkPos,
            _seed: u64,
        ) -> tiamot_core::chunk::Chunk {
            tiamot_core::chunk::Chunk::air(pos)
        }
    }

    fn world() -> World {
        let mut registry = tiamot_core::Registry::new();
        for name in ["test:stone", "test:lamp"] {
            registry.register(name).expect("register");
        }
        let db = WorldDb::open_in_memory(&mut registry).expect("open");
        World::open(db, 1).expect("world")
    }

    fn lighting() -> Lighting {
        lighting_with(tiamot_core::light::SeeThrough::default())
    }

    /// The same, for a world that has some glass in it.
    fn lighting_with(see_through: tiamot_core::light::SeeThrough) -> Lighting {
        dimming_lighting(see_through, tiamot_core::light::Dimming::default())
    }

    /// The same, for a world whose mods dim light: a canopy (ask 24).
    fn dimming_lighting(
        see_through: tiamot_core::light::SeeThrough,
        dimming: tiamot_core::light::Dimming,
    ) -> Lighting {
        Lighting::new(
            Emissions::new([(LAMP, Light::new(0, MAX_LEVEL, 0, 0))]),
            see_through,
            dimming,
        )
    }

    /// Loads a chunk so it is resident, without caring what is in it.
    fn resident(world: &mut World, pos: ChunkPos) {
        world
            .chunk(tiamot_core::domain::OVERWORLD, pos, &mut Empty)
            .expect("chunk");
    }

    #[test]
    fn an_air_chunk_under_open_sky_is_fully_lit() {
        let mut world = world();
        let mut light = lighting();
        let pos = ChunkPos::new(0, 0, 0);
        resident(&mut world, pos);

        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);

        assert_eq!(light.at(BlockPos::new(8, 15, 8)).sun(), MAX_LEVEL);
        assert_eq!(
            light.at(BlockPos::new(8, 0, 8)).sun(),
            MAX_LEVEL,
            "sunlight did not reach the bottom of an empty chunk"
        );
    }

    #[test]
    fn a_chunk_arriving_over_a_lit_one_takes_its_sky_away() {
        // The floor loads first because it is nearer, and used to keep the
        // sun it saw through the gap that was only "not loaded yet". Both
        // arrival paths: a chunk of rock is dark for free and must still roof
        // what is under it, and a canopy with a hole goes through the relight.
        let floor = ChunkPos::new(0, 0, 0);
        let above = ChunkPos::new(0, 1, 0);
        // A corner twenty blocks from the hole, beyond the fifteen daylight
        // spreads sideways: nearer, a lit floor is the right answer.
        let under = BlockPos::new(0, 0, 0);

        for holed in [false, true] {
            let mut world = world();
            let mut light = lighting();
            resident(&mut world, floor);
            light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, floor);
            assert_eq!(light.at(under).sun(), MAX_LEVEL, "no sky to lose");

            resident(&mut world, above);
            {
                let chunk = world
                    .chunk(tiamot_core::domain::OVERWORLD, above, &mut Empty)
                    .expect("chunk");
                for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
                    let local = tiamot_core::coords::LocalBlock::from_index(index);
                    let hole = holed && (10..13).contains(&local.x) && (10..13).contains(&local.z);
                    if !hole {
                        chunk.set_block_local(local, BlockValue::Uniform(STONE));
                    }
                }
            }
            let shortcuts = light.dark_shortcuts;
            let touched = light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, above);
            assert_eq!(
                light.dark_shortcuts > shortcuts,
                !holed,
                "the fixture did not exercise the path it names"
            );

            assert_eq!(
                light.at(under).sun(),
                0,
                "the floor kept its sky under an arriving roof (holed = {holed})"
            );
            assert!(
                touched.contains(&floor),
                "the floor's light changed and was not reported: {touched:?}"
            );
            if holed {
                assert_eq!(
                    light.at(BlockPos::new(11, 0, 11)).sun(),
                    MAX_LEVEL,
                    "the floor under the hole lost the sky it still has"
                );
            }
        }
    }

    #[test]
    fn a_canopy_shades_the_floor_under_it_without_roofing_it() {
        // **Contract §8.2, World ask 24.** Leaves are permeable, so a canopy
        // is not a roof and a forest floor is not a cellar. But passing light
        // untouched made a rainforest floor as bright as a meadow: the world
        // mod measured 85% of its floor under a whole leaf block, and most of
        // those columns still read `sun = 15`.
        //
        // Measured three ways in one test, because the claim is the
        // DIFFERENCE: the same trees, and the only change is whether the
        // material dims.
        const LEAF: tiamot_core::MaterialId = tiamot_core::MaterialId(9);

        let forest = || {
            let mut world = world();
            let pos = ChunkPos::new(0, 0, 0);
            resident(&mut world, pos);
            {
                let chunk = world
                    .chunk(tiamot_core::domain::OVERWORLD, pos, &mut Empty)
                    .expect("chunk");
                // Ground at y = 0, and a canopy three blocks thick at y = 9.
                for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
                    let local = tiamot_core::coords::LocalBlock::from_index(index);
                    if local.y == 0 {
                        chunk.set_block_local(local, BlockValue::Uniform(STONE));
                    } else if (9..12).contains(&local.y) {
                        chunk.set_block_local(local, BlockValue::Uniform(LEAF));
                    }
                }
            }
            (world, pos)
        };

        let floor = BlockPos::new(8, 1, 8);
        let see_through = || tiamot_core::light::SeeThrough::new([LEAF]);

        // Leaves that pass light untouched: the floor is a meadow. This is
        // what the mod reported, and it is the control.
        let (world, pos) = forest();
        let mut light = lighting_with(see_through());
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);
        assert_eq!(
            light.at(floor).sun(),
            MAX_LEVEL,
            "the control is wrong: undimmed leaves should pass full daylight"
        );

        // The same canopy, dimming two levels a block: shade, not darkness.
        let (world, pos) = forest();
        let mut light =
            dimming_lighting(see_through(), tiamot_core::light::Dimming::new([(LEAF, 2)]));
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);
        let shaded = light.at(floor).sun();
        assert!(
            shaded > 0 && shaded < MAX_LEVEL,
            "a canopy should shade its floor, not roof it: {shaded}"
        );

        // And a thicker canopy is darker than a thinner one, which is what
        // makes a rainforest read differently from a copse.
        let (world, pos) = forest();
        let mut heavy =
            dimming_lighting(see_through(), tiamot_core::light::Dimming::new([(LEAF, 5)]));
        heavy.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);
        assert!(
            heavy.at(floor).sun() < shaded,
            "five levels a block lit the floor at {} against two levels' {shaded}",
            heavy.at(floor).sun()
        );

        // A canopy is still not a roof: the block INSIDE the leaves is lit,
        // and a uniform chunk of them is never short-circuited to black.
        assert!(
            light.at(BlockPos::new(8, 10, 8)).sun() > 0,
            "the canopy itself went dark"
        );
    }

    #[test]
    fn a_glass_roof_does_not_make_a_dark_room() {
        // **Contract §8.1.** A window that blocked light would be a see-through
        // wall rather than glass, and a greenhouse is the first thing anybody
        // builds with it.
        //
        // Measured both ways in one test, because the interesting claim is the
        // DIFFERENCE: the same room, the same roof, and the only change is
        // whether the roof's material is in the transparency table.
        const GLASS: tiamot_core::MaterialId = tiamot_core::MaterialId(9);

        let room = |material: tiamot_core::MaterialId| {
            let mut world = world();
            let pos = ChunkPos::new(0, 0, 0);
            resident(&mut world, pos);
            {
                let chunk = world
                    .chunk(tiamot_core::domain::OVERWORLD, pos, &mut Empty)
                    .expect("chunk");
                // Solid up to y = 8, so nothing reaches the floor from the side.
                for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
                    let local = tiamot_core::coords::LocalBlock::from_index(index);
                    if local.y <= 8 {
                        chunk.set_block_local(local, BlockValue::Uniform(STONE));
                    }
                }
                // A room hollowed out under it, roofed with `material`.
                for x in 4..12 {
                    for z in 4..12 {
                        for y in 4..8 {
                            chunk.set_block_local(
                                tiamot_core::coords::LocalBlock::new(x, y, z),
                                BlockValue::AIR,
                            );
                        }
                        chunk.set_block_local(
                            tiamot_core::coords::LocalBlock::new(x, 8, z),
                            BlockValue::Uniform(material),
                        );
                    }
                }
            }
            (world, pos)
        };

        // Roofed in stone, with glass registered but not used: dark.
        let (world, pos) = room(STONE);
        let mut light = lighting_with(tiamot_core::light::SeeThrough::new([GLASS]));
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);
        assert_eq!(
            light.at(BlockPos::new(8, 7, 8)).sun(),
            0,
            "a stone roof let daylight in"
        );

        // The same room roofed in glass: lit.
        let (world, pos) = room(GLASS);
        let mut light = lighting_with(tiamot_core::light::SeeThrough::new([GLASS]));
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);
        assert!(
            light.at(BlockPos::new(8, 7, 8)).sun() > 0,
            "a glass roof made a dark room"
        );

        // And the table is what decides, not the material id: the same glass
        // roof in a world where nothing is registered transparent stays dark.
        // Without this the test would pass for a build that treated every
        // material as see-through.
        let (world, pos) = room(GLASS);
        let mut light = lighting();
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);
        assert_eq!(
            light.at(BlockPos::new(8, 7, 8)).sun(),
            0,
            "light passed a material no mod registered as transparent"
        );
    }

    #[test]
    fn a_lamp_lights_the_dark_around_it() {
        let mut world = world();
        let mut light = lighting();
        let pos = ChunkPos::new(0, 0, 0);
        resident(&mut world, pos);
        // Fill it solid so sunlight cannot get in, then hollow out a room.
        {
            let chunk = world
                .chunk(tiamot_core::domain::OVERWORLD, pos, &mut Empty)
                .expect("chunk");
            for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
                chunk.set_block_local(
                    tiamot_core::coords::LocalBlock::from_index(index),
                    BlockValue::Uniform(STONE),
                );
            }
            for x in 4..12 {
                for y in 4..12 {
                    for z in 4..12 {
                        chunk.set_block_local(
                            tiamot_core::coords::LocalBlock::new(x, y, z),
                            BlockValue::AIR,
                        );
                    }
                }
            }
            chunk.set_block_local(
                tiamot_core::coords::LocalBlock::new(8, 8, 8),
                BlockValue::Uniform(LAMP),
            );
        }

        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);

        assert_eq!(light.at(BlockPos::new(8, 8, 8)).red(), MAX_LEVEL);
        assert_eq!(light.at(BlockPos::new(9, 8, 8)).red(), MAX_LEVEL - 1);
        assert_eq!(
            light.at(BlockPos::new(8, 8, 8)).sun(),
            0,
            "a sealed room saw daylight"
        );
    }

    #[test]
    fn light_from_a_neighbouring_chunk_reaches_across_the_seam() {
        // The reason `chunk_loaded` relights a region a block wider than the
        // chunk. Without it a chunk arriving next to a lit one is dark down its
        // edge until something else disturbs it, and the seam is visible.
        let mut world = world();
        let mut light = lighting();
        let west = ChunkPos::new(-1, 0, 0);
        let east = ChunkPos::new(0, 0, 0);
        resident(&mut world, west);
        resident(&mut world, east);

        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, west);
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, east);

        // The block either side of the boundary is lit from the sky in both
        // chunks, so instead put a lamp at the very edge of the west chunk and
        // check it crosses.
        {
            let chunk = world
                .chunk(tiamot_core::domain::OVERWORLD, west, &mut Empty)
                .expect("chunk");
            chunk.set_block_local(
                tiamot_core::coords::LocalBlock::new(15, 8, 8),
                BlockValue::Uniform(LAMP),
            );
        }
        let touched = light.edited(
            tiamot_core::domain::OVERWORLD,
            &world,
            BlockPos::new(-1, 8, 8),
        );

        assert!(
            touched.contains(&east),
            "an edit at the seam did not report the neighbour as changed: {touched:?}"
        );
        assert!(
            light.at(BlockPos::new(0, 8, 8)).red() > 0,
            "a lamp on the boundary did not light the next chunk"
        );
    }

    #[test]
    fn propagation_never_generates_a_chunk() {
        // The trap `World::resident` documents, from lighting's side: a flood
        // that could generate terrain would turn one lamp into unbounded
        // worldgen inside the tick.
        let mut world = world();
        let mut light = lighting();
        let pos = ChunkPos::new(0, 0, 0);
        resident(&mut world, pos);
        let before = world.cached();

        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);

        assert_eq!(
            world.cached(),
            before,
            "relighting generated {} chunks",
            world.cached() - before
        );
    }

    #[test]
    fn a_chunk_that_is_pitch_black_is_still_reported() {
        // A client has no layer for a chunk it has never been told about, so
        // "dark" and "not arrived" are the same to it. Reporting only what
        // CHANGED would leave every underground chunk unsent, and a client
        // cannot tell that from a message still in flight.
        let mut world = world();
        let mut light = lighting();
        let pos = ChunkPos::new(0, -4, 0);
        {
            let chunk = world
                .chunk(tiamot_core::domain::OVERWORLD, pos, &mut Empty)
                .expect("chunk");
            for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
                chunk.set_block_local(
                    tiamot_core::coords::LocalBlock::from_index(index),
                    BlockValue::Uniform(STONE),
                );
            }
        }

        let touched = light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);

        assert!(
            touched.contains(&pos),
            "a chunk that relit to unchanged darkness was not reported: {touched:?}"
        );
        assert!(
            light.layer(pos).is_some_and(|layer| layer
                .is_uniform()
                .is_some_and(tiamot_core::light::Light::is_dark)),
            "solid rock with no lamps should be uniformly dark"
        );
        // And it was answered from the palette, not relit: the whole point of
        // the shortcut, and the assertion that would catch it silently falling
        // back to four passes over the chunk.
        assert_eq!(
            light.dark_shortcuts(),
            1,
            "a chunk of one opaque unlit material should be answered as dark without a relight"
        );
    }

    #[test]
    fn the_dark_shortcut_refuses_glass_and_lamps() {
        // The shortcut's two tables, each doing its job. A chunk of one
        // material is not dark just because it is one material: glass under
        // the sky is lit straight through, and a chunk of lamps is lit from
        // within. Either taken as dark would be a wrong answer shipped to
        // every client for the cheapest possible reason.
        const GLASS: MaterialId = MaterialId(9);
        let mut world = world();
        // Glass has to BE glass to the table, and the sky has to reach it:
        // `resident` makes the chunks around it exist, since an unloaded
        // neighbour is opaque and would roof the glass in rock.
        let mut light = lighting_with(tiamot_core::light::SeeThrough::new([GLASS]));
        let fill = |world: &mut World, at: ChunkPos, material: MaterialId| {
            resident(world, at);
            let chunk = world
                .chunk(tiamot_core::domain::OVERWORLD, at, &mut Empty)
                .expect("chunk");
            for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
                chunk.set_block_local(
                    tiamot_core::coords::LocalBlock::from_index(index),
                    BlockValue::Uniform(material),
                );
            }
        };
        let dark = |light: &Lighting, at: ChunkPos| {
            light.layer(at).is_some_and(|layer| {
                layer
                    .is_uniform()
                    .is_some_and(tiamot_core::light::Light::is_dark)
            })
        };

        // Glass, under open sky: the whole chunk lights up through it.
        let glass_at = ChunkPos::new(2, 0, 0);
        fill(&mut world, glass_at, GLASS);
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, glass_at);
        assert!(
            !dark(&light, glass_at),
            "a chunk of glass under the sky was answered as dark"
        );

        // Lamps, deep underground: lit from within, however buried.
        let lamps_at = ChunkPos::new(0, -6, 0);
        fill(&mut world, lamps_at, LAMP);
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, lamps_at);
        assert!(
            !dark(&light, lamps_at),
            "a chunk of lamps was answered as dark"
        );
        assert_eq!(
            light.dark_shortcuts(),
            0,
            "neither chunk may take the shortcut"
        );
    }

    #[test]
    fn a_forgotten_chunk_takes_its_light_with_it() {
        let mut world = world();
        let mut light = lighting();
        let pos = ChunkPos::new(0, 0, 0);
        resident(&mut world, pos);
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);
        assert_eq!(light.len(), 1);

        light.forget(pos);
        assert!(light.is_empty());
        assert_eq!(light.at(BlockPos::new(0, 0, 0)), Light::DARK);
    }

    #[test]
    fn an_uninteresting_chunk_costs_one_word() {
        // A fully lit air chunk and a sealed dark one are both uniform, and the
        // layer should say so — 8 KiB per chunk for "all daylight" would be a
        // pure waste across a streamed world.
        let mut world = world();
        let mut light = lighting();
        let pos = ChunkPos::new(0, 0, 0);
        resident(&mut world, pos);
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, pos);

        let layer = light.layer(pos).expect("lit");
        assert!(
            layer.is_compact(),
            "a uniformly lit chunk kept its dense array"
        );
        assert_eq!(layer.memory_usage(), 0);
    }

    #[test]
    fn a_lamp_lights_across_a_chunk_boundary_and_stops_when_it_goes() {
        // Reported from the window: placing a lamp beside a chunk boundary
        // sometimes left the NEIGHBOUR dark, and removing one left the
        // neighbour lit. Both directions, because they are different halves of
        // the incremental path and only one of them was broken.
        let mut world = world();
        let mut light = lighting();

        // Two chunks side by side, both resident and both lit, so this is the
        // incremental path rather than a chunk arriving.
        let here = ChunkPos::new(0, 0, 0);
        let next = ChunkPos::new(1, 0, 0);
        resident(&mut world, here);
        resident(&mut world, next);
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, here);
        light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, next);

        // A block just inside the first chunk's far edge, and one just over the
        // line in the second.
        let lamp = BlockPos::new(15, 8, 8);
        let across = BlockPos::new(16, 8, 8);
        assert_eq!(across.chunk(), next, "the test aims at the wrong chunk");

        {
            let chunk = world
                .chunk(tiamot_core::domain::OVERWORLD, here, &mut Empty)
                .expect("chunk");
            chunk.set_block_local(
                tiamot_core::coords::LocalBlock::new(15, 8, 8),
                BlockValue::Uniform(LAMP),
            );
        }
        let touched = light.edited(tiamot_core::domain::OVERWORLD, &world, lamp);

        assert!(
            light.at(across).red() > 0,
            "the lamp lit nothing across the chunk boundary: {:?}",
            light.at(across)
        );
        assert!(
            touched.contains(&next),
            "the neighbour's light changed and was not reported, so a client never hears about \
             it: {touched:?}"
        );

        // And now take it away again.
        {
            let chunk = world
                .chunk(tiamot_core::domain::OVERWORLD, here, &mut Empty)
                .expect("chunk");
            chunk.set_block_local(
                tiamot_core::coords::LocalBlock::new(15, 8, 8),
                BlockValue::AIR,
            );
        }
        let touched = light.edited(tiamot_core::domain::OVERWORLD, &world, lamp);

        assert_eq!(
            light.at(across).red(),
            0,
            "the lamp went and its light stayed in the next chunk over: {:?}",
            light.at(across)
        );
        assert!(
            touched.contains(&next),
            "the neighbour went dark and was not reported: {touched:?}"
        );
    }

    /// What relighting one chunk costs, for the case that dominates a join.
    ///
    /// **Ignored: it measures rather than asserts.** A timing assertion here
    /// would be a gate on shared silicon, which this repository has learned
    /// twice not to write. Run it by hand — `cargo test -p server
    /// measure_chunk_loaded -- --ignored --nocapture` — when changing anything
    /// on the relight path, and put the number in the module docs.
    ///
    /// The module docs quoted 1.38 ms from Task 10 and nothing re-measured it;
    /// Task 02b's 30 µs went stale the same way and the relight cap was
    /// sized on it — a count, until a terrain mod's caves made one relight
    /// cost several times the reference world's and the count became a clock. This exists so the next number does not have to be
    /// archaeology.
    #[test]
    #[ignore = "measures rather than asserts; run by hand"]
    fn measure_chunk_loaded() {
        // **The case the module docs quote**: a chunk of air under OPEN SKY —
        // nothing resident above it — with its neighbours resident.
        let mut world = world();
        let mut light = lighting();
        for x in -1..=1 {
            for z in -1..=1 {
                for y in -1..=0 {
                    resident(&mut world, ChunkPos::new(x, y, z));
                }
            }
        }
        let centre = ChunkPos::new(0, 0, 0);
        for _ in 0..20 {
            light.forget(centre);
            light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, centre);
        }
        let mut total = std::time::Duration::ZERO;
        const N: u32 = 200;
        for _ in 0..N {
            light.forget(centre);
            let t = std::time::Instant::now();
            light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, centre);
            total += t.elapsed();
        }
        println!("chunk_loaded (air, open sky): mean {:?}", total / N);
    }

    /// What a canopy costs a relight — World ask 24.
    ///
    /// The dimming lookup is one `get_block_local` per flood visit, gated on
    /// any material dimming at all, so a world with no foliage pays a bool.
    /// This measures the world that does pay, against the same chunk with the
    /// table empty. Run it the same way as the harness above.
    #[test]
    #[ignore = "measures rather than asserts; run by hand"]
    fn measure_canopy_relight() {
        const LEAF: tiamot_core::MaterialId = tiamot_core::MaterialId(9);

        let scene = || {
            let mut world = world();
            for x in -1..=1 {
                for z in -1..=1 {
                    for y in -1..=0 {
                        resident(&mut world, ChunkPos::new(x, y, z));
                    }
                }
            }
            {
                let chunk = world
                    .chunk(
                        tiamot_core::domain::OVERWORLD,
                        ChunkPos::new(0, 0, 0),
                        &mut Empty,
                    )
                    .expect("chunk");
                // A canopy four blocks thick across the whole chunk: more
                // foliage than any real forest puts over one column.
                for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
                    let local = tiamot_core::coords::LocalBlock::from_index(index);
                    if (10..14).contains(&local.y) {
                        chunk.set_block_local(local, BlockValue::Uniform(LEAF));
                    }
                }
            }
            world
        };

        let measure = |name: &str, mut light: Lighting| {
            let world = scene();
            let centre = ChunkPos::new(0, 0, 0);
            for _ in 0..20 {
                light.forget(centre);
                light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, centre);
            }
            let mut total = std::time::Duration::ZERO;
            const N: u32 = 200;
            for _ in 0..N {
                light.forget(centre);
                let t = std::time::Instant::now();
                light.chunk_loaded(tiamot_core::domain::OVERWORLD, &world, centre);
                total += t.elapsed();
            }
            println!("chunk_loaded ({name}): mean {:?}", total / N);
        };

        let see_through = tiamot_core::light::SeeThrough::new([LEAF]);
        measure(
            "canopy, dimming nothing",
            dimming_lighting(see_through.clone(), tiamot_core::light::Dimming::default()),
        );
        measure(
            "canopy, dimming 2 levels a block",
            dimming_lighting(see_through, tiamot_core::light::Dimming::new([(LEAF, 2)])),
        );
    }
}
