// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.show_over`, over a real server — Life ask 15.
//!
//! "Hit a cow and a row of hearts shows over it for a second, draining." The
//! mod could not do that: nothing put a picture in the world over an entity,
//! so it drew each heart in pixels, one particle per pixel — 65 messages a
//! blow.
//!
//! What this proves is the part a unit test against a double cannot: that the
//! seam is installed on a real server, that the badge reaches a player over
//! the wire, and — the design claim — that a mod calling it every tick costs
//! ONE message per entity per network pass rather than a queue of stale bars.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-show-over").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The bytes the heart is made of, so the test knows the hash without reading
/// the file back.
const PICTURE: &[u8] = b"\x89PNG\r\n\x1a\nbytes standing in for a heart";

/// A mod that spawns a mob beside the player and hangs hearts over it.
///
/// Three claims are built into the fixture: the badge over the mob (which must
/// arrive), five calls in one tick (of which only the last may), and a badge
/// over an entity that does not exist (which must reach nobody).
fn write_herd(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("herd");
    std::fs::create_dir_all(dir.join("textures")).expect("mod dir");
    std::fs::write(dir.join("textures/heart.png"), PICTURE).expect("picture");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"herd\"\nname = \"Herd\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "marker" }
local heart = game.register_picture{ id = "heart", file = "textures/heart.png" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

local cow = nil
local ticks = 0
game.register_on_tick(function()
    ticks = ticks + 1
    if cow == nil then
        cow = game.spawn_entity{
            pos = { x = 2, y = 0, z = 2 },
            model = "engine:humanoid",
            collider = { width = 0.6, height = 1.8 },
        }
        return
    end
    -- **Five in one tick, counting down.** A queue would show a player all
    -- five, oldest first; a latest-state slot shows the last. The counts are
    -- what tell the two apart.
    for count = 5, 1, -1 do
        game.show_over(cow, { picture = heart, count = count, seconds = 5,
                              colour = { r = 1, g = 0.2, b = 0.2 } })
    end
    -- An entity that has never existed. The mod hears 0 and nobody is told,
    -- which the test reads as a marker rather than as a message that is
    -- missing for some other reason.
    if ticks == 10 then
        local told = game.show_over(999999, { picture = heart, count = 3 })
        game.set_block({ x = 0, y = 40 + told, z = 20 }, "herd:marker")
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
fn a_badge_hangs_over_a_mob_and_the_newest_one_is_the_only_one_sent() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_herd("mods")),
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
                        .find(|def| def.name == "herd:marker")
                        .map(|def| def.id)
                }) {
                    break id;
                }
                bot.recv().await.expect("recv");
            };

            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            while tokio::time::Instant::now() < deadline && bot.badges_received().len() < 5 {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }

            let badges = bot.badges_received();
            assert!(
                badges.len() >= 5,
                "the badge over the mob never arrived: {badges:?}"
            );
            let first = badges[0];
            assert_eq!(
                first.picture,
                tiamat_core::content::hash_bytes(PICTURE),
                "the picture's hash did not survive the wire"
            );
            assert_eq!(first.colour, [255, 51, 51, 255]);
            assert!((first.seconds - 5.0).abs() < f32::EPSILON);

            // **The design claim.** Five calls a tick, and the one that
            // reaches the player is the last of them. A queue would deliver
            // 5, 4, 3, 2, 1 in order and this would read 5 — which is the
            // sabotage that proves the assertion is about the map.
            assert!(
                badges.iter().all(|badge| badge.count == 1),
                "a stale badge was sent: the queue is not latest-state ({:?})",
                badges.iter().map(|badge| badge.count).collect::<Vec<_>>()
            );

            // And one message per network pass, not five: over the same
            // window the mod made five calls a tick.
            assert!(
                badges.len() < 40,
                "{} badges for a mod calling show_over five times a tick",
                badges.len()
            );

            // A badge over an entity nobody has: told nobody, and the marker
            // at y = 40 says so.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < deadline
                && !bot.saw_block(tiamat_core::BlockPos::new(0, 40, 20), marker)
                && !bot.saw_block(tiamat_core::BlockPos::new(0, 41, 20), marker)
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }
            assert!(
                bot.saw_block(tiamat_core::BlockPos::new(0, 40, 20), marker),
                "a badge over an entity that does not exist was sent to somebody"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
