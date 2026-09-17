// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only
//! Terrain that costs more than a tick, generated off it.
//!
//! # The report
//!
//! 2026-09-13, from the window, with every player standing still: tick after
//! tick at 50–100 ms, each `serving 0 chunks, 1 summaries` and `gen 69.2ms`.
//! The terrain mod's chunk had come to cost more than the whole tick, and the
//! serve clock cannot help with that — a generation call is indivisible, and
//! one request is always served so a slow machine still finishes loading. With
//! a 60 ms chunk, that floor *is* the overrun.
//!
//! So generation left the tick: `tiamot_server::worldgen` runs each mod set in
//! its own VM on worker threads, and the tick adopts, lights, encodes and sends
//! what comes back. This file holds the mechanism through the real endpoint.
//!
//! # What is asserted, and what deliberately is not
//!
//! Not a wall-clock cost — a shared CI runner cannot promise one. The generator
//! here burns a few million Lua instructions per chunk, which is tens of
//! milliseconds anywhere and several ticks' worth on a slow runner; the test
//! asserts that the tick did not pay for it: chunks arrive, the workers made
//! them, no tick was dropped, and the over-budget count stays far below what
//! one generation per tick on the tick would produce (every tick, for as long
//! as the world fills).
use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_server::{ServerHandle, Settings};

/// What the fixture's `return 0.2, 0.8, 0.4` comes back as.
///
/// **A multiplier, quantised on the tint's scale where 128 is 1.0** — not a
/// colour over 0..255. Written through `Tint::quantise` rather than as three
/// magic numbers, so the day that scale moves again this says what it means
/// instead of what it used to equal.
fn declared() -> [u8; 3] {
    [0.2, 0.8, 0.4].map(tiamot_core::proto::Tint::quantise)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-offtick").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod whose generator is deliberately expensive: a Lua loop that costs tens
/// of milliseconds a chunk before it fills anything, and a biome tint so the
/// colour path is exercised through the same endpoint.
fn write_slow_generator(name: &str) -> PathBuf {
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
        "local stone = game.register_block{ id = \"stone\" }\n\
         local field = game.density{\n\
         \x20   op = \"sub\",\n\
         \x20   a = { op = \"noise\", stream = \"terrain\", frequency = 0.03, octaves = 2 },\n\
         \x20   b = { op = \"mul\", a = { op = \"y\" }, b = { op = \"const\", value = 0.06 } },\n\
         }\n\
         game.register_chunk_tint(function(pos)\n\
         \x20   return 0.2, 0.8, 0.4\n\
         end)\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   local burn = 0\n\
         \x20   for i = 1, 1500000 do burn = burn + (i % 7) end\n\
         \x20   buf:fill_density(field, stone, { detail = \"sampled\" })\n\
         end)\n",
    )
    .expect("script");
    root
}

fn start(name: &str, mods: PathBuf) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance {
            horizontal: 2,
            vertical: 2,
        },
        mods_path: Some(mods),
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

#[test]
fn expensive_terrain_is_generated_by_the_workers_and_the_tick_keeps_its_budget() {
    let server = start("expensive", write_slow_generator("expensive-mods"));
    let control = server.control().clone();
    let addr = server.local_addr();
    let fingerprint = server.cert_fingerprint();
    let expected = tiamot_core::interest::chunks_around(
        tiamot_core::BlockPos::new(0, 1, 0).chunk(),
        ViewDistance {
            horizontal: 2,
            vertical: 2,
        },
    )
    .len();
    block_on(async {
        let started = control.tick();
        let mut alice = Bot::connect(addr, Identity::generate().expect("identity"), fingerprint)
            .await
            .expect("connect");
        alice.join("Alice").await.expect("join");
        let arrived = alice
            .collect_chunks(expected, Duration::from_secs(120))
            .await
            .expect("chunks");
        assert_eq!(
            arrived.len(),
            expected,
            "the whole neighbourhood should arrive, however slow the generator"
        );
        let ran = control.tick().saturating_sub(started);

        // **The mechanism, not the machine.** Every chunk was generated off the
        // tick; had they been generated on it, every tick with a request in it
        // would have run over — which is every tick until the world filled.
        assert!(
            control.generated_off_tick() as usize >= expected,
            "{} chunks came from the workers for {expected} served",
            control.generated_off_tick()
        );
        assert_eq!(
            control.dropped(),
            0,
            "the tick fell behind and dropped ticks"
        );
        let over = control.over_budget_ticks();
        assert!(
            over * 4 < ran.max(1),
            "{over} of {ran} ticks ran over budget: the generator's cost reached the tick"
        );
        println!(
            "off-tick generation: {expected} chunks over {ran} ticks, {over} over budget, {} from workers",
            control.generated_off_tick()
        );

        // And the colour came with them: a mod's tint reaches the client through
        // the endpoint, which it never did before the generator forwarded it.
        let tints = alice.chunk_tints_received();
        assert!(!tints.is_empty());
        for (pos, tint) in &tints {
            assert_eq!(*tint, declared(), "the tint for {pos:?} is not the mod's");
        }
        alice.disconnect().await;
    });
    assert!(server.stop(), "clean shutdown with workers running");
}

#[test]
fn a_summary_of_land_never_visited_is_generated_off_the_tick() {
    // The case from the report: horizon summaries, each a whole chunk's
    // generation for a silhouette. With the workers, a summary request for a
    // chunk nobody has stood on is parked and answered when the worker is
    // done, and the tick counts it as generated off the tick.
    let server = start("horizon", write_slow_generator("horizon-mods"));
    let control = server.control().clone();
    let addr = server.local_addr();
    let fingerprint = server.cert_fingerprint();
    block_on(async {
        let mut alice = Bot::connect(addr, Identity::generate().expect("identity"), fingerprint)
            .await
            .expect("connect");
        alice.join("Alice").await.expect("join");
        // The detail radius is tiny here, so the horizon starts almost at once;
        // wait for a few summaries to land.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let mut summaries = 0;
        while summaries < 4 && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(5), alice.recv()).await {
                Ok(Ok(tiamot_core::proto::ServerMessage::ChunkSummary { .. })) => summaries += 1,
                Ok(Ok(_)) => {}
                Ok(Err(err)) => panic!("connection failed: {err}"),
                Err(_) => {}
            }
        }
        assert!(
            summaries >= 4,
            "only {summaries} summaries arrived in two minutes"
        );
        assert_eq!(control.dropped(), 0);
        alice.disconnect().await;
    });
    assert!(server.stop());
}

/// A mod that reports whether `game.world_seed` is the world's seed from both
/// kinds of VM it runs in: a tick (the tick's VM) and a chunk tint (a worker's).
fn write_seed_reader(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("seeded");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"seeded\"\nname = \"Seeded\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local ground = game.register_block{ id = \"ground\" }\n\
         game.register_block{ id = \"marker\" }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n\
         game.register_chunk_tint(function(pos)\n\
         \x20   if game.world_seed ~= nil and game.world_seed == pos.seed then\n\
         \x20       return 0.2, 0.8, 0.4\n\
         \x20   end\n\
         \x20   return 1, 0, 0\n\
         end)\n\
         local turn = 0\n\
         game.register_on_tick(function()\n\
         \x20   turn = turn + 1\n\
         \x20   if turn ~= 10 then return end\n\
         \x20   if game.world_seed == 5 then\n\
         \x20       game.set_block({ x = 1, y = 8, z = 1 }, \"seeded:marker\")\n\
         \x20   else\n\
         \x20       game.set_block({ x = 3, y = 8, z = 1 }, \"seeded:marker\")\n\
         \x20   end\n\
         end)\n",
    )
    .expect("script");
    root
}

#[test]
fn every_vm_knows_the_world_seed() {
    // Asked for by the world mod: a generator is handed the seed on `pos`, and
    // nothing that runs on the tick was, so a spawn aimed by sampling the
    // terrain density had no seed to sample with. Both VMs through the real
    // server, because a setter only a unit test calls is a setter nothing calls.
    let server = start("seeded", write_seed_reader("seeded-mods"));
    let addr = server.local_addr();
    let fingerprint = server.cert_fingerprint();
    block_on(async {
        let mut alice = Bot::connect(addr, Identity::generate().expect("identity"), fingerprint)
            .await
            .expect("connect");
        alice.join("Alice").await.expect("join");
        let marker = alice
            .material_table()
            .expect("a material table on join")
            .into_iter()
            .find(|entry| entry.name == "seeded:marker")
            .map(|entry| entry.id)
            .expect("the mod registers a marker");

        alice
            .expect_block(
                tiamot_core::BlockPos::new(1, 8, 1),
                marker,
                Duration::from_secs(20),
            )
            .await
            .expect("the tick's VM should read the world seed");

        let tints = alice.chunk_tints_received();
        assert!(!tints.is_empty(), "no chunk arrived to carry a tint");
        for (pos, tint) in &tints {
            assert_eq!(
                *tint,
                declared(),
                "the generating VM for {pos:?} did not know the world seed"
            );
        }
        assert!(
            server.control().generated_off_tick() > 0,
            "nothing was generated by a worker, so the worker VM was not asked"
        );
        alice.disconnect().await;
    });
    assert!(server.stop());
}
