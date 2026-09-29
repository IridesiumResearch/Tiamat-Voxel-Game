// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A flight over the default world's peat fen: does the tick stay flat?
//!
//! # The report this is about
//!
//! 2026-09-28, singleplayer, flying over `peat_fen` with the default mods:
//! "as i play a world the world slowly gets slower". The first run of this
//! probe reproduced it — `save chunks` in over-budget lines climbing from 7 ms
//! to 38 ms across four minutes of flight and 40–46 ms after stopping, and
//! over-budget ticks rising from 2 to 55 per thirty seconds — and found why:
//! every chunk saved forgot its summaries with a delete the primary key could
//! not narrow, so each save walked every summary the world had ever stored.
//! The table only grows, so the world got slower with age.
//!
//! # What it measures
//!
//! One bot, the real default mods, a real server with its log captured. The
//! bot stands at spawn, is teleported to the fen (`/tp peat fen`, the world
//! mod's command), rises, and flies tangent to the biome rings at sprint for
//! four minutes, then hovers for two. Every second it samples the server's
//! tick durations and the running `PhaseLedger` — the per-phase means over
//! EVERY tick, not only the ones that ran over — and at the end prints one
//! row per thirty seconds of flight: over-budget ticks, the mean tick, and
//! the means of `save chunks`, `fluid` and `serving` with the pieces inside
//! them, beside the summary rows the world holds.
//!
//! **What passes**: `save chunks` flat across the flight and the hover, and
//! the over-budget count in the last windows of flight near the first. A
//! number that climbs with the minutes is a cost that grows with the world.
//! Nothing is asserted — wall-clock numbers on a shared machine are for a
//! person to read, not a gate to trip — but the table is the verdict.
//!
//! # Why this is `#[ignore]`, and what it needs
//!
//! It runs for about seven minutes, and it needs the default mods, which are
//! separate repositories: symlink `tiamat_default_world`, `_life`, `_ui`,
//! `_craft`, `_progress` and `tiamat_weather` into `game/` first. With any
//! enabled mod missing it names them and passes: the server would refuse to
//! start, and without the world mod there is no fen to fly over.
//!
//! ```console
//! FEN_OUT=/tmp/fen cargo test --release -p bot --test fen_soak -- --ignored --nocapture
//! ```
//!
//! `FEN_OUT` gets `server.log`, `samples.csv` (one row a second) and
//! `table.txt`. `FEN_FLY`, `FEN_STAND` and `FEN_SETTLE` are the phase lengths
//! in seconds, `FEN_SEED` the world seed, `FEN_MODS` the enabled mod list.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::actions;
use tiamat_server::sim::PhaseLedger;
use tiamat_server::{ServerHandle, Settings};

/// Seconds per row of the final table.
const WINDOW_SECONDS: f64 = 30.0;

/// A tick over this many microseconds ran over its budget.
const BUDGET_MICROS: u32 = 50_000;

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("FEN_OUT")
        .map_or_else(|| std::env::temp_dir().join("fen-soak"), PathBuf::from);
    std::fs::create_dir_all(&dir).expect("out dir");
    dir
}

fn seconds(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Where the bot is, in blocks.
fn blocks(at: &bot::client::PlayerPosition) -> [f64; 3] {
    let span = f64::from(tiamat_core::CHUNK_SUBNODES);
    let per = f64::from(tiamat_core::SUBNODES_PER_AXIS);
    [
        (f64::from(at.chunk.x) * span + f64::from(at.local[0])) / per,
        (f64::from(at.chunk.y) * span + f64::from(at.local[1])) / per,
        (f64::from(at.chunk.z) * span + f64::from(at.local[2])) / per,
    ]
}

/// A duration in milliseconds, for the table.
fn ms(took: Duration) -> f64 {
    took.as_secs_f64() * 1000.0
}

/// One second of the run.
struct Sample {
    /// Seconds since the run began.
    at: f64,
    phase: &'static str,
    /// Tick durations in that second, microseconds.
    ticks: Vec<u32>,
    /// The server's running ledger at the end of it.
    ledger: PhaseLedger,
    summary_rows: u64,
    dirty: u64,
    resident: u64,
}

struct Sampler {
    csv: std::fs::File,
    started: Instant,
    samples: Vec<Sample>,
    last: PhaseLedger,
}

impl Sampler {
    fn row(&mut self, server: &ServerHandle, phase: &'static str, at: [f64; 3], moved: f64) {
        let control = server.control();
        let ticks = control.take_tick_samples();
        let ledger = control.phase_ledger();
        let second = ledger.since(&self.last);
        let n = ticks.len().max(1) as f64;
        let mean = ticks.iter().map(|&t| f64::from(t)).sum::<f64>() / n / 1000.0;
        let over = ticks.iter().filter(|&&t| t > BUDGET_MICROS).count();
        let sample = Sample {
            at: self.started.elapsed().as_secs_f64(),
            phase,
            ticks,
            ledger: ledger.clone(),
            summary_rows: control.summary_rows(),
            dirty: control.dirty_chunks(),
            resident: control.resident_chunks(),
        };
        let line = format!(
            "{:.1},{phase},{:.1},{:.1},{:.1},{moved:.2},{},{mean:.2},{over},{:.2},{:.2},{:.2},{:.2},{:.2},{},{},{},{}",
            sample.at,
            at[0],
            at[1],
            at[2],
            sample.ticks.len(),
            ms(second.mean("save chunks")),
            ms(second.mean("fluid")),
            ms(second.mean("serving")),
            ms(second.part_mean("serving", "relight")),
            ms(sql_mean(&second)),
            sample.summary_rows,
            sample.dirty,
            sample.resident,
            server.shared().queued_chunk_requests(),
        );
        let _ = writeln!(self.csv, "{line}");
        let _ = self.csv.flush();
        tracing::warn!(target: "fen_soak", "SAMPLE {line}");
        self.last = ledger;
        self.samples.push(sample);
    }

    /// One row per thirty seconds from the start of the flight.
    fn table(&self, flight_began: f64) -> String {
        let mut windows: BTreeMap<i64, Vec<&Sample>> = BTreeMap::new();
        for sample in &self.samples {
            let window =
                tiamat_core::detgen::floor_to_i64((sample.at - flight_began) / WINDOW_SECONDS);
            windows.entry(window).or_default().push(sample);
        }
        let mut out = String::new();
        let _ = writeln!(
            out,
            "window | phase      | ticks | over | mean  | save chunks (per save) | fluid (solver, bcast, edits, hooks, relight; active) | serving (relight, bcast, locks, fluid) | light | sqlite | summary rows | dirty | resident"
        );
        let mut before = PhaseLedger::default();
        for (window, samples) in &windows {
            let Some(last) = samples.last() else { continue };
            let span = last.ledger.since(&before);
            before = last.ledger.clone();
            let ticks: Vec<u32> = samples
                .iter()
                .flat_map(|s| s.ticks.iter().copied())
                .collect();
            let over = ticks.iter().filter(|&&t| t > BUDGET_MICROS).count();
            let mut phases: Vec<&str> = samples.iter().map(|s| s.phase).collect();
            phases.dedup();
            let saves = span.appearances("save chunks").max(1);
            let per_save = span.mean("save chunks") * u32::try_from(span.ticks()).unwrap_or(0)
                / u32::try_from(saves).unwrap_or(1);
            let _ = writeln!(
                out,
                "{:+5}s | {:<10} | {:5} | {:4} | {:5.1} | {:5.2} ({:5.2}) | {:5.2} ({:.2}, {:.2}, {:.2}, {:.2}, {:.2}; {}) | {:5.2} ({:.2}, {:.2}, {:.2}, {:.2}) | {:5.2} | {:5.2} | {:7} | {:5} | {:5}",
                window * WINDOW_SECONDS as i64,
                phases.join("/"),
                ticks.len(),
                over,
                ms(span.tick_mean()),
                ms(span.mean("save chunks")),
                ms(per_save),
                ms(span.mean("fluid")),
                ms(span.part_mean("fluid", "solver")),
                ms(span.part_mean("fluid", "broadcast")),
                ms(span.part_mean("fluid", "edits")),
                ms(span.part_mean("fluid", "hooks")),
                ms(span.part_mean("fluid", "relight")),
                span.count_mean("fluid", "active").unwrap_or(0),
                ms(span.mean("serving")),
                ms(span.part_mean("serving", "relight")),
                ms(span.part_mean("serving", "broadcast")),
                ms(span.part_mean("serving", "locks")),
                ms(span.part_mean("serving", "fluid load")),
                ms(span.mean("light")),
                ms(sql_mean(&span)),
                last.summary_rows,
                last.dirty,
                last.resident,
            );
        }
        out
    }
}

/// Time in the database per tick, every phase together.
fn sql_mean(ledger: &PhaseLedger) -> Duration {
    [
        "edits",
        "players",
        "digging",
        "placing",
        "mods",
        "serving",
        "light",
        "player hooks",
        "entities",
        "fluid",
        "unload",
        "save chunks",
        "save mod state",
        "save entities",
        "saving",
    ]
    .into_iter()
    .map(|phase| ledger.part_mean(phase, "sqlite"))
    .sum()
}

#[test]
#[ignore = "a seven-minute soak over the default mods, run by hand"]
fn fly_over_the_fen() {
    let mods = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../game")
        .canonicalize()
        .expect("game dir");
    let enabled: Vec<String> = std::env::var("FEN_MODS")
        .unwrap_or_else(|_| {
            "core,core_sky,tiamat_default_world,tiamat_default_life,tiamat_weather,\
             tiamat_default_ui,tiamat_default_craft,tiamat_default_progress"
                .to_owned()
        })
        .split(',')
        .map(|name| name.trim().to_owned())
        .collect();
    // **Every enabled mod, by the id its manifest gives**, not only the world
    // mod and not by directory name: a server refuses to start with an
    // enabled mod missing, and that would fail here as a panic in `start`
    // rather than as the skip the module promises. A symlink to a checkout
    // that is not there reads as missing, too.
    let found: std::collections::BTreeSet<String> = tiamat_core::modload::scan_directory(&mods)
        .expect("game/ has a mod whose manifest does not parse")
        .into_iter()
        .map(|found| found.manifest.id)
        .collect();
    let missing: Vec<&str> = enabled
        .iter()
        .filter(|id| !found.contains(id.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "fen_soak: not in game/: {} — symlink the default mods in to run it",
            missing.join(", ")
        );
        return;
    }

    let out = out_dir();
    let log = std::fs::File::create(out.join("server.log")).expect("log file");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log))
        .init();

    let fly_seconds = seconds("FEN_FLY", 240);
    let stand_seconds = seconds("FEN_STAND", 120);
    let settle_seconds = seconds("FEN_SETTLE", 30);

    let world = out.join("world");
    let _ = std::fs::remove_dir_all(&world);
    std::fs::create_dir_all(&world).expect("world dir");

    let identity = Identity::generate().expect("identity");
    let operator = identity.uuid_as_root().to_hex();
    tracing::warn!(target: "fen_soak", "PHASE start mods={enabled:?}");

    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: world,
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: vec![operator],
        view_distance: ViewDistance::DEFAULT,
        mods_path: Some(mods),
        enabled_mods: Some(enabled),
        seed: Some(seconds("FEN_SEED", 20_260_928)),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start");

    let mut csv = std::fs::File::create(out.join("samples.csv")).expect("csv");
    let _ = writeln!(
        csv,
        "t,phase,x,y,z,moved,ticks,mean_ms,over50,save_chunks_ms,fluid_ms,serving_ms,serving_relight_ms,sqlite_ms,summary_rows,dirty,resident,queued_requests"
    );
    let mut sampler = Sampler {
        csv,
        started: Instant::now(),
        samples: Vec::new(),
        last: PhaseLedger::default(),
    };

    let flight_began = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let mut bot = Bot::connect(server.local_addr(), identity, server.cert_fingerprint())
                .await
                .expect("connect");
            bot.join("Flyer").await.expect("join");
            tracing::warn!(target: "fen_soak", "PHASE joined");

            // Spawn: stand for fifteen seconds.
            let mut at = bot.walk([0.0; 3], 0, 1).await.expect("walk");
            for _ in 0..15 {
                at = bot.walk([0.0; 3], 0, 20).await.expect("walk");
                sampler.row(&server, "spawn", blocks(&at), 0.0);
            }
            bot.chat("/tp peat fen").await.expect("chat");
            tracing::warn!(target: "fen_soak", "PHASE tp asked");
            for _ in 0..settle_seconds {
                at = bot.walk([0.0; 3], 0, 20).await.expect("walk");
                sampler.row(&server, "settle", blocks(&at), 0.0);
            }
            for notice in bot.notices() {
                tracing::warn!(target: "fen_soak", "NOTICE {notice}");
            }
            let here = blocks(&at);
            tracing::warn!(
                target: "fen_soak",
                "PHASE landed at {:.1} {:.1} {:.1}",
                here[0],
                here[1],
                here[2]
            );

            // Tangent to the rings, so the flight stays in one ring.
            let radius = (here[0] * here[0] + here[2] * here[2]).sqrt();
            let heading = if radius > 1.0 {
                [(-here[2] / radius) as f32, 0.0, (here[0] / radius) as f32]
            } else {
                [0.0, 0.0, 1.0]
            };
            tracing::warn!(target: "fen_soak", "PHASE rise, heading {heading:?}");
            at = bot
                .walk([0.0; 3], actions::FLY | actions::JUMP, 40)
                .await
                .expect("rise");

            tracing::warn!(target: "fen_soak", "PHASE fly start");
            let flight_began = sampler.started.elapsed().as_secs_f64();
            let flying = Instant::now();
            let mut last = blocks(&at);
            let mut stuck = false;
            while flying.elapsed() < Duration::from_secs(fly_seconds) {
                let mut act = actions::FLY | actions::SPRINT;
                if stuck {
                    act |= actions::JUMP;
                }
                at = bot.walk(heading, act, 20).await.expect("fly");
                let now = blocks(&at);
                let (dx, dz) = (now[0] - last[0], now[2] - last[2]);
                let moved = (dx * dx + dz * dz).sqrt();
                stuck = moved < 8.0;
                last = now;
                sampler.row(&server, "fly", now, moved);
            }
            tracing::warn!(target: "fen_soak", "PHASE stand start");
            let standing = Instant::now();
            while standing.elapsed() < Duration::from_secs(stand_seconds) {
                at = bot.walk([0.0; 3], actions::FLY, 20).await.expect("hover");
                sampler.row(&server, "stand", blocks(&at), 0.0);
            }
            tracing::warn!(target: "fen_soak", "PHASE stand end");
            tracing::warn!(
                target: "fen_soak",
                "END chunks received: {}",
                bot.chunks_received().len()
            );
            bot.disconnect().await;
            flight_began
        });

    let table = sampler.table(flight_began);
    println!("{table}");
    std::fs::write(out.join("table.txt"), &table).expect("table");
    for line in table.lines() {
        tracing::warn!(target: "fen_soak", "TABLE {line}");
    }
    let stopping = Instant::now();
    let ok = server.stop();
    tracing::warn!(
        target: "fen_soak",
        "END server stopped ok={ok} in {:.1}s",
        stopping.elapsed().as_secs_f64()
    );
}
