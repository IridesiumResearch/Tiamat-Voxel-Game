// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What one chunk costs to mesh.
//!
//! # Why this exists
//!
//! The mesher is the client's largest per-frame cost and, until this, the only
//! major one with no standing measurement. Task 02b's spike measured 0.108 ms
//! for a realistic chunk and that figure is quoted where the remesh budget is
//! set, but it was a spike: nothing re-measured it, so a change that made
//! meshing an order of magnitude slower would have shown up only as somebody's
//! frame rate.
//!
//! It was reported from the window as exactly that — a worst frame of 34 ms
//! with 26 of them inside a single chunk's mesh, in release. There was no
//! number to compare it against.
//!
//! # What the cases are
//!
//! Meshing cost tracks the CELLS SCANNED, not the size of the result: every
//! chunk is 110,592 sub-node cells whatever is in it. So the cases here differ
//! in how much *geometry* they produce rather than in how much they scan, which
//! is what separates the scan cost from the merge and shading cost.
//!
//! - `air` — nothing at all, which is most of the sky and most of a world.
//!   `mesh_chunk` skips it without scanning, so this measures the skip.
//! - `uniform` — solid stone. The scan, with almost nothing to emit.
//! - `terrain` — the shape a real chunk has: ground, air above, a hole.
//! - `chiselled` — every block partial. The sub-node worst case, and the one
//!   charter rule 19 committed to keeping fast.
//!
//! Read against the frame, not in isolation: at 60 fps a frame is 16.7 ms, and
//! the client's remesh budget gives meshing 2 ms of it.

use criterion::{Criterion, criterion_group, criterion_main};

use client::mesher::{self, Absent, Neighbours, NoFluid, NoGlass, Sight};
use client::shade::{BlockLight, Uniform};
use tiamat_core::{BlockPos, BlockValue, Chunk, ChunkPos, MaterialId};

/// Full daylight everywhere, so the bench measures geometry rather than light.
const DAY: Uniform = Uniform(tiamat_core::light::Light::DAYLIGHT);

/// Light that varies from block to block, the way propagated light does.
///
/// **Why a second light source is worth benchmarking.** The mesher may only
/// merge two faces whose corner light AGREES, so real light splits quads along
/// every shadow edge — and a constant, which agrees with itself everywhere,
/// measures a merge rate no lit world ever achieves. The client meshes with
/// `ChunkStore::light_for` in every mode that propagates light, so the constant
/// case is the floor and this is nearer the bill.
///
/// Cheap to evaluate on purpose: the cost being measured is the collapse in
/// merging, not the cost of the lookup, and mixing the two would leave no way
/// to tell which had grown.
struct Dappled;

impl client::shade::BlockLight for Dappled {
    fn at(&self, x: i32, y: i32, z: i32) -> tiamat_core::light::Light {
        // A coarse checker rather than noise: deterministic, no allocation, and
        // it changes often enough to break merges without being so busy that
        // every single face becomes its own quad.
        let step = u8::try_from((x.abs() / 2 + y.abs() / 2 + z.abs() / 2) % 4).unwrap_or(0);
        tiamat_core::light::Light::new(tiamat_core::light::MAX_LEVEL - step, 0, 0, 0)
    }
}

/// Light that costs what the client's lookup costs.
///
/// `Uniform` and `Dappled` answer from a register, which hides the cost the
/// mesher actually pays: in the game every `at` goes through
/// `MeshInput::at` — which neighbour slot, which local block, an `Option`
/// chain, an index into that chunk's light. This fixture does the same work
/// over a 3×3×3 set of stored chunks holding `Dappled`'s values, so a change
/// in how many times a corner is sampled shows up at its true weight.
struct Stored {
    layers: Vec<Option<Vec<tiamat_core::light::Light>>>,
}

impl Stored {
    fn new() -> Self {
        let layers = (0..27)
            .map(|slot| {
                let (sx, sy, sz) = (slot % 3 - 1, slot / 3 % 3 - 1, slot / 9 - 1);
                let mut layer = Vec::with_capacity(16 * 16 * 16);
                for z in 0..16 {
                    for y in 0..16 {
                        for x in 0..16 {
                            layer.push(Dappled.at(sx * 16 + x, sy * 16 + y, sz * 16 + z));
                        }
                    }
                }
                Some(layer)
            })
            .collect();
        Self { layers }
    }
}

impl client::shade::BlockLight for Stored {
    fn at(&self, x: i32, y: i32, z: i32) -> tiamat_core::light::Light {
        let slot_of = |value: i32| value.div_euclid(16) + 1;
        let (sx, sy, sz) = (slot_of(x), slot_of(y), slot_of(z));
        if !(0..3).contains(&sx) || !(0..3).contains(&sy) || !(0..3).contains(&sz) {
            return tiamat_core::light::Light::DARK;
        }
        let slot = (sx + sy * 3 + sz * 9) as usize;
        let local = |value: i32| value.rem_euclid(16) as usize;
        self.layers
            .get(slot)
            .and_then(Option::as_ref)
            .map_or(tiamat_core::light::Light::DARK, |layer| {
                layer[local(x) + local(y) * 16 + local(z) * 256]
            })
    }
}

const STONE: MaterialId = MaterialId(2);

/// Solid stone, corner to corner.
fn uniform() -> Chunk {
    Chunk::new(ChunkPos::new(0, 0, 0), STONE)
}

/// Nothing at all — the sky, and the majority of any world's chunks.
fn air() -> Chunk {
    Chunk::new(ChunkPos::new(0, 0, 0), MaterialId::AIR)
}

/// The shape a chunk of real terrain has: ground below, air above, one hole.
fn terrain() -> Chunk {
    let mut chunk = Chunk::new(ChunkPos::new(0, 0, 0), MaterialId::AIR);
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..8 {
                chunk
                    .set_block(BlockPos::new(x, y, z), BlockValue::Uniform(STONE))
                    .expect("in chunk");
            }
        }
    }
    chunk
        .set_block(BlockPos::new(8, 7, 8), BlockValue::Uniform(MaterialId::AIR))
        .expect("in chunk");
    chunk
}

/// Every block partial: the sub-node worst case.
///
/// One cell missing from each block is the most expensive thing a chunk can be
/// — nothing merges, and every block has to be read cell by cell rather than
/// taken whole by the `Uniform` fast path.
fn chiselled() -> Chunk {
    let mut chunk = Chunk::new(ChunkPos::new(0, 0, 0), STONE);
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..16 {
                chunk
                    .set_subnode(BlockPos::new(x, y, z).subnode(1, 1, 1), MaterialId::AIR)
                    .expect("in chunk");
            }
        }
    }
    chunk
}

/// Glass, foliage, sprites, models and flickers all registered, as a world with
/// the default mods has them.
///
/// **This is the path the game runs.** With `NoGlass` the mesher skips every
/// classification pass; with a populated `Sight` it has to look at every cell
/// for every kind whether the chunk holds one or not, and that cost is what
/// this measures — over chunks that hold none of them, and over one that does.
fn populated() -> Sight {
    Sight {
        glass: [GLASS.0].into_iter().collect(),
        foliage: [LEAVES.0].into_iter().collect(),
        sprites: [GRASS.0, FLOWER.0].into_iter().collect(),
        crosses: [FLOWER.0].into_iter().collect(),
        models: [LANTERN.0].into_iter().collect(),
        flickers: [TORCH.0].into_iter().collect(),
    }
}

const GLASS: MaterialId = MaterialId(7);
const LEAVES: MaterialId = MaterialId(8);
const GRASS: MaterialId = MaterialId(9);
const FLOWER: MaterialId = MaterialId(10);
const LANTERN: MaterialId = MaterialId(11);
const TORCH: MaterialId = MaterialId(12);

/// Terrain with one of everything on it: a pane of glass, a canopy of leaves,
/// a meadow of sprites, a lantern and a torch.
fn mixed() -> Chunk {
    let mut chunk = terrain();
    for x in 2..6 {
        chunk
            .set_block(BlockPos::new(x, 8, 2), BlockValue::Uniform(GLASS))
            .expect("in chunk");
    }
    for x in 8..14 {
        for z in 8..14 {
            chunk
                .set_block(BlockPos::new(x, 11, z), BlockValue::Uniform(LEAVES))
                .expect("in chunk");
        }
    }
    for x in 0..16 {
        for z in 0..8 {
            let plant = if (x + z) % 3 == 0 { FLOWER } else { GRASS };
            chunk
                .set_subnode(BlockPos::new(x, 8, z).subnode(1, 0, 1), plant)
                .expect("in chunk");
        }
    }
    chunk
        .set_block(BlockPos::new(10, 8, 3), BlockValue::Uniform(LANTERN))
        .expect("in chunk");
    chunk
        .set_block(BlockPos::new(12, 8, 3), BlockValue::Uniform(TORCH))
        .expect("in chunk");
    chunk
}

fn meshing(c: &mut Criterion) {
    let mut group = c.benchmark_group("mesh_chunk_stored");
    let stored = Stored::new();
    for (name, chunk) in [
        ("uniform", uniform()),
        ("terrain", terrain()),
        ("chiselled", chiselled()),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let neighbours = Neighbours::none();
                std::hint::black_box(mesher::mesh_chunk(
                    std::hint::black_box(&chunk),
                    &neighbours,
                    Absent::Air,
                    &stored,
                    &NoFluid,
                    &NoGlass,
                ))
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("mesh_chunk_sight");
    let sight = populated();
    for (name, chunk) in [
        ("terrain", terrain()),
        ("chiselled", chiselled()),
        ("mixed", mixed()),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let neighbours = Neighbours::none();
                std::hint::black_box(mesher::mesh_chunk(
                    std::hint::black_box(&chunk),
                    &neighbours,
                    Absent::Air,
                    &DAY,
                    &NoFluid,
                    &sight,
                ))
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("mesh_chunk");
    for (name, chunk) in [
        ("air", air()),
        ("uniform", uniform()),
        ("terrain", terrain()),
        ("chiselled", chiselled()),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let neighbours = Neighbours::none();
                std::hint::black_box(mesher::mesh_chunk(
                    std::hint::black_box(&chunk),
                    &neighbours,
                    Absent::Air,
                    &DAY,
                    &NoFluid,
                    &NoGlass,
                ))
            });
        });
        // The same chunk under light that varies, which is what the client
        // actually meshes with. See `Dappled`.
        group.bench_function(format!("{name}_lit"), |b| {
            b.iter(|| {
                let neighbours = Neighbours::none();
                std::hint::black_box(mesher::mesh_chunk(
                    std::hint::black_box(&chunk),
                    &neighbours,
                    Absent::Air,
                    &Dappled,
                    &NoFluid,
                    &NoGlass,
                ))
            });
        });
    }
    group.finish();

    // **The floor a step cannot go below.** `MeshJob` splits the shading and
    // merging into 294 steps, but the grid is expanded once and in one go — a
    // single scan of 110,592 cells. Whatever the budget, a chunk costs at least
    // this before any of it can be interrupted, so this is the number that says
    // whether splitting the rest was enough.
    let mut group = c.benchmark_group("mesh_start");
    for (name, chunk) in [
        ("uniform", uniform()),
        ("terrain", terrain()),
        ("chiselled", chiselled()),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let neighbours = Neighbours::none();
                std::hint::black_box(mesher::MeshJob::start(
                    std::hint::black_box(&chunk),
                    &neighbours,
                    Absent::Air,
                    &NoFluid,
                    &NoGlass,
                ))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, meshing);
criterion_main!(benches);
