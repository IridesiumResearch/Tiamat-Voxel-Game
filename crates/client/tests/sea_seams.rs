// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A sea streamed from a real server draws no walls down its own seams.
//!
//! Reported twice from the window: sheets of water standing between the chunks
//! of an ocean. The first cause was a fluid face drawn against a chunk that had
//! not arrived (`f9874c0`), and the test that holds it builds its ocean in the
//! client's own store by hand — which can only ever check the store it was
//! handed. This one starts a server, streams a sea out of a real generator over
//! a real connection, and meshes what arrives the way the frame loop does.
//!
//! The question it asks of every chunk is the one a player asks by looking: is
//! there a fluid face on a chunk boundary with water on the far side of it?
//! Such a face is interior to one body of water and must not exist. A surface
//! the sea really has is not one — it has air above it, not water.
//!
//! **Non-vacuous by construction.** Each run ends by giving one chunk a layer of
//! a fluid the client was never told about, which `fluid_at` reports as water
//! and the mesher draws as nothing, and asserts the search then finds walls. A
//! search that cannot see a wall would otherwise pass this file for ever.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use client::cache::ContentCache;
use client::mesher;
use client::net::{Command, Connection, Event, Pinning};
use client::world::{ABSENT_POLICY, ChunkStore};
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_core::{BlockPos, ChunkPos};
use tiamot_server::{ServerHandle, Settings};

/// Meshing asks for light and none of these worlds are about light.
const DAY: client::shade::Uniform = client::shade::Uniform(tiamot_core::light::Light::DAYLIGHT);

/// How long a sea has to arrive before the wait is called a failure.
const PATIENCE: Duration = Duration::from_secs(40);

/// How long nothing may arrive before the world counts as settled.
const QUIET: Duration = Duration::from_secs(2);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-sea-seams").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Writes a one-mod world whose `init.lua` is `lua`.
fn write_sea(name: &str, lua: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("sea");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"sea\"\nname = \"Sea\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(dir.join("init.lua"), lua).expect("script");
    root
}

/// A fluid face on a chunk seam with water on the far side of it.
struct Wall {
    chunk: ChunkPos,
    axis: u8,
    positive: bool,
    at: (u32, u32, u32),
}

impl std::fmt::Debug for Wall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let side = ["x", "y", "z"][usize::from(self.axis)];
        let sign = if self.positive { "+" } else { "-" };
        write!(f, "{:?} {sign}{side} at {:?}", self.chunk, self.at)
    }
}

/// Every fluid face on a seam whose far block holds water.
///
/// Meshes each held chunk exactly as `App::remesh` does — the store's own
/// neighbours, absent policy and fluid view — so what this reads is what the
/// player would be looking at.
fn seam_walls(store: &ChunkStore) -> Vec<Wall> {
    let mut walls = Vec::new();
    let edge = tiamot_core::CHUNK_SUBNODES;
    let per = tiamot_core::SUBNODES_PER_AXIS;
    for pos in store.positions().collect::<Vec<_>>() {
        let Some(chunk) = store.get(pos) else {
            continue;
        };
        let mesh = mesher::mesh_chunk(
            chunk,
            &store.neighbours(pos),
            ABSENT_POLICY,
            &DAY,
            &store.fluid_for(pos),
            &mesher::NoGlass,
        );
        let corner = BlockPos::from_chunk_corner(pos);
        for quad in mesh.fluid_vertices.chunks_exact(4) {
            let (axis, positive) = quad[0].face();
            let min = quad.iter().fold((u32::MAX, u32::MAX, u32::MAX), |m, v| {
                let (x, y, z) = v.position();
                (m.0.min(x), m.1.min(y), m.2.min(z))
            });
            let max = quad.iter().fold((0, 0, 0), |m, v| {
                let (x, y, z) = v.position();
                (m.0.max(x), m.1.max(y), m.2.max(z))
            });
            let plane = [min.0, min.1, min.2][usize::from(axis)];
            if !((positive && plane == edge) || (!positive && plane == 0)) {
                continue;
            }
            // The blocks this quad covers, asked one at a time: a merged face
            // can span a shoreline, and one wet block behind it is a wall.
            let (ua, va) = match axis {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let span = |a: usize| {
                let lo = [min.0, min.1, min.2][a] / per;
                let hi = [max.0, max.1, max.2][a].div_ceil(per).max(lo + 1);
                lo as i32..hi as i32
            };
            let wet_across = span(ua).any(|u| {
                span(va).any(|v| {
                    let mut block = [0i32; 3];
                    block[usize::from(axis)] = if positive {
                        tiamot_core::CHUNK_BLOCKS as i32
                    } else {
                        -1
                    };
                    block[ua] = u;
                    block[va] = v;
                    let at = BlockPos::new(
                        corner.x + block[0],
                        corner.y + block[1],
                        corner.z + block[2],
                    );
                    !store.fluid_at(at).is_empty()
                })
            });
            if wet_across {
                walls.push(Wall {
                    chunk: pos,
                    axis,
                    positive,
                    at: min,
                });
            }
        }
    }
    walls
}

/// A client watching one sea, accumulating what arrives the way a frame loop
/// would.
struct Diver {
    server: ServerHandle,
    connection: Connection,
    store: ChunkStore,
    tally: std::collections::BTreeMap<&'static str, usize>,
}

impl Diver {
    fn join(name: &str, lua: &str, view: ViewDistance) -> Self {
        let server = ServerHandle::start(&Settings {
            bind_addr: "127.0.0.1:0".parse().expect("loopback"),
            world_path: scratch(&format!("{name}-world")),
            identity_path: None,
            max_players: 2,
            allowlist: Allowlist::open(),
            operators: Vec::new(),
            view_distance: view,
            mods_path: Some(write_sea(&format!("{name}-mods"), lua)),
            enabled_mods: None,
            seed: Some(11),
            rcon: None,
            materials: Vec::new(),
        })
        .expect("start");
        let home = scratch(&format!("{name}-home"));
        let connection = Connection::open(
            server.local_addr(),
            Identity::generate().expect("identity"),
            "Diver".to_owned(),
            ContentCache::open(&home.join("content")).expect("cache"),
            Pinning::Remembered(&home.join("known-hosts")),
        )
        .expect("connect");
        Self {
            server,
            connection,
            store: ChunkStore::new(),
            tally: std::collections::BTreeMap::new(),
        }
    }

    /// Applies everything waiting. Returns how much of it was TERRAIN.
    ///
    /// Two kinds of event are deliberately not counted. The clock and the
    /// player's own state arrive every tick for ever, so counting them would
    /// never settle. And the horizon is thousands of summaries out to 32 chunks
    /// whatever the view distance is — it was still arriving after forty
    /// seconds here, long after all 203 chunks of the sea and their water had —
    /// and no summary carries fluid, so none of it can put a wall in a sea.
    fn pump(&mut self) -> usize {
        let mut world = 0;
        while let Some(event) = self.connection.poll() {
            match event {
                Event::Chunk(chunk, tint, _) => {
                    world += 1;
                    *self.tally.entry("chunk").or_default() += 1;
                    self.store.set_tint(chunk.pos(), tint);
                    self.store.insert(*chunk);
                }
                Event::ChunkSummary { pos, summary } => {
                    *self.tally.entry("summary").or_default() += 1;
                    self.store.set_summary(pos, *summary);
                }
                Event::ChunkLight(pos, layer) => {
                    world += 1;
                    *self.tally.entry("light").or_default() += 1;
                    self.store.set_light(pos, *layer);
                }
                Event::ChunkFluid(pos, layer) => {
                    world += 1;
                    *self.tally.entry("fluid").or_default() += 1;
                    self.store.set_fluid(pos, *layer);
                }
                Event::Fluids { fluids } => self.store.set_fluid_table(&fluids),
                Event::ChunkUnload(pos) => {
                    world += 1;
                    *self.tally.entry("unload").or_default() += 1;
                    self.store.remove(pos);
                }
                Event::Edit(edit) => {
                    world += 1;
                    *self.tally.entry("edit").or_default() += 1;
                    self.store.apply(&edit);
                }
                Event::Disconnected { reason } => panic!("the connection ended: {reason}"),
                _ => {}
            }
        }
        world
    }

    /// Streams until nothing has arrived for [`QUIET`].
    ///
    /// A condition rather than a fixed wait: a sea takes as long to arrive as
    /// the machine takes, and a test that counts seconds is a bet on the
    /// slowest runner in the matrix.
    fn settle(&mut self) {
        let deadline = Instant::now() + PATIENCE;
        let mut last = Instant::now();
        while Instant::now() < deadline {
            if self.pump() > 0 {
                last = Instant::now();
            } else if last.elapsed() >= QUIET && !self.store.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        panic!(
            "the sea never stopped arriving: {} chunks, {:?}",
            self.store.len(),
            self.tally
        );
    }

    fn say(&mut self, line: &str) {
        assert!(
            self.connection.send(Command::Chat(line.to_owned())),
            "the server stopped listening"
        );
    }

    /// Asserts the sea has no walls, and that the search could have found one.
    fn assert_seamless(&mut self) {
        let walls = seam_walls(&self.store);
        assert!(
            walls.is_empty(),
            "{} walls on the seams of a sea of {} chunks: {:?}",
            walls.len(),
            self.store.len(),
            walls.iter().take(8).collect::<Vec<_>>()
        );

        // The counter-example. A layer of a fluid this client was never told
        // about is water to `fluid_at` and nothing to the mesher, so every wet
        // neighbour stands a wall against it.
        use tiamot_core::phys::FluidLookup;
        let wet = self
            .store
            .positions()
            .find(|pos| self.store.chunk_has_fluid(*pos));
        let Some(wet) = wet else {
            panic!("no chunk of the sea holds water at all");
        };
        let held = self.store.fluid_layer(wet).cloned().expect("held");
        let mut unknown = tiamot_core::fluid::FluidLayer::default();
        for index in 0..tiamot_core::BLOCKS_PER_CHUNK {
            let local = tiamot_core::coords::LocalBlock::from_index(index);
            unknown.set(
                local,
                tiamot_core::fluid::Fluid::new(
                    tiamot_core::fluid::FluidId(u8::MAX),
                    tiamot_core::fluid::MAX_VOLUME,
                ),
            );
        }
        self.store.set_fluid(wet, unknown);
        let found = seam_walls(&self.store).len();
        self.store.set_fluid(wet, held);
        assert!(
            found > 0,
            "the search found nothing when {wet:?} was given water the client cannot draw, \
             so it could not have found a real wall either"
        );
    }

    fn stop(self) {
        drop(self.connection);
        assert!(self.server.stop());
    }
}

/// A sea at y = 8 over a floor at y = -40: deep enough to cross chunk layers.
const FLAT: &str = "local sand = game.register_block{ id = \"sand\" }\n\
     game.register_block{ id = \"water\" }\n\
     game.register_fluid{ id = \"water\", material = \"sea:water\" }\n\
     game.register_on_generate(function(buf, pos)\n\
     \x20   buf:fill_below_heightmap(game.flat_heightmap(-40), sand)\n\
     \x20   buf:fill_fluid_below(8, \"sea:water\")\n\
     end)\n";

/// A sea at y = 0 over sub-node terrain that breaks the surface: islands,
/// shorelines and a seabed that crosses blocks at an angle.
const ISLANDS: &str = "local sand = game.register_block{ id = \"sand\" }\n\
     game.register_block{ id = \"water\" }\n\
     game.register_fluid{ id = \"water\", material = \"sea:water\" }\n\
     local field = game.density{\n\
     \x20   op = \"sub\",\n\
     \x20   a = { op = \"noise\", stream = \"terrain\", frequency = 0.03, octaves = 3 },\n\
     \x20   b = { op = \"mul\",\n\
     \x20         a = { op = \"add\", a = { op = \"y\" }, b = { op = \"const\", value = 6 } },\n\
     \x20         b = { op = \"const\", value = 0.05 } },\n\
     }\n\
     game.register_on_generate(function(buf, pos)\n\
     \x20   buf:fill_density(field, sand, { detail = \"smooth\" })\n\
     \x20   buf:fill_fluid_below(0, \"sea:water\")\n\
     end)\n\
     game.register_on_chat(function(event)\n\
     \x20   local x, z = event.text:match(\"^go (%-?%d+) (%-?%d+)$\")\n\
     \x20   if x then\n\
     \x20       game.move_player(event.player, { x = tonumber(x), y = 4, z = tonumber(z) })\n\
     \x20   end\n\
     \x20   return false\n\
     end)\n";

const VIEW: ViewDistance = ViewDistance {
    horizontal: 3,
    vertical: 3,
};

#[test]
fn a_streamed_sea_draws_no_walls_between_its_chunks() {
    let mut diver = Diver::join("flat", FLAT, VIEW);
    diver.settle();
    diver.assert_seamless();
    diver.stop();
}

#[test]
fn a_sea_over_subnode_terrain_draws_no_walls_between_its_chunks() {
    let mut diver = Diver::join("islands", ISLANDS, VIEW);
    diver.settle();
    diver.assert_seamless();
    diver.stop();
}

#[test]
fn a_sea_crossed_by_a_swimmer_draws_no_walls_between_its_chunks() {
    // **The streaming churn, which a standing client never sees.** Crossing a
    // sea unloads chunks behind, turns others into horizon summaries and brings
    // them back, and every one of those is a neighbour changing under a mesh
    // that has already been built.
    let mut diver = Diver::join("swum", ISLANDS, VIEW);
    diver.settle();
    for place in ["go 40 8", "go 80 60", "go 20 60", "go 8 8"] {
        diver.say(place);
        diver.settle();
        diver.assert_seamless();
    }
    diver.stop();
}
