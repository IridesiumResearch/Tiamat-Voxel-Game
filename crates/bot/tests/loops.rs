// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.play_loop` / `game.stop_loop` addressed to one player, over a real
//! server — weather ask W5. The cue test covers the loop everybody hears.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_core::proto::ServerMessage;
use tiamot_server::{ServerHandle, Settings};

const PATIENCE: Duration = Duration::from_secs(10);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tiamot-loops-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod that, on every join, plays one loop to everybody and one to the
/// first player alone with a fade, then stops the private one for them with a
/// longer fade.
fn write_mod(name: &str) -> PathBuf {
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
local first
game.register_on_player_join(function(event)
    if first == nil then first = event.player end
    game.play_loop{ id = "wind", sound = "storm:wind", everywhere = true }
    game.play_loop{ id = "rain", sound = "storm:rain", everywhere = true,
                    gain = 0.5, player = first, fade_ticks = 10 }
    game.stop_loop{ id = "rain", fade_ticks = 20, player = first }
end)
"#,
    )
    .expect("script");
    root
}

fn starts(bot: &Bot) -> Vec<(String, u32)> {
    bot.received()
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::StartLoop { id, fade_ticks, .. } => Some((id, fade_ticks)),
            _ => None,
        })
        .collect()
}

fn stops(bot: &Bot) -> Vec<(String, u32)> {
    bot.received()
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::StopLoop { id, fade_ticks } => Some((id, fade_ticks)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_loop_addressed_to_one_player_reaches_that_player_alone_with_its_fades() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 2,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_mod("mods")),
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
            // Until a bot has heard the loop everybody gets, which the mod
            // sends only from the join hook — so the server has processed
            // this join before the next bot makes one. Two joins in one tick
            // arrive in UUID order, and "first" would be a coin toss.
            let heard_wind = |bot: &Bot| starts(bot).iter().any(|(id, _)| id == "storm:wind");
            let mut first = connect("First").await;
            let deadline = tokio::time::Instant::now() + PATIENCE;
            while tokio::time::Instant::now() < deadline && !heard_wind(&first) {
                let _ = tokio::time::timeout(Duration::from_millis(100), first.recv()).await;
            }
            assert!(heard_wind(&first), "the loop for everybody never started");

            let mut second = connect("Second").await;
            let deadline = tokio::time::Instant::now() + PATIENCE;
            while tokio::time::Instant::now() < deadline && !heard_wind(&second) {
                let _ = tokio::time::timeout(Duration::from_millis(100), second.recv()).await;
                let _ = tokio::time::timeout(Duration::from_millis(50), first.recv()).await;
            }
            assert!(
                heard_wind(&second),
                "the second player never heard the wind"
            );

            let rain_started = starts(&first)
                .into_iter()
                .find(|(id, _)| id == "storm:rain")
                .expect("the addressed loop reached the player it was for");
            assert_eq!(rain_started.1, 10, "the fade in rides the start");
            let rain_stopped = stops(&first)
                .into_iter()
                .find(|(id, _)| id == "storm:rain")
                .expect("the addressed stop reached the player it was for");
            assert_eq!(rain_stopped.1, 20, "the fade out rides the stop");

            assert!(
                !starts(&second).iter().any(|(id, _)| id == "storm:rain"),
                "a loop addressed to one player was started for another: {:?}",
                starts(&second)
            );
            assert!(
                !stops(&second).iter().any(|(id, _)| id == "storm:rain"),
                "a stop addressed to one player reached another: {:?}",
                stops(&second)
            );
            first.disconnect().await;
            second.disconnect().await;
        });
    assert!(server.stop());
}
