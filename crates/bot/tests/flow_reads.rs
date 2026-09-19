// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.get_block` inside `on_fluid_flow` — World ask 37's second half.
//!
//! The hook could queue writes and not read: every `game.get_block` inside it
//! answered nil, 1,208 of 1,208 in the world mod's flood over a meadow. The
//! cause was that the tick held the world mutably while it dispatched the
//! hooks, so the lease slot was empty — the same shape the dig, place and use
//! hooks had before `7579e22`. This proves the reads land now.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::BlockPos;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-flow-reads").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A wall, a puddle pressing on it, and a hook that reads the wall back.
///
/// The marker it raises is the whole test: `saw` is only set from INSIDE the
/// callback, and only when `game.get_block` answered with the material the
/// event already named. A read that came back nil, or came back as something
/// else, leaves the marker unplaced and the assertion fails.
fn write_reader(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("flowread");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"flowread\"\nname = \"Flow Read\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "wall" }
game.register_block{ id = "brine_block" }
game.register_block{ id = "read_it" }
game.register_fluid{ id = "brine", material = "flowread:brine_block", tick_rate = 1 }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local WALL = game.get_block_id("flowread:wall")
local saw = false
local nils = 0

game.register_on_fluid_flow(function(event)
    -- The read the ask is about. `event.into` is the block the fluid could
    -- not enter, and the hook already knows what is in it — so the engine's
    -- own answer is checkable rather than merely non-nil.
    local at = game.get_block({ x = event.into.x, y = event.into.y, z = event.into.z })
    if at == nil then
        nils = nils + 1
        return
    end
    if at.material == WALL then
        saw = true
    end
end)

local ticks = 0
game.register_on_tick(function()
    ticks = ticks + 1
    if ticks == 20 then
        -- A wall on three sides and the fourth open, so the brine presses on
        -- something solid rather than simply running away.
        game.set_block({ x = 3, y = 1, z = 2 }, "flowread:wall")
        game.set_block({ x = 1, y = 1, z = 2 }, "flowread:wall")
        game.set_block({ x = 2, y = 1, z = 1 }, "flowread:wall")
        game.set_block({ x = 2, y = 1, z = 3 }, "flowread:wall")
    elseif ticks == 25 then
        game.set_fluid({ x = 2, y = 1, z = 2 }, { fluid = "flowread:brine", volume = 27 })
    elseif ticks == 80 then
        if saw then
            game.set_block({ x = 2, y = 4, z = 2 }, "flowread:read_it")
        else
            game.log("the flow hook read nil " .. nils .. " times and never the wall")
        end
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn a_blocked_flow_can_read_the_block_that_blocked_it() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_reader("mods")),
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
                        .find(|def| def.name == "flowread:read_it")
                        .map(|def| def.id)
                }) {
                    break id;
                }
                bot.recv().await.expect("recv");
            };

            let raised = BlockPos::new(2, 4, 2);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while tokio::time::Instant::now() < deadline && !bot.saw_block(raised, marker) {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }
            assert!(
                bot.saw_block(raised, marker),
                "`game.get_block` inside `on_fluid_flow` never answered with the blocking block"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
