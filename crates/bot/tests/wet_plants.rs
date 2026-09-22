// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A passable cell does not displace the water round it — World ask 29.
//!
//! "Grass, when under water, should get saturated so it doesn't have an air
//! bubble around it." A block's fluid volume was 27 less the cells it held, so
//! a tuft of grass in a pond left a pocket of air the size of the tuft.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-wet-plants").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Two sealed wells, one with a tuft of grass in it and one without, each
/// poured a whole block of water. The mod raises a marker over the grass well
/// when both hold the same volume.
fn write_pond(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("wetplants");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"wetplants\"\nname = \"Wet Plants\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
-- Three cells of the block, passable: a tuft, exactly what the world mod grows.
game.register_block{ id = "tuft", passable = true }
game.register_block{ id = "water_block" }
game.register_block{ id = "same" }
game.register_fluid{ id = "water", material = "wetplants:water_block", tick_rate = 1 }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local BARE = { x = 2, y = 1, z = 2 }
local WEEDY = { x = 6, y = 1, z = 6 }

local ticks = 0
game.register_on_tick(function()
    ticks = ticks + 1
    if ticks == 20 then
        for _, well in ipairs({ BARE, WEEDY }) do
            for _, d in ipairs({ { 1, 0 }, { -1, 0 }, { 0, 1 }, { 0, -1 } }) do
                game.set_block({ x = well.x + d[1], y = well.y, z = well.z + d[2] }, "wetplants:ground")
            end
            -- Floor and lid: `flat_heightmap(0)` fills BELOW y = 0, so y = 0 is
            -- open air and a well without a floor of its own simply drains.
            game.set_block({ x = well.x, y = well.y - 1, z = well.z }, "wetplants:ground")
            game.set_block({ x = well.x, y = well.y + 1, z = well.z }, "wetplants:ground")
        end
        -- The bottom cell layer of the weedy well is plant: the nine cells
        -- with y = 0, indexed x + 3*y + 9*z.
        game.set_block(WEEDY, "wetplants:tuft", 0x1C0E07)
    elseif ticks == 25 then
        for _, well in ipairs({ BARE, WEEDY }) do
            game.set_fluid(well, { fluid = "wetplants:water", volume = 27 })
        end
    elseif ticks == 60 then
        local bare = game.get_fluid(BARE)
        local weedy = game.get_fluid(WEEDY)
        -- The volumes, written into the world as heights, so the test can read
        -- them back without a log it cannot see.
        game.set_block({ x = 2, y = 40 + (bare.volume or 0), z = 8 }, "wetplants:same")
        game.set_block({ x = 6, y = 40 + (weedy.volume or 0), z = 8 }, "wetplants:same")
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn a_plant_under_water_does_not_stand_in_a_bubble_of_air() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_pond("mods")),
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
                        .find(|def| def.name == "wetplants:same")
                        .map(|def| def.id)
                }) {
                    break id;
                }
                bot.recv().await.expect("recv");
            };

            let read = |bot: &Bot, x: i32| {
                (0..=27).find(|volume| bot.saw_block(BlockPos::new(x, 40 + volume, 8), marker))
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while tokio::time::Instant::now() < deadline
                && !(read(&bot, 2).is_some() && read(&bot, 6).is_some())
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }
            let bare = read(&bot, 2);
            let weedy = read(&bot, 6);
            assert_eq!(
                (bare, weedy),
                (Some(27), Some(27)),
                "bare well and weedy well should both hold a whole block of water"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
