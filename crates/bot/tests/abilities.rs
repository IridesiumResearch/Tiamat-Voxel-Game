// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.set_player_abilities` — Life mod's asks 1 and 9, over a real server.
//!
//! **Two halves that are one thing.** Flight is a permission the operator list
//! holds, so a Creative world on a dedicated server has grounded builders
//! unless the host makes every one of them an operator. And a design where cold
//! slows you and an empty hunger bar stops you sprinting could not be expressed
//! at all: a player's body is stepped from their own inputs, and
//! `game.set_entity` on their mirror is overwritten every tick.
//!
//! What is measured here is the SERVER'S answer — the position it reports back
//! — because that is the authority. The client predicting the same thing is the
//! other half and is tested where the predictor is.

use std::path::PathBuf;

use bot::Bot;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_core::proto::actions;
use tiamot_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-abilities").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Flat ground, and a mod that slows whoever joins to a quarter speed and
/// refuses them the sprint key — but only once they say so by walking past
/// x = 6, so the test can measure both sides of the same body.
fn write_mod(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("abil");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"abil\"\nname = \"Abilities\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)

-- A world option would be the tidy way; a chat word is the testable one.
game.register_on_chat(function(event)
    if event.text == "slow" then
        -- A typo and a negative speed are errors, not silent defaults. Were
        -- either accepted, this hook would fail and the slowing below with it.
        assert(not pcall(game.set_player_abilities, event.player, { sped = 2 }))
        assert(not pcall(game.set_player_abilities, event.player, { speed = -1 }))
        game.set_player_abilities(event.player, { speed = 0.25, sprint = false })
        return ""
    elseif event.text == "fly" then
        game.set_player_abilities(event.player, { fly = true })
        return ""
    elseif event.text == "normal" then
        game.set_player_abilities(event.player, nil)
        return ""
    end
end)
"#,
    )
    .expect("script");
    root
}

fn start(name: &str) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 2,
        allowlist: Allowlist::open(),
        // **Deliberately empty.** The whole point of ask 9 is that a mod can
        // grant flight to somebody who is NOT an operator.
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_mod(&format!("{name}-mods"))),
        enabled_mods: None,
        seed: Some(5),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
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

/// How far a bot walks north in `ticks`, in cells.
async fn walked(bot: &mut Bot, ticks: u64) -> f32 {
    let before = bot.walk([0.0, 0.0, 0.0], 0, 4).await.expect("settle");
    let after = bot
        .walk([0.0, 0.0, 1.0], actions::SPRINT, ticks)
        .await
        .expect("walk");
    let span = tiamot_core::CHUNK_SUBNODES as f32;
    (after.chunk.z - before.chunk.z) as f32 * span + (after.local[2] - before.local[2])
}

#[test]
fn a_mod_can_slow_a_player_and_take_their_sprint_away() {
    let server = start("slow");
    block_on(async {
        let mut bot = Bot::connect(
            server.local_addr(),
            Identity::generate().expect("identity"),
            server.cert_fingerprint(),
        )
        .await
        .expect("connect");
        bot.join("Walker").await.expect("join");
        bot.sleep_ticks(10).await;

        let full = walked(&mut bot, 40).await;
        assert!(
            full > 10.0,
            "the fixture never moved: {full} cells in forty ticks"
        );

        bot.chat("slow").await.expect("chat");
        bot.sleep_ticks(10).await;
        let slowed = walked(&mut bot, 40).await;

        // A quarter of the speed, and the sprint refused on top of it — so
        // well under half. Not an exact ratio: the first few ticks are
        // acceleration, and the acceleration is scaled too.
        assert!(
            slowed < full * 0.5,
            "slowed to {slowed} cells against {full}: the multiplier did not reach the step"
        );
        assert!(slowed > 0.0, "a quarter speed is not a stop");

        // And cleared, which is the other half of "replaced whole".
        bot.chat("normal").await.expect("chat");
        bot.sleep_ticks(10).await;
        let back = walked(&mut bot, 40).await;
        assert!(
            back > full * 0.8,
            "cleared to {back} cells against the original {full}"
        );
        bot.disconnect().await;
    });
    assert!(server.stop());
}

#[test]
fn a_mod_can_grant_flight_to_somebody_who_is_not_an_operator() {
    // Ask 9's whole case: a Creative world whose builders are not all
    // administrators. `operators` above is empty, so without the grant the
    // server refuses the flight bit and gravity wins.
    let server = start("fly");
    block_on(async {
        let mut bot = Bot::connect(
            server.local_addr(),
            Identity::generate().expect("identity"),
            server.cert_fingerprint(),
        )
        .await
        .expect("connect");
        bot.join("Builder").await.expect("join");
        bot.sleep_ticks(10).await;

        // The counter-example first: flight asked for and refused.
        let before = bot
            .walk([0.0, 0.0, 0.0], actions::FLY | actions::JUMP, 40)
            .await
            .expect("walk");
        bot.chat("fly").await.expect("chat");
        bot.sleep_ticks(10).await;
        let after = bot
            .walk([0.0, 0.0, 0.0], actions::FLY | actions::JUMP, 40)
            .await
            .expect("walk");

        let span = tiamot_core::CHUNK_SUBNODES as f32;
        let risen =
            (after.chunk.y - before.chunk.y) as f32 * span + (after.local[1] - before.local[1]);
        assert!(
            risen > 5.0,
            "a body granted flight and holding jump rose {risen} cells in forty ticks"
        );
        bot.disconnect().await;
    });
    assert!(server.stop());
}
