// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A world reopened under a changed mod set, over a real server.
//!
//! # The two id spaces (charter rule 8)
//!
//! A **world id** is the number the world database assigned a material name
//! when the world first saw it; a **runtime id** is the number this session's
//! registry gave it. A chunk in memory holds RUNTIME ids (`persist::codec`
//! translates on every load and save), so every table the tick consults with an
//! id read from a chunk has to be keyed in runtime ids. They are the same
//! numbers on a world opened by the mod set that made it — which is why every
//! table was built in world ids for a long time and nothing noticed.
//!
//! These tests make them differ. A world is created under two mods, the first
//! of which registers five blocks and nothing else, then reopened under the
//! second alone: every block the second mod registers is now numbered five
//! LOWER by the session than by the world. A table keyed in the wrong space is
//! then off by five, and a lookup of block *k* lands on what the mod registered
//! as block *k − 5* — so the second mod's blocks are laid out such that the one
//! five before each victim is plain, and the one five after it is innocent, to
//! show the wrong thing happening in both directions.
//!
//! What a client is told (the material table, the horizon) is in WORLD ids,
//! because that is what its atlas and its chunks are keyed by.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::ServerMessage;
use tiamat_core::{BlockPos, SubNodePos};
use tiamat_server::{ServerHandle, Settings};

/// How long to wait for something the server has to tick before it is true.
const PATIENCE: Duration = Duration::from_secs(30);

/// How many blocks the mod that loads in front registers, and so how far apart
/// the two id spaces are for everything after it.
const SHIFT: u16 = 5;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-divergent-ids").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

/// The mod that loads in front and is later removed.
fn write_first(root: &std::path::Path) {
    let dir = root.join("first");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"first\"\nname = \"First\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    let count = SHIFT;
    std::fs::write(
        dir.join("init.lua"),
        format!("for n = 1, {count} do game.register_block{{ id = \"filler_\" .. n }} end\n"),
    )
    .expect("script");
}

/// The mod under test.
///
/// **The order the blocks are registered in is the test.** Block *k* is the
/// victim of a table keyed in the wrong space when the block *k − 5* has no
/// rule of its own, and the block *k + 5* is the innocent that inherits the
/// victim's rule. The index is written beside each one.
///
/// `floor` is what the strip under the spawn is made of and `barrier` what
/// stands across it, each a local block name or `0` for nothing.
fn write_second(root: &std::path::Path, floor: &str, barrier: &str) {
    let dir = root.join("second");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"second\"\nname = \"Second\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    let script = r#"
local ground = game.register_block{ id = "ground" }                       -- 0
game.register_block{ id = "granite", hardness = 60.0 }                    -- 1
game.register_block{ id = "pad2" }                                        -- 2
game.register_block{ id = "pad3" }                                        -- 3
game.register_block{ id = "pad4" }                                        -- 4
game.register_block{ id = "pad5" }                                        -- 5
local quick = game.register_block{ id = "quick", hardness = 0.1 }         -- 6
local lamp = game.register_block{                                         -- 7
    id = "lamp", light_emit = { r = 15, g = 15, b = 15 },
}
local grass = game.register_block{ id = "grass", passable = true }        -- 8
local glass = game.register_block{ id = "glass", transparent = true }     -- 9
local ice = game.register_block{ id = "ice", friction = 0.0 }             -- 10
local ore = game.register_block{                                          -- 11
    id = "ore", hardness = 0.1, drops = { ["second:nugget"] = 27 },
}
local dud = game.register_block{ id = "dud" }                             -- 12
local wall = game.register_block{ id = "wall" }                           -- 13
local slab = game.register_block{ id = "slab" }                           -- 14
local floor_b = game.register_block{ id = "floor_b" }                     -- 15
game.register_block{ id = "nugget" }                                      -- 16
local leaf = game.register_block{                                         -- 17
    id = "leaf", cutout = true, light_falloff = 2,
}
local tuff = game.register_block{ id = "tuff", hardness = 40.0 }          -- 18
game.register_block{                                                      -- 19
    id = "sponge", absorbs = { rate = 27, becomes = "soaked" },
}
game.register_block{ id = "soaked" }                                      -- 20
game.register_block{                                                      -- 21
    id = "lava_block", light_emit = { r = 12, g = 6, b = 0 },
}
game.register_block{ id = "rain_block" }                                  -- 22
game.register_block{ id = "mark" }                                        -- 23
game.register_fluid{ id = "rain", material = "second:rain_block", tick_rate = 1 }
game.register_fluid{ id = "glow", material = "second:lava_block", tick_rate = 1 }

-- What a player digs with, and a tool that is only good at one thing.
game.register_tool{ id = "hand", brush = "block", speed_multiplier = 1.0, default = true }
game.register_tool{
    id = "pick", brush = "block", speed_multiplier = 0.001,
    speeds = { ["second:tuff"] = 1000.0 },
}

local FLOOR = @FLOOR@
local BARRIER = @BARRIER@

game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)

    if pos.x == 0 and pos.y == 0 and pos.z == 0 then
        -- The digging row, on the ground beside the spawn.
        buf:set_block(1, 0, 4, quick)
        buf:set_block(2, 0, 4, ore)
        buf:set_block(3, 0, 4, tuff)
        -- Something across the strip a body walks along from the spawn.
        for y = 0, 2 do
            for z = 0, 1 do
                buf:set_block(6, y, z, BARRIER)
            end
        end
    elseif pos.x == 0 and pos.y == -1 and pos.z == 0 then
        -- The floor the spawn stands on: the top layer of the ground.
        for x = 0, 14 do
            for z = 0, 1 do
                buf:set_block(x, 15, z, FLOOR)
            end
        end
        -- A lamp and a dud, each in a pocket of rock so nothing but the block
        -- itself can light the pocket. World y is -3.
        buf:set_block(2, 13, 2, lamp)
        buf:set_block(3, 13, 2, 0)
        buf:set_block(2, 13, 8, dud)
        buf:set_block(3, 13, 8, 0)
        -- A pocket a glowing fluid is poured into.
        buf:set_block(12, 13, 12, 0)
    elseif pos.x == 1 and pos.y == 0 and pos.z == 0 then
        -- A whole chunk roofed in glass: the sun reaches the floor of it.
        for x = 0, 15 do
            for z = 0, 15 do
                buf:set_block(x, 2, z, glass)
            end
        end
    elseif pos.x == -1 and pos.y == 0 and pos.z == 0 then
        -- The same, in a block nothing said was see-through.
        for x = 0, 15 do
            for z = 0, 15 do
                buf:set_block(x, 2, z, slab)
            end
        end
    elseif pos.x == 0 and pos.y == 0 and pos.z == -1 then
        -- The same, in leaves that dim what passes.
        for x = 0, 15 do
            for z = 0, 15 do
                buf:set_block(x, 2, z, leaf)
            end
        end
    end
end)

-- Read by a client that asks: the number the SESSION gave `ground`, which a
-- client cannot otherwise learn, so a test can check the two id spaces differ.
local armed = nil
local ticks = 0
game.register_on_chat(function(event)
    if event.text == "probe" then
        game.set_block({ x = 1, y = 20 + game.get_block_id("second:ground"), z = 1 }, "second:mark")
        return false
    elseif event.text == "pour" then
        armed = ticks
        game.set_fluid({ x = 12, y = -3, z = 12 }, { fluid = "second:glow", volume = 27 })
        -- A well: ground that drinks, walled on four sides so what is poured
        -- into it can only go down.
        game.set_block({ x = 13, y = 0, z = 6 }, "second:sponge")
        for _, d in ipairs({ { 1, 0 }, { -1, 0 }, { 0, 1 }, { 0, -1 } }) do
            game.set_block({ x = 13 + d[1], y = 1, z = 6 + d[2] }, "second:ground")
        end
        return false
    end
end)
game.register_on_tick(function()
    ticks = ticks + 1
    if armed == nil then return end
    if ticks == armed + 5 then
        game.set_fluid({ x = 13, y = 1, z = 6 }, { fluid = "second:rain", volume = 27 })
    elseif ticks == armed + 100 then
        local below = game.get_block({ x = 13, y = 0, z = 6 })
        local left = game.get_fluid({ x = 13, y = 1, z = 6 })
        if below.material == game.get_block_id("second:soaked") and left.empty then
            game.set_block({ x = 13, y = 3, z = 6 }, "second:mark")
        end
    end
end)
"#
    .replace("@FLOOR@", floor)
    .replace("@BARRIER@", barrier);
    std::fs::write(dir.join("init.lua"), script).expect("script");
}

fn mods(name: &str, with_first: bool, floor: &str, barrier: &str) -> PathBuf {
    let root = scratch(name);
    if with_first {
        write_first(&root);
    }
    write_second(&root, floor, barrier);
    root
}

fn start_at(mods: PathBuf, world: PathBuf) -> ServerHandle {
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
        seed: Some(11),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

/// A world made under both mods and reopened under the second alone.
///
/// Opening the world is what writes its id table, so nobody joins the first
/// run. The second run is the one that is returned.
fn reopened(name: &str, floor: &str, barrier: &str) -> ServerHandle {
    let world = scratch(&format!("{name}-world"));
    let first = start_at(
        mods(&format!("{name}-1"), true, floor, barrier),
        world.clone(),
    );
    assert!(first.stop(), "the world did not flush cleanly");
    start_at(mods(&format!("{name}-2"), false, floor, barrier), world)
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

/// A material's id as a client is told it: the WORLD id.
fn wire_id(bot: &Bot, name: &str) -> u16 {
    bot.material_table()
        .expect("a material table")
        .into_iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.id)
        .unwrap_or_else(|| panic!("the mod registers {name}"))
}

/// Proves the two id spaces differ in this run, so nothing below can pass by
/// coincidence, and that the blocks sit `SHIFT` apart as the layout assumes.
async fn assert_divergent(bot: &mut Bot) {
    bot.chat("probe").await.expect("chat");
    let mark = wire_id(bot, "second:mark");
    let world_ground = wire_id(bot, "second:ground");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let runtime_ground = loop {
        if let Some(found) = (20..52).find(|y| bot.saw_block(BlockPos::new(1, *y, 1), mark)) {
            break found - 20;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the probe never reported the session's id for ground"
        );
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    };
    assert_eq!(
        i32::from(world_ground) - runtime_ground,
        i32::from(SHIFT),
        "the world id and the runtime id of `ground` should differ by the {SHIFT} \
         blocks of the mod that is gone (world {world_ground}, runtime {runtime_ground})"
    );
    // And the layout: block k is k after ground, in the world's numbering too.
    for (k, name) in [
        (1, "granite"),
        (6, "quick"),
        (7, "lamp"),
        (8, "grass"),
        (9, "glass"),
        (10, "ice"),
        (11, "ore"),
        (12, "dud"),
        (13, "wall"),
        (14, "slab"),
        (15, "floor_b"),
        (16, "nugget"),
        (17, "leaf"),
        (18, "tuff"),
    ] {
        assert_eq!(
            wire_id(bot, &format!("second:{name}")),
            world_ground + k,
            "{name} is not {k} blocks after ground"
        );
    }
}

#[test]
fn light_comes_from_the_blocks_that_glow_and_passes_the_blocks_that_pass_it() {
    // Emissions, see-through and dimming, all consulted with a material read
    // from a chunk in memory. Six readings, each a different table or a
    // different direction of the same mistake:
    //
    // - the lamp lights its pocket, and a block five places after it does not;
    // - a glass roof lets the sun through, and a block five places after glass
    //   is opaque, so a chunk roofed in it is lit from the sides alone;
    // - leaves dim what passes them rather than stopping it or passing it whole;
    // - a glowing fluid still glows, which is the same table read through the
    //   fluid registry's material.
    let server = reopened("light", "ground", "0");
    block_on(async {
        let mut bot = join(&server, "Reader").await;
        assert_divergent(&mut bot).await;
        bot.chat("pour").await.expect("chat");

        // Sub-Node Contract §8.1: light passes through a whole block of one
        // transparent material. The floor under a glass roof is in full sun.
        let under_glass = BlockPos::new(23, 1, 7);
        let glass = bot
            .expect_light(under_glass, |light| light.sun() > 0, PATIENCE)
            .await
            .expect("the chunk under the glass roof was never lit");
        assert_eq!(
            glass.sun(),
            15,
            "sunlight did not pass a glass roof: {glass:?}"
        );

        // A roof of something nobody said was see-through stops it, and what
        // reaches the floor has walked in from the edge.
        let under_slab = BlockPos::new(-9, 1, 7);
        let slab = bot
            .expect_light(under_slab, |light| light.sun() > 0, PATIENCE)
            .await
            .expect("the chunk under the slab roof was never lit");
        assert!(
            slab.sun() < 15,
            "sunlight passed a roof that is not see-through: {slab:?}"
        );

        // Contract §8.2: a canopy is permeable and shades. Between a pane of
        // glass and a roof.
        let under_leaf = BlockPos::new(7, 1, -9);
        let leaf = bot
            .expect_light(under_leaf, |light| light.sun() > 0, PATIENCE)
            .await
            .expect("the chunk under the canopy was never lit");
        assert!(
            leaf.sun() > slab.sun() && leaf.sun() < glass.sun(),
            "leaves should dim what passes them: {leaf:?} against glass {glass:?} and a roof {slab:?}"
        );

        // A lamp, in a pocket of rock.
        let lamp = bot
            .expect_light(BlockPos::new(3, -3, 2), |light| light.red() > 0, PATIENCE)
            .await
            .expect("a lamp did not light the pocket beside it");
        assert!(lamp.red() >= 10, "a lamp lit its pocket weakly: {lamp:?}");
        // Its neighbour five blocks along in the registry, in the same rock.
        let dud = bot
            .light_at(BlockPos::new(3, -3, 8))
            .expect("the same chunk carries both pockets");
        assert_eq!(
            (dud.red(), dud.green(), dud.blue()),
            (0, 0, 0),
            "a block that emits nothing lit its pocket"
        );

        // A pond of a fluid drawn as lava glows.
        let pond = bot
            .expect_light(BlockPos::new(12, -3, 12), |light| light.red() > 0, PATIENCE)
            .await;
        assert!(pond.is_ok(), "a glowing fluid did not glow: {pond:?}");
    });
    server.stop();
}

/// Aims at the middle of a block and keeps at it until it is gone, or gives up.
async fn dig_within(bot: &mut Bot, pos: BlockPos, tool: Option<&str>, patience: Duration) -> bool {
    bot.select_tool(tool).await.expect("select a tool");
    let target = SubNodePos::new(pos.x * 3 + 1, pos.y * 3 + 1, pos.z * 3 + 1);
    let deadline = tokio::time::Instant::now() + patience;
    let mut aimed = None;
    while tokio::time::Instant::now() < deadline {
        // Re-aiming at the same cell keeps its progress, so this is only there
        // to survive a message going missing.
        if aimed.is_none_or(|at: tokio::time::Instant| at.elapsed() > Duration::from_secs(1)) {
            bot.start_dig(target).await.expect("start a dig");
            aimed = Some(tokio::time::Instant::now());
        }
        if bot.block_is_empty(pos) {
            return true;
        }
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    }
    bot.block_is_empty(pos)
}

#[test]
fn a_material_breaks_by_its_own_hardness_drops_its_own_yield_and_answers_its_own_tool() {
    // Hardness, drop rules and a tool's speed on a material, all keyed by a
    // material and all consulted with the one the dig read from the chunk.
    //
    // - `quick` takes a tenth of a second by hand, and the block five places
    //   before it takes a minute: keyed in the wrong space, `quick` would take
    //   a minute;
    // - `ore` drops nuggets, not itself;
    // - `tuff` takes forty seconds by hand and a fortieth of one with the pick
    //   that is fast on it and nothing else.
    let server = reopened("dig", "ground", "0");
    block_on(async {
        let mut bot = join(&server, "Miner").await;
        assert_divergent(&mut bot).await;
        bot.settle().await.expect("settle");
        let patience = Duration::from_secs(8);

        let quick = BlockPos::new(1, 0, 4);
        assert!(
            dig_within(&mut bot, quick, Some("second:hand"), patience).await,
            "a block whose hardness is a tenth of a second was not broken in {patience:?}: \
             it is being timed as some other material"
        );

        let ore = BlockPos::new(2, 0, 4);
        assert!(
            dig_within(&mut bot, ore, Some("second:hand"), patience).await,
            "the ore was not broken in {patience:?}"
        );
        let nugget = wire_id(&bot, "second:nugget");
        let ore_id = wire_id(&bot, "second:ore");
        let deadline = tokio::time::Instant::now() + patience;
        while bot.units_of(nugget) < 27 && tokio::time::Instant::now() < deadline {
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        assert_eq!(
            bot.units_of(nugget),
            27,
            "the ore should have paid nuggets: {:?}",
            bot.inventory()
        );
        assert_eq!(
            bot.units_of(ore_id),
            0,
            "the ore paid itself, ignoring what its mod said it drops: {:?}",
            bot.inventory()
        );

        let tuff = BlockPos::new(3, 0, 4);
        assert!(
            dig_within(&mut bot, tuff, Some("second:pick"), patience).await,
            "the pick, which is fast on tuff, did not break it in {patience:?}"
        );
    });
    server.stop();
}

/// Walks east from the spawn for a while and says which block it ended in.
async fn walk_east(bot: &mut Bot) -> i32 {
    bot.settle().await.expect("settle");
    bot.walk([1.0, 0.0, 0.0], 0, 60)
        .await
        .expect("walk")
        .block()
        .x
}

#[test]
fn a_floor_that_is_slick_is_slick_and_a_floor_five_places_after_it_is_not() {
    // Contract §2: grip is read under the centre of the feet, and zero is a
    // floor that neither slows nor pushes — a body on it does not walk.
    let server = reopened("ice", "ice", "0");
    block_on(async {
        let mut bot = join(&server, "Skater").await;
        assert_divergent(&mut bot).await;
        let reached = walk_east(&mut bot).await;
        assert!(
            reached <= 3,
            "a body walked on a floor with no grip, to x {reached}"
        );
    });
    server.stop();

    let server = reopened("ice-control", "floor_b", "0");
    block_on(async {
        let mut bot = join(&server, "Walker").await;
        assert_divergent(&mut bot).await;
        let reached = walk_east(&mut bot).await;
        assert!(
            reached >= 8,
            "a body could not walk on an ordinary floor, and got only to x {reached}: \
             it is being given another material's grip"
        );
    });
    server.stop();
}

#[test]
fn a_body_walks_through_what_is_passable_and_not_through_what_is_five_places_after_it() {
    // Contract §2: `passable` is collision only. The barrier stands at x = 6.
    let server = reopened("pass", "ground", "grass");
    block_on(async {
        let mut bot = join(&server, "Walker").await;
        assert_divergent(&mut bot).await;
        let reached = walk_east(&mut bot).await;
        assert!(
            reached > 7,
            "a body stopped at a passable wall, at x {reached}"
        );
    });
    server.stop();

    let server = reopened("pass-control", "ground", "wall");
    block_on(async {
        let mut bot = join(&server, "Walker").await;
        assert_divergent(&mut bot).await;
        let reached = walk_east(&mut bot).await;
        assert!(
            reached < 6,
            "a body walked through a wall that is not passable, to x {reached}"
        );
    });
    server.stop();
}

#[test]
fn ground_that_drinks_drinks_and_becomes_what_its_mod_named() {
    // Fluid absorbency, and the successor it resolves. Read back by the mod
    // itself: it marks the well only if the block below the water is now
    // `soaked` — the runtime id `game.get_block_id` hands out — and the water
    // is gone.
    let server = reopened("well", "ground", "0");
    block_on(async {
        let mut bot = join(&server, "Gardener").await;
        assert_divergent(&mut bot).await;
        bot.chat("pour").await.expect("chat");
        let mark = wire_id(&bot, "second:mark");
        bot.expect_block(BlockPos::new(13, 3, 6), mark, PATIENCE)
            .await
            .expect("the ground did not drink the water and turn into what its mod named");
    });
    server.stop();
}

#[test]
fn the_horizon_names_a_material_by_the_number_the_clients_atlas_does() {
    // A summary is built from chunks in memory, which hold runtime ids, and read
    // by a client whose atlas and chunks are keyed by world ids.
    let server = reopened("horizon", "ground", "0");
    block_on(async {
        let mut bot = join(&server, "Watcher").await;
        assert_divergent(&mut bot).await;
        let ground = wire_id(&bot, "second:ground");

        let deadline = tokio::time::Instant::now() + PATIENCE;
        let mut checked = 0usize;
        let mut wrong = Vec::new();
        while tokio::time::Instant::now() < deadline && checked < 8 {
            let Ok(Ok(message)) =
                tokio::time::timeout(Duration::from_millis(500), bot.recv()).await
            else {
                continue;
            };
            let ServerMessage::ChunkSummary { pos, blob } = message else {
                continue;
            };
            // Far from the fixtures, so all there is is air and ground.
            if pos.x.abs() <= 1 && pos.z.abs() <= 1 {
                continue;
            }
            let summary = tiamat_core::lod::codec::decode(&blob).expect("a summary that decodes");
            for cell in summary.cells() {
                if !cell.is_air() && cell.get() != ground {
                    wrong.push((pos, cell.get()));
                }
            }
            checked += 1;
        }
        assert!(checked > 0, "no horizon arrived");
        assert!(
            wrong.is_empty(),
            "the horizon named ground {ground} to the client and something else in \
             {} cells, first {:?}",
            wrong.len(),
            wrong.first()
        );
    });
    server.stop();
}
