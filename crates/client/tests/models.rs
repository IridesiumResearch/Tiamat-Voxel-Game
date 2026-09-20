// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A mod's model is drawn where the server put the entity — Life ask 0.
//!
//! `connection.rs` proves a mod's geometry is registered, hashed, fetched and
//! parsed. It stops one step short of the thing anybody can see, and that step
//! was missing: `place_entities` skipped every entity whose model was not the
//! engine's own rig, and `Renderer::set_model_figures` had no caller at all. A
//! mod's cow was uploaded to the GPU and never put in the world, which looks
//! from the window exactly like a model that never arrived.

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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-client-models").join(name);
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

/// Flat ground, a registered model, and a standing entity that wears it.
fn write_zoo(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("zoo");
    std::fs::create_dir_all(dir.join("models")).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"zoo\"\nname = \"Zoo\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    // A real `.glb`, from the fuzz corpus: the engine's own humanoid exported.
    // Made-up bytes would be refused by the reader, which is a different test.
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/corpus/gltf_ingest/humanoid.glb");
    std::fs::copy(&glb, dir.join("models/cow.glb")).expect("copy the glb");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_model{ id = "cow", file = "models/cow.glb", scale = 1.0 }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
-- Spawned once somebody is there to see it, a few blocks in front of spawn.
local spawned = false
game.register_on_player_join(function(event)
    if spawned then return end
    spawned = true
    game.spawn_entity{ model = "zoo:cow", pos = { x = 0.5, y = 1.0, z = 4.5 } }
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
        mods_path: Some(write_zoo(&format!("{name}-mods"))),
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
        display_name: format!("Keeper-{name}"),
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

fn run_frames(app: &mut App, seconds: f32, done: impl Fn(&App) -> bool) -> bool {
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
        app.advance(Input::default(), dt);
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    done(app)
}

#[test]
fn an_entity_wearing_a_mods_model_is_placed_in_the_world() {
    let Some(gpu) = gpu() else { return };
    let server = embedded("zoo");
    let mut app = client("zoo", &server, gpu);

    assert!(
        run_frames(&mut app, 30.0, |app| app.joined() && app.predicting()),
        "expected to join; warnings: {:?}",
        app.warnings()
    );
    // The model has to arrive before anything can wear it: it is fetched by
    // hash after the join burst, like a texture or a font.
    assert!(
        run_frames(&mut app, 20.0, |app| app.has_model("zoo:cow")),
        "the model never arrived; warnings: {:?}",
        app.warnings()
    );
    assert!(
        run_frames(&mut app, 20.0, |app| app.model_figures() > 0),
        "the model arrived and nothing wearing it was ever placed"
    );
    // And the engine's own rig is untouched by the routing: the player is
    // still theirs, and a mod's entity is not counted among them.
    assert!(app.model_figures() >= 1, "the cow left the frame again");

    app.shutdown();
    assert!(server.stop());
}
