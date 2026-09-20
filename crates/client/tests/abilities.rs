// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.set_player_abilities` from the client's side: a slowed player does
//! not rubber-band.
//!
//! `crates/bot/tests/abilities.rs` proves the SERVER applies a mod's speed. It
//! cannot prove the other half, because a bot does not predict. The client
//! steps its own body ahead of the server, so a speed the server applied and
//! the client did not is a disagreement every single tick — which is the whole
//! reason `ServerMessage::Abilities` exists. This is the test that the client
//! heard it and steps with the same numbers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use client::app::{App, Input};
use client::cache::ContentCache;
use client::config::{Config, RenderMode};
use client::net::Connection;
use client::render::{Gpu, Renderer};
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_server::transport::Impairment;
use tiamot_server::{ServerHandle, Settings};

/// The multiplier the fixture mod applies. Far enough from 1 that a client
/// ignoring it predicts well over twice as far as the server lets it go.
const SPEED: f32 = 0.4;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("tiamot-client-abilities")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// See `tests/screenshot.rs` for why a missing adapter skips rather than fails.
fn gpu() -> Option<Gpu> {
    match Gpu::headless() {
        Ok(gpu) => Some(gpu),
        Err(err) => {
            assert!(
                std::env::var("TIAMOT_REQUIRE_GPU").is_err(),
                "TIAMOT_REQUIRE_GPU is set and no adapter was available: {err}"
            );
            println!("SKIPPING: no graphics adapter on this machine ({err})");
            None
        }
    }
}

/// Flat ground, and a mod that slows everybody the moment they join.
fn write_mod(name: &str) -> PathBuf {
    write_mod_granting(name, &format!("speed = {SPEED}, sprint = false"))
}

/// The same, granting whatever the caller names.
fn write_mod_granting(name: &str, grant: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("slow");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"slow\"\nname = \"Slow\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        format!(
            r#"
local ground = game.register_block{{ id = "ground" }}
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_on_player_join(function(event)
    game.set_player_abilities(event.player, {{ {grant} }})
end)
"#
        ),
    )
    .expect("script");
    root
}

fn embedded(name: &str) -> ServerHandle {
    embedded_granting(name, None)
}

/// The same, with a mod that grants exactly what is named.
fn embedded_granting(name: &str, grant: Option<&str>) -> ServerHandle {
    let mods = grant.map_or_else(
        || write_mod(&format!("{name}-mods")),
        |grant| write_mod_granting(&format!("{name}-mods"), grant),
    );
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(7),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("the embedded server must start")
}

fn client(name: &str, server: &ServerHandle, gpu: Gpu) -> App {
    let home = scratch(&format!("{name}-home"));
    let config = Config {
        display_name: format!("Slowed-{name}"),
        ..Config::default()
    };
    let connection = Connection::open_impaired(
        server.local_addr(),
        Identity::generate().expect("identity"),
        config.display_name.clone(),
        ContentCache::open(&home.join("content")).expect("cache"),
        client::net::Pinning::Remembered(&home.join("known-hosts")),
        Impairment::default(),
    )
    .expect("connect");
    let renderer = Renderer::new(gpu, RenderMode::Textured, 320, 240).expect("renderer");
    App::new(config, connection, renderer)
}

/// Runs the real frame loop, paced at wall time, for `seconds` or until `done`.
///
/// **Paced, and that is load-bearing** — see `session.rs`'s flight test. An
/// unpaced loop runs the client far ahead of the server, the replay re-derives
/// everything from the client's own intents, and the divergence reads zero
/// however wrong the client is.
fn run_frames(app: &mut App, input: Input, seconds: f32, done: impl Fn(&App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f32(seconds);
    let mut last = Instant::now();
    while Instant::now() < deadline {
        assert!(
            app.pump_network(),
            "the connection ended: {:?}",
            app.warnings()
        );
        app.remesh();
        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        app.advance(input, dt);
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    done(app)
}

#[test]
fn a_slowed_player_predicts_the_speed_the_server_applies() {
    let Some(gpu) = gpu() else { return };
    let server = embedded("slowed");
    let mut app = client("slowed", &server, gpu);

    assert!(
        run_frames(&mut app, Input::default(), 30.0, |app| app.joined()
            && app.predicting()
            && app.meshed_chunks() >= 1),
        "expected to join; warnings: {:?}",
        app.warnings()
    );
    // The message arrived, and was adopted as sent.
    assert!(
        run_frames(&mut app, Input::default(), 5.0, |app| app
            .abilities()
            .speed
            .to_bits()
            == SPEED.to_bits()),
        "the client never heard the mod's speed: it has {:?}",
        app.abilities()
    );
    assert!(!app.abilities().sprint);

    // Sprinting, which the mod refused — so the client must ALSO filter the
    // gait, or it predicts a sprint the server turned into a walk.
    let forward = Input {
        forward: 1.0,
        sprint: true,
        ..Input::default()
    };
    // **Stand through two full pacing windows first.** The server spawns at a
    // fixed block, which on this flat world is one block in the air, so its
    // body drops a block over the first five ticks while the client's — with
    // no chunk under it yet, and an absent chunk reads as solid — stands
    // still. That is a real disagreement of 2.4 cells, and the windowed
    // divergence holds it for a second after it happens. Read the window it
    // lands in and every run reads 2.400, honest or not.
    run_frames(&mut app, Input::default(), 2.5, |_| false);
    // Settle: the first reconcile after starting to move is a transient.
    run_frames(&mut app, forward, 1.5, |_| false);

    let start = app.camera().position.to_world();
    let server_start = app.server_travelled();
    let mut worst = 0.0f32;
    let mut corrected = 0.0f32;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        run_frames(&mut app, forward, 0.1, |_| false);
        worst = worst.max(app.pacing().worst_divergence_cells());
        corrected = corrected.max(app.pacing().worst_correction_cells());
    }
    let ended = app.camera().position.to_world();
    let (dx, dz) = (ended.0 - start.0, ended.2 - start.2);
    let predicted = (dx * dx + dz * dz).sqrt();
    let served = app.server_travelled() - server_start;
    println!(
        "slowed to {SPEED}: client {predicted:.2} blocks, server {served:.2} blocks, \
         worst divergence {worst:.3} cells, worst correction {corrected:.3}"
    );

    assert!(
        predicted > 1.0,
        "held forward and barely moved: {predicted:.2}"
    );
    // **Not the end position**: reconciliation pulls the client back to the
    // server, so the two end within a few hundredths of a block whether the
    // client applied the speed or not. The correction and the per-tick
    // divergence are what a player sees as rubber-banding, and both were
    // checked by deliberately leaving the client's tuning at the default.
    assert!(
        corrected < 0.05,
        "the client was corrected by up to {corrected:.3} cells: it is not predicting the \
         speed the server applies"
    );
    assert!(
        worst < 0.05,
        "the client disagreed with the server by up to {worst:.3} cells a tick"
    );

    app.shutdown();
    assert!(server.stop());
}

#[test]
fn a_player_refused_the_sky_keys_cannot_wind_their_own_clock() {
    // Life ask 10a. Nothing on the server moves when a client scrubs its
    // clock, but the client draws stored sunlight scaled by the sky's
    // intensity — so winding to noon lights a player's night, which is seeing
    // in the dark for free in a world that meant its nights. Unbinding the
    // keys is not a fix: anybody can bind them again.
    let Some(gpu) = gpu() else { return };
    let server = embedded_granting("sky", Some("wind_sky = false"));
    let mut app = client("sky", &server, gpu);

    assert!(
        run_frames(&mut app, Input::default(), 30.0, |app| app.joined()
            && app.predicting()),
        "expected to join; warnings: {:?}",
        app.warnings()
    );
    assert!(
        run_frames(&mut app, Input::default(), 5.0, |app| !app
            .abilities()
            .wind_sky),
        "the client never heard the refusal: {:?}",
        app.abilities()
    );
    println!("PROBE abilities {:?}", app.abilities());

    let before = app.sky_time();
    app.nudge_time(0.3);
    app.nudge_time(-0.1);
    assert!(
        (app.sky_time() - before).abs() < 1e-6,
        "the clock moved from {before} to {} on a client that may not wind it",
        app.sky_time()
    );
    assert!(!app.time_is_local(), "the clock was taken off the server");

    app.shutdown();
    assert!(server.stop());
}
