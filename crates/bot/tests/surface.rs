// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.surface_at` over a real server — weather ask W6. The lease is what
//! answers it and the server is what installs the lease, so this is the test
//! that says the column is read from the world players are actually in.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-surface").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Flat ground with its top at y = -1, a passable tuft on it at two columns,
/// and a mod that lays snow on whatever `surface_at` says the top is: bare
/// ground at (2, 2); a tuft at (6, 6) looked through; a tuft at (10, 10)
/// landed on.
fn write_snow(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("snow");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"snow\"\nname = \"Snow\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
local tuft = game.register_block{ id = "tuft", passable = true }
game.register_block{ id = "snow" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
    if pos.x == 0 and pos.y == 0 and pos.z == 0 then
        buf:set_block(6, 0, 6, tuft)
        buf:set_block(10, 0, 10, tuft)
    end
end)
local done = {}
local function lay(name, x, z, skip, expect_y, expect_material)
    if done[name] then return end
    local top = game.surface_at{ x = x, z = z, from = 12, depth = 20, skip_passable = skip }
    if top == nil then return end
    done[name] = true
    if top.y == expect_y and top.material == expect_material then
        game.set_block({ x = x, y = top.y + 1, z = z }, "snow:snow")
    else
        game.log("surface at " .. name .. " was y=" .. top.y .. " material=" .. top.material)
    end
end
game.register_on_tick(function()
    lay("bare", 2, 2, false, -1, ground)
    lay("through", 6, 6, true, -1, ground)
    lay("onto", 10, 10, false, 0, tuft)
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn snow_lands_where_the_engine_says_the_surface_is() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_snow("mods")),
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

            let snow = loop {
                if let Some(id) = bot
                    .material_table()
                    .and_then(|table| table.into_iter().find(|def| def.name == "snow:snow"))
                    .map(|def| def.id)
                {
                    break id;
                }
                bot.recv().await.expect("recv");
            };
            let bare = BlockPos::new(2, 0, 2);
            let through = BlockPos::new(6, 0, 6);
            let onto = BlockPos::new(10, 1, 10);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            while tokio::time::Instant::now() < deadline
                && !(bot.saw_block(bare, snow)
                    && bot.saw_block(through, snow)
                    && bot.saw_block(onto, snow))
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }
            assert!(
                bot.saw_block(bare, snow),
                "no snow on bare ground: {:?}",
                bot.notices()
            );
            assert!(
                bot.saw_block(through, snow),
                "no snow on the ground under a tuft looked through"
            );
            assert!(
                bot.saw_block(onto, snow),
                "no snow on top of a tuft landed on"
            );
            assert!(
                !bot.saw_block(BlockPos::new(6, 1, 6), snow),
                "a tuft looked through was landed on"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
