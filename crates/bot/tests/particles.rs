// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.emit_particles`, over a real server.
//!
//! The seam is installed by the server and nothing else, so this is the test
//! that says it is installed at all — the class of bug where every unit test
//! passes against its own double and every real server does nothing.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-particles").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The bytes the sprayer's picture is made of, so a test can work out the hash
/// the server will send without reading the file back.
const PICTURE: &[u8] = b"\x89PNG\r\n\x1a\nbytes standing in for a heart";

/// A mod that sprays four bursts a second, told apart by colour: one beside
/// the player (red), one far out of radius (green), one at the player's own
/// coordinates in another domain (blue), and one carrying a picture (grey — a colour with no full channel in
/// it, so the three checks below still mean what they say).
fn write_sprayer(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("sprayer");
    std::fs::create_dir_all(dir.join("textures")).expect("mod dir");
    std::fs::write(dir.join("textures/heart.png"), PICTURE).expect("picture");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"sprayer\"\nname = \"Sprayer\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local ground = game.register_block{ id = \"ground\" }\n\
         local heart = game.register_picture{ id = \"heart\", file = \"textures/heart.png\" }\n\
         game.register_domain{ id = \"elsewhere\" }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n\
         local turn = 0\n\
         game.register_on_tick(function()\n\
         \x20   turn = turn + 1\n\
         \x20   if turn % 20 ~= 0 then return end\n\
         \x20   game.emit_particles{ pos = { x = 2, y = 2, z = 2 }, count = 12,\n\
         \x20       colour = { r = 1, g = 0, b = 0 }, velocity = { y = 6 }, gravity = 20 }\n\
         \x20   game.emit_particles{ pos = { x = 900, y = 2, z = 2 }, radius = 64,\n\
         \x20       colour = { r = 0, g = 1, b = 0 } }\n\
         \x20   game.emit_particles{ pos = { x = 2, y = 2, z = 2, domain = \"sprayer:elsewhere\" },\n\
         \x20       colour = { r = 0, g = 0, b = 1 } }\n\
         \x20   game.emit_particles{ pos = { x = 2, y = 2, z = 2 }, count = 1,\n\
         \x20       colour = { r = 0.5, g = 0.5, b = 0.5 }, texture = heart }\n\
         end)\n",
    )
    .expect("script");
    root
}

#[test]
fn a_spray_reaches_the_player_beside_it_and_nobody_out_of_reach_or_elsewhere() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("spray-world"),
        identity_path: None,
        max_players: 2,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_sprayer("spray-mods")),
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

            // Three sprays' worth: long enough that a missing colour is missing
            // rather than late.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < deadline
                && bot
                    .particles_received()
                    .iter()
                    .filter(|b| b.colour == [255, 0, 0, 255])
                    .count()
                    < 3
            {
                let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
            }

            let bursts = bot.particles_received();
            let red: Vec<_> = bursts
                .iter()
                .filter(|b| b.colour == [255, 0, 0, 255])
                .collect();
            assert!(
                red.len() >= 3,
                "the spray beside the player did not arrive: {bursts:?}"
            );
            let first = red[0];
            assert_eq!(first.count, 12);
            assert!(
                first
                    .pos
                    .iter()
                    .all(|value| (value - 2.0).abs() < f64::EPSILON),
                "{:?}",
                first.pos
            );
            assert!((first.velocity[1] - 6.0).abs() < f32::EPSILON);
            assert!(
                !bursts.iter().any(|b| b.colour[1] == 255),
                "a spray 900 blocks away with a radius of 64 was sent"
            );
            assert!(
                !bursts.iter().any(|b| b.colour[2] == 255),
                "a spray in another domain was sent to a player in the overworld"
            );

            // Life ask 15: a burst may name a picture, and the hash that
            // reaches the client is the one `register_picture` answered — the
            // same hash the content index serves the bytes under, or the
            // client would look up art nobody has.
            assert_eq!(
                first.texture, None,
                "a burst that named no picture carried one anyway"
            );
            let pictured: Vec<_> = bursts.iter().filter(|b| b.texture.is_some()).collect();
            assert!(
                !pictured.is_empty(),
                "the burst carrying a picture never arrived: {bursts:?}"
            );
            assert_eq!(
                pictured[0].texture,
                Some(tiamat_core::content::hash_bytes(PICTURE)),
                "the picture's hash did not survive the wire"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}

/// A mod that sprays only the first player to join, by name.
fn write_addressed_sprayer(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("sprayer");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"sprayer\"\nname = \"Sprayer\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local ground = game.register_block{ id = \"ground\" }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n\
         local first\n\
         game.register_on_player_join(function(event)\n\
         \x20   if first == nil then first = event.player end\n\
         end)\n\
         local turn = 0\n\
         game.register_on_tick(function()\n\
         \x20   turn = turn + 1\n\
         \x20   if first == nil or turn % 20 ~= 0 then return end\n\
         \x20   -- Blue for the first player alone; red for everyone in reach.\n\
         \x20   game.emit_particles{ pos = { x = 2, y = 2, z = 2 }, count = 4,\n\
         \x20       colour = { r = 0, g = 0, b = 1 }, player = first }\n\
         \x20   game.emit_particles{ pos = { x = 2, y = 2, z = 2 }, count = 4,\n\
         \x20       colour = { r = 1, g = 0, b = 0 } }\n\
         end)\n",
    )
    .expect("script");
    root
}

#[test]
fn a_burst_addressed_to_one_player_reaches_that_player_alone() {
    // **Weather ask W4(a).** Every burst used to go to everyone within
    // `radius`, so a mod's own "particles off" setting could not be honoured —
    // a player who turned rain off still got their neighbour's — and rain had
    // to be emitted per patch of ground to avoid doubling. `player` narrows a
    // burst to one UUID. It narrows and never widens: the red burst here still
    // reaches both players, because both are in reach.
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("addressed-world"),
        identity_path: None,
        max_players: 2,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_addressed_sprayer("addressed-mods")),
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
            let connect = |name: &'static str| async {
                let mut bot = Bot::connect(
                    server.local_addr(),
                    Identity::generate().expect("identity"),
                    server.cert_fingerprint(),
                )
                .await
                .expect("connect");
                bot.join(name).await.expect("join");
                bot
            };
            let blue = |bot: &Bot| {
                bot.particles_received()
                    .iter()
                    .filter(|b| b.colour == [0, 0, 255, 255])
                    .count()
            };
            let red = |bot: &Bot| {
                bot.particles_received()
                    .iter()
                    .filter(|b| b.colour == [255, 0, 0, 255])
                    .count()
            };

            let mut first = connect("First").await;
            // **The mod must have seen this player join before the other one
            // does.** The server derives arrivals from a per-tick diff of who
            // is present, in UUID order, so two bots that join within one tick
            // arrive together and "first" is whichever random UUID sorts
            // lower. The mod sprays nothing until it has a first player, so a
            // red burst here proves it is this one.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < deadline && red(&first) == 0 {
                let _ = tokio::time::timeout(Duration::from_millis(100), first.recv()).await;
            }
            assert!(red(&first) > 0, "the scene never sprayed at all");
            let mut second = connect("Second").await;

            // Until the addressed player has three blue bursts, or long enough
            // that a missing colour is missing rather than late.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < deadline && (blue(&first) < 3 || red(&second) < 3) {
                let _ = tokio::time::timeout(Duration::from_millis(100), first.recv()).await;
                let _ = tokio::time::timeout(Duration::from_millis(100), second.recv()).await;
            }

            assert!(
                blue(&first) >= 3,
                "the addressed player did not get the burst sent to them: {:?}",
                first.particles_received()
            );
            assert!(
                red(&second) >= 3,
                "the unaddressed burst did not reach the second player, so the scene never \
                 sprayed at all: {:?}",
                second.particles_received()
            );
            assert_eq!(
                blue(&second),
                0,
                "a burst addressed to one player was sent to another in reach: {:?}",
                second.particles_received()
            );
            first.disconnect().await;
            second.disconnect().await;
        });
    assert!(server.stop());
}
