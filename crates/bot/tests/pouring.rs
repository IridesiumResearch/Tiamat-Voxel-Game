// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Pouring and scooping with the real `game/core_milk`, through the mod API.
//!
//! Separate from `fluid_multiplayer`, which pours with a synthetic mod on
//! purpose: that file is about the wire, and this one is about what the shipped
//! reference mod actually does when a player right-clicks. Charter rule 1 puts
//! the whole of that behaviour in `game/`, so it is testable only from outside.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-pouring").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn start(name: &str) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(name),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(repo().join("game")),
        enabled_mods: bot::fixture::enabled_mods_for(&repo().join("game"))
            .expect("the reference mods' manifests"),
        seed: Some(7),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

async fn join(server: &ServerHandle, name: &str) -> Bot {
    let mut bot = Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect");
    bot.join(name).await.expect("join");
    bot
}

/// The milk block, as the server numbered it this session (charter rule 8).
async fn milk_id(bot: &Bot) -> u16 {
    bot.material_table()
        .expect("the server should have sent a material table")
        .into_iter()
        .find(|entry| entry.name.ends_with(":milk"))
        .map(|entry| entry.id)
        .expect("the reference mods should register milk")
}

/// Drives the connection until a condition holds, or gives up.
async fn until(bot: &mut Bot, timeout: Duration, done: impl Fn(&Bot) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if done(bot) {
            return true;
        }
        let _ = tokio::time::timeout(Duration::from_millis(50), bot.recv()).await;
    }
    done(bot)
}

/// Gives a bot milk to pour, the way every other test acquires material:
/// the world is seeded with blocks and the bot digs them out.
///
/// There is no "grant" in the engine and there should not be — a client that
/// could conjure material would be deciding something the server owns.
async fn stock_up(server: &ServerHandle, bot: &mut Bot, milk: u16, blocks: i32) {
    // Away from where the pouring happens, so a seeded block is never mistaken
    // for a poured one, and along z so the bot can stand beside each in turn.
    for i in 0..blocks {
        let at = BlockPos::new(8, 4, i);
        assert!(
            server.seed_block(at, milk),
            "the world should accept a seeded milk block"
        );
    }
    for i in 0..blocks {
        let at = BlockPos::new(8, 4, i);
        // Within reach, which the server bounds.
        bot.move_to(8.0, 0.0, i as f32 + 2.0)
            .await
            .expect("walk to the seeded block");
        bot.dig_block(at).await.expect("dig the seeded milk");
    }
    assert!(
        bot.units_of(milk) >= tiamat_core::UNITS_PER_BLOCK,
        "digging seeded milk credited none of it, so there is nothing to pour"
    );
}

/// Carves a known basin: a solid floor with air above it.
///
/// The world is generated, so the block a test names may be inside a hill or
/// over a hole — and milk that cannot spread measures the terrain rather than
/// the mod. Seeding both layers makes the fixture the test's own.
///
/// Air is material 0 (charter rule 8), which is how the roof comes off.
fn carve_basin(
    server: &ServerHandle,
    floor: u16,
    y: i32,
    xs: std::ops::RangeInclusive<i32>,
    z: i32,
) {
    for x in xs {
        for dz in -1..=1 {
            assert!(
                server.seed_block(BlockPos::new(x, y - 1, z + dz), floor),
                "the world should accept a seeded floor"
            );
            // Two blocks of headroom, so nothing overhead drips into the basin.
            for dy in 0..=1 {
                assert!(
                    server.seed_block(BlockPos::new(x, y + dy, z + dz), 0),
                    "the world should accept seeded air"
                );
            }
        }
    }
}

/// A material the world can stand on, as the server numbered it this session.
async fn floor_id(bot: &Bot) -> u16 {
    bot.material_table()
        .expect("the server should have sent a material table")
        .into_iter()
        // `core_blocks:white` is the reference set's plain solid block; there
        // is no "stone" in `game/`, which is the point of the reference mods
        // being fixtures rather than a game.
        .find(|entry| entry.name.ends_with(":white"))
        .map(|entry| entry.id)
        .expect("the reference mods should register a plain solid block")
}

/// Pours at a block, the way a right-click does.
///
/// `Bot::place` is the wrong primitive here and the reason is the whole point
/// of this file: `core_milk` CANCELS the terrain write, because leaving the
/// block behind would seal the milk inside solid stone (Sub-Node Contract §4).
/// So no block ever appears and `place` waits out its patience for one. This
/// sends the click and lets the assertions watch the fluid layer instead.
async fn pour_at(bot: &mut Bot, pos: BlockPos, milk: u16) {
    let centre = tiamat_core::SubNodePos::new(pos.x * 3 + 1, pos.y * 3 + 1, pos.z * 3 + 1);
    bot.place_from_inventory(centre, milk)
        .await
        .expect("the pour should reach the server");
}

/// Walls the four sides of a block so conserved milk stays in it.
///
/// **Sub-Node Contract §4 is why this exists.** Milk is conserved and levels
/// itself, so twenty-seven cells poured onto an open floor are gone from the
/// block you poured into within a couple of fluid ticks — spread over its
/// neighbours, which is correct and useless to a test about one block.
fn wall_in(server: &ServerHandle, floor: u16, at: BlockPos) {
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        assert!(
            server.seed_block(BlockPos::new(at.x + dx, at.y, at.z + dz), floor),
            "the world should accept a seeded wall"
        );
    }
}

#[test]
fn a_pour_and_a_scoop_give_back_exactly_what_was_carried() {
    // **The conservation round trip, through the mod API and nothing else.**
    // Charter rule 15 wants conservation asserted on simulation invariants; this
    // is the same invariant seen from where a player stands, which is the only
    // place it can go wrong in a way somebody notices.
    //
    // Under the old model this test could not have been written: a bucket
    // created a source out of nothing and scooping destroyed one, so units in
    // and units out had no relationship at all.
    let server = start("pour-and-scoop");
    block_on(async {
        let mut bot = join(&server, "Pourer").await;
        let milk = milk_id(&bot).await;
        // **Two blocks' worth, and the second one is not slack.** `core_milk`
        // pours the material a player is HOLDING, so somebody who pours their
        // last drop has nothing left to click with and cannot scoop it back —
        // see the note in `game/core_milk/init.lua`. A test that carried one
        // bucket would be unable to exercise the second half of its own round
        // trip.
        stock_up(&server, &mut bot, milk, 2).await;
        let floor = floor_id(&bot).await;
        let ground = BlockPos::new(2, 4, 2);
        carve_basin(&server, floor, ground.y, 1..=4, ground.z);
        wall_in(&server, floor, ground);
        bot.sleep_ticks(4).await;

        let carried = bot.units_of(milk);
        assert_eq!(
            carried,
            2 * tiamat_core::UNITS_PER_BLOCK,
            "the fixture should start with exactly two blocks' worth"
        );

        bot.move_to(2.0, 0.0, 4.0).await.expect("walk to the pour");
        pour_at(&mut bot, ground, milk).await;

        assert!(
            until(&mut bot, Duration::from_secs(15), |bot| {
                bot.fluid_at(ground).volume() == tiamat_core::UNITS_PER_BLOCK
            })
            .await,
            "a whole bucket poured into a walled block should hold all of it; it holds {}",
            bot.fluid_at(ground).volume()
        );
        // **Waited for, not asserted straight away.** The fluid layer and the
        // inventory are two messages, and the pour arriving says nothing about
        // whether the debit has. Asserting here read the units the client had
        // before it was told.
        assert!(
            until(&mut bot, Duration::from_secs(15), |bot| bot.units_of(milk)
                == carried - tiamat_core::UNITS_PER_BLOCK)
            .await,
            "the pour was not charged a bucket: {} units left of {carried}",
            bot.units_of(milk)
        );

        // And back out again.
        pour_at(&mut bot, ground, milk).await;
        assert!(
            until(&mut bot, Duration::from_secs(15), |bot| bot
                .fluid_at(ground)
                .is_empty())
            .await,
            "clicking a block that already holds milk should scoop it"
        );
        assert!(
            until(&mut bot, Duration::from_secs(15), |bot| bot.units_of(milk)
                == carried)
            .await,
            "what came back out is not what went in: {} units against {carried} — milk was \
             created or destroyed by a round trip that has no sink in it",
            bot.units_of(milk)
        );

        bot.disconnect().await;
    });
    server.stop();
}

/// The material a mod registered, as the server numbered it this session.
fn material_named(bot: &Bot, suffix: &str) -> u16 {
    bot.material_table()
        .expect("the server should have sent a material table")
        .into_iter()
        .find(|entry| entry.name.ends_with(suffix))
        .map(|entry| entry.id)
        .unwrap_or_else(|| panic!("the reference mods should register `{suffix}`"))
}

/// Whether the bot has been told `pos` became `material`.
fn became(bot: &Bot, pos: BlockPos, material: u16) -> bool {
    bot.received().iter().any(|message| {
        matches!(
            message,
            tiamat_core::proto::ServerMessage::BlockDelta {
                edit: tiamat_core::proto::Edit::Block { pos: got, material: got_material },
                ..
            } if *got == pos && *got_material == material
        )
    })
}

/// Walls a two-block trough, so one bucket settles as two half-full blocks.
fn trough(server: &ServerHandle, floor: u16, left: BlockPos) {
    let right = BlockPos::new(left.x + 1, left.y, left.z);
    for at in [left, right] {
        for dz in [-1, 1] {
            assert!(
                server.seed_block(BlockPos::new(at.x, at.y, at.z + dz), floor),
                "the world should accept a seeded wall"
            );
        }
    }
    for x in [left.x - 1, right.x + 1] {
        assert!(
            server.seed_block(BlockPos::new(x, left.y, left.z), floor),
            "the world should accept a seeded end wall"
        );
    }
}

#[test]
fn scooping_a_shallow_puddle_gives_back_a_partial_bucket() {
    // **The decision this protects**: a bucket is a MEASUREMENT, not a switch.
    // Scooping half a puddle gives half a bucket back, and it costs no new
    // concept to say so — milk in an inventory is units of a material like
    // anything else (charter rule 5), so "half a bucket" is just fewer units.
    //
    // The rejected alternative was Minecraft's, where a bucket tops itself up
    // out of neighbouring blocks until it is full. That makes scooping one
    // block drain water the player never pointed at.
    let server = start("partial-bucket");
    block_on(async {
        let mut bot = join(&server, "Pourer").await;
        let milk = milk_id(&bot).await;
        stock_up(&server, &mut bot, milk, 2).await;
        let floor = floor_id(&bot).await;
        let ground = BlockPos::new(2, 4, 2);
        carve_basin(&server, floor, ground.y, 1..=4, ground.z);
        // **A sealed trough of exactly two blocks.** One bucket levels itself
        // across them and then STOPS, which is what makes the amount readable:
        // an open puddle is still moving when the test samples it, and the
        // first version of this test scooped a block that had lost a cell
        // between the reading and the click.
        trough(&server, floor, ground);
        bot.sleep_ticks(4).await;

        bot.move_to(2.0, 0.0, 4.0).await.expect("walk to the pour");
        pour_at(&mut bot, ground, milk).await;

        // Levelled and settled: half a bucket each, give or take the odd cell
        // that cannot be split.
        let neighbour = BlockPos::new(ground.x + 1, ground.y, ground.z);
        assert!(
            until(&mut bot, Duration::from_secs(20), |bot| {
                let here = bot.fluid_at(ground).volume();
                let there = bot.fluid_at(neighbour).volume();
                here + there == tiamat_core::UNITS_PER_BLOCK && here.abs_diff(there) <= 1
            })
            .await,
            "the bucket never levelled across the trough: {} and {}",
            bot.fluid_at(ground).volume(),
            bot.fluid_at(neighbour).volume()
        );
        let there = bot.fluid_at(ground).volume();
        assert!(
            there > 0 && there < tiamat_core::UNITS_PER_BLOCK,
            "the block holds {there} cells, which is not a partial bucket"
        );

        // **Waited for before it is read, not merely read.** The milk arriving
        // in the trough says nothing about whether the DEBIT for it has
        // reached this client — they are two messages — so sampling here
        // caught the inventory as it was before the first pour was charged,
        // and the total expected below was then a bucket too high. Red on the
        // slowest CI runner and nowhere else, which is what that always looks
        // like.
        let carried = 2 * tiamat_core::UNITS_PER_BLOCK;
        assert!(
            until(&mut bot, Duration::from_secs(20), |bot| {
                bot.units_of(milk) == carried - tiamat_core::UNITS_PER_BLOCK
            })
            .await,
            "the first pour was never charged: {} units of {carried}",
            bot.units_of(milk)
        );
        let before = bot.units_of(milk);

        pour_at(&mut bot, ground, milk).await;

        assert!(
            until(&mut bot, Duration::from_secs(20), |bot| bot.units_of(milk)
                == before + there)
            .await,
            "a scoop of {there} cells left {} units rather than {}: a partial bucket was \
             either rounded up to a whole one or refused",
            bot.units_of(milk),
            before + there
        );

        bot.disconnect().await;
    });
    server.stop();
}

#[test]
fn ground_that_drinks_gets_darker_and_the_milk_it_took_is_accounted_for() {
    // **Sub-Node Contract §4.3, end to end through the mod API.** The engine's
    // whole part is "this block takes `rate` cells and becomes `becomes`";
    // `core_milk` decides that ground drinks nine cells at a time and turns
    // into damp ground, and it registers three materials rather than asking
    // the engine for saturation state.
    //
    // Saturation as MATERIALS is what makes this testable from outside at all:
    // a state bit would be invisible to a client, and the proof that it worked
    // would have to be a server-side assertion about a number nobody can see.
    let server = start("saturation");
    block_on(async {
        let mut bot = join(&server, "Pourer").await;
        let milk = milk_id(&bot).await;
        let ground = material_named(&bot, "core:ground");
        let damp = material_named(&bot, "core:damp");
        stock_up(&server, &mut bot, milk, 2).await;

        let at = BlockPos::new(2, 4, 2);
        let floor = floor_id(&bot).await;
        carve_basin(&server, floor, at.y, 1..=4, at.z);
        // Thirsty ground under the pour, walled so the milk cannot simply run
        // away instead of soaking in.
        assert!(
            server.seed_block(BlockPos::new(at.x, at.y - 1, at.z), ground),
            "the world should accept seeded ground"
        );
        wall_in(&server, floor, at);
        bot.sleep_ticks(4).await;

        bot.move_to(2.0, 0.0, 4.0).await.expect("walk to the pour");
        pour_at(&mut bot, at, milk).await;

        assert!(
            until(&mut bot, Duration::from_secs(20), |bot| became(
                bot,
                BlockPos::new(at.x, at.y - 1, at.z),
                damp
            ))
            .await,
            "the ground never turned damp, so nothing absorbed — or it absorbed and the swap \
             never reached a client, which looks identical from here and is why this waits for \
             the BLOCK rather than for the milk to go"
        );

        // And the milk it took is gone from the world rather than still
        // standing on top of ground that has drunk it.
        assert!(
            until(&mut bot, Duration::from_secs(20), |bot| {
                bot.fluid_at(at).volume() < tiamat_core::UNITS_PER_BLOCK
            })
            .await,
            "the ground turned damp without the puddle losing anything, so absorption \
             created the saturation out of nothing"
        );

        bot.disconnect().await;
    });
    server.stop();
}

#[test]
fn a_puddle_pooled_out_on_open_ground_soaks_away_and_leaves_the_ground_wet() {
    // **Reported from the window**: "the water needs to disappear when fully
    // pooled out, leaving behind a random [number of] saturated blocks
    // underneath it."
    //
    // Both halves matter and they pull opposite ways. The milk has to GO — a
    // bucket poured on open ground should not leave a permanent film — and the
    // ground has to KEEP it, or the milk was destroyed rather than absorbed.
    //
    // What makes it terminate is that `core:soaked` does not absorb: each block
    // of ground takes 27 cells, one block's worth, and no more. A puddle
    // thinned out over a wide area therefore soaks away completely, while a
    // lake deep enough to swim in wets the ground under it and then stays.
    let server = start("pooled-out");
    block_on(async {
        let mut bot = join(&server, "Pourer").await;
        let milk = milk_id(&bot).await;
        stock_up(&server, &mut bot, milk, 1).await;

        // The generated world is a layer of `core:ground` at y = -1 over
        // `core:white`, so y = 0 is the first air block — `fill_below_heightmap`
        // fills strictly BELOW the surface it is given. Nothing is seeded here
        // on purpose: this is what a player standing anywhere and emptying a
        // bucket actually gets.
        let at = BlockPos::new(2, 0, 2);
        bot.move_to(2.0, 0.0, 4.0).await.expect("walk to the pour");
        pour_at(&mut bot, at, milk).await;

        assert!(
            until(&mut bot, Duration::from_secs(30), |bot| {
                bot.fluid_at(at).volume() > 0
            })
            .await,
            "the pour never landed, so there is nothing to watch soak away"
        );

        // And then it is gone — from the block it was poured into and from
        // every block it spread to.
        let footprint: Vec<BlockPos> = (-4..=4)
            .flat_map(|dx| (-4..=4).map(move |dz| BlockPos::new(at.x + dx, at.y, at.z + dz)))
            .collect();
        assert!(
            until(&mut bot, Duration::from_secs(60), |bot| {
                footprint.iter().all(|pos| bot.fluid_at(*pos).is_empty())
            })
            .await,
            "milk is still standing on open ground: {:?}",
            footprint
                .iter()
                .map(|pos| bot.fluid_at(*pos).volume())
                .filter(|volume| *volume > 0)
                .collect::<Vec<_>>()
        );

        // **And the ground took it rather than the world losing it.** Without
        // this the test passes for a solver that quietly deleted the puddle.
        let damp = material_named(&bot, "core:damp");
        let soaked = material_named(&bot, "core:soaked");
        assert!(
            footprint.iter().any(
                |pos| became(&bot, BlockPos::new(pos.x, pos.y - 1, pos.z), damp)
                    || became(&bot, BlockPos::new(pos.x, pos.y - 1, pos.z), soaked)
            ),
            "the milk vanished without wetting anything under it"
        );

        bot.disconnect().await;
    });
    server.stop();
}

#[test]
fn a_spawned_lake_wets_its_bed_and_then_stays() {
    // **The other half of "water disappears when fully pooled out."** A thin
    // puddle has to go; a lake must not. What separates them is that
    // `core:soaked` stops absorbing — every block of bed takes one block's
    // worth of milk and no more — so a body deep enough to matter wets what is
    // under it and then holds.
    //
    // Also the test for the `lake` chat command itself, which exists because
    // building a body of milk a bucket at a time is more carrying than anybody
    // wants to do to look at a shoreline.
    let server = start("spawned-lake");
    block_on(async {
        let mut bot = join(&server, "Pourer").await;

        bot.chat("lake").await.expect("ask for a lake");

        // Somewhere in the basin the mod digs: a few blocks along, at or just
        // below the surface. Found rather than named, because where the lake
        // lands depends on where the player is standing.
        let basin: Vec<BlockPos> = (-12..=12)
            .flat_map(|dx| {
                (-12..=12).flat_map(move |dz| (-3..=0).map(move |dy| BlockPos::new(dx, dy, dz)))
            })
            .collect();
        let held = |bot: &Bot| -> u32 { basin.iter().map(|pos| bot.fluid_at(*pos).volume()).sum() };

        assert!(
            until(&mut bot, Duration::from_secs(30), |bot| held(bot) > 27 * 8).await,
            "the `lake` command produced no lake worth the name: {} cells",
            held(&bot)
        );

        // **Wait for the lake to stop GROWING before measuring it.** The
        // threshold above is crossed while the `lake` command is still laying
        // water down, so `full` used to be a reading taken mid-fill — and the
        // soak assertion below then compared a part-poured lake against a
        // finished one and called the difference growth. Windows CI caught it
        // exactly that way: "301 cells before, 605 after".
        let mut full = held(&bot);
        for _ in 0..30 {
            bot.sleep_ticks(10).await;
            let now = held(&bot);
            if now <= full {
                full = now;
                break;
            }
            full = now;
        }
        for _ in 0..12 {
            bot.sleep_ticks(20).await;
        }
        let settled = held(&bot);

        assert!(
            settled > 0,
            "the lake soaked away entirely, so nothing deep can ever stand"
        );
        // **The bed drank, evidenced by the bed and not by arithmetic.** This
        // used to be `settled < full`, which is only true if some of the
        // soaking happens after the reading — and a lake that finishes drinking
        // while it is still being laid down would fail it while behaving
        // perfectly. What the claim actually is is that the ground under the
        // water got wet, which the ground itself says.
        let damp = material_named(&bot, "core:damp");
        let soaked = material_named(&bot, "core:soaked");
        assert!(
            basin
                .iter()
                .filter(|pos| !bot.fluid_at(**pos).is_empty())
                .any(|pos| {
                    let under = BlockPos::new(pos.x, pos.y - 1, pos.z);
                    became(&bot, under, damp) || became(&bot, under, soaked)
                }),
            "nothing under the lake ever became damp or soaked, so the bed never drank: \
             {full} cells before, {settled} after"
        );

        // And it has genuinely stopped, rather than being partway through
        // draining when the test looked.
        for _ in 0..6 {
            bot.sleep_ticks(20).await;
        }
        assert_eq!(
            held(&bot),
            settled,
            "the lake is still shrinking, so it is a slow puddle rather than a lake"
        );

        bot.disconnect().await;
    });
    server.stop();
}

/// A mod whose generator lays a river with no banks at all.
///
/// Flat ground up to `y = 0`, and a three-wide ribbon of water standing on it
/// from `y = 0` to `y = 2` — held in by nothing whatsoever. `fill_fluid_terraced`
/// says as much: "whatever stands at its edge must hold the fluid in", and here
/// deliberately nothing does. It is the worst river a generator could write, so
/// Sub-Node Contract §4.5 either keeps it exactly where it was put or the whole
/// rule is doing nothing.
fn write_leaky_river(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("leaky");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"leaky\"\nname = \"Leaky\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local stone = game.register_block{ id = \"stone\" }\n\
         local water_block = game.register_block{ id = \"water\" }\n\
         game.register_fluid{ id = \"water\", material = \"leaky:water\" }\n\
         local surface = game.density{ op = \"const\", value = 3 }\n\
         local channel = game.density{\n\
         \x20   op = \"sub\",\n\
         \x20   a = { op = \"const\", value = 2 },\n\
         \x20   b = { op = \"abs\", a = { op = \"z\" } },\n\
         }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), stone)\n\
         \x20   buf:fill_fluid_terraced{\n\
         \x20       level = surface,\n\
         \x20       within = channel,\n\
         \x20       fluid = \"leaky:water\",\n\
         \x20   }\n\
         end)\n",
    )
    .expect("script");
    root
}

fn start_with(mods: PathBuf, world: PathBuf) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: world,
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(7),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

#[test]
fn a_generated_river_with_no_banks_stays_where_worldgen_put_it() {
    // **Reported from the window: "rivers and oceans are kinda falling off the
    // edges".** They were. The solver is a work queue, so nothing moves unless
    // something touches it — and reading a chunk's fluid off the disk touched
    // every block in it, which re-argued worldgen's water with the physics on
    // every load. The first of those arguments is the one this river loses.
    //
    // Sub-Node Contract §4.5: a body is not woken by loading. Asserted through
    // the real endpoint, over a restart, because the load path from the
    // database is the one that was wrong.
    let mods = write_leaky_river("leaky-mods");
    let world = scratch("leaky-world");

    // In the channel and out of it, all inside the chunk the player spawns in —
    // which is the one chunk certain to have arrived by the time anything is
    // read. The ribbon is |z| < 2, three blocks deep.
    let inside = [
        BlockPos::new(0, 0, 0),
        BlockPos::new(5, 1, 1),
        BlockPos::new(11, 2, 1),
    ];
    let outside = [BlockPos::new(3, 0, 4), BlockPos::new(9, 1, 7)];

    // A different name each time: a name is a per-server claim bound to a UUID
    // (charter rule 13), and the second session is a fresh identity.
    let check = |server: &ServerHandle, who: &str, when: &str| {
        block_on(async {
            let mut bot = join(server, who).await;
            bot.collect_chunks(9, Duration::from_secs(20))
                .await
                .expect("the neighbourhood");
            // A moment of world time, so that if the river were going to run it
            // would have started: the solver visits 512 blocks a fluid tick.
            tokio::time::sleep(Duration::from_secs(2)).await;
            for pos in inside {
                assert_eq!(
                    bot.fluid_at(pos).volume(),
                    tiamat_core::fluid::MAX_VOLUME,
                    "{when}: the river is missing at {pos:?}"
                );
            }
            for pos in outside {
                assert_eq!(
                    bot.fluid_at(pos).volume(),
                    0,
                    "{when}: the river has run out over the plain at {pos:?}"
                );
            }
            bot.disconnect().await;
        });
    };

    let server = start_with(mods.clone(), world.clone());
    check(&server, "Wader", "as generated");
    assert!(server.stop(), "clean shutdown");

    // And again from the database, which is the path that was re-arguing it.
    let server = start_with(mods, world);
    check(&server, "Paddler", "after a restart");
    assert!(server.stop());
}

/// The reference mods plus one more fluid that moves every tick.
///
/// **The case per-fluid tuning had to be tested in.** With milk alone at
/// `tick_rate = 4`, the whole pass skips three ticks in four and nothing ever
/// runs on milk's off-ticks. Register a fluid at rate one beside it and the
/// solver runs every tick, deferring milk's blocks on the ticks that are not
/// theirs — which is the situation a world with water, milk and lava is in.
fn reference_mods_plus_a_quick_fluid(name: &str) -> PathBuf {
    let mods = scratch(&format!("{name}-mods"));
    for entry in std::fs::read_dir(repo().join("game")).expect("read game/") {
        let entry = entry.expect("entry");
        let file_name = entry.file_name();
        if !entry.path().is_dir() || !file_name.to_string_lossy().starts_with("core_") {
            continue;
        }
        let target = mods.join(&file_name);
        std::fs::create_dir_all(&target).expect("mod dir");
        for file in std::fs::read_dir(entry.path()).expect("read mod") {
            let file = file.expect("entry");
            if file.path().is_file() {
                std::fs::copy(file.path(), target.join(file.file_name())).expect("copy");
            }
        }
    }
    let quick = mods.join("quick");
    std::fs::create_dir_all(&quick).expect("mod dir");
    std::fs::write(
        quick.join("mod.toml"),
        "id = \"quick\"\nname = \"Quick\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        quick.join("init.lua"),
        "game.register_block{ id = \"quick\" }\n\
         game.register_fluid{ id = \"quick\", material = \"quick:quick\", tick_rate = 1 }\n",
    )
    .expect("script");
    mods
}

#[test]
fn a_puddle_still_soaks_away_beside_a_fluid_that_runs_every_tick() {
    // Per-fluid tuning's first version stalled the last cell of a puddle when
    // the solver ran on milk's off-ticks. Milk alone never has the solver run
    // on its off-ticks; a second fluid at rate one does, every tick. This is
    // the puddle test in that world.
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("mixed-rates"),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(reference_mods_plus_a_quick_fluid("mixed-rates")),
        enabled_mods: None,
        seed: Some(7),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start");
    block_on(async {
        let mut bot = join(&server, "Pourer").await;
        let milk = milk_id(&bot).await;
        stock_up(&server, &mut bot, milk, 1).await;
        let at = BlockPos::new(2, 0, 2);
        bot.move_to(2.0, 0.0, 4.0).await.expect("walk to the pour");
        pour_at(&mut bot, at, milk).await;
        assert!(
            until(&mut bot, Duration::from_secs(30), |bot| {
                bot.fluid_at(at).volume() > 0
            })
            .await,
            "the pour never landed"
        );
        let footprint: Vec<BlockPos> = (-4..=4)
            .flat_map(|dx| (-4..=4).map(move |dz| BlockPos::new(at.x + dx, at.y, at.z + dz)))
            .collect();
        assert!(
            until(&mut bot, Duration::from_secs(60), |bot| {
                footprint.iter().all(|pos| bot.fluid_at(*pos).is_empty())
            })
            .await,
            "beside a fluid that runs every tick, milk is still standing on open ground: {:?}",
            footprint
                .iter()
                .map(|pos| bot.fluid_at(*pos).volume())
                .filter(|volume| *volume > 0)
                .collect::<Vec<_>>()
        );
        bot.disconnect().await;
    });
    server.stop();
}
