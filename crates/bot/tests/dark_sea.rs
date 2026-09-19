// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `light_falloff` on a fluid — World ask 25.
//!
//! The deep ocean's brief has "total light extinction" on its plains, a hundred
//! blocks down. Water is a fluid and a fluid is air as far as the block store
//! is concerned (Sub-Node Contract §4), so sunlight fell to the sea floor at
//! full strength — and the plains were dark only in that nothing grew on them.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::BlockPos;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-dark-sea").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A shaft of open air and a shaft of water beside it, under the same sky.
/// The mod writes the sunlight it reads at the bottom of each into the world as
/// a height, so the test can read them back.
fn write_ocean(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("darksea");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"darksea\"\nname = \"Dark Sea\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "brine_block" }
game.register_block{ id = "mark" }
-- One level a block: the sun stops falling free and the shaft goes dark
-- fifteen blocks down.
game.register_fluid{ id = "brine", material = "darksea:brine_block", tick_rate = 1,
                     light_falloff = 1 }
game.register_on_generate(function(buf, pos)
    -- Ground everywhere below y = 0; the shafts are cut into the air above.
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local FLOOR = 0
local TOP = 11
local DRY = { x = 2, z = 2 }
local WET = { x = 6, z = 6 }

local ticks = 0
game.register_on_tick(function()
    ticks = ticks + 1
    if ticks == 20 then
        -- Walls round the wet shaft so its water cannot run away, from the
        -- floor to above the surface.
        for y = FLOOR, TOP do
            for _, d in ipairs({ { 1, 0 }, { -1, 0 }, { 0, 1 }, { 0, -1 } }) do
                game.set_block({ x = WET.x + d[1], y = y, z = WET.z + d[2] }, "darksea:ground")
            end
        end
    elseif ticks == 30 then
        for y = FLOOR, TOP do
            game.set_fluid({ x = WET.x, y = y, z = WET.z }, { fluid = "darksea:brine", volume = 27 })
        end
    elseif ticks == 90 then
        local dry = game.get_light({ x = DRY.x, y = FLOOR, z = DRY.z })
        local wet = game.get_light({ x = WET.x, y = FLOOR, z = WET.z })
        game.set_block({ x = 2, y = 40 + (dry.sun or 0), z = 20 }, "darksea:mark")
        game.set_block({ x = 6, y = 40 + (wet.sun or 0), z = 20 }, "darksea:mark")
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn sunlight_dims_with_depth_through_a_fluid_that_asked_it_to() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_ocean("mods")),
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
                        .find(|def| def.name == "darksea:mark")
                        .map(|def| def.id)
                }) {
                    break id;
                }
                bot.recv().await.expect("recv");
            };

            let read = |bot: &Bot, x: i32| {
                (0..=15).find(|level| bot.saw_block(BlockPos::new(x, 40 + level, 20), marker))
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
            while tokio::time::Instant::now() < deadline
                && !(read(&bot, 2).is_some() && read(&bot, 6).is_some())
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }
            let dry = read(&bot, 2).expect("the dry shaft reported a level");
            let wet = read(&bot, 6).expect("the wet shaft reported a level");
            assert_eq!(
                dry, 15,
                "an open shaft is lit to its floor, as it always was"
            );
            // The surface is y = 11, so the water is twelve blocks deep and
            // `light_falloff = 1` costs a level a block: 15 arrives at the air
            // above, 14 at the surface, 3 at the floor.
            assert_eq!(
                wet, 3,
                "twelve blocks of water at one level a block should read 3 at the floor"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
