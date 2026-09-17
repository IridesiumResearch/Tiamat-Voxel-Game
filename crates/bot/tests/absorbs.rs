// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `absorbs = { fluid = ... }` over a real server — weather ask W7: ground
//! that names the fluid it drinks takes that one and refuses the other.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::BlockPos;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-absorbs").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Two wells on flat ground, each a block of thirsty ground walled on four
/// sides so the fluid poured into it can only go down. Rain in one, oil in
/// the other; the thirsty ground drinks rain alone. Four seconds later the
/// mod reports what is left by placing a marker block over each well.
fn write_sponge(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("sponge");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"sponge\"\nname = \"Sponge\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "thirsty", absorbs = { rate = 27, fluid = "rain" } }
game.register_block{ id = "rain_block" }
game.register_block{ id = "oil_block" }
game.register_block{ id = "drank" }
game.register_block{ id = "refused" }
game.register_fluid{ id = "rain", material = "sponge:rain_block", tick_rate = 1 }
game.register_fluid{ id = "oil", material = "sponge:oil_block", tick_rate = 1 }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
local wells = { { x = 2, z = 2, fluid = "sponge:rain", marker = "sponge:drank" },
                { x = 6, z = 6, fluid = "sponge:oil", marker = "sponge:refused" } }
local ticks = 0
game.register_on_tick(function()
    ticks = ticks + 1
    if ticks == 20 then
        for _, well in ipairs(wells) do
            game.set_block({ x = well.x, y = 0, z = well.z }, "sponge:thirsty")
            for _, d in ipairs({ { 1, 0 }, { -1, 0 }, { 0, 1 }, { 0, -1 } }) do
                game.set_block({ x = well.x + d[1], y = 1, z = well.z + d[2] }, "sponge:ground")
            end
        end
    elseif ticks == 25 then
        for _, well in ipairs(wells) do
            game.set_fluid({ x = well.x, y = 1, z = well.z }, { fluid = well.fluid, volume = 27 })
        end
    elseif ticks == 105 then
        for _, well in ipairs(wells) do
            local left = game.get_fluid({ x = well.x, y = 1, z = well.z })
            local expect_gone = well.fluid == "sponge:rain"
            if left.empty == expect_gone then
                game.set_block({ x = well.x, y = 3, z = well.z }, well.marker)
            else
                game.log("well at " .. well.x .. " has " .. left.volume .. " left")
            end
        end
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn ground_that_names_its_fluid_drinks_that_one_and_refuses_the_other() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_sponge("mods")),
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

            let id_of = |bot: &Bot, name: &str| {
                bot.material_table()
                    .and_then(|table| table.into_iter().find(|def| def.name == name))
                    .map(|def| def.id)
            };
            let (drank, refused) = loop {
                if let (Some(drank), Some(refused)) =
                    (id_of(&bot, "sponge:drank"), id_of(&bot, "sponge:refused"))
                {
                    break (drank, refused);
                }
                bot.recv().await.expect("recv");
            };
            let rain_well = BlockPos::new(2, 3, 2);
            let oil_well = BlockPos::new(6, 3, 6);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while tokio::time::Instant::now() < deadline
                && !(bot.saw_block(rain_well, drank) && bot.saw_block(oil_well, refused))
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }
            assert!(
                bot.saw_block(rain_well, drank),
                "the ground did not drink the rain it names"
            );
            assert!(
                bot.saw_block(oil_well, refused),
                "the ground drank oil, which it does not name"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
