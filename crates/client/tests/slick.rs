// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A floor's `friction` from the client's side: a player sliding on ice does
//! not rubber-band. World ask 27.
//!
//! The server slides a body on a slick floor; the client predicts its own
//! movement, so it has to slide the same way or every step on ice is a
//! correction. That is why `friction` travels in `MaterialDef` rather than
//! staying on the server, and this is the test that it arrived and is stepped
//! with — measured against a client deliberately left without it.

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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-client-slick").join(name);
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

/// A flat world floored with ice.
fn write_mod(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("slick");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"slick\"\nname = \"Slick\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ice = game.register_block{ id = "ice", friction = 0.1 }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ice)
end)
"#,
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
        display_name: format!("Skater-{name}"),
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
/// The longest a frame may take before the correction bound stops being
/// evidence, in seconds — see `abilities.rs`, which says why: past this the
/// client is behind real time, the next server state is ahead of anything it
/// predicted, and the correction that follows would be there whatever the
/// client stepped with. Seen on macOS CI as 0.365 cells of correction on ice.
const LONGEST_HONEST_FRAME: f32 = 0.25;

/// The chunk columns within `radius` of the body that the client does not
/// hold yet, at the body's own level and the one under its feet — taxicab, the
/// interest set's own shape. An absent chunk reads as solid to prediction, so
/// a slide that starts before the ice ahead has arrived is stopped by a wall
/// the server does not have. See `abilities.rs`.
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
fn a_player_on_ice_predicts_the_slide_the_server_applies() {
    let Some(gpu) = gpu() else { return };
    let server = embedded("ice");
    let mut app = client("ice", &server, gpu);

    assert!(
        run_frames(&mut app, Input::default(), 30.0, |app| app.joined()
            && app.predicting()
            && app.meshed_chunks() >= 1),
        "expected to join; warnings: {:?}",
        app.warnings()
    );
    // And the ice the slide will cross, before anything is measured.
    assert!(
        run_frames(&mut app, Input::default(), 30.0, |app| {
            missing_ground_around(app, 1).is_empty()
        }),
        "the ice around the spawn never all arrived; the client holds {} and lacks {:?}",
        app.store().len(),
        missing_ground_around(&app, 1)
    );
    // Stand through the join's own disagreement — the fixed spawn is a block
    // above this floor, see `abilities.rs` — and two pacing windows after it.
    run_frames(&mut app, Input::default(), 2.5, |_| false);

    let forward = Input {
        forward: 1.0,
        ..Input::default()
    };
    let mut worst = 0.0f32;
    let mut corrected = 0.0f32;
    // Read beside the two above — see `abilities.rs` for both: a correction
    // with `unloaded` set is a wall the client invented, and one after a
    // frame past `LONGEST_HONEST_FRAME` is the runner, not the ice.
    let mut unloaded = false;
    let mut longest = 0.0f32;
    let mut watch = |app: &mut App, input: Input, seconds: f32| {
        let deadline = Instant::now() + Duration::from_secs_f32(seconds);
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
            app.advance(input, dt.min(0.1));
            worst = worst.max(app.pacing().worst_divergence_cells());
            corrected = corrected.max(app.pacing().worst_correction_cells());
            unloaded |= app.pacing().predicted_into_unloaded();
            std::thread::sleep(Duration::from_millis(16));
        }
    };
    // Push off, then let go and glide: the glide is where a client that did
    // not know the floor was slick would stop dead and be dragged on.
    watch(&mut app, forward, 2.0);
    let released = app.server_travelled();
    watch(&mut app, Input::default(), 2.0);
    let glided = app.server_travelled() - released;
    println!(
        "on ice: pushed {released:.2} blocks, glided {glided:.2} more; worst divergence \
         {worst:.3} cells, worst correction {corrected:.3}, predicted into chunks it had \
         not received: {unloaded}, longest frame {:.0} ms",
        longest * 1000.0
    );

    // The server slid. On stone a body stops in about half a block.
    assert!(
        glided > 1.5,
        "let go on ice, the server moved the body only {glided:.2} more blocks"
    );
    // Printed whether it is checked or not, as `abilities.rs` does.
    if longest > LONGEST_HONEST_FRAME {
        println!(
            "SKIPPING the correction bounds: a frame took {:.0} ms, past the {:.0} ms the \
             client can catch up from, so the server got ahead of anything the client \
             predicted and the correction says nothing about ice. The slide itself was \
             asserted above.",
            longest * 1000.0,
            LONGEST_HONEST_FRAME * 1000.0
        );
        app.shutdown();
        assert!(server.stop());
        return;
    }
    // And the client predicted it. Checked by breaking it: with the client's
    // table left empty, the same run read 1.674 cells of divergence and 0.463
    // of correction against 0.000 and 0.000 here, over the same 5.5-block
    // glide on the server.
    assert!(
        corrected < 0.05,
        "the client was corrected by up to {corrected:.3} cells on ice"
    );
    assert!(
        worst < 0.05,
        "the client disagreed with the server by up to {worst:.3} cells a tick on ice"
    );

    app.shutdown();
    assert!(server.stop());
}
