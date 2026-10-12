// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.plans`, over a real server — and `game.stamp_schematic`, which rides
//! the same stamp queue (weather ask W35).
//!
//! # Why this test exists at all
//!
//! `set_plan_access` is the seventh `set_*_access` on the VM, and the sixth one
//! taught this codebase the lesson: a seam installed only by its own tests is
//! not installed. `game.set_block` had a queue type, a setter, stubs and unit
//! tests, and nothing ever called the setter on a running server — so it did
//! nothing, silently, for three tasks, because an empty slot is exactly what a
//! mod gets during worldgen and is not an error there. See `mod_edits.rs`.
//!
//! Plans have MORE places to be dead than that did: a capture goes through the
//! lease, a save goes through the world's database, and a stamp goes through a
//! queue the tick has to remember to pump. Each of those is a slot that answers
//! "nothing happened" when it is not wired, so each is exercised here through a
//! server nobody stubbed.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

/// How long to wait for something the server has to tick before it is true.
///
/// Thirty seconds for the reason `mod_edits.rs` gives: nothing here is slower
/// for it, because every wait ends on the condition rather than on the clock,
/// and ten seconds is a bet on how fast the machine is under a full parallel
/// workspace run.
const PATIENCE: Duration = Duration::from_secs(30);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-plans").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod that builds two blocks, captures them, and stamps the plan elsewhere.
///
/// Every step reports itself by placing a marker block, which is how a mod's
/// answer reaches a test: there is no "what did your capture say" message on
/// the wire and there should not be one.
///
/// **Each marker is latched on its own.** Chunks arrive over several ticks, so
/// a mod that did all of this inside one `if` would lose whichever step's
/// terrain was not loaded on the tick the first one succeeded — the mistake
/// `mod_edits.rs` records having made.
fn write_builder(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("builder");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"builder\"\nname = \"Builder\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
local brick = game.register_block{ id = "brick" }
game.register_block{ id = "saw_capture" }
game.register_block{ id = "saw_list" }
game.register_block{ id = "saw_forget" }

game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local seen = {}
local function once(marker, at)
    if seen[marker] then
        return false
    end
    seen[marker] = true
    game.set_block(at, marker)
    return true
end

game.register_on_tick(function()
    -- The build to capture: two bricks in the air, where nothing generated
    -- could be mistaken for them.
    game.set_block({ x = 2, y = 9, z = 2 }, "builder:brick")
    game.set_block({ x = 3, y = 9, z = 2 }, "builder:brick")

    local placed = game.get_block({ x = 3, y = 9, z = 2 })
    if placed == nil or placed.material ~= brick then
        return
    end

    if not seen["captured"] then
        local made = game.plans.capture("hut", { x = 2, y = 9, z = 2 }, { x = 3, y = 9, z = 2 })
        if made == nil then
            return
        end
        seen["captured"] = true
        -- Two whole blocks of brick: 54 units, charter rule 5.
        if made.blocks == 2 and made.materials["builder:brick"] == 54
            and made.size.x == 2 and made.size.y == 1 and made.size.z == 1 then
            once("builder:saw_capture", { x = 5, y = 9, z = 2 })
        end
        local names = game.plans.list()
        if #names == 1 and names[1] == "hut" then
            once("builder:saw_list", { x = 6, y = 9, z = 2 })
        end
        -- And put it somewhere else, which is the whole point of having one.
        game.plans.stamp("hut", { x = 20, y = 9, z = 2 })
    end
end)

-- Forgetting is a separate hook so it cannot race the stamp above: a plan the
-- mod deleted before the tick pumped it would still stamp, and a test that
-- happened to pass either way would say nothing.
game.register_on_chat(function(event)
    if event.text == "forget" then
        if game.plans.forget("hut") and #game.plans.list() == 0
            and game.plans.info("hut") == nil then
            game.set_block({ x = 7, y = 9, z = 2 }, "builder:saw_forget")
        end
        return false
    end
end)
"#,
    )
    .expect("script");
    root
}

/// A mod that stamps a plan it did not capture, and never builds anything.
///
/// The second half of the persistence test: it shares the `builder` id, so the
/// plans the first run saved are its own, and it registers the same blocks in
/// the same order so the world's material ids still mean what they meant.
fn write_stamper(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("builder");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"builder\"\nname = \"Builder\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "brick" }
game.register_block{ id = "saw_capture" }
game.register_block{ id = "saw_list" }
game.register_block{ id = "saw_forget" }

game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local done = false
game.register_on_tick(function()
    if done then
        return
    end
    -- A fresh VM, so this local is per RUN: the plan is the only thing that
    -- crossed the restart.
    if game.plans.stamp("hut", { x = 30, y = 9, z = 2 }) then
        done = true
    end
end)
"#,
    )
    .expect("script");
    root
}

fn start(mods: PathBuf, world: PathBuf) -> ServerHandle {
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

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

async fn watcher(server: &ServerHandle, name: &str) -> Bot {
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

fn material(bot: &Bot, name: &str) -> u16 {
    bot.material_table()
        .expect("the server sends a material table on join")
        .into_iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.id)
        .unwrap_or_else(|| panic!("the mod registers {name}"))
}

#[test]
fn a_mod_captures_a_build_and_stamps_it_somewhere_else() {
    let world = scratch("stamp-world");
    let server = start(write_builder("stamp"), world);
    block_on(async {
        let mut bot = watcher(&server, "Bystander").await;
        let brick = material(&bot, "builder:brick");

        // What the capture SAID, first: a mod that is handed a plan whose size
        // or tally is wrong cannot tell until it builds the thing.
        for (at, marker) in [
            (BlockPos::new(5, 9, 2), "builder:saw_capture"),
            (BlockPos::new(6, 9, 2), "builder:saw_list"),
        ] {
            let id = material(&bot, marker);
            bot.expect_block(at, id, PATIENCE)
                .await
                .unwrap_or_else(|err| panic!("the mod never reported {marker}: {err}"));
        }

        // And what it BUILT: both blocks of the plan, at the offset it was
        // stamped to, which is the whole path — capture, save, load, queue,
        // apply, broadcast.
        for at in [BlockPos::new(20, 9, 2), BlockPos::new(21, 9, 2)] {
            bot.expect_block(at, brick, PATIENCE)
                .await
                .unwrap_or_else(|err| {
                    panic!("a stamped plan did not reach the world at {at:?}: {err}")
                });
        }

        // Then take it away again, on a hook of its own so it cannot race the
        // stamp above.
        bot.chat("forget").await.expect("chat");
        let forgotten = material(&bot, "builder:saw_forget");
        bot.expect_block(BlockPos::new(7, 9, 2), forgotten, PATIENCE)
            .await
            .expect("forgetting a plan should remove it from the store");
    });
}

#[test]
fn a_plan_outlives_the_world_that_captured_it() {
    // A plan is kept in the world's own database rather than in memory, which
    // is a claim only a restart can check. The second run's mod cannot capture
    // anything — it has no build to capture from and never calls capture — so
    // the only way the blocks appear is the saved plan being read back.
    let world = scratch("restart-world");
    let first = start(write_builder("restart-first"), world.clone());
    block_on(async {
        let mut bot = watcher(&first, "Builder").await;
        let brick = material(&bot, "builder:brick");
        bot.expect_block(BlockPos::new(20, 9, 2), brick, PATIENCE)
            .await
            .expect("the first run should capture and stamp");
    });
    assert!(first.stop(), "the world did not flush cleanly");

    let second = start(write_stamper("restart-second"), world);
    block_on(async {
        let mut bot = watcher(&second, "Visitor").await;
        let brick = material(&bot, "builder:brick");
        for at in [BlockPos::new(30, 9, 2), BlockPos::new(31, 9, 2)] {
            bot.expect_block(at, brick, PATIENCE)
                .await
                .unwrap_or_else(|err| {
                    panic!("a plan saved by an earlier run did not stamp at {at:?}: {err}")
                });
        }
    });
}

// ---------------------------------------------------------------------------
// `game.stamp_schematic` (weather ask W35): a schematic written at run time.
// ---------------------------------------------------------------------------

/// Where the `wrecker` fixture roots its wreck: beside the player, who
/// spawns at (0, 1, 0), on the ground (the flat ground's top block is
/// y = -1).
///
/// **Placed so every turn stays in chunks the player holds whole.** At the
/// smallest view a client holds the chunks beside its own in full and the
/// diagonal ones as summaries, which carry no cells to check; the wreck
/// reaches twelve blocks from its root along whichever way it faces, and from
/// here that is always the root's chunk and a neighbour along one axis.
const ROOT: (i32, i32, i32) = (8, 0, 8);

/// Where the fixture says the stamp was accepted.
const ACCEPTED: (i32, i32, i32) = (-6, 0, 1);

/// The cells at x = 0 of a block: a slab on its `-x` face, which no turn
/// leaves where it was.
fn west() -> u32 {
    (0..27)
        .filter(|i| i % 3 == 0)
        .fold(0, |mask, i| mask | (1 << i))
}

/// The cells at y = 2 of a block: its top layer.
fn top() -> u32 {
    (0..27)
        .filter(|i| (i / 3) % 3 == 2)
        .fold(0, |mask, i| mask | (1 << i))
}

/// The wreck, block for block, in the order the fixture's Lua builds it:
/// `(dx, dy, dz, material, mask)`, with the material's short name and `""`
/// for air. Written twice — here and in [`write_wrecker`] — on purpose: the
/// test's copy is what the engine's answer is checked against.
fn wreck() -> Vec<(i32, i32, i32, &'static str, u32)> {
    let full = tiamat_core::block::OCCUPANCY_FULL;
    let mut list = Vec::new();
    // A hull twelve long, four high and seven wide, shell only: its floor on
    // the ground, a rail of slabs round the top.
    for dx in -3..=8_i32 {
        for dy in 0..=3 {
            for dz in -1..=5 {
                if dx == -3 || dx == 8 || dy == 0 || dy == 3 || dz == -1 || dz == 5 {
                    let material = if (dx + 2 * dz + dy).rem_euclid(5) == 0 {
                        "char"
                    } else {
                        "plank"
                    };
                    list.push((dx, dy, dz, material, if dy == 3 { west() } else { full }));
                }
            }
        }
    }
    // Merged INTO the ground: the ground's other cells stay.
    for dx in 10..=12 {
        list.push((dx, -1, 0, "plank", top()));
    }
    // Carved: a hole, and a scrape off the top of one block.
    list.push((10, -1, 2, "", full));
    list.push((11, -1, 2, "", top()));
    // A block, and a later layer over it.
    list.push((12, 0, 4, "plank", full));
    list.push((12, 0, 4, "char", west()));
    // Last, so its arrival says the rest is down: the stamp goes in order.
    list.push((0, 6, 0, "flag", full));
    list
}

/// A mod that builds [`wreck`] at load and stamps it at `turn` when a player
/// says "stamp".
///
/// On a chat line rather than at the first tick, so the player is there to be
/// told: a stamp that landed before anybody joined would arrive as chunk data
/// and never as the edits this test watches for.
fn write_wrecker(name: &str, turn: u32) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("wrecker");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"wrecker\"\nname = \"Wrecker\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    let script = r#"
local ground = game.register_block{ id = "ground" }
local plank = game.register_block{ id = "plank" }
local char = game.register_block{ id = "char" }
local flag = game.register_block{ id = "flag" }
game.register_block{ id = "accepted" }
local AIR = 0

game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local FULL = game.OCCUPANCY_FULL
local WEST, TOP = 0, 0
for i = 0, 26 do
    if i % 3 == 0 then WEST = WEST | (1 << i) end
    if (i // 3) % 3 == 2 then TOP = TOP | (1 << i) end
end

local list = {}
local function put(dx, dy, dz, material, mask)
    list[#list + 1] = { dx, dy, dz, material, mask }
end
for dx = -3, 8 do
    for dy = 0, 3 do
        for dz = -1, 5 do
            if dx == -3 or dx == 8 or dy == 0 or dy == 3 or dz == -1 or dz == 5 then
                put(dx, dy, dz, ((dx + 2 * dz + dy) % 5 == 0) and char or plank,
                    dy == 3 and WEST or FULL)
            end
        end
    end
end
for dx = 10, 12 do put(dx, -1, 0, plank, TOP) end
put(10, -1, 2, AIR, FULL)
put(11, -1, 2, AIR, TOP)
put(12, 0, 4, plank, FULL)
put(12, 0, 4, char, WEST)
put(0, 6, 0, flag, FULL)
local WRECK = game.schematic(list)

game.register_on_chat(function(event)
    if event.text == "stamp" then
        if game.stamp_schematic(WRECK, { x = 8, y = 0, z = 8 }, { turn = TURN }) then
            game.set_block({ x = -6, y = 0, z = 1 }, "wrecker:accepted")
        end
        return false
    end
end)
"#
    .replace("TURN", &turn.to_string());
    std::fs::write(dir.join("init.lua"), script).expect("script");
    root
}

/// One quarter turn, as the engine documents it: `+z` to `+x`, so the
/// offset `(dx, dz)` becomes `(dz, -dx)` and the cell `(x, y, z)` becomes
/// `(z, y, 2 - x)`. Written out here rather than borrowed from the engine,
/// so the engine's turn is checked against the convention and not against
/// itself.
fn turned(dx: i32, dz: i32, mask: u32, quarters: u32) -> (i32, i32, u32) {
    let (mut dx, mut dz, mut mask) = (dx, dz, mask);
    for _ in 0..quarters {
        (dx, dz) = (dz, -dx);
        let mut next = 0;
        for index in 0..27 {
            if mask & (1 << index) != 0 {
                let (x, y, z) = (index % 3, (index / 3) % 3, index / 9);
                next |= 1 << (z + 3 * y + 9 * (2 - x));
            }
        }
        mask = next;
    }
    (dx, dz, mask)
}

/// Stamps the wreck at `quarters` in a world of its own and checks every block
/// it touched, cell by cell, against what `buf:scatter` would have written
/// there: a whole mask replaces the block, and a partial one sets its cells
/// and keeps the rest (Sub-Node Contract §7.4) — so the model below is the
/// scatter's own loop, run over the world the generator made.
fn a_wreck_lands_whole_at(quarters: u32) {
    let world = scratch(&format!("wreck-world-{quarters}"));
    let server = start(write_wrecker(&format!("wreck-{quarters}"), quarters), world);
    block_on(async {
        let mut bot = watcher(&server, "Castaway").await;
        let ids: std::collections::BTreeMap<&str, u16> =
            ["ground", "plank", "char", "flag", "accepted"]
                .into_iter()
                .map(|name| (name, material(&bot, &format!("wrecker:{name}"))))
                .collect();
        let id = |name: &str| ids[name];
        let air = tiamat_core::MaterialId::AIR;
        let ground = tiamat_core::MaterialId(id("ground"));

        let mut expected: std::collections::BTreeMap<BlockPos, tiamat_core::block::Cells> =
            std::collections::BTreeMap::new();
        for (dx, dy, dz, name, mask) in wreck() {
            let (dx, dz, mask) = turned(dx, dz, mask, quarters);
            let at = BlockPos::new(ROOT.0 + dx, ROOT.1 + dy, ROOT.2 + dz);
            let material = if name.is_empty() {
                air
            } else {
                tiamat_core::MaterialId(id(name))
            };
            let cells = expected
                .entry(at)
                .or_insert([if at.y < 0 { ground } else { air }; 27]);
            for (index, cell) in cells.iter_mut().enumerate() {
                if mask & (1 << index) != 0 {
                    *cell = material;
                }
            }
        }
        assert!(
            expected.len() > 200,
            "the wreck is meant to be a few hundred blocks"
        );

        // **Every chunk the wreck will touch held BEFORE it is stamped.** A
        // chunk the server answered on one tick is written on the
        // connection's next beat, and a block delta from the tick between is
        // written as it comes — so a chunk still streaming in can arrive
        // after an edit to it, holding the world from before, and the client
        // has nothing to apply the edit to. That is the streaming layer's
        // race, not the stamp's; waiting the stream out keeps this test about
        // the stamp.
        let chunks: std::collections::BTreeSet<_> = expected.keys().map(|at| at.chunk()).collect();
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let held = bot.chunks_received();
            if chunks.iter().all(|chunk| held.contains(chunk)) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the wreck's chunks never all arrived: wanted {chunks:?}; {}",
                bot.chunk_report(1)
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }

        bot.chat("stamp").await.expect("chat");
        bot.expect_block(
            BlockPos::new(ACCEPTED.0, ACCEPTED.1, ACCEPTED.2),
            id("accepted"),
            PATIENCE,
        )
        .await
        .expect("the stamp was refused, or the mod errored");
        let control = server.control();
        let (from_tick, from_time, over_before) = (
            control.tick(),
            std::time::Instant::now(),
            control.over_budget_ticks(),
        );

        // The flag stands over the root, which no turn moves.
        bot.expect_block(
            BlockPos::new(ROOT.0, ROOT.1 + 6, ROOT.2),
            id("flag"),
            PATIENCE,
        )
        .await
        .expect("the wreck never finished landing");
        println!(
            "turn {quarters}: {} blocks landed in {} ticks ({:?}); {} ticks over budget while it \
             did, slowest tick of the run {} µs",
            expected.len(),
            control.tick() - from_tick,
            from_time.elapsed(),
            control.over_budget_ticks() - over_before,
            control.slowest_tick_micros(),
        );

        // Every block it touched, cell by cell, as a client was told it.
        for (at, cells) in &expected {
            let told = bot.block_as_told(*at).expect("a chunk the bot holds");
            assert_eq!(
                &told, cells,
                "turn {quarters}: {at:?} is not what the scatter would have written"
            );
        }
    });
}

#[test]
fn a_schematic_stamped_at_run_time_lands_whole_unturned() {
    a_wreck_lands_whole_at(0);
}

#[test]
fn a_schematic_stamped_at_run_time_lands_whole_turned_once() {
    a_wreck_lands_whole_at(1);
}

#[test]
fn a_schematic_stamped_at_run_time_lands_whole_turned_twice() {
    a_wreck_lands_whole_at(2);
}

#[test]
fn a_schematic_stamped_at_run_time_lands_whole_turned_three_times() {
    a_wreck_lands_whole_at(3);
}
