// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Capturing a box of the world into a plan, and stamping one back.
//!
//! The server half of [`tiamat_core::plan`]: what a mod's `game.plans` calls
//! actually reach.
//!
//! # Reading goes through the lease; writing goes through the queue
//!
//! A capture reads terrain, so it happens inside the window the tick lends the
//! world to the mods — see [`crate::lease`] — and answers
//! [`PlanError::Unavailable`] outside it, exactly as sight and pathfinding do.
//!
//! A stamp does not write anything at all when it is asked for. It joins a
//! queue this module owns, and the tick pumps a bounded number of blocks off it
//! per pass onto the same edit queue an operator's edits use. That is what
//! keeps one mod call from putting a quarter of a million blocks into a single
//! tick, and it is why `game.plans.stamp` returns "accepted" rather than
//! "done".
//!
//! **A schematic stamped at run time rides the same queue** (weather ask
//! W35): `game.stamp_schematic` hands its blocks, already turned, to
//! [`Stamps`] beside the plans, under the same budget and the same limit on
//! how many may wait. Only what each block becomes differs — a plan replaces
//! the blocks it names (Sub-Node Contract §7.3), and a schematic merges by
//! cell, as `buf:scatter` writes it (§7.4).
//!
//! # Saving is immediate, and does not need the debounce storage has
//!
//! A mod's `game.storage` is written back on the same debounced save the chunks
//! use, because mods POLL: a state machine writing "still following" every tick
//! would otherwise rewrite its whole bag twenty times a second. A plan is not
//! written that way. It is saved when somebody says "keep this build", which is
//! one blob and one statement, rarely — so it goes straight to the world's
//! database through the same lease the capture used, and the whole store never
//! has to be held in memory.

use std::collections::{BTreeMap, BTreeSet};

use tiamat_core::detgen::StampBlock;
use tiamat_core::plan::{Plan, PlanError};
use tiamat_core::proto::Edit;
use tiamat_core::{BlockPos, BlockView};
use tracing::debug;

use crate::world::World;

/// How many blocks of a pending stamp are queued per tick.
///
/// **A pacing constant, not a limit on plan size.** Every block a stamp writes
/// costs what any other edit costs — it is applied, it is relit, and it is
/// broadcast to everyone watching — so a plan applied in one tick would be a
/// mod call that spends the whole 50 ms budget (charter rule 18) and floods
/// every client in the same frame.
///
/// At 20 Hz this is 5,120 blocks a second: a house of a few thousand blocks
/// appears in under a second, and the largest plan the format allows
/// ([`tiamat_core::plan::MAX_CELLS`], 65,536) takes about thirteen. Watching a
/// big structure build itself over a few seconds is also the more useful
/// behaviour of the two — a mod can play a sound over it, and a player can see
/// where it is going.
pub const BLOCKS_PER_TICK: usize = 256;

/// How many stamps may be waiting at once.
///
/// Small deliberately. A mod that has asked for eight buildings and not seen
/// one of them finish is not helped by being allowed to ask for a ninth, and
/// the refusal is what tells it to wait.
pub const MAX_PENDING: usize = 8;

/// The world's material names, both ways round.
///
/// Built once at startup from the table `game.set_block` resolves through, so
/// the two agree by construction (charter rule 8): a plan holds NAMES, and a
/// house captured under one mod set and stamped under another must not come
/// back made of whatever those numbers mean today.
///
/// The numbers are this session's RUNTIME ids, which is what a plan reads out
/// of a chunk in memory and what a stamp's edits are written as.
pub struct Names {
    by_name: BTreeMap<String, u16>,
    by_id: BTreeMap<u16, String>,
}

impl Names {
    /// Inverts the name table `game.set_block` already has.
    #[must_use]
    pub fn new(by_name: BTreeMap<String, u16>) -> Self {
        let by_id = by_name
            .iter()
            .map(|(name, id)| (*id, name.clone()))
            .collect();
        Self { by_name, by_id }
    }
}

/// One stamp in progress.
struct Stamping {
    domain: String,
    at: BlockPos,
    source: Source,
    /// How many of the source's cells have been queued.
    ///
    /// Advanced only when a whole cell got onto the queue. A cell that ran out
    /// of room half way is re-emitted from its start next tick, which is safe
    /// because every edit it makes is an idempotent write — and re-emitting
    /// from the START is what keeps a mixed block's layers in their order.
    next: usize,
}

/// What a stamp writes.
enum Source {
    /// A plan: each block it names is replaced (Sub-Node Contract §7.3).
    Plan {
        plan: Plan,
        /// Positions whose block has already been replaced this stamp.
        ///
        /// A mixed block is several cells at one position (see
        /// [`tiamat_core::plan`]): the first REPLACES the block and the rest
        /// add their cells to it. Without this the second material would wipe
        /// the first, and the block would come out holding whichever material
        /// was captured last.
        opened: BTreeSet<[u16; 3]>,
    },
    /// A schematic's blocks, already turned, in the order they are written:
    /// merged by cell, as `buf:scatter` writes them (§7.4) — see
    /// [`schematic_writes`].
    Schematic(Vec<StampBlock>),
}

impl Source {
    /// How many cells it holds: what `next` counts towards.
    fn len(&self) -> usize {
        match self {
            Self::Plan { plan, .. } => plan.len(),
            Self::Schematic(blocks) => blocks.len(),
        }
    }
}

/// One thing a stamp puts on the edit queue.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Write {
    /// An edit, applied as it stands.
    Edit(Edit),
    /// Cells added to whatever the block holds (Sub-Node Contract §7.4),
    /// resolved where the queue is drained, because what it becomes depends
    /// on what the block holds then.
    Merge {
        pos: BlockPos,
        material: u16,
        occupancy: u32,
    },
}

impl Write {
    /// What it is charged against [`BLOCKS_PER_TICK`]: the most edits it can
    /// become.
    ///
    /// **A merge is charged a cell each**, though into air or its own
    /// material it is one edit: into a block of another material it is one
    /// `Edit::SubNode` per cell (§7.4), and the budget is a bound on what a
    /// tick may apply, so it is charged the case that costs most. A wreck of a
    /// few hundred blocks still lands in a handful of ticks.
    fn cost(&self) -> usize {
        match self {
            Self::Edit(_) => 1,
            Self::Merge { occupancy, .. } => occupancy.count_ones() as usize,
        }
    }
}

/// Puts one block's writes on the queue, charging each to `budget` and
/// counting it in `queued`.
///
/// `false` when the block must wait for the next tick: its writes would
/// overrun what is left of this tick's budget, or the edit queue filled part
/// way through them. The caller leaves its cursor on the block, so it is
/// re-emitted from its start — safe, because every write is idempotent, and
/// from the START so a mixed block's layers keep their order.
///
/// **"This tick's", not "this call's".** The pump steps the same stamp again
/// while budget is left, so a test of what THIS call had queued let a block
/// that did not fit go on the second call — a tick over its budget by up to a
/// block's worth, every tick a merge-heavy stamp ran. A block costs at most
/// 27, far under a whole tick's budget, so one that waits always fits the
/// next; the exception, a budget nothing has been spent from, is kept so a
/// stamp can never wait for ever.
fn queue_block(
    domain: &str,
    writes: &[Write],
    queue: &dyn EditQueue,
    budget: &mut usize,
    queued: &mut usize,
) -> bool {
    let cost: usize = writes.iter().map(Write::cost).sum();
    if cost > *budget && *budget < BLOCKS_PER_TICK {
        return false;
    }
    for write in writes {
        let accepted = match write {
            Write::Edit(edit) => queue.queue_seed(domain, edit.clone()),
            Write::Merge {
                pos,
                material,
                occupancy,
            } => queue.queue_merge(domain, *pos, *material, *occupancy),
        };
        if !accepted {
            return false;
        }
        *queued += write.cost();
        *budget = budget.saturating_sub(write.cost());
    }
    true
}

/// The stamps waiting to be applied.
///
/// Behind its own lock rather than inside the world: a mod asks for a stamp
/// from a script call and the tick pumps it, and those are two different
/// moments on the same thread.
///
/// It holds the name table as well as the queue, so the tick's pump needs one
/// handle rather than two — and so there is one answer to "what does this plan
/// build with", shared with the capture that made it.
pub struct Stamps {
    pending: std::sync::Mutex<std::collections::VecDeque<Stamping>>,
    names: std::sync::Arc<Names>,
}

impl Stamps {
    /// An empty queue over a world's materials.
    #[must_use]
    pub fn new(names: std::sync::Arc<Names>) -> Self {
        Self {
            pending: std::sync::Mutex::new(std::collections::VecDeque::new()),
            names,
        }
    }

    /// Puts a plan in the queue. `false` when too many are already waiting.
    fn accept(&self, domain: &str, at: BlockPos, plan: Plan) -> bool {
        self.enqueue(
            domain,
            at,
            Source::Plan {
                plan,
                opened: BTreeSet::new(),
            },
        )
    }

    /// Puts a schematic's blocks, already turned, in the queue — the same
    /// queue and the same refusal as a plan. See
    /// [`tiamat_core::plan::Access::stamp_schematic`].
    fn accept_schematic(&self, domain: &str, at: BlockPos, blocks: Vec<StampBlock>) -> bool {
        self.enqueue(domain, at, Source::Schematic(blocks))
    }

    fn enqueue(&self, domain: &str, at: BlockPos, source: Source) -> bool {
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        if pending.len() >= MAX_PENDING {
            return false;
        }
        pending.push_back(Stamping {
            domain: domain.to_owned(),
            at,
            source,
            next: 0,
        });
        true
    }

    /// How many stamps are still waiting.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.pending.lock().map_or(0, |pending| pending.len())
    }

    /// Queues up to [`BLOCKS_PER_TICK`] blocks of the stamps in progress.
    ///
    /// Called once per tick, before the tick drains the edit queue, so what is
    /// pumped here lands in the same pass rather than waiting for the next one.
    ///
    /// Returns how many blocks were queued, which is what the caller logs.
    pub fn pump(&self, queue: &dyn EditQueue) -> usize {
        let Ok(mut pending) = self.pending.lock() else {
            return 0;
        };
        let mut budget = BLOCKS_PER_TICK;
        let mut queued = 0;
        while budget > 0 {
            let Some(stamping) = pending.front_mut() else {
                break;
            };
            let placed = stamping.step(queue, &self.names, &mut budget);
            queued += placed;
            if stamping.next >= stamping.source.len() {
                debug!(
                    domain = %stamping.domain,
                    blocks = stamping.source.len(),
                    "a stamp is fully queued"
                );
                pending.pop_front();
            } else if placed == 0 {
                // The queue is full, or the next block is bigger than what is
                // left of the budget. Nothing else in the pending list would
                // fare any better until next tick, so stop rather than
                // spinning over stamps that all wait the same way.
                break;
            }
        }
        queued
    }
}

impl Stamping {
    /// Queues as many of this stamp's remaining cells as `budget` allows.
    fn step(&mut self, queue: &dyn EditQueue, names: &Names, budget: &mut usize) -> usize {
        match self.source {
            Source::Plan { .. } => self.step_plan(queue, &names.by_name, budget),
            Source::Schematic(_) => self.step_schematic(queue, &names.by_id, budget),
        }
    }

    /// [`Self::step`] for a plan: each cell replaces its block, or layers
    /// onto one this stamp already replaced.
    fn step_plan(
        &mut self,
        queue: &dyn EditQueue,
        by_name: &BTreeMap<String, u16>,
        budget: &mut usize,
    ) -> usize {
        let Source::Plan { plan, opened } = &mut self.source else {
            return 0;
        };
        let mut queued = 0;
        for cell in plan.cells().skip(self.next) {
            if *budget == 0 {
                break;
            }
            let Some(name) = plan.palette().get(cell.material as usize) else {
                self.next += 1;
                continue;
            };
            let Some(&material) = by_name.get(name.as_str()) else {
                // A material this world has never registered. The plan is not
                // wrong — it was captured from a world with a different mod set
                // — and there is nothing to place, so the cell is skipped and
                // said so once rather than failing the whole building.
                debug!(material = %name, "a stamped plan names a material nothing registered");
                self.next += 1;
                continue;
            };

            let pos = BlockPos::new(
                self.at.x + i32::from(cell.at[0]),
                self.at.y + i32::from(cell.at[1]),
                self.at.z + i32::from(cell.at[2]),
            );
            // **Part of a whole material is skipped, not waited on.** The
            // edit queue refuses a partial cell of a `whole` material
            // (Sub-Node Contract §7.5), and a refusal reads the same as a
            // full queue here — so a plan holding one waited for ever, and
            // every stamp behind it with it. The schematic path has had this
            // rule since it landed; found landing it (milestone 0.5.0, T4).
            if cell.occupancy != tiamat_core::block::OCCUPANCY_FULL && queue.is_whole(material) {
                tracing::warn!(
                    material = %name,
                    x = pos.x,
                    y = pos.y,
                    z = pos.z,
                    "a stamped plan holds part of a whole material; skipped, a whole material is \
                     written only as its own shape with a full mask (Sub-Node Contract §7.5)"
                );
                self.next += 1;
                continue;
            }
            let writes: Vec<Write> =
                block_edits(pos, material, cell.occupancy, opened.contains(&cell.at))
                    .into_iter()
                    .map(Write::Edit)
                    .collect();
            // Leave a mixed block's layers for a tick with room for all of
            // them, rather than splitting one across two; and on a full edit
            // queue `next` stays where it is, so this cell is re-emitted from
            // its start next tick.
            if !queue_block(&self.domain, &writes, queue, budget, &mut queued) {
                break;
            }
            opened.insert(cell.at);
            self.next += 1;
        }
        queued
    }

    /// [`Self::step`] for a schematic: each block merged by cell, in the
    /// order given — see [`schematic_writes`].
    fn step_schematic(
        &mut self,
        queue: &dyn EditQueue,
        by_id: &BTreeMap<u16, String>,
        budget: &mut usize,
    ) -> usize {
        let Source::Schematic(blocks) = &self.source else {
            return 0;
        };
        let mut queued = 0;
        for block in blocks.iter().skip(self.next) {
            if *budget == 0 {
                break;
            }
            let writes = schematic_writes(self.at, block, by_id, queue);
            if !writes.is_empty() && !queue_block(&self.domain, &writes, queue, budget, &mut queued)
            {
                break;
            }
            self.next += 1;
        }
        queued
    }
}

/// The writes that put one block of a schematic into the world, exactly as
/// `buf:scatter` writes it into a chunk being generated.
///
/// **A merge write by cell** (Sub-Node Contract §7.4), which is what the
/// scatter does and what a schematic means — a trunk's base sits IN the
/// surface block rather than replacing it with trunk and air:
///
/// - A whole mask replaces the block: one [`Edit::Block`], the write a
///   generator's `set_block` is. Air clears the block, and a `whole` material
///   is written as its own shape, as every block-resolution write of one is
///   (§7.5).
/// - A partial mask sets the cells it names to the material and keeps every
///   other cell the block held: a [`Write::Merge`], resolved against the block
///   where the queue is drained, through the implementation a player's
///   placement uses.
/// - A partial mask of AIR carves those cells: one [`Edit::SubNode`] each,
///   which is the scatter's own per-cell write. Not a merge, because "add air
///   to this block" has no union for the merge to take.
///
/// And two writes the scatter would make that the running world refuses, each
/// skipped and said so rather than stalling the stamp behind a write that will
/// never be accepted: part of a `whole` material (§7.5 — "written as its
/// shape, or not at all"), and a material this world never registered. A
/// block whose offset would leave the world's coordinates is skipped too; the
/// script API refuses those before they get here.
fn schematic_writes(
    at: BlockPos,
    block: &StampBlock,
    by_id: &BTreeMap<u16, String>,
    queue: &dyn EditQueue,
) -> Vec<Write> {
    let mask = block.mask & tiamat_core::block::OCCUPANCY_FULL;
    let material = block.material.0;
    if mask == 0 {
        return Vec::new();
    }
    let air = block.material == tiamat_core::MaterialId::AIR;
    if !air && !by_id.contains_key(&material) {
        debug!(
            material,
            "a stamped schematic names a material nothing registered"
        );
        return Vec::new();
    }
    let (Some(x), Some(y), Some(z)) = (
        at.x.checked_add(block.dx),
        at.y.checked_add(block.dy),
        at.z.checked_add(block.dz),
    ) else {
        return Vec::new();
    };
    let pos = BlockPos::new(x, y, z);
    if mask == tiamat_core::block::OCCUPANCY_FULL {
        return vec![Write::Edit(Edit::Block { pos, material })];
    }
    if air {
        return (0..tiamat_core::block::SUBNODES_PER_BLOCK)
            .filter(|index| mask & (1 << index) != 0)
            .map(|index| {
                let (dx, dy, dz) = tiamat_core::block::subnode_offset(index);
                Write::Edit(Edit::SubNode {
                    pos: pos.subnode(dx as i32, dy as i32, dz as i32),
                    material,
                })
            })
            .collect();
    }
    if queue.is_whole(material) {
        tracing::warn!(
            material,
            x,
            y,
            z,
            "a stamped schematic names part of a whole material; a whole material is written as \
             its own shape, with a full mask (Sub-Node Contract §7.5)"
        );
        return Vec::new();
    }
    vec![Write::Merge {
        pos,
        material,
        occupancy: mask,
    }]
}

/// The edits that put one plan cell into the world.
///
/// Sub-Node Contract §7.3: a block the plan names is replaced by what the plan
/// says it is, and a block it does not name is not touched at all.
///
/// A whole block is one [`Edit::Block`], a chiselled one is one
/// [`Edit::Partial`], and a LAYER onto a block this stamp has already replaced
/// is one [`Edit::SubNode`] per filled cell — which is the only shape that can
/// add a second material to a block without erasing the first.
fn block_edits(pos: BlockPos, material: u16, occupancy: u32, opened: bool) -> Vec<Edit> {
    if !opened {
        return vec![if occupancy == tiamat_core::block::OCCUPANCY_FULL {
            Edit::Block { pos, material }
        } else {
            Edit::Partial {
                pos,
                material,
                occupancy,
            }
        }];
    }
    (0..tiamat_core::block::SUBNODES_PER_BLOCK)
        .filter(|index| occupancy & (1 << index) != 0)
        .map(|index| {
            let (dx, dy, dz) = tiamat_core::block::subnode_offset(index);
            Edit::SubNode {
                pos: pos.subnode(dx as i32, dy as i32, dz as i32),
                material,
            }
        })
        .collect()
}

/// Where a stamp's edits go.
///
/// A trait so the pacing can be tested without a transport: what the pump has
/// to get right is how many edits it emits and in what order, and neither of
/// those is a network question.
pub trait EditQueue: Send + Sync {
    /// Queues one edit. `false` when the queue is full.
    fn queue_seed(&self, domain: &str, edit: Edit) -> bool;

    /// Queues cells to be added to whatever a block holds (Sub-Node Contract
    /// §7.4). `false` when the queue is full.
    fn queue_merge(&self, domain: &str, pos: BlockPos, material: u16, occupancy: u32) -> bool;

    /// Whether a material is `whole` (Sub-Node Contract §7.5): written as its
    /// own shape or not at all, so never merged and never written a cell at a
    /// time.
    ///
    /// Asked before a write rather than learned from a refusal, because a
    /// refusal and a full queue are both `false`, and a stamp that read a
    /// write that can never land as "wait for room" would wait for ever and
    /// hold up every stamp behind it.
    fn is_whole(&self, material: u16) -> bool;
}

impl EditQueue for crate::transport::Shared {
    fn queue_seed(&self, domain: &str, edit: Edit) -> bool {
        Self::queue_seed(self, domain, edit)
    }

    fn queue_merge(&self, domain: &str, pos: BlockPos, material: u16, occupancy: u32) -> bool {
        Self::queue_merge(self, domain, pos, material, occupancy)
    }

    fn is_whole(&self, material: u16) -> bool {
        self.whole_shape(tiamat_core::MaterialId(material))
            .is_some()
    }
}

/// Reads a box of the world into a plan.
///
/// Chunk-local: the box is walked in the order a chunk stores its blocks, and
/// the chunk it is in is looked up once per row rather than once per block.
///
/// # Errors
///
/// [`PlanError::Side`] for a box bigger than [`tiamat_core::plan::MAX_SIDE`],
/// [`PlanError::NotLoaded`] if any of it is in a chunk that is not resident,
/// and [`PlanError::TooManyCells`] for a box holding more blocks than a plan
/// may.
pub fn capture(
    world: &World,
    by_id: &BTreeMap<u16, String>,
    domain: &str,
    from: BlockPos,
    to: BlockPos,
) -> Result<Plan, PlanError> {
    let low = BlockPos::new(from.x.min(to.x), from.y.min(to.y), from.z.min(to.z));
    let high = BlockPos::new(from.x.max(to.x), from.y.max(to.y), from.z.max(to.z));
    let side = |axis: &'static str, low: i32, high: i32| -> Result<u16, PlanError> {
        let side = i64::from(high) - i64::from(low) + 1;
        u16::try_from(side)
            .ok()
            .filter(|side| *side <= tiamat_core::plan::MAX_SIDE)
            .ok_or(PlanError::Side {
                axis,
                side: u32::try_from(side).unwrap_or(u32::MAX),
            })
    };
    let size = [
        side("x", low.x, high.x)?,
        side("y", low.y, high.y)?,
        side("z", low.z, high.z)?,
    ];
    let mut plan = Plan::new(size)?;

    let terrain = world.solid(domain);
    let mut held: Option<(tiamat_core::coords::ChunkPos, &tiamat_core::chunk::Chunk)> = None;
    for z in low.z..=high.z {
        for y in low.y..=high.y {
            for x in low.x..=high.x {
                let pos = BlockPos::new(x, y, z);
                let chunk =
                    match held {
                        Some((at, chunk)) if at == pos.chunk() => chunk,
                        _ => {
                            // **Resident only.** A capture must not be able to make
                            // the server generate chunks inside the tick budget,
                            // one call at a time — the rule `sight::Access` states
                            // and for the same reason.
                            let chunk = terrain
                                .resident(pos.chunk())
                                .ok_or(PlanError::NotLoaded { x, y, z })?;
                            held = Some((pos.chunk(), chunk));
                            chunk
                        }
                    };
                let Some(view) = chunk.get_block(pos) else {
                    return Err(PlanError::NotLoaded { x, y, z });
                };
                let at = [(x - low.x) as u16, (y - low.y) as u16, (z - low.z) as u16];
                record(&mut plan, by_id, at, &view)?;
            }
        }
    }
    Ok(plan)
}

/// Writes one block of the world into a plan.
///
/// A mixed block becomes one entry per material, each with its own mask — the
/// layering [`tiamat_core::plan`] documents. Air is not recorded at all.
fn record(
    plan: &mut Plan,
    by_id: &BTreeMap<u16, String>,
    at: [u16; 3],
    view: &BlockView<'_>,
) -> Result<(), PlanError> {
    // A material the world has no name for is `engine:unknown`, which charter
    // rule 8 says a preserved id reads as. Recording it keeps the plan the same
    // shape as the thing it was captured from; stamping it back finds no
    // registered material and leaves that block alone.
    let name = |material: tiamat_core::MaterialId| -> &str {
        by_id
            .get(&material.0)
            .map_or(tiamat_core::material::UNKNOWN_NAME, String::as_str)
    };
    match view {
        BlockView::Uniform(material) => {
            if !material.is_air() {
                plan.set(at, name(*material), tiamat_core::block::OCCUPANCY_FULL)?;
            }
        }
        BlockView::Partial {
            material,
            occupancy,
        } => {
            if !material.is_air() {
                plan.set(
                    at,
                    name(*material),
                    occupancy & tiamat_core::block::OCCUPANCY_FULL,
                )?;
            }
        }
        BlockView::Mixed(cells) => {
            // One pass, in cell order, so the entries come out in a fixed order
            // whatever the block holds — a capture is not a simulation result,
            // but a plan that differed run to run would make every test of one
            // a flake.
            let mut done = 0u32;
            for (index, material) in cells.iter().enumerate() {
                let bit = 1u32 << index;
                if material.is_air() || done & bit != 0 {
                    continue;
                }
                let mut mask = 0u32;
                for (other, also) in cells.iter().enumerate() {
                    if also == material {
                        mask |= 1 << other;
                    }
                }
                done |= mask;
                plan.set(at, name(*material), mask)?;
            }
        }
    }
    Ok(())
}

/// The handle the VM reaches plans through.
///
/// Holds the lease (for the world and its database), the world's material name
/// table both ways round, and the stamp queue.
pub struct Shared {
    lease: crate::lease::Shared,
    names: std::sync::Arc<Names>,
    stamps: std::sync::Arc<Stamps>,
}

impl Shared {
    /// Wraps the lease, the world's materials, and the stamp queue.
    #[must_use]
    pub const fn new(
        lease: crate::lease::Shared,
        names: std::sync::Arc<Names>,
        stamps: std::sync::Arc<Stamps>,
    ) -> Self {
        Self {
            lease,
            names,
            stamps,
        }
    }
}

impl tiamat_core::plan::Access for Shared {
    fn capture(&self, domain: &str, from: BlockPos, to: BlockPos) -> Result<Plan, PlanError> {
        self.lease
            .with_world(|world| capture(world, &self.names.by_id, domain, from, to))
            .unwrap_or(Err(PlanError::Unavailable))
    }

    fn save(&self, mod_id: &str, name: &str, plan: &Plan) -> bool {
        self.lease
            .with_world(|world| match world.save_plan(mod_id, name, plan) {
                Ok(()) => true,
                Err(err) => {
                    // Logged rather than fatal, like a mod's storage: losing a
                    // saved building is bad and taking the server down over it
                    // is worse.
                    tracing::error!("could not save plan `{name}` for mod `{mod_id}`: {err}");
                    false
                }
            })
            .unwrap_or(false)
    }

    fn load(&self, mod_id: &str, name: &str) -> Option<Plan> {
        self.lease
            .with_world(|world| world.load_plan(mod_id, name).ok().flatten())
            .flatten()
    }

    fn names(&self, mod_id: &str) -> Vec<String> {
        self.lease
            .with_world(|world| world.plan_names(mod_id).unwrap_or_default())
            .unwrap_or_default()
    }

    fn forget(&self, mod_id: &str, name: &str) -> bool {
        self.lease
            .with_world(|world| world.delete_plan(mod_id, name).unwrap_or(false))
            .unwrap_or(false)
    }

    fn stamp(&self, domain: &str, at: BlockPos, plan: Plan) -> bool {
        self.stamps.accept(domain, at, plan)
    }

    fn stamp_schematic(&self, domain: &str, at: BlockPos, blocks: Vec<StampBlock>) -> bool {
        self.stamps.accept_schematic(domain, at, blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamat_core::MaterialId;
    use tiamat_core::chunk::Chunk;
    use tiamat_core::coords::ChunkPos;

    const MATERIALS: [&str; 3] = ["test:stone", "test:wood", "test:glass"];

    /// A generator that fills everything below y = 0 and leaves the rest air.
    struct Flat(MaterialId);

    impl crate::world::ChunkSource for Flat {
        fn generate(&mut self, _domain: &str, pos: ChunkPos, _world_seed: u64) -> Chunk {
            if pos.y < 0 {
                Chunk::new(pos, self.0)
            } else {
                Chunk::new(pos, MaterialId::AIR)
            }
        }
    }

    /// A world with the three test materials, and the name table for them.
    fn world(name: &str) -> (World, Names, Vec<MaterialId>) {
        let dir = std::env::temp_dir().join("tiamat-plan-tests");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(format!("{name}.sqlite"));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let mut registry = tiamat_core::Registry::new();
        let ids: Vec<MaterialId> = MATERIALS
            .iter()
            .map(|name| registry.register(name).expect("register"))
            .collect();
        let db = tiamat_core::WorldDb::open(&path, &mut registry).expect("open");
        // The runtime ids above: what a chunk in memory holds and what an edit
        // is applied in, which is how the server builds the real table.
        let by_name = MATERIALS
            .iter()
            .zip(&ids)
            .map(|(name, id)| ((*name).to_owned(), id.get()))
            .collect();
        (
            World::open(db, 99).expect("open world"),
            Names::new(by_name),
            ids,
        )
    }

    /// An edit queue a test can read back, refusing after `room` writes.
    struct Slate {
        taken: std::sync::Mutex<Vec<(String, Write)>>,
        room: std::sync::Mutex<usize>,
        /// The materials this queue's world calls `whole`.
        whole: Vec<u16>,
    }

    impl Slate {
        fn with_room(room: usize) -> Self {
            Self {
                taken: std::sync::Mutex::new(Vec::new()),
                room: std::sync::Mutex::new(room),
                whole: Vec::new(),
            }
        }

        fn edits(&self) -> Vec<Edit> {
            self.taken
                .lock()
                .expect("lock")
                .iter()
                .filter_map(|(_, write)| match write {
                    Write::Edit(edit) => Some(edit.clone()),
                    Write::Merge { .. } => None,
                })
                .collect()
        }

        fn writes(&self) -> Vec<Write> {
            self.taken
                .lock()
                .expect("lock")
                .iter()
                .map(|(_, write)| write.clone())
                .collect()
        }

        fn take(&self, domain: &str, write: Write) -> bool {
            let mut room = self.room.lock().expect("lock");
            if *room == 0 {
                return false;
            }
            *room -= 1;
            self.taken
                .lock()
                .expect("lock")
                .push((domain.to_owned(), write));
            true
        }
    }

    impl EditQueue for Slate {
        fn queue_seed(&self, domain: &str, edit: Edit) -> bool {
            self.take(domain, Write::Edit(edit))
        }

        fn queue_merge(&self, domain: &str, pos: BlockPos, material: u16, occupancy: u32) -> bool {
            self.take(
                domain,
                Write::Merge {
                    pos,
                    material,
                    occupancy,
                },
            )
        }

        fn is_whole(&self, material: u16) -> bool {
            self.whole.contains(&material)
        }
    }

    fn cottage(cells: usize) -> Plan {
        let mut plan = Plan::new([16, 16, 16]).expect("a valid size");
        for index in 0..cells {
            let at = [
                (index % 16) as u16,
                ((index / 16) % 16) as u16,
                (index / 256) as u16,
            ];
            plan.set(at, "test:stone", tiamat_core::block::OCCUPANCY_FULL)
                .expect("in bounds");
        }
        plan
    }

    #[test]
    fn a_capture_reads_the_terrain_and_leaves_the_air_out() {
        let (mut world, names, ids) = world("capture");
        let mut flat = Flat(ids[0]);
        let overworld = tiamat_core::domain::OVERWORLD;
        world
            .chunk(overworld, ChunkPos::new(0, -1, 0), &mut flat)
            .expect("load");
        world
            .chunk(overworld, ChunkPos::new(0, 0, 0), &mut flat)
            .expect("load");

        // A box straddling the surface: four solid layers and four of air.
        let plan = capture(
            &world,
            &names.by_id,
            overworld,
            BlockPos::new(0, -4, 0),
            BlockPos::new(1, 3, 1),
        )
        .expect("capture");

        assert_eq!(plan.size(), [2, 8, 2]);
        assert_eq!(
            plan.len(),
            16,
            "the air above the surface was recorded as blocks"
        );
        // Units, not blocks: sixteen whole ones (charter rule 5).
        assert_eq!(plan.tally().get("test:stone"), Some(&(16 * 27)));
    }

    #[test]
    fn a_capture_reaching_terrain_that_is_not_loaded_is_refused_whole() {
        // A plan is sparse, so a block that was not captured and a block that
        // held nothing are one state: a partial capture would come back as a
        // house with holes in it and nothing could tell.
        let (world, names, _) = world("capture-absent");
        let refusal = capture(
            &world,
            &names.by_id,
            tiamat_core::domain::OVERWORLD,
            BlockPos::new(0, -4, 0),
            BlockPos::new(1, 1, 1),
        )
        .expect_err("nothing is loaded");
        assert!(
            matches!(refusal, PlanError::NotLoaded { .. }),
            "{refusal:?}"
        );
    }

    #[test]
    fn a_box_longer_than_a_plan_may_be_is_refused_before_it_is_walked() {
        let (world, names, _) = world("capture-huge");
        let refusal = capture(
            &world,
            &names.by_id,
            tiamat_core::domain::OVERWORLD,
            BlockPos::new(0, 0, 0),
            BlockPos::new(0, 0, 500),
        )
        .expect_err("too long");
        assert_eq!(
            refusal,
            PlanError::Side {
                axis: "z",
                side: 501
            }
        );
    }

    #[test]
    fn a_mixed_block_is_captured_as_layers_and_stamped_back_as_layers() {
        // The case the whole layering convention exists for. A block holding
        // two materials must come back holding two: capturing it as one would
        // be a silent loss in exactly the sub-node detail charter rule 19 says
        // is the point of the engine.
        let (mut world, names, ids) = world("capture-mixed");
        let mut flat = Flat(ids[0]);
        let overworld = tiamat_core::domain::OVERWORLD;
        let block = BlockPos::new(0, 4, 0);
        world
            .chunk(overworld, block.chunk(), &mut flat)
            .expect("load");

        // Two cells of wood and one of glass, in an otherwise empty block.
        let wood = names.by_name["test:wood"];
        let glass = names.by_name["test:glass"];
        for (cell, material) in [(0, wood), (1, wood), (2, glass)] {
            let (dx, dy, dz) = tiamat_core::block::subnode_offset(cell);
            world
                .apply(
                    overworld,
                    &Edit::SubNode {
                        pos: block.subnode(dx as i32, dy as i32, dz as i32),
                        material,
                    },
                    &mut flat,
                )
                .expect("apply");
        }

        let plan = capture(&world, &names.by_id, overworld, block, block).expect("capture");
        assert_eq!(plan.len(), 2, "the mixed block lost a material");
        assert_eq!(plan.tally().get("test:wood"), Some(&2), "two cells of wood");
        assert_eq!(plan.tally().get("test:glass"), Some(&1));

        // And stamping it emits the layers in order: the first REPLACES the
        // block, and the second adds its cells with sub-node edits — which is
        // the only shape that does not wipe the first.
        let stamps = Stamps::new(std::sync::Arc::new(names));
        let slate = Slate::with_room(64);
        assert!(stamps.accept(overworld, BlockPos::new(9, 9, 9), plan));
        stamps.pump(&slate);

        let edits = slate.edits();
        assert_eq!(
            edits.len(),
            2,
            "expected one replace and one cell: {edits:?}"
        );
        assert!(
            matches!(edits[0], Edit::Partial { material, .. } if material == wood),
            "the first layer should replace the block: {edits:?}"
        );
        assert!(
            matches!(edits[1], Edit::SubNode { material, .. } if material == glass),
            "the second layer should add a cell: {edits:?}"
        );
        assert_eq!(stamps.waiting(), 0, "the stamp did not finish");
    }

    #[test]
    fn a_stamp_is_paced_across_ticks_and_finishes() {
        // The reason `stamp` is a request rather than a write. A plan applied
        // in one tick would spend the whole 50 ms budget and flood every
        // watching client in one frame.
        let (_, names, _) = world("stamp-pacing");
        let stamps = Stamps::new(std::sync::Arc::new(names));
        let slate = Slate::with_room(usize::MAX);
        let blocks = BLOCKS_PER_TICK * 2 + 5;
        assert!(stamps.accept(
            tiamat_core::domain::OVERWORLD,
            BlockPos::new(0, 0, 0),
            cottage(blocks),
        ));

        let mut ticks = 0;
        let mut placed = 0;
        while stamps.waiting() > 0 {
            let this_tick = stamps.pump(&slate);
            assert!(
                this_tick <= BLOCKS_PER_TICK,
                "a tick queued {this_tick} blocks, over the budget"
            );
            placed += this_tick;
            ticks += 1;
            assert!(ticks < 10, "the stamp never finished");
        }
        assert_eq!(placed, blocks, "the stamp lost blocks");
        assert_eq!(ticks, 3, "expected three ticks for {blocks} blocks");
    }

    #[test]
    fn a_full_edit_queue_makes_a_stamp_wait_rather_than_lose_blocks() {
        // The queue is shared with every other edit on the server, so a stamp
        // meeting a full one is ordinary. What must not happen is the stamp
        // walking past the blocks it could not queue: a house with a band of
        // missing wall is worse than a slow one.
        let (_, names, _) = world("stamp-full");
        let stamps = Stamps::new(std::sync::Arc::new(names));
        let slate = Slate::with_room(10);
        assert!(stamps.accept(
            tiamat_core::domain::OVERWORLD,
            BlockPos::new(0, 0, 0),
            cottage(30),
        ));

        assert_eq!(stamps.pump(&slate), 10, "it should fill the room there was");
        assert_eq!(stamps.pump(&slate), 0, "a full queue took more anyway");
        assert_eq!(stamps.waiting(), 1, "the rest of the plan was dropped");

        *slate.room.lock().expect("lock") = 20;
        assert_eq!(stamps.pump(&slate), 20, "the rest did not resume");
        assert_eq!(stamps.waiting(), 0);
        assert_eq!(slate.edits().len(), 30, "the stamp lost blocks");
    }

    #[test]
    fn only_so_many_stamps_may_be_waiting_at_once() {
        // A mod that has asked for eight buildings and seen none of them finish
        // is not helped by being allowed to ask for a ninth.
        let (_, names, _) = world("stamp-pending");
        let stamps = Stamps::new(std::sync::Arc::new(names));
        for _ in 0..MAX_PENDING {
            assert!(stamps.accept(
                tiamat_core::domain::OVERWORLD,
                BlockPos::new(0, 0, 0),
                cottage(1),
            ));
        }
        assert!(
            !stamps.accept(
                tiamat_core::domain::OVERWORLD,
                BlockPos::new(0, 0, 0),
                cottage(1)
            ),
            "a ninth stamp was accepted"
        );
        assert_eq!(stamps.waiting(), MAX_PENDING);
        // A schematic waits in the same line, so it is the same refusal.
        assert!(
            !stamps.accept_schematic(
                tiamat_core::domain::OVERWORLD,
                BlockPos::new(0, 0, 0),
                slab(1),
            ),
            "a schematic jumped a full queue"
        );
    }

    /// A schematic of `blocks` whole blocks of the first test material, in a
    /// row along x.
    fn slab(blocks: usize) -> Vec<StampBlock> {
        (0..blocks)
            .map(|dx| StampBlock {
                dx: i32::try_from(dx).expect("a short row"),
                dy: 0,
                dz: 0,
                material: tiamat_core::MaterialId(2),
                mask: tiamat_core::block::OCCUPANCY_FULL,
            })
            .collect()
    }

    #[test]
    fn a_plan_holding_part_of_a_whole_material_skips_it_rather_than_waiting() {
        // Found landing W35: the edit queue refuses a partial cell of a
        // `whole` material, the plan stamp read the refusal as a full queue,
        // and the stamp — and every stamp behind it — waited for ever. The
        // cell is skipped and the rest of the plan lands.
        let (_, names, ids) = world("plan-whole");
        let (stone, glass) = (ids[0], ids[2]);
        let stamps = Stamps::new(std::sync::Arc::new(names));
        let slate = Slate {
            whole: vec![glass.get()],
            ..Slate::with_room(64)
        };
        let mut plan = Plan::new([4, 4, 4]).expect("a valid size");
        plan.set([0, 0, 0], MATERIALS[2], 0b1).expect("in bounds");
        plan.set([1, 0, 0], MATERIALS[0], tiamat_core::block::OCCUPANCY_FULL)
            .expect("in bounds");
        let at = BlockPos::new(10, 20, 30);
        assert!(stamps.accept(tiamat_core::domain::OVERWORLD, at, plan));
        stamps.pump(&slate);
        assert_eq!(
            slate.edits(),
            vec![Edit::Block {
                pos: BlockPos::new(11, 20, 30),
                material: stone.get(),
            }],
            "the stone lands; the part of a whole material is skipped"
        );
        assert_eq!(
            stamps.waiting(),
            0,
            "the stamp finished rather than waiting for ever"
        );
    }

    #[test]
    fn a_schematic_is_written_as_the_scatter_writes_it() {
        // `buf:scatter` writes a whole mask as the block and a partial one cell
        // by cell into whatever the block held. At run time that is a replace
        // and a merge (Contract §7.4), in the schematic's own order, at the
        // root plus each offset.
        let (_, names, ids) = world("schematic-writes");
        let (stone, wood, glass) = (ids[0], ids[1], ids[2]);
        let stamps = Stamps::new(std::sync::Arc::new(names));
        let slate = Slate {
            whole: vec![glass.get()],
            ..Slate::with_room(64)
        };
        let full = tiamat_core::block::OCCUPANCY_FULL;
        let air = MaterialId::AIR;
        let block = |dx, dy, dz, material: MaterialId, mask| StampBlock {
            dx,
            dy,
            dz,
            material,
            mask,
        };
        let at = BlockPos::new(10, 20, 30);
        assert!(stamps.accept_schematic(
            tiamat_core::domain::OVERWORLD,
            at,
            vec![
                block(0, 0, 0, stone, full),
                block(1, 0, 0, wood, 0b111),
                block(2, 0, 0, air, full),
                block(3, -1, 0, air, 0b11),
                // Part of a whole material, which nothing may write: skipped
                // rather than waited on.
                block(4, 0, 0, glass, 0b1),
                // A material nothing registered: skipped.
                block(5, 0, 0, MaterialId(999), full),
                // A whole material written whole: its shape is the queue's
                // business.
                block(6, 0, 0, glass, full),
                // A later block over an earlier one lands over it.
                block(0, 0, 0, wood, 1 << 26),
                // No cells at all.
                block(7, 0, 0, wood, 1 << 27),
            ],
        ));
        stamps.pump(&slate);

        let cell = |pos: BlockPos, x, y, z| Edit::SubNode {
            pos: pos.subnode(x, y, z),
            material: air.get(),
        };
        assert_eq!(
            slate.writes(),
            vec![
                Write::Edit(Edit::Block {
                    pos: BlockPos::new(10, 20, 30),
                    material: stone.get(),
                }),
                Write::Merge {
                    pos: BlockPos::new(11, 20, 30),
                    material: wood.get(),
                    occupancy: 0b111,
                },
                Write::Edit(Edit::Block {
                    pos: BlockPos::new(12, 20, 30),
                    material: air.get(),
                }),
                Write::Edit(cell(BlockPos::new(13, 19, 30), 0, 0, 0)),
                Write::Edit(cell(BlockPos::new(13, 19, 30), 1, 0, 0)),
                Write::Edit(Edit::Block {
                    pos: BlockPos::new(16, 20, 30),
                    material: glass.get(),
                }),
                Write::Merge {
                    pos: BlockPos::new(10, 20, 30),
                    material: wood.get(),
                    occupancy: 1 << 26,
                },
            ]
        );
        assert_eq!(stamps.waiting(), 0, "the stamp did not finish");
    }

    #[test]
    fn a_schematic_is_paced_as_a_plan_is_and_a_merge_is_charged_its_cells() {
        // The point of handing a schematic to this queue rather than writing it
        // where the mod asked: a structure of a few hundred blocks lands over a
        // few ticks, each under the budget.
        let (_, names, _) = world("schematic-pacing");
        let stamps = Stamps::new(std::sync::Arc::new(names));
        let slate = Slate::with_room(usize::MAX);
        let blocks = BLOCKS_PER_TICK + 44;
        assert!(stamps.accept_schematic(
            tiamat_core::domain::OVERWORLD,
            BlockPos::new(0, 0, 0),
            slab(blocks),
        ));
        assert_eq!(stamps.pump(&slate), BLOCKS_PER_TICK);
        assert_eq!(stamps.pump(&slate), 44);
        assert_eq!(stamps.waiting(), 0);
        assert_eq!(slate.writes().len(), blocks, "the stamp lost blocks");

        // A merge can become a cell edit per cell, so it is charged its cells:
        // two layers is eighteen, and fourteen of them fill a tick's 256 to
        // 252 — the fifteenth waits rather than overrunning it.
        let two_layers = 0x3_FFFF;
        let merges: Vec<StampBlock> = slab(20)
            .into_iter()
            .map(|block| StampBlock {
                mask: two_layers,
                ..block
            })
            .collect();
        let slate = Slate::with_room(usize::MAX);
        assert!(stamps.accept_schematic(
            tiamat_core::domain::OVERWORLD,
            BlockPos::new(0, 0, 0),
            merges,
        ));
        assert_eq!(stamps.pump(&slate), 14 * 18);
        assert_eq!(slate.writes().len(), 14);
        assert_eq!(stamps.pump(&slate), 6 * 18);
        assert_eq!(slate.writes().len(), 20, "the stamp lost blocks");
        assert_eq!(stamps.waiting(), 0);
    }

    #[test]
    fn a_full_edit_queue_makes_a_schematic_wait_rather_than_lose_blocks() {
        let (_, names, _) = world("schematic-full");
        let stamps = Stamps::new(std::sync::Arc::new(names));
        let slate = Slate::with_room(10);
        assert!(stamps.accept_schematic(
            tiamat_core::domain::OVERWORLD,
            BlockPos::new(0, 0, 0),
            slab(30),
        ));
        assert_eq!(stamps.pump(&slate), 10, "it should fill the room there was");
        assert_eq!(stamps.pump(&slate), 0, "a full queue took more anyway");
        assert_eq!(stamps.waiting(), 1, "the rest of the schematic was dropped");

        *slate.room.lock().expect("lock") = 20;
        assert_eq!(stamps.pump(&slate), 20, "the rest did not resume");
        assert_eq!(stamps.waiting(), 0);
        let rows: Vec<i32> = slate
            .edits()
            .iter()
            .filter_map(|edit| match edit {
                Edit::Block { pos, .. } => Some(pos.x),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            (0..30).collect::<Vec<_>>(),
            "a block was lost or repeated"
        );
    }

    /// What a capture costs, as a share of the tick it happens inside.
    ///
    /// `#[ignore]` because it is a measurement rather than a gate: a threshold
    /// here would be a machine-speed test on every CI runner. Run it with
    /// `cargo test --release -p server -- --ignored --nocapture` after touching
    /// the walk. Measured 2026-09-08 in release, on this container:
    ///
    /// | box | cost | of a 50 ms tick |
    /// |---|---|---|
    /// | 16³ house, half solid | 46 µs | 0.1% |
    /// | 64³ of air (the longest legal walk) | 2.4 ms | 4.8% |
    /// | 64³ of solid stone (refused: too many blocks) | 2.1 ms | 4.2% |
    ///
    /// The 64³ number is what `MAX_SIDE` is really bounding, and it is why the
    /// bound is 64 rather than something rounder: a mod may spend a twentieth
    /// of one tick on an explicit capture, and may not spend half of one.
    #[test]
    #[ignore = "a measurement, not a gate; run it with --release --ignored"]
    fn what_a_capture_costs() {
        let (mut world, names, ids) = world("capture-cost");
        let mut flat = Flat(ids[0]);
        let overworld = tiamat_core::domain::OVERWORLD;
        for x in -1..=4 {
            for y in -5..=5 {
                for z in -1..=4 {
                    world
                        .chunk(overworld, ChunkPos::new(x, y, z), &mut flat)
                        .expect("load");
                }
            }
        }
        for (what, from, to) in [
            ("solid", BlockPos::new(0, -63, 0), BlockPos::new(63, 0, 63)),
            ("air", BlockPos::new(0, 1, 0), BlockPos::new(63, 64, 63)),
            ("a house", BlockPos::new(0, -8, 0), BlockPos::new(15, 7, 15)),
        ] {
            let start = std::time::Instant::now();
            let plan = capture(&world, &names.by_id, overworld, from, to);
            let taken = start.elapsed();
            println!(
                "{what}: {taken:?} ({:.1}% of a 50 ms tick), {:?}",
                taken.as_secs_f64() * 1000.0 / 50.0 * 100.0,
                plan.as_ref().map(tiamat_core::plan::Plan::len)
            );
        }
    }
}
