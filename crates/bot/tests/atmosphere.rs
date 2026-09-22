// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.set_sky_modifier` over a real server — weather ask W1. The seam is
//! installed by the server and nothing else, so this is the test that says it
//! is installed at all.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-atmosphere").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod that darkens a player's sky when they join, sets the same thing
/// again every tick (which must cost nothing), and clears it a second later.
fn write_storm(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("storm");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"storm\"\nname = \"Storm\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
local since = {}
game.register_on_player_join(function(event)
    since[event.player] = 0
    -- Lightning beside them, and a strike far out of sight (W3).
    game.flash{ pos = { x = 30, y = 80, z = 0 }, radius = 256, intensity = 1.5,
                colour = { 0.9, 0.92, 1.0 }, attack_ticks = 1, decay_ticks = 6 }
    game.flash{ pos = { x = 5000, y = 80, z = 0 }, radius = 256, intensity = 0.25 }
end)
game.register_on_tick(function()
    for player, ticks in pairs(since) do
        since[player] = ticks + 1
        if ticks < 20 then
            game.set_sky_modifier(player, { intensity = 0.55, sky = { 0.55, 0.58, 0.62 },
                                            sky_mix = 0.7, fog_distance = 0.6, ease_ticks = 400 })
            game.set_precipitation(player, { rate = 900, size = 0.06, velocity = { x = 3, y = -22, z = 0 },
                                             area = { x = 16, y = 3, z = 16 }, above = 18, ease_ticks = 200 })
        elseif ticks == 20 then
            game.set_sky_modifier(player, nil)
            game.set_precipitation(player, nil)
        end
    end
end)
"#,
    )
    .expect("script");
    root
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "the values asserted are set, not computed"
)]
fn a_mods_weather_reaches_the_players_sky_once_per_change_and_clears_and_lightning_is_seen_in_reach()
 {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_storm("mods")),
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

            // Until the clear arrives, or long enough that it is missing
            // rather than late.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < deadline
                && !bot.sky_modifiers_received().contains(&None)
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }

            let received = bot.sky_modifiers_received();
            assert_eq!(
                received.len(),
                2,
                "a modifier set twenty times is one message, and its clearing one more: {received:?}"
            );
            let storm = received[0].expect("the storm");
            assert_eq!(storm.intensity, 0.55);
            assert_eq!(storm.sky, [0.55, 0.58, 0.62]);
            assert_eq!(storm.sky_mix, 0.7);
            assert_eq!(storm.fog_distance, 0.6);
            assert_eq!(storm.ease_ticks, 400);
            assert_eq!(received[1], None, "nil clears it");

            let rain = bot.precipitation_received();
            assert_eq!(
                rain.len(),
                2,
                "rain set twenty times is one message, and its end one more: {rain:?}"
            );
            let storm = rain[0].expect("the rain");
            assert_eq!(storm.rate, 900.0);
            assert_eq!(storm.above, 18.0);
            assert_eq!(storm.ease_ticks, 200);
            assert_eq!(storm.burst.velocity, [3.0, -22.0, 0.0]);
            assert_eq!(rain[1], None, "nil stops it");

            let flashes = bot.flashes_received();
            assert_eq!(
                flashes.len(),
                1,
                "the strike beside the player and no other: {flashes:?}"
            );
            assert_eq!(flashes[0].intensity, 1.5);
            assert_eq!(flashes[0].colour, [0.9, 0.92, 1.0]);
            assert_eq!((flashes[0].attack_ticks, flashes[0].decay_ticks), (1, 6));
            bot.disconnect().await;
        });
    assert!(server.stop());
}
