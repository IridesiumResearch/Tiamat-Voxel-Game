// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A canopy shades the floor under it — World ask 24.
//!
//! The rainforest's brief has its canopy blocking 85-90% of direct sunlight.
//! Leaves are `cutout`, and a `cutout` material passed light exactly the way
//! glass does (Contract §8.2), so the mod measured 85% of its floor under a
//! whole leaf block and most of those columns still reading `sun = 15`.
//!
//! Two clearings here, one roofed in leaves that dim and one in leaves that do
//! not, under the same sky, read through `game.get_light` — which is the same
//! number the mod itself was reading when it filed the ask.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-canopy").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Two shafts under the same sky: one open, one roofed with a canopy that
/// dims. The mod writes the sunlight it reads at the bottom of each into the
/// world as a height, so the test can read them back.
fn write_forest(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("canopy");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"canopy\"\nname = \"Canopy\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "mark" }
-- Leaves: see-through in places, and two levels of light lost per block of
-- them. Without the falloff this is a pane of glass as far as the sun is
-- concerned, which is exactly what the ask reported.
game.register_block{ id = "leaf", cutout = true, light_falloff = 2 }
-- The same leaves, dimming nothing: the control.
game.register_block{ id = "clear_leaf", cutout = true }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local FLOOR = 0
local CANOPY = 6
-- Far apart, so neither canopy stands over the other's floor.
local SHADED = { x = 2, z = 2 }
local BRIGHT = { x = 40, z = 40 }
-- **Wide enough that the middle is not lit from the side.** At three blocks
-- of overhang the floor read 11 whatever the leaves did, because daylight
-- walked in under the edge — the canopy was not what was being measured.
local REACH = 8

local ticks = 0
game.register_on_tick(function()
    ticks = ticks + 1
    if ticks == 20 then
        -- Three blocks of canopy over each floor, wide enough that no light
        -- reaches the middle from the side.
        for y = CANOPY, CANOPY + 2 do
            for dx = -REACH, REACH do
                for dz = -REACH, REACH do
                    game.set_block({ x = SHADED.x + dx, y = y, z = SHADED.z + dz }, "canopy:leaf")
                    game.set_block({ x = BRIGHT.x + dx, y = y, z = BRIGHT.z + dz }, "canopy:clear_leaf")
                end
            end
        end
    elseif ticks == 90 then
        local shaded = game.get_light({ x = SHADED.x, y = FLOOR + 1, z = SHADED.z })
        local bright = game.get_light({ x = BRIGHT.x, y = FLOOR + 1, z = BRIGHT.z })
        game.set_block({ x = 2, y = 40 + (shaded.sun or 0), z = 20 }, "canopy:mark")
        game.set_block({ x = 6, y = 40 + (bright.sun or 0), z = 20 }, "canopy:mark")
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn a_canopy_that_dims_shades_the_floor_and_one_that_does_not_lights_it() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_forest("mods")),
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
                        .find(|def| def.name == "canopy:mark")
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
            let shaded = read(&bot, 2).expect("the shaded floor reported a level");
            let bright = read(&bot, 6).expect("the lit floor reported a level");
            // **The control first.** Leaves that dim nothing are a pane of
            // glass as far as the sun is concerned, which is the behaviour the
            // ask reported: a whole canopy overhead and the floor still at 15.
            assert_eq!(
                bright, 15,
                "undimmed leaves should pass daylight untouched, as they always did"
            );
            // Three blocks of canopy at two levels a block, and then the fall
            // from the canopy to the floor at the ordinary level a block —
            // daylight's free straight-down fall ends at the first dimming
            // block, exactly as it does under water. Shade, not a cellar.
            // Measured: 6 against the meadow's 15, with three blocks of
            // canopy at two levels a block and the ordinary level a block
            // below it. Shade, not a cellar — a forest floor a player can see
            // by, which is what the brief asked for.
            assert!(
                (1..=9).contains(&shaded),
                "a canopy should shade its floor rather than roof it: {shaded} against {bright}"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
