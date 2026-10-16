// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Swarm load: twenty bots, sustained, with the server watched throughout.
//!
//! Task 07's stated test. The question is not raw speed — the macro benchmark
//! answers that — but whether a server under real concurrent load stays
//! *stable*: no tick blowing the budget by an order of magnitude, no memory
//! climbing without bound, and every bot finishing.
//!
//! The server runs in this process, where its tick samples and its resident
//! size can be read; the bots run in a child process, `bot swarm`, so that
//! what is measured is the server and not the harness (see the comment at
//! the spawn for the night that taught this).
//!
//! # Why this is `#[ignore]` by default
//!
//! It takes a minute of wall clock, which does not belong in the `cargo test`
//! a developer runs twenty times an hour. CI runs it explicitly. A test that
//! makes the fast loop slow is a test people learn to skip.
//!
//! Run it with:
//!
//! ```console
//! cargo test -p bot --test swarm_load --release -- --ignored --nocapture
//! ```
//!
//! `SWARM_BOTS` and `SWARM_SECONDS` set the count and the length (twenty for
//! sixty seconds by default; the nightly soak runs fifty for five minutes,
//! which is the performance target's player count), and `SWARM_OUT` names a
//! directory to write the tick table into as `table.txt`, so a nightly keeps
//! it as an artefact.

use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tiamat_core::identity::Allowlist;
use tiamat_core::interest::ViewDistance;
use tiamat_core::tick::TICK_DURATION;
use tiamat_server::{ServerHandle, Settings};

/// Resident set size in kibibytes, on Linux.
///
/// Returns `None` elsewhere, and the memory assertion is skipped rather than
/// faked — a memory check that silently passes on macOS and Windows is worse
/// than one that says it did not run.
#[cfg(target_os = "linux")]
fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn rss_kib() -> Option<u64> {
    None
}

/// An environment knob, or its default when unset or unparsable.
fn knob<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The reference mods, which register the tools and generate the terrain.
fn reference_mods() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../game")
        .canonicalize()
        .expect("the reference mods live at the repo root")
}

#[test]
#[ignore = "takes about a minute; CI runs it explicitly"]
fn twenty_bots_for_sixty_seconds_leave_the_server_healthy() {
    let bots: u32 = knob("SWARM_BOTS", 20);
    let seconds: u64 = knob("SWARM_SECONDS", 60);

    let dir = std::env::temp_dir().join("tiamat-swarm-load");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let server = ServerHandle::start(&Settings {
        world_options: Vec::new(),
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: dir,
        identity_path: None,
        max_players: 64,
        allow_custom_skins: false,
        sticky_names: false,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        // The reference mods, because the swarm DIGS rather than writing blocks
        // into the world. Charter rule 1 leaves the engine with no tools of its
        // own, so a modless server is one where every bot fails at its first
        // dig — and `core_worldgen` is what puts ground under them to dig.
        //
        // This test ran modless until the light work landed, which was harmless
        // only for as long as `wander` could edit the world directly. Nightly
        // caught it; nothing in the fast CI loop could, because nothing else
        // runs this workload.
        mods_path: Some(reference_mods()),
        enabled_mods: bot::fixture::enabled_mods_for(&reference_mods())
            .expect("the reference mods' manifests"),
        seed: Some(21),
        rcon: None,
        materials: vec!["load:stone".to_owned()],
    })
    .expect("start");

    let addr = server.local_addr();
    let control = server.control().clone();

    // Let startup settle, then take the baseline. Measuring from process start
    // would count the world opening and the first chunk generation as "growth".
    std::thread::sleep(Duration::from_secs(3));
    let _ = control.take_tick_samples();
    let rss_after_warmup = rss_kib();

    // **The bots run in a child process, so that this process is the server.**
    //
    // They ran here, as tasks on a runtime, and the memory gate below measured
    // them along with the server. A bot keeps every message it is sent —
    // `Bot::received` is how a scenario asserts about the wire — and beside a
    // swarm that is some 130 messages a second, so fifty bots for five minutes
    // put a gigabyte into the process under test. Measured on 2026-10-16 with
    // the two ends split: the server went from 18 to 50 MiB and flattened; the
    // bots' process climbed in a straight line to 1.08 GiB. The bound below,
    // written for twenty bots and a minute, failed nightly 78 on the runner
    // and sat at 96% of itself here, both saying nothing about the server.
    // Out of process, the gate measures what it claims to.
    let swarm_dir = std::env::temp_dir().join("tiamat-swarm-load-bots");
    let _ = std::fs::remove_dir_all(&swarm_dir);
    std::fs::create_dir_all(&swarm_dir).expect("swarm scratch dir");
    let stdout_path = swarm_dir.join("stdout.txt");
    let stderr_path = swarm_dir.join("stderr.txt");
    // Files rather than pipes: fifty bots' logging could fill a pipe nobody is
    // draining and stall the child, and a swarm that hangs should be the
    // deadline's finding, not a deadlock's.
    let mut child = Command::new(env!("CARGO_BIN_EXE_bot"))
        .arg("swarm")
        .arg(bots.to_string())
        .arg("--server")
        .arg(addr.to_string())
        .arg("--duration")
        .arg(seconds.to_string())
        .arg("--seed")
        .arg("1")
        .stdin(Stdio::null())
        .stdout(File::create(&stdout_path).expect("swarm stdout"))
        .stderr(File::create(&stderr_path).expect("swarm stderr"))
        .spawn()
        .expect("the bot binary, which cargo builds for this test");
    let grace = 180;
    let deadline = Instant::now() + Duration::from_secs(seconds + grace);
    let status = loop {
        match child.try_wait().expect("wait on the swarm") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the swarm did not finish within {grace} s of its {seconds} s run");
            }
            None => std::thread::sleep(Duration::from_millis(500)),
        }
    };
    let swarm_stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    let swarm_stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    // The swarm's own report: `bots healthy: N/M` on stdout, and one
    // `bot I failed: …` line on stderr per bot that did not finish.
    let healthy: u32 = swarm_stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("bots healthy: "))
        .and_then(|rest| rest.split('/').next())
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0);
    let failures: Vec<String> = swarm_stderr
        .lines()
        .filter(|line| line.contains("failed"))
        .map(|line| line.trim().to_owned())
        .collect();
    let samples = control.take_tick_samples();
    let report = bot::bench::TickReport::from_samples(
        &samples,
        control.over_budget_ticks(),
        control.dropped(),
        bots,
        seconds,
    );
    let rss_after_load = rss_kib();

    let mut summary = report.to_table();
    let _ = writeln!(summary, "  bots healthy: {healthy}/{bots}");
    // Which phase the worst tick spent itself on, so a nightly failure names a
    // cause rather than handing whoever reads it a bisect.
    if let Some(phases) = control.slowest_phases() {
        let _ = writeln!(summary, "  the slowest tick: {phases}");
    }
    for failure in &failures {
        let _ = writeln!(summary, "  failure: {failure}");
    }
    if let (Some(before), Some(after)) = (rss_after_warmup, rss_after_load) {
        let _ = writeln!(
            summary,
            "  RSS: {before} KiB after warmup, {after} KiB after load"
        );
    }
    // The swarm's side of it, client-observed round trips included, so the
    // artefact carries both ends.
    let _ = writeln!(summary, "  the swarm exited with {status}:");
    for line in swarm_stdout.lines() {
        let _ = writeln!(summary, "    {}", line.trim_end());
    }
    println!("{summary}");
    // Written before the assertions, so a failed gate still leaves the table
    // that says why.
    if let Some(out) = std::env::var_os("SWARM_OUT") {
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out).expect("out dir");
        std::fs::write(out.join("table.txt"), &summary).expect("table");
    }

    assert!(
        status.success(),
        "the swarm exited with {status}; failures: {failures:?}"
    );
    assert_eq!(
        healthy, bots,
        "every bot should finish; failures: {failures:?}"
    );
    assert!(
        report.ticks > 0,
        "the server should have ticked; a run with no samples proves nothing"
    );

    // **The gate is the SUSTAINED count, not the worst tick.**
    //
    // This asserted `max < 5x budget` on the grounds that "nothing a shared
    // runner does accounts for a tick five times over budget". It does, and
    // routinely: 442 ms against a mean of 1.9 ms on the 2026-09-03 nightly,
    // 1.68 s on a Windows runner on 09-04, and **7.4 seconds** on 09-06. Every
    // one of those printed a healthy server on the same line as the failure.
    //
    // A maximum over N samples measures the worst thing that happened to the
    // process. On dedicated hardware that is the code; on a shared runner it is
    // the neighbour, and chasing the threshold upward is chasing the worst
    // pause a machine nobody here owns can take — which 7.4 s shows is not a
    // knowable number.
    //
    // A tenth of a run over budget is the threshold `tick_stability` measured
    // rather than picked: 23% of ticks before a real regression, 0% after, and
    // the worst runner noise ever recorded here is 0.9%. It sits an order of
    // magnitude clear of the noise and still catches a server that stopped
    // keeping up — which the single-worst-sample check did not, because a
    // server that is uniformly slow has an unremarkable maximum.
    let ticks = report.ticks.max(1) as u64;
    assert!(
        report.over_budget * 10 < ticks,
        "{} of {ticks} ticks ran over the {TICK_DURATION:?} budget under {bots} bots. A \
         tenth of a run over budget is a server that cannot keep up, not a noisy runner. \
         Mean was {} and p99 {}.",
        report.over_budget,
        bot::bench::TickReport::budget_share(report.mean_us),
        bot::bench::TickReport::budget_share(report.p99_us)
    );

    // And the catastrophe check, with COMPANY. One tick over budget is a stall
    // by definition — the machine stopped and the server did no more work than
    // it was allowed to. A real problem produces more than one.
    let slowest = Duration::from_micros(report.max_us);
    assert!(
        report.over_budget <= 1 || slowest < TICK_DURATION * 60,
        "slowest tick was {slowest:?} ({} of budget) with {} of {ticks} over budget — \
         that is not one machine stall, it is the server",
        bot::bench::TickReport::budget_share(report.max_us),
        report.over_budget
    );

    // Memory must be bounded after warmup. A server whose RSS climbs with
    // uptime is one that dies overnight, and no throughput number matters then.
    match (rss_after_warmup, rss_after_load) {
        (Some(before), Some(after)) => {
            // Generous: fifty players' worth of chunks, sessions and dirty
            // state is real growth. What this catches is unbounded growth, not
            // working set. The bound predates the bots leaving this process;
            // the server alone measured 18 → 50 MiB over fifty bots and five
            // minutes (2026-10-16), so it can tighten once the nightly's
            // tables have shown that number a few times.
            let limit = before.saturating_mul(3).max(before + 512 * 1024);
            assert!(
                after <= limit,
                "RSS grew from {before} KiB to {after} KiB under load, past the {limit} KiB \
                 bound. That is the shape of a leak rather than a working set."
            );
        }
        _ => println!("  (RSS unavailable on this platform; the memory check did not run)"),
    }

    assert!(server.stop(), "the server should shut down cleanly");
}
