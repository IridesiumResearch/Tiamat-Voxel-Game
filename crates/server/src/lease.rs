// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Lending the world to the mods, for exactly as long as they are running.
//!
//! Everything a mod may learn about terrain comes through here — sight today,
//! pathfinding with it — because they all need the same thing at the same
//! moment: the world, during the part of the tick that runs mod callbacks.
//!
//! # The problem this solves
//!
//! Every other mod-facing store — lighting, fluid, entities, storage — sits
//! behind its own `Arc<RwLock<…>>` for the whole run, so the VM can hold a
//! handle to it from freeze onwards. The world cannot. The tick thread owns
//! [`World`] by value and holds it *mutably* through chunk generation, through
//! every edit it applies, and through the lighting and fluid passes, so there
//! is no handle that is safe to hold at an arbitrary moment.
//!
//! Putting `World` behind a lock anyway is where the obvious version of this
//! goes wrong. Chunk generation runs mod callbacks *while* the world is
//! borrowed, so a mod calling into the world from `on_generate` would take the
//! same lock the tick is already holding — a self-deadlock on the simulation
//! thread, from Lua, on a mod author's mistake. Defusing that with `try_read`
//! turns the deadlock into a silent wrong answer, which is worse.
//!
//! # What this does instead
//!
//! The tick **moves** the world into a slot for the part of the tick that runs
//! mod callbacks, and moves it back out afterwards. Between those two moments
//! the tick has no world at all — the borrow checker enforces that, because the
//! value is gone — so there is nothing for a mod's read to contend with, and the
//! lock behind the slot is only ever taken by one side at a time.
//!
//! It also makes the window a fact about the code rather than a convention.
//! A mod that reads the world outside it gets [`Sighting::Unavailable`], which
//! is exactly true: at that moment the engine is mid-edit and there is no
//! consistent world to answer from.
//!
//! # Cost
//!
//! Two moves of a `World` per lend — a memcpy of a handful of pointers and
//! lengths, not of any chunk — and one uncontended mutex acquisition per mod
//! call. The chunks themselves never move.

use std::sync::{Arc, Mutex};

use tiamat_core::path;
use tiamat_core::sight::{self, Sighting};

use crate::world::World;

/// The slot the world is lent through.
///
/// Held by the tick, which is the only thing that may put a world in or take
/// one out. Hand [`Self::handle`] to the VM.
pub struct Lease {
    slot: Arc<Mutex<Option<World>>>,
    /// Pathfinding expansions left in this tick, shared by every mod.
    ///
    /// See [`path::TICK_BUDGET`]. A per-call ceiling bounds one mob and does
    /// nothing about two hundred of them, so the engine holds the pool and the
    /// tick refills it — which makes the cost of navigation a property of the
    /// engine rather than of how carefully every installed mod was written.
    allowance: Arc<std::sync::atomic::AtomicU32>,
    /// The passable materials, sorted, in the id space the chunks hold — for
    /// `surface_at` to look through a tuft.
    passable: Arc<Vec<u16>>,
    /// The world's fluid, for `surface_at` to find a pond's surface or look
    /// through it. Read under the lease, which is the order the tick already
    /// takes them in.
    fluid: Option<Arc<std::sync::RwLock<crate::fluid::Ponds>>>,
    /// The players' authoritative bodies, for `looking_at` to find an eye and
    /// the direction it is pointing.
    ///
    /// **These and not the mirrors in the entity store**, for the reason
    /// `ent::Access::move_player` gives: the mirror is a copy the tick
    /// overwrites, and its `pitch` is the drawn figure's rather than the
    /// camera's. The body carries `look` as the wire sent it.
    bodies: Option<Arc<crate::transport::PlayerBodies>>,
}

impl Default for Lease {
    fn default() -> Self {
        Self::new()
    }
}

impl Lease {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slot: Arc::new(Mutex::new(None)),
            allowance: Arc::new(std::sync::atomic::AtomicU32::new(path::TICK_BUDGET)),
            passable: Arc::new(Vec::new()),
            fluid: None,
            bodies: None,
        }
    }

    /// What `surface_at` looks through: the passable materials, and the fluid.
    ///
    /// Set once at startup, before [`Self::handle`] is taken. `passable` is
    /// sorted here; ids are in the space the chunks in memory hold.
    pub fn with_terrain(
        mut self,
        mut passable: Vec<u16>,
        fluid: Arc<std::sync::RwLock<crate::fluid::Ponds>>,
    ) -> Self {
        passable.sort_unstable();
        self.passable = Arc::new(passable);
        self.fluid = Some(fluid);
        self
    }

    /// The bodies `looking_at` casts from.
    ///
    /// Set once at startup, before [`Self::handle`] is taken, like
    /// [`Self::with_terrain`]. A lease without them answers `None` to every
    /// `looking_at`, which is what a test with no players should hear.
    #[must_use]
    pub fn with_players(mut self, bodies: Arc<crate::transport::PlayerBodies>) -> Self {
        self.bodies = Some(bodies);
        self
    }

    /// Refills the tick's pathfinding pool.
    ///
    /// Called once per tick by the simulation loop. Explicit rather than folded
    /// into [`Self::lending`], which happens more than once a tick: a pool that
    /// refilled per lend would be two pools, and neither would be the one the
    /// budget was reasoned about.
    pub fn open_tick(&self) {
        self.allowance
            .store(path::TICK_BUDGET, std::sync::atomic::Ordering::Relaxed);
    }

    /// The mod-facing handle. Answers [`Sighting::Unavailable`] whenever the
    /// slot is empty, which is every moment outside a lend.
    #[must_use]
    pub fn handle(&self) -> Shared {
        Shared {
            slot: Arc::clone(&self.slot),
            allowance: Arc::clone(&self.allowance),
            passable: Arc::clone(&self.passable),
            fluid: self.fluid.clone(),
            bodies: self.bodies.clone(),
        }
    }

    /// Runs `body` with the world lent to the mods, and gives it back.
    ///
    /// The world is moved in and out rather than borrowed, so the caller
    /// **cannot** touch it inside `body` — that is the point, and the compiler
    /// is what enforces it. Anything the tick needs from the world goes before
    /// or after this call, never inside.
    ///
    /// # Panics
    ///
    /// If the slot's lock is poisoned, or if the world is not in it afterwards.
    /// Both mean the simulation thread has already panicked somewhere inside
    /// `body`, at which point there is no world to carry on with — the tick is
    /// over either way, and a lost world is worth saying so about.
    pub fn lending<T>(&self, world: World, body: impl FnOnce() -> T) -> (World, T) {
        {
            let mut slot = self.slot.lock().expect("the world lease is poisoned");
            debug_assert!(slot.is_none(), "the world was lent twice without a return");
            *slot = Some(world);
        }

        let produced = body();

        let world = self
            .slot
            .lock()
            .expect("the world lease is poisoned")
            .take()
            .expect("the world was lent and did not come back");
        (world, produced)
    }
}

/// The mod-facing end of a [`Lease`].
///
/// One of these is installed on the VM at startup and lives for the whole run;
/// what changes is whether there is a world in the slot behind it.
pub struct Shared {
    slot: Arc<Mutex<Option<World>>>,
    allowance: Arc<std::sync::atomic::AtomicU32>,
    passable: Arc<Vec<u16>>,
    fluid: Option<Arc<std::sync::RwLock<crate::fluid::Ponds>>>,
    bodies: Option<Arc<crate::transport::PlayerBodies>>,
}

impl Shared {
    /// Runs `read` against the lent world, or answers `None` when there is none.
    ///
    /// **The escape hatch for a reader the lease has no trait for.** Sight and
    /// pathfinding are questions the engine asks the world on a mod's behalf,
    /// and each has its own seam; capturing a plan is a bulk read of terrain
    /// and of the world's own database, which is neither. Everything that
    /// reaches the lent world still comes through this file, which is the
    /// property worth keeping.
    ///
    /// `None` means the same thing [`Sighting::Unavailable`] does: no world was
    /// lent at that moment, so the caller asked outside the window.
    pub fn with_world<T>(&self, read: impl FnOnce(&World) -> T) -> Option<T> {
        let slot = self.slot.lock().ok()?;
        slot.as_ref().map(read)
    }
}

impl sight::Access for Shared {
    fn line_of_sight(&self, domain: &str, from: [f64; 3], to: [f64; 3]) -> Sighting {
        // A poisoned lease means the simulation thread panicked while the world
        // was lent, which is not something a mod should be told about with an
        // error it would have to handle.
        let Ok(slot) = self.slot.lock() else {
            return Sighting::Unavailable;
        };
        let Some(world) = slot.as_ref() else {
            return Sighting::Unavailable;
        };

        // Bound to a domain before the trait sees it: sight is cast through
        // the terrain of the space the looker is in, and `ChunkLookup` cannot
        // carry which that is.
        if sight::between(&world.solid(domain), from, to) {
            Sighting::Clear
        } else {
            Sighting::Blocked
        }
    }

    fn block_at(&self, domain: &str, pos: tiamat_core::BlockPos) -> sight::Reading {
        let Ok(slot) = self.slot.lock() else {
            return sight::Reading::Unavailable;
        };
        let Some(world) = slot.as_ref() else {
            return sight::Reading::Unavailable;
        };
        // **Resident only, and deliberately.** `World::chunk` generates what is
        // missing; a mod asking about somewhere far away must not be able to
        // make the server generate a chunk inside the tick budget, one call at
        // a time, for nobody. `Solid::resident` is the half that cannot.
        let terrain = world.solid(domain);
        let Some(chunk) = terrain.resident(pos.chunk()) else {
            return sight::Reading::Absent;
        };
        let Some(view) = chunk.get_block(pos) else {
            return sight::Reading::Absent;
        };
        // **In the id space a mod can compare against — which is the one the
        // chunk already holds.** A chunk in memory holds RUNTIME ids: the codec
        // translates to the world's own table on save and back on load
        // (`persist::codec`), so what `get_block` reads is what
        // `game.get_block_id` hands out, with no conversion here.
        //
        // There used to be one, and it was a defect: `runtime_material` maps a
        // WORLD id to a runtime id, and applying it to a runtime id is a second
        // translation through a table the value is not in. It coincides with
        // the identity for as long as runtime and world ids happen to agree —
        // every fresh world with its original mod set — and comes apart the
        // moment a mod is removed: a brick read back as a filler nobody has
        // loaded. `a_block_read_back_is_in_the_id_space_a_mod_speaks` is the
        // test, and it passed for a day on a stored answer from the run before.
        let runtime = |material: tiamat_core::MaterialId| material;
        match view {
            tiamat_core::BlockView::Uniform(material) => sight::Reading::Single {
                material: runtime(material),
                occupancy: if material.is_air() {
                    0
                } else {
                    tiamat_core::block::OCCUPANCY_FULL
                },
            },
            tiamat_core::BlockView::Partial {
                material,
                occupancy,
            } => sight::Reading::Single {
                material: runtime(material),
                occupancy: occupancy & tiamat_core::block::OCCUPANCY_FULL,
            },
            tiamat_core::BlockView::Mixed(cells) => {
                sight::Reading::Mixed(Box::new(std::array::from_fn(|index| runtime(cells[index]))))
            }
        }
    }

    fn surface_at(
        &self,
        domain: &str,
        column: [i32; 2],
        from: i32,
        depth: u32,
        skip: sight::Skip,
    ) -> Option<sight::Surface> {
        let slot = self.slot.lock().ok()?;
        let world = slot.as_ref()?;
        let terrain = world.solid(domain);
        // The fluid, only if a fluid surface is an answer — and read under
        // the lease, which is the order every mod call already takes them in.
        let ponds = if skip.fluid {
            None
        } else {
            self.fluid.as_ref().and_then(|fluid| fluid.read().ok())
        };
        let fluidics = ponds.as_ref().and_then(|ponds| ponds.get(domain));
        let depth = i32::try_from(depth.min(sight::MAX_SURFACE_DEPTH)).unwrap_or(i32::MAX);
        let passable =
            |material: tiamat_core::MaterialId| self.passable.binary_search(&material.0).is_ok();

        // **One chunk resolved per sixteen rows.** `resident` hands back a
        // reference tied to the world, not to the borrow, so a column is
        // walked with a lookup per chunk and not per block — the pattern
        // lighting uses. An unloaded chunk ends the walk with no answer:
        // never generated to find out, as `block_at` has it.
        let mut held: Option<(tiamat_core::ChunkPos, &tiamat_core::Chunk)> = None;
        for y in (from.saturating_sub(depth - 1)..=from).rev() {
            let pos = tiamat_core::BlockPos::new(column[0], y, column[1]);
            let chunk = match held {
                Some((at, chunk)) if at == pos.chunk() => chunk,
                _ => {
                    let chunk = terrain.resident(pos.chunk())?;
                    held = Some((pos.chunk(), chunk));
                    chunk
                }
            };
            let (material, occupancy) = match chunk.get_block_local(pos.local()) {
                tiamat_core::BlockView::Uniform(material) if material.is_air() => {
                    (tiamat_core::MaterialId::AIR, 0)
                }
                tiamat_core::BlockView::Uniform(material) => {
                    (material, tiamat_core::block::OCCUPANCY_FULL)
                }
                tiamat_core::BlockView::Partial {
                    material,
                    occupancy,
                } => (material, occupancy & tiamat_core::block::OCCUPANCY_FULL),
                tiamat_core::BlockView::Mixed(cells) => {
                    // The first cell that would stop the fall names the block;
                    // failing one, the first that is there at all.
                    let mut occupancy = 0u32;
                    let mut named = None;
                    let mut solid = None;
                    for (index, cell) in cells.iter().enumerate() {
                        if cell.is_air() {
                            continue;
                        }
                        occupancy |= 1 << index;
                        named.get_or_insert(*cell);
                        if !passable(*cell) {
                            solid.get_or_insert(*cell);
                        }
                    }
                    (
                        solid.or(named).unwrap_or(tiamat_core::MaterialId::AIR),
                        occupancy,
                    )
                }
            };
            if occupancy != 0 {
                if skip.passable && passable(material) {
                    continue;
                }
                return Some(sight::Surface {
                    y,
                    material,
                    occupancy,
                    fluid: None,
                });
            }
            if let Some(fluidics) = fluidics {
                let fluid = fluidics.at(pos);
                if !fluid.is_empty() {
                    return Some(sight::Surface {
                        y,
                        material: tiamat_core::MaterialId::AIR,
                        occupancy: 0,
                        fluid: Some(fluid),
                    });
                }
            }
        }
        None
    }

    fn looking_at(&self, uuid: [u8; 32]) -> Option<sight::Looked> {
        let bodies = self.bodies.as_ref()?;
        // The body first, and the lock dropped before the world's: the tick
        // takes them in this order too, and a mod call that took them the other
        // way round would be the one ordering that can deadlock.
        let (domain, origin, eye, direction) = {
            let bodies = bodies.lock().ok()?;
            let player = bodies.get(&tiamat_core::PlayerUuid::from_bytes(uuid))?;
            (
                player.domain.clone(),
                player.origin,
                player.body.eye(),
                look_direction(player.look),
            )
        };

        let slot = self.slot.lock().ok()?;
        let world = slot.as_ref()?;
        // **Not `.passing(...)`.** A tuft of grass is passable to a body and
        // is still something you can point at and dig, so targeting reads the
        // terrain as it is rather than as a walk through it does.
        let terrain = world.solid(&domain);
        let voxels = tiamat_core::phys::Voxels::new(&terrain, origin);
        let hit =
            tiamat_core::phys::ray::cast(&voxels, eye, direction, tiamat_core::phys::ray::REACH)?;

        // The hit is in the body's own chunk frame (charter rule 7); everything
        // a mod speaks is absolute. Getting this conversion wrong is how a mod
        // acts on a block a chunk away, so it is one named step.
        let corner = tiamat_core::BlockPos::from_chunk_corner(origin);
        let per = tiamat_core::SUBNODES_PER_AXIS as i32;
        let cell = tiamat_core::SubNodePos::new(
            corner.x * per + hit.cell[0],
            corner.y * per + hit.cell[1],
            corner.z * per + hit.cell[2],
        );
        // Resident only, as `block_at` is: a ray that left the loaded world
        // found nothing, rather than making the server generate a chunk from
        // inside a mod call.
        let material = terrain
            .resident(cell.chunk())
            .and_then(|chunk| chunk.get_subnode(cell))?;
        Some(sight::Looked {
            domain,
            cell,
            material,
            face: hit.normal,
        })
    }

    fn gaze(&self, uuid: [u8; 32]) -> Option<sight::Gaze> {
        let bodies = self.bodies.as_ref()?.lock().ok()?;
        let player = bodies.get(&tiamat_core::PlayerUuid::from_bytes(uuid))?;
        Some(sight::Gaze {
            domain: player.domain.clone(),
            direction: look_direction(player.look),
        })
    }
}

/// The unit vector a player's `look` points along.
///
/// **The camera's yaw, not the drawn figure's.** A body counts yaw the other
/// way round and `ent::figure_yaw` converts between them; casting from the
/// converted angle would put the ray at the mirror image of where the player is
/// looking, which is the mistake `figure_yaw` was written to fix in the other
/// direction and would be invisible at yaw zero.
///
/// `detgen::trig` and not `f32::sin_cos`: a mod acts on this answer, so it
/// becomes world state, and charter rule 4 does not exempt it the way it
/// exempts the client's own camera.
fn look_direction(look: [f32; 2]) -> [f32; 3] {
    use tiamat_core::detgen::trig;
    let turn = std::f32::consts::TAU;
    let yaw = look[0] * turn;
    let pitch = look[1] * turn;
    let cos_pitch = trig::cos(pitch);
    // The negated x, for the reason `Camera::forward` gives: the world is
    // right-handed with +y up and +z north, so east is -x and a growing yaw
    // has to swing the forward vector that way.
    [
        -cos_pitch * trig::sin(yaw),
        trig::sin(pitch),
        cos_pitch * trig::cos(yaw),
    ]
}

impl path::Access for Shared {
    fn steer(
        &self,
        domain: &str,
        from: [f64; 3],
        to: [f64; 3],
        height: i32,
    ) -> Option<path::Steer> {
        let slot = self.slot.lock().ok()?;
        let world = slot.as_ref()?;
        // Unmetered, unlike `find_path`: a steer is two block lookups, not a
        // search, so there is nothing here worth taking out of the tick's
        // pathfinding budget — and a mob that could not steer because somebody
        // else had searched would stop walking for no reason it could see.
        Some(path::steer(&world.solid(domain), from, to, height.max(1)))
    }

    fn find_path(
        &self,
        domain: &str,
        from: [f64; 3],
        to: [f64; 3],
        options: path::Options,
    ) -> path::Route {
        let Ok(slot) = self.slot.lock() else {
            return path::Route::Unavailable;
        };
        let Some(world) = slot.as_ref() else {
            return path::Route::Unavailable;
        };

        // A search names blocks, and a mod names a point in one. Truncating
        // would put a mob west of the origin in the block next door, which is
        // the bug that only ever shows up on one side of the map — so the
        // conversion is the entity API's own, `floor` and not a cast.
        let Some(from) = block_of(from) else {
            return path::Route::Unreachable;
        };
        let Some(to) = block_of(to) else {
            return path::Route::Unreachable;
        };

        // Whatever this call asked for, capped by what the tick has left. A
        // pool that is empty answers `Exhausted` without searching, which is
        // something a mob already has to handle — and is the difference between
        // a busy tick and a tick that runs eight times over budget.
        let remaining = self.allowance.load(std::sync::atomic::Ordering::Relaxed);
        if remaining == 0 {
            return path::Route::Exhausted;
        }
        let options = path::Options {
            budget: options.allowance().min(remaining),
            ..options
        };

        let (route, spent) = path::search_counted(&world.solid(domain), from, to, &options);
        // A saturating subtract, spelled out. `fetch_update` says this in one
        // call and is deprecated on nightly in favour of a `try_update` that
        // stable does not have yet — and the fuzz job builds this crate on
        // nightly with `-D warnings`, so a version that compiles warning-free
        // on both toolchains has to avoid the name entirely. The closure here
        // never failed anyway: it returned `Some` unconditionally.
        let mut left = self.allowance.load(std::sync::atomic::Ordering::Relaxed);
        while let Err(actual) = self.allowance.compare_exchange_weak(
            left,
            left.saturating_sub(spent),
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            left = actual;
        }
        route
    }
}

/// The block a world point is in, or `None` if it is not a number.
///
/// Charter rule 4: `0/0` in Lua is a quiet NaN and it reaches here the same way
/// it reaches an entity patch. Refused rather than floored, because
/// `NaN as i32` is zero and a search starting at the world origin because a mod
/// divided by zero is the kind of answer that looks like a pathfinding bug.
fn block_of(point: [f64; 3]) -> Option<tiamat_core::BlockPos> {
    if !point.iter().all(|value| value.is_finite()) {
        return None;
    }
    let transform = tiamat_core::ent::Transform::from_world(point[0], point[1], point[2]);
    Some(transform.block())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamat_core::MaterialId;
    use tiamat_core::chunk::Chunk;
    use tiamat_core::coords::ChunkPos;
    use tiamat_core::path::{self, Access as _};
    use tiamat_core::sight::Access as _;

    fn world() -> World {
        let mut registry = tiamat_core::Registry::new();
        registry.register("test:stone").expect("register");
        // A second material, so a test can tell east from west by what it hit
        // rather than only by whether it hit anything.
        registry.register("test:chalk").expect("register");
        let db = tiamat_core::persist::WorldDb::open_in_memory(&mut registry).expect("open");
        World::open(db, 1).expect("world")
    }

    /// A generator that makes nothing, so a chunk is loaded only where a test
    /// asked for one and everywhere else stays honestly absent.
    struct Empty;

    impl crate::world::ChunkSource for Empty {
        fn generate(&mut self, _domain: &str, pos: ChunkPos, _world_seed: u64) -> Chunk {
            Chunk::air(pos)
        }
    }

    #[test]
    fn nothing_is_visible_outside_a_lend() {
        let lease = Lease::new();
        let handle = lease.handle();
        assert_eq!(
            handle.line_of_sight(tiamat_core::domain::OVERWORLD, [0.0; 3], [1.0, 0.0, 0.0]),
            Sighting::Unavailable
        );
    }

    #[test]
    fn a_lent_world_answers_and_comes_back() {
        let lease = Lease::new();
        let handle = lease.handle();

        let mut world = world();
        world
            .chunk(
                tiamat_core::domain::OVERWORLD,
                ChunkPos::new(0, 0, 0),
                &mut Empty,
            )
            .expect("the chunk loads");

        let (world, seen) = lease.lending(world, || {
            handle.line_of_sight(
                tiamat_core::domain::OVERWORLD,
                [0.5, 0.5, 0.5],
                [2.5, 0.5, 0.5],
            )
        });

        assert_eq!(seen, Sighting::Clear);
        assert_eq!(world.cached(), 1, "the world came back with its chunk");

        // And the slot is empty again, so the next mod call outside a lend is
        // told so rather than reading a world the tick has moved on from.
        assert_eq!(
            handle.line_of_sight(tiamat_core::domain::OVERWORLD, [0.0; 3], [1.0, 0.0, 0.0]),
            Sighting::Unavailable
        );
    }

    /// A world with a wall of stone three blocks north of the body below.
    fn wall() -> World {
        let mut world = world();
        world
            .chunk(
                tiamat_core::domain::OVERWORLD,
                ChunkPos::new(0, 0, 0),
                &mut Empty,
            )
            .expect("the chunk loads");
        for x in 0..16 {
            for y in 0..16 {
                place(
                    &mut world,
                    tiamat_core::BlockPos::new(x, y, 10),
                    MaterialId(1),
                );
            }
        }
        world
    }

    /// One player, standing at cell (24, 24, 24) — block (8, 8, 8) — looking
    /// where `look` points.
    fn watcher(look: [f32; 2]) -> (tiamat_core::PlayerUuid, Arc<crate::transport::PlayerBodies>) {
        let uuid = tiamat_core::PlayerUuid::from_bytes([9; 32]);
        let mut sim = crate::transport::endpoint::PlayerSim::spawned_at(
            tiamat_core::BlockPos::new(8, 8, 8),
            0,
        );
        sim.body.position = [24.0, 24.0, 24.0];
        sim.look = look;
        let bodies = Arc::new(crate::transport::PlayerBodies::new(
            [(uuid, sim)].into_iter().collect(),
        ));
        (uuid, bodies)
    }

    #[test]
    fn a_crosshair_names_the_cell_it_is_on_in_world_coordinates() {
        // The conversion out of the body's chunk frame is the part worth a
        // test: a mod acting on a cell a chunk away from the one somebody is
        // pointing at is a bug that only shows up away from the origin.
        let (uuid, bodies) = watcher([0.0, 0.0]);
        let lease = Lease::new().with_players(Arc::clone(&bodies));
        let handle = lease.handle();

        let (_world, looked) = lease.lending(wall(), || handle.looking_at(*uuid.as_bytes()));
        let looked = looked.expect("the wall is three blocks north and within reach");
        assert_eq!(looked.cell.z, 30, "the near face of the wall is cell z 30");
        assert_eq!(looked.material, MaterialId(1));
        assert_eq!(
            looked.face,
            [0, 0, -1],
            "the face points back out of the wall, so a placement goes in front of it"
        );
        assert_eq!(looked.domain, tiamat_core::domain::OVERWORLD);
    }

    #[test]
    fn looking_the_other_way_finds_nothing() {
        // Half a turn of yaw. If the yaw conversion were mirrored — the
        // mistake `ent::figure_yaw` exists to make and unmake — this would hit
        // the wall, and the test above would pass anyway.
        let (uuid, bodies) = watcher([0.5, 0.0]);
        let lease = Lease::new().with_players(bodies);
        let handle = lease.handle();

        let (_world, looked) = lease.lending(wall(), || handle.looking_at(*uuid.as_bytes()));
        assert_eq!(looked, None, "a body facing south found the wall behind it");
    }

    #[test]
    fn a_quarter_turn_of_yaw_looks_east_and_not_west() {
        // The other half of the same trap, and the one yaw zero cannot catch:
        // east is -x in this world (+y is up and +z is north, so +x is west),
        // and a forward vector written with the wrong sign turns the view left
        // when the mouse goes right. `Camera::forward` carries the same note
        // because it was written that way once.
        let (uuid, bodies) = watcher([0.25, 0.0]);
        let lease = Lease::new().with_players(bodies);
        let handle = lease.handle();

        // One block either side of the body, in different materials.
        let mut world = wall();
        place(
            &mut world,
            tiamat_core::BlockPos::new(6, 9, 8),
            MaterialId(1),
        );
        place(
            &mut world,
            tiamat_core::BlockPos::new(10, 9, 8),
            MaterialId(2),
        );

        let (_world, looked) = lease.lending(world, || handle.looking_at(*uuid.as_bytes()));
        let looked = looked.expect("a block one either side, and one of them is east");
        assert_eq!(
            looked.material,
            MaterialId(1),
            "a quarter turn of yaw looked west; east is -x"
        );
        assert_eq!(looked.cell.x, 20, "the near face of the east block");
        assert_eq!(looked.face, [1, 0, 0]);
    }

    #[test]
    fn a_crosshair_on_nothing_is_nothing_and_so_is_no_world() {
        // Straight up: the sky is not a target, and neither is an unlent world.
        let (uuid, bodies) = watcher([0.0, 0.2]);
        let lease = Lease::new().with_players(bodies);
        let handle = lease.handle();
        assert_eq!(
            handle.looking_at(*uuid.as_bytes()),
            None,
            "answered from outside a lend"
        );
        let (_world, looked) = lease.lending(wall(), || handle.looking_at(*uuid.as_bytes()));
        assert_eq!(looked, None);
    }

    #[test]
    fn a_lease_with_no_players_answers_nothing_rather_than_panicking() {
        let lease = Lease::new();
        let handle = lease.handle();
        let (_world, looked) = lease.lending(wall(), || handle.looking_at([9; 32]));
        assert_eq!(looked, None);
    }

    /// A room with a floor and a pillar in it.
    ///
    /// The pillar's top is somewhere a body could stand and nowhere it can get
    /// to, which is what makes a search spend its whole allowance: an
    /// unreachable goal that is merely *outside* the loaded world is rejected
    /// before the first expansion, and would prove nothing about a budget.
    fn room() -> World {
        let mut world = world();
        let stone = MaterialId(1);
        world
            .chunk(
                tiamat_core::domain::OVERWORLD,
                ChunkPos::new(0, 0, 0),
                &mut Empty,
            )
            .expect("the chunk loads");
        for x in 0..16 {
            for z in 0..16 {
                place(&mut world, tiamat_core::BlockPos::new(x, 0, z), stone);
            }
        }
        for y in 1..=3 {
            place(&mut world, tiamat_core::BlockPos::new(8, y, 8), stone);
        }
        world
    }

    fn place(world: &mut World, pos: tiamat_core::BlockPos, material: MaterialId) {
        world
            .apply(
                tiamat_core::domain::OVERWORLD,
                &tiamat_core::proto::Edit::Block {
                    pos,
                    material: material.get(),
                },
                &mut Empty,
            )
            .expect("place");
    }

    /// Start on the floor; goal on top of the pillar, three blocks up.
    const FLOOR: [f64; 3] = [1.5, 1.0, 1.5];
    const PILLAR_TOP: [f64; 3] = [8.5, 4.0, 8.5];

    #[test]
    fn the_ticks_pathfinding_pool_is_shared_and_refilled() {
        // The protection this exists for: two hundred mobs each making one
        // affordable search is not affordable. Once the pool is empty every
        // later search says so without looking, and the next tick refills it.
        let lease = Lease::new();
        let handle = lease.handle();
        lease.open_tick();

        let mut world = room();

        // One search over the whole room, to prove the fixture is a search and
        // not an early refusal.
        let (returned, first) = lease.lending(world, || {
            handle.find_path(
                tiamat_core::domain::OVERWORLD,
                FLOOR,
                PILLAR_TOP,
                path::Options::default(),
            )
        });
        world = returned;
        assert_eq!(
            first,
            path::Route::Unreachable,
            "the pillar top should be standable and unreachable"
        );

        // Drain the rest of the pool.
        let (returned, ()) = lease.lending(world, || {
            for _ in 0..path::TICK_BUDGET.div_ceil(64) {
                let _ = handle.find_path(
                    tiamat_core::domain::OVERWORLD,
                    FLOOR,
                    PILLAR_TOP,
                    path::Options::default(),
                );
            }
        });
        world = returned;

        let (returned, drained) = lease.lending(world, || {
            handle.find_path(
                tiamat_core::domain::OVERWORLD,
                FLOOR,
                PILLAR_TOP,
                path::Options::default(),
            )
        });
        world = returned;
        assert_eq!(
            drained,
            path::Route::Exhausted,
            "the pool did not run out, so it is not a pool"
        );

        // And a new tick gets a fresh one.
        lease.open_tick();
        let (_world, refilled) = lease.lending(world, || {
            handle.find_path(
                tiamat_core::domain::OVERWORLD,
                FLOOR,
                PILLAR_TOP,
                path::Options::default(),
            )
        });
        assert_eq!(
            refilled,
            path::Route::Unreachable,
            "the pool was not refilled, so one busy tick would starve every later one"
        );
    }

    #[test]
    fn terrain_the_world_has_not_loaded_blocks() {
        // The difference this test exists for: `Blocked` is an answer about the
        // world and `Unavailable` is an answer about the engine, and a mod that
        // cannot tell them apart cannot tell "the mob lost sight of you"
        // from "the mob was asked at the wrong moment".
        let lease = Lease::new();
        let handle = lease.handle();

        let (_world, seen) = lease.lending(world(), || {
            handle.line_of_sight(
                tiamat_core::domain::OVERWORLD,
                [0.5, 0.5, 0.5],
                [2.5, 0.5, 0.5],
            )
        });
        assert_eq!(seen, Sighting::Blocked);
    }

    #[test]
    fn the_top_of_a_column_is_found_in_one_call_and_looks_through_what_it_is_told_to() {
        // **Weather ask W6.** The room's floor is at y = 0 and its pillar
        // reaches y = 3. A tuft of a passable material on the floor is the
        // surface unless the caller looks through it; a pond's surface is an
        // answer unless the caller looks through that too; an unloaded chunk
        // is no answer, and so is a column with nothing in reach.
        let stone = MaterialId(1);
        let grass = MaterialId(2);
        let mut world = room();
        place(&mut world, tiamat_core::BlockPos::new(1, 1, 1), grass);

        let fluid = std::sync::Arc::new(std::sync::RwLock::new(crate::fluid::Ponds::new(
            tiamat_core::fluid::Fluids::new(),
            tiamat_core::fluid::Absorbency::default(),
        )));
        let milk = tiamat_core::fluid::Fluid::new(tiamat_core::fluid::FluidId(1), 27);
        fluid
            .write()
            .expect("fluid")
            .of(tiamat_core::domain::OVERWORLD)
            .set(tiamat_core::BlockPos::new(3, 1, 3), milk);

        let lease = Lease::new().with_terrain(vec![grass.0], fluid);
        let handle = lease.handle();
        let domain = tiamat_core::domain::OVERWORLD;
        let look = |handle: &Shared, x: i32, z: i32, from: i32, depth: u32, skip: sight::Skip| {
            handle.surface_at(domain, [x, z], from, depth, skip)
        };
        let nothing = sight::Skip::default();
        let through_tufts = sight::Skip {
            passable: true,
            fluid: false,
        };
        let through_ponds = sight::Skip {
            passable: false,
            fluid: true,
        };

        assert_eq!(
            look(&handle, 8, 8, 15, 16, nothing),
            None,
            "outside a lend there is no world to read"
        );
        let (_, seen) = lease.lending(world, || {
            (
                look(&handle, 8, 8, 15, 16, nothing),
                look(&handle, 5, 5, 15, 16, nothing),
                look(&handle, 1, 1, 15, 16, nothing),
                look(&handle, 1, 1, 15, 16, through_tufts),
                look(&handle, 3, 3, 15, 16, nothing),
                look(&handle, 3, 3, 15, 16, through_ponds),
                look(&handle, 5, 5, 15, 3, nothing),
                look(&handle, 40, 5, 15, 16, nothing),
            )
        });
        let full = tiamat_core::block::OCCUPANCY_FULL;
        assert_eq!(
            seen.0.map(|s| (s.y, s.material, s.occupancy)),
            Some((3, stone, full)),
            "the pillar's top"
        );
        assert_eq!(seen.1.map(|s| s.y), Some(0), "the floor");
        assert_eq!(
            seen.2.map(|s| (s.y, s.material)),
            Some((1, grass)),
            "a tuft is a surface"
        );
        assert_eq!(
            seen.3.map(|s| (s.y, s.material)),
            Some((0, stone)),
            "unless it is looked through, and then the floor under it is"
        );
        let pond = seen.4.expect("the pond's surface");
        assert_eq!(
            (pond.y, pond.material, pond.fluid),
            (1, MaterialId::AIR, Some(milk))
        );
        assert_eq!(
            seen.5.map(|s| s.y),
            Some(0),
            "looked through, the pond's bed"
        );
        assert_eq!(seen.6, None, "nothing within three blocks of y = 15");
        assert_eq!(seen.7, None, "a column in a chunk that is not loaded");
    }
}
