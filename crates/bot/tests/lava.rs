// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A fluid that glows lights the world around it, and one that does not does not.
//!
//! **Asked for from the window: "I need to make lava."** Two things were
//! missing, and this file is about the one that is simulation rather than
//! presentation. A block holding lava is AIR in the block store — a block holds
//! terrain and fluid independently, Sub-Node Contract §4 — so the light engine,
//! which takes emission from a block's material, saw nothing at all and a lake
//! of lava left its cave pitch black.
//!
//! The rule is that a fluid glows with whatever the block it is DRAWN as emits.
//! A mod writes `light_emit` on the block once and the fluid and the block
//! agree by construction, rather than by a second field that has to be kept in
//! step with the first.
//!
//! Everything here is read through a real server and a real client: what the
//! bot knows about light is what it was sent (`Bot::light_at`), so these are
//! assertions about the wire rather than about an internal table.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::BlockPos;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-lava").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A world whose floor is at y = 0 with a pool of `fluid` sunk into it.
///
/// Roofed, because the question is whether the LAVA lights the room: under open
/// sky every block is already at full daylight and nothing could be measured.
fn write_world(name: &str, light_emit: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("lava");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"lava\"\nname = \"Lava\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        format!(
            "local rock = game.register_block{{ id = \"rock\" }}\n\
             game.register_block{{ id = \"molten\"{light_emit} }}\n\
             game.register_fluid{{ id = \"lava\", material = \"lava:molten\", opacity = 1.0 }}\n\
             game.register_on_generate(function(buf, pos)\n\
             \x20   -- A roofed room: rock to y = 16, hollowed out from y = 1 to\n\
             \x20   -- y = 4, so the only light in it is whatever the pool gives.\n\
             \x20   buf:fill_below_heightmap(game.flat_heightmap(16), rock)\n\
             \x20   local x0, z0 = pos.x * 16, pos.z * 16\n\
             \x20   for x = x0, x0 + 15 do\n\
             \x20       for z = z0, z0 + 15 do\n\
             \x20           for y = 1, 4 do\n\
             \x20               buf:set_world(x, y, z, 0)\n\
             \x20           end\n\
             \x20       end\n\
             \x20   end\n\
             \x20   -- One block deep, in the hollow: below y = 2 is the room's\n\
             \x20   -- floor course at y = 1, and y = 0 is solid rock that takes\n\
             \x20   -- none.\n\
             \x20   buf:fill_fluid_below(2, \"lava:lava\")\n\
             end)\n"
        ),
    )
    .expect("script");
    root
}

fn start(name: &str, mods: PathBuf) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 2,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(3),
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

/// The brightest block light in the room, once the world has stopped arriving.
fn room_light(name: &str, light_emit: &str) -> u8 {
    let server = start(name, write_world(&format!("{name}-mods"), light_emit));
    let brightest = block_on(async {
        let mut bot = Bot::connect(
            server.local_addr(),
            Identity::generate().expect("identity"),
            server.cert_fingerprint(),
        )
        .await
        .expect("connect");
        bot.join("Miner").await.expect("join");
        loop {
            let batch = bot
                .collect_chunks(64, Duration::from_secs(2))
                .await
                .expect("collect");
            if batch.is_empty() {
                break;
            }
        }
        // Light arrives after the chunk it belongs to; give the tick the room
        // to send it rather than assuming it already has.
        bot.sleep_ticks(20).await;

        // The air of the room, one block above the pool and across it.
        let mut brightest = 0;
        for x in 0..16 {
            for z in 0..16 {
                let at = BlockPos::new(x, 2, z);
                if let Some(light) = bot.light_at(at) {
                    // The block channels, not the sun: a room lit by the sky
                    // would prove nothing about a pool.
                    brightest = brightest
                        .max(light.red())
                        .max(light.green())
                        .max(light.blue());
                }
            }
        }
        bot.disconnect().await;
        brightest
    });
    assert!(server.stop());
    brightest
}

#[test]
fn a_pool_of_glowing_fluid_lights_the_room_it_stands_in() {
    // The material the fluid is drawn as emits, so the fluid does.
    let lit = room_light("glowing", ", light_emit = { r = 15, g = 8, b = 2 }");
    assert!(
        lit >= 8,
        "a roofed room over a pool of glowing fluid is at {lit}, so the pool is lighting nothing"
    );
}

/// The same room, empty, with the lava poured DURING play instead of generated.
fn write_pouring_world(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("lava");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"lava\"\nname = \"Lava\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local rock = game.register_block{ id = "rock" }
game.register_block{ id = "molten", light_emit = { r = 15, g = 8, b = 2 } }
game.register_fluid{ id = "lava", material = "lava:molten", opacity = 1.0 }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(16), rock)
    local x0, z0 = pos.x * 16, pos.z * 16
    for x = x0, x0 + 15 do
        for z = z0, z0 + 15 do
            for y = 1, 4 do
                buf:set_world(x, y, z, 0)
            end
        end
    end
end)

-- Poured AFTER somebody joins, and ten seconds after that, so the room has
-- certainly been served and seen dark first. The only thing that can light it
-- from then on is the fluid step's own relight.
local due = nil
game.register_on_player_join(function()
    due = 200
end)
game.register_on_tick(function()
    if due == nil then return end
    due = due - 1
    if due > 0 then return end
    due = nil
    for x = 0, 15 do
        for z = 0, 15 do
            game.set_fluid({ x = x, y = 1, z = z }, { fluid = "lava:lava", volume = 27 })
        end
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn lava_poured_during_play_lights_the_room_without_the_chunk_being_served_again() {
    // **The half the test above cannot reach.** Lava that is GENERATED is lit
    // when its chunk is served; lava that arrives later is lit only by the
    // tick's own relight pass — and that pass used to run BEFORE the fluid
    // step, so every position the fluid step queued was pushed onto a vector
    // that was dropped at the end of the tick. A lake that flowed took its
    // light with it the next time somebody reloaded the chunk, and no sooner.
    let server = start("poured", write_pouring_world("poured-mods"));
    let lit = block_on(async {
        let mut bot = Bot::connect(
            server.local_addr(),
            Identity::generate().expect("identity"),
            server.cert_fingerprint(),
        )
        .await
        .expect("connect");
        bot.join("Miner").await.expect("join");
        loop {
            let batch = bot
                .collect_chunks(64, Duration::from_secs(2))
                .await
                .expect("collect");
            if batch.is_empty() {
                break;
            }
        }
        // Dark to begin with: the room is roofed and nothing has been poured.
        bot.sleep_ticks(20).await;
        let before = bot
            .light_at(BlockPos::new(8, 2, 8))
            .map_or(0, |light| light.red());
        assert_eq!(before, 0, "the room was lit before anything was poured");

        // Past the pour, and past the fluid tick that carries it.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut lit = 0;
        while tokio::time::Instant::now() < deadline && lit == 0 {
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            lit = bot
                .light_at(BlockPos::new(8, 2, 8))
                .map_or(0, |light| light.red());
        }
        bot.disconnect().await;
        lit
    });
    assert!(server.stop());
    assert!(
        lit >= 8,
        "lava poured into a served room left it at {lit}: the fluid step's relight never ran"
    );
}

#[test]
fn a_pool_of_ordinary_fluid_lights_nothing() {
    // **The counter-example, and the reason it is here.** Without it the test
    // above would pass just as well if the room were lit by something else
    // entirely — a sky leak, a default, a bug that lights every cave. The only
    // difference between the two worlds is the `light_emit` on the block the
    // fluid is drawn as.
    let dark = room_light("ordinary", "");
    assert_eq!(
        dark, 0,
        "a room over a pool of fluid that emits nothing is at {dark}, so the light in the test \
         above did not come from the fluid"
    );
}
