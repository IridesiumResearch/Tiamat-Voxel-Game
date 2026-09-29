// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Riding from the client's side: a mounted player predicts the mount — Life
//! ask 18.
//!
//! `crates/bot/tests/riding.rs` proves the SERVER drives the mount from the
//! rider's keys. It cannot prove the other half, because a bot does not
//! predict. While riding, the body the server steps from this client's inputs
//! is the ENTITY — its box, its pace — so a client still predicting its own
//! body would disagree with every state it was sent, and rubber-band on
//! horseback. This is the test that the client heard which body it drives and
//! steps that one with the server's numbers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use client::app::{App, Input};
use client::cache::ContentCache;
use client::config::{Config, RenderMode};
use client::net::Connection;
use client::render::{Gpu, Renderer};
use tiamat_core::ChunkPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::transport::Impairment;
use tiamat_server::{ServerHandle, Settings};

/// The mount's pace. Far enough from 1 that a client stepping its own body's
/// tuning goes nearly twice as far as the server lets the mount go.
const PACE: f32 = 0.6;

/// How far above the mount's feet the rider sits, in blocks.
const SEAT: f64 = 1.5;

/// The longest a frame may take before the correction bound stops being
/// evidence, in seconds — see `tests/abilities.rs`, whose reasoning this is.
const LONGEST_HONEST_FRAME: f32 = 0.25;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-client-riding").join(name);
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
                std::env::var("TIAMAT_REQUIRE_GPU").is_err(),
                "TIAMAT_REQUIRE_GPU is set and no adapter was available: {err}"
            );
            println!("SKIPPING: no graphics adapter on this machine ({err})");
            None
        }
    }
}

/// Flat ground, and a mod that stands a horse-sized creature where each
/// player first stands and seats them on it.
fn write_mod(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("stable");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"stable\"\nname = \"Stable\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
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
local waiting = {{}}
game.register_on_player_join(function(event)
    waiting[event.player] = true
end)
game.register_on_tick(function()
    for uuid in pairs(waiting) do
        local id = game.player_entity(uuid)
        local body = id and game.entity(id)
        if body and body.on_ground then
            local horse = game.spawn_entity{{
                pos = {{ x = body.pos.x, y = body.pos.y, z = body.pos.z }},
                model = "engine:humanoid",
                collider = {{ width = 3, height = 4.5 }},
                speed = {PACE},
            }}
            if horse and game.mount(uuid, horse, {{ seat = {{ y = {SEAT} }} }}) then
                waiting[uuid] = nil
            end
        end
    end
end)
"#
        ),
    )
    .expect("script");
    root
}

fn embedded(name: &str) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_mod(&format!("{name}-mods"))),
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
        display_name: format!("Rider-{name}"),
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
/// **Paced** — see `tests/abilities.rs`: an unpaced loop runs the client far
/// ahead of the server and the divergence reads zero however wrong it is.
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

/// The chunk columns within `radius` of the camera the client does not hold
/// yet — see `tests/abilities.rs` for why a measurement waits for them.
fn missing_ground_around(app: &App, radius: i32) -> Vec<ChunkPos> {
    let here = app.camera().position.chunk;
    let mut missing = Vec::new();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            if dx.abs() + dz.abs() > radius {
                continue;
            }
            for dy in [0, -1] {
                let pos = ChunkPos::new(here.x + dx, here.y + dy, here.z + dz);
                if app.store().get(pos).is_none() {
                    missing.push(pos);
                }
            }
        }
    }
    missing
}

#[test]
fn a_rider_predicts_the_mount_and_an_agreeing_server_corrects_nothing() {
    let Some(gpu) = gpu() else { return };
    let server = embedded("agree");
    let mut app = client("agree", &server, gpu);

    assert!(
        run_frames(&mut app, Input::default(), 30.0, |app| app.joined()
            && app.predicting()
            && app.meshed_chunks() >= 1),
        "expected to join; warnings: {:?}",
        app.warnings()
    );
    assert!(
        run_frames(&mut app, Input::default(), 30.0, |app| {
            missing_ground_around(app, 1).is_empty()
        }),
        "the chunks around the spawn never all arrived; the client lacks {:?}",
        missing_ground_around(&app, 1)
    );
    // The server seated us, and the client took the mount as the body it
    // drives.
    assert!(
        run_frames(&mut app, Input::default(), 10.0, |app| app
            .riding()
            .is_some()),
        "the client never heard it was riding"
    );

    // Two pacing windows standing, so the seat's snap is out of the readings
    // (it is not a correction, and is not counted as one — but a divergence
    // window opened before it would still be open).
    run_frames(&mut app, Input::default(), 2.5, |_| false);
    // Sprinting, so the gait reaches the mount as well as the direction; and
    // a moment for the first reconcile after starting to move.
    let forward = Input {
        forward: 1.0,
        sprint: true,
        ..Input::default()
    };
    run_frames(&mut app, forward, 1.5, |_| false);

    let start = app.camera().position.to_world();
    let mut worst = 0.0f32;
    let mut corrected = 0.0f32;
    let mut unloaded = false;
    let mut longest = 0.0f32;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = Instant::now();
    while Instant::now() < deadline {
        assert!(
            app.pump_network(),
            "the connection ended: {:?}",
            app.warnings()
        );
        app.remesh();
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32();
        last = now;
        longest = longest.max(dt);
        app.advance(forward, dt.min(0.1));
        worst = worst.max(app.pacing().worst_divergence_cells());
        corrected = corrected.max(app.pacing().worst_correction_cells());
        unloaded |= app.pacing().predicted_into_unloaded();
        std::thread::sleep(Duration::from_millis(16));
    }
    let ended = app.camera().position.to_world();
    let (dx, dz) = (ended.0 - start.0, ended.2 - start.2);
    let rode = (dx * dx + dz * dz).sqrt();
    println!(
        "riding at {PACE}: the camera rode {rode:.2} blocks, worst divergence {worst:.3} cells, \
         worst correction {corrected:.3}, predicted into chunks it had not received: \
         {unloaded}, longest frame {:.0} ms",
        longest * 1000.0
    );
    assert!(app.riding().is_some(), "the ride ended on its own");
    assert!(rode > 1.0, "held forward and barely moved: {rode:.2}");
    if longest > LONGEST_HONEST_FRAME {
        println!(
            "SKIPPING the correction bound: a frame took {:.0} ms, past the {:.0} ms the \
             client can catch up from — see tests/abilities.rs",
            longest * 1000.0,
            LONGEST_HONEST_FRAME * 1000.0
        );
        app.shutdown();
        assert!(server.stop());
        return;
    }
    assert!(
        corrected < 0.05,
        "the rider was corrected by up to {corrected:.3} cells: the client is not predicting \
         the mount the server steps (predicted into chunks it had not received: {unloaded})"
    );
    assert!(
        worst < 0.05,
        "the rider's client disagreed with the server by up to {worst:.3} cells a tick \
         (predicted into chunks it had not received: {unloaded})"
    );

    app.shutdown();
    assert!(server.stop());
}
