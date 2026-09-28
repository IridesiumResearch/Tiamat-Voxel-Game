// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Chunks nobody is near leave memory, and come back whole.
//!
//! **Reported from the window**: "especially after being in a world for a
//! while the client server prediction seems to get worse." The server never
//! unloaded a chunk: everything a player ever walked past stayed resident,
//! with its light, its fluid and its mobs stepped every tick, and a world a
//! few hours old spent its budget on country nobody was in. A tick over budget
//! drops ticks, and a client cannot predict a server that drops ticks.
//!
//! Two things are asserted over a real server. That with nobody connected the
//! resident set goes to nothing — a chunk needs a player near it to stay. And
//! that what was unloaded comes back whole: the terrain from the database, and
//! a mob that was frozen with its chunk and thawed when somebody came back.
//! The second is the one that matters; an unload that lost mobs would be a
//! leak fixed by a deletion.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::ChunkPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

/// The chunk under spawn: the ground the player stands on.
const UNDER_SPAWN: ChunkPos = ChunkPos::new(0, -1, 0);

/// The mob's nametag, which is how the bot tells it from a player mirror.
const KEEPER: &str = "keeper";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-unload").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Flat ground, a hand, and one mob spawned beside the first player to stand.
///
/// From the tick rather than the join hook, once the body is on the ground —
/// a join hook runs before the player has a body to stand beside. Spawned
/// once for the life of the VM rather than once per join, so a second join
/// finds the mob the database gave back and not a fresh one.
fn write_keeper(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("keeper");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"keeper\"\nname = \"Keeper\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local ground = game.register_block{ id = 'ground' }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n\
         game.register_tool{ id = 'hand', brush = 'block', speed_multiplier = 1.0, default = true }\n\
         local waiting = {}\n\
         local spawned = false\n\
         game.register_on_player_join(function(event)\n\
         \x20   waiting[event.player] = true\n\
         end)\n\
         game.register_on_tick(function()\n\
         \x20   if spawned then return end\n\
         \x20   for uuid in pairs(waiting) do\n\
         \x20       local id = game.player_entity(uuid)\n\
         \x20       local body = id and game.entity(id)\n\
         \x20       if body and body.on_ground then\n\
         \x20           local keeper = game.spawn_entity{\n\
         \x20               pos = { x = body.pos.x + 2, y = body.pos.y, z = body.pos.z },\n\
         \x20               model = 'engine:humanoid',\n\
         \x20               nametag = 'keeper',\n\
         \x20               collider = { width = 1.8, height = 5.4 },\n\
         \x20           }\n\
         \x20           if keeper ~= nil then spawned = true end\n\
         \x20       end\n\
         \x20   end\n\
         end)\n",
    )
    .expect("script");
    root
}

fn start(name: &str, mods: PathBuf) -> ServerHandle {
    ServerHandle::start(&Settings {
        world_options: Vec::new(),
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods.clone()),
        enabled_mods: bot::fixture::enabled_mods_for(&mods).expect("the mod's manifest"),
        seed: Some(3),
        rcon: None,
        materials: Vec::new(),
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

/// Waits until the bot has been sent the chunk under spawn, or gives up.
async fn holds_ground(bot: &mut Bot, patience: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + patience;
    while tokio::time::Instant::now() < deadline {
        if bot.chunks_received().contains(&UNDER_SPAWN) {
            return true;
        }
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    }
    false
}

/// Waits until the bot has been told about the keeper, or gives up.
async fn sees_keeper(bot: &mut Bot, patience: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + patience;
    while tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        if bot
            .entities()
            .values()
            .any(|entity| entity.nametag.as_deref() == Some(KEEPER))
        {
            return true;
        }
    }
    false
}

#[test]
fn chunks_nobody_is_near_leave_memory_and_come_back_whole() {
    let server = start("round-trip", write_keeper("round-trip"));
    let control = server.control().clone();

    block_on(async {
        let mut bot = join(&server, "First").await;
        assert!(
            sees_keeper(&mut bot, Duration::from_secs(10)).await,
            "the keeper never appeared beside the first player"
        );
        assert!(
            holds_ground(&mut bot, Duration::from_secs(10)).await,
            "the ground under spawn never arrived"
        );
        assert!(
            control.resident_chunks() > 0,
            "a server with a player in it holds that player's chunks"
        );
        assert_eq!(
            control.unloaded(),
            0,
            "nothing is far from a player standing at spawn, so nothing leaves"
        );
        bot.disconnect().await;

        // With nobody connected, nothing is near anybody. Every chunk goes:
        // the clean ones on the first sweep, the ones with unsaved edits once
        // the debounce has written them.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while control.resident_chunks() > 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{} chunks still resident with nobody connected",
                control.resident_chunks()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(control.unloaded() > 0);

        // Back. The terrain is read from the database, and the keeper was
        // frozen with its chunk and thawed with it: the same mob, not a new
        // one — the mod spawns once.
        let mut again = join(&server, "Second").await;
        assert!(
            holds_ground(&mut again, Duration::from_secs(10)).await,
            "the ground did not come back from the database"
        );
        assert!(
            sees_keeper(&mut again, Duration::from_secs(10)).await,
            "the keeper was lost with its chunk: frozen and never thawed, or never written"
        );
    });

    assert!(server.stop());
}
