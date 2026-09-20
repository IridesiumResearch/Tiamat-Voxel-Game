// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Water running into a plant sweeps it away — World ask 37.
//!
//! "Grass should probably get broken by water." A tuft is `passable` and sits
//! under the fluid's `waterlogs_at`, so a flood runs straight THROUGH it and
//! stands in the same block, and `register_on_fluid_flow` never reports it: a
//! flow into a block nothing blocks is not a blocked flow. The world mod's own
//! measurement of a flood over a meadow was 1,208 reports, every one of them
//! the water's edge pressing sideways on ground, and none from a plant.
//!
//! Two plants here, and the second is the point: one declares `washes_away`
//! and one does not, so a test that simply cleared everything wet would fail.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::BlockPos;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-washes-away").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Flat ground, two tufts side by side in walled wells, and water poured in
/// over each. The mod reports what is left by writing markers into the world,
/// which is how a headless test reads a server's own answer.
fn write_meadow(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("meadow");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"meadow\"\nname = \"Meadow\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
-- The world mod's own shape: a few cells of a passable block, standing on the
-- surface. One washes away and one does not.
game.register_block{ id = "tuft", passable = true, washes_away = true }
game.register_block{ id = "reed", passable = true }
game.register_block{ id = "water_block" }
game.register_block{ id = "marker" }
game.register_fluid{ id = "water", material = "meadow:water_block", tick_rate = 1 }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local TUFT = { x = 2, y = 1, z = 2 }
local REED = { x = 6, y = 1, z = 6 }
-- The bottom cell layer of the block: the nine cells with y = 0, indexed
-- x + 3*y + 9*z. A tuft standing on the ground, as the world mod grows one.
local STANDING = 0x1C0E07

local ticks = 0
game.register_on_tick(function()
    ticks = ticks + 1
    if ticks == 20 then
        for _, plant in ipairs({ TUFT, REED }) do
            -- A walled well, so the water stays where it is poured instead of
            -- running off and reaching the plant from somewhere unpredictable.
            for _, d in ipairs({ { 1, 0 }, { -1, 0 }, { 0, 1 }, { 0, -1 } }) do
                game.set_block({ x = plant.x + d[1], y = plant.y, z = plant.z + d[2] }, "meadow:ground")
                game.set_block({ x = plant.x + d[1], y = plant.y + 1, z = plant.z + d[2] }, "meadow:ground")
            end
            game.set_block({ x = plant.x, y = plant.y - 1, z = plant.z }, "meadow:ground")
        end
        game.set_block(TUFT, "meadow:tuft", STANDING)
        game.set_block(REED, "meadow:reed", STANDING)
    elseif ticks == 30 then
        -- Poured ABOVE each plant, so what reaches it is a flow into its block
        -- rather than a mod writing fluid straight into the plant.
        for _, plant in ipairs({ TUFT, REED }) do
            game.set_fluid({ x = plant.x, y = plant.y + 1, z = plant.z },
                { fluid = "meadow:water", volume = 27 })
        end
    elseif ticks == 70 then
        -- What is left, written where the test can see it. y = 41 for gone,
        -- y = 42 for still standing.
        for i, plant in ipairs({ TUFT, REED }) do
            local at = game.get_block(plant)
            local empty = (at == nil) or (at.occupancy == 0)
            game.set_block({ x = i * 2, y = empty and 41 or 42, z = 20 }, "meadow:marker")
        end
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn water_running_into_a_plant_sweeps_it_away_and_leaves_its_neighbour() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_meadow("mods")),
        enabled_mods: None,
        seed: Some(4),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let mut bot = Bot::connect(
                server.local_addr(),
                Identity::generate().expect("identity"),
                server.cert_fingerprint(),
            )
            .await
            .expect("connect");
            bot.join("Watcher").await.expect("join");

            let marker = loop {
                if let Some(id) = bot.material_table().and_then(|table| {
                    table
                        .into_iter()
                        .find(|def| def.name == "meadow:marker")
                        .map(|def| def.id)
                }) {
                    break id;
                }
                bot.recv().await.expect("recv");
            };

            // `Some(true)` gone, `Some(false)` still standing, `None` not yet
            // reported — the mod writes exactly one of the two heights.
            let read = |bot: &Bot, x: i32| {
                if bot.saw_block(BlockPos::new(x, 41, 20), marker) {
                    Some(true)
                } else if bot.saw_block(BlockPos::new(x, 42, 20), marker) {
                    Some(false)
                } else {
                    None
                }
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
            while tokio::time::Instant::now() < deadline
                && !(read(&bot, 2).is_some() && read(&bot, 4).is_some())
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }

            assert_eq!(
                read(&bot, 2),
                Some(true),
                "the water ran through the tuft and left it standing"
            );
            assert_eq!(
                read(&bot, 4),
                Some(false),
                "a plant that never said `washes_away` was cleared anyway"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
