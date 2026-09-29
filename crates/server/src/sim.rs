// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The simulation thread.
//!
//! Owns the world and advances it at a fixed 20 Hz. Everything that mutates
//! world state happens here, on one thread, in tick order — which is what makes
//! the world reproducible. The network layer hands work *in* and reads state
//! *out*; it never touches the world directly.
//!
//! # Why the loop is generic over its clock
//!
//! The pacing rules — when to sleep, when to run several ticks, when to give up
//! on catching up — are the part most likely to be wrong, and they are exactly
//! the part that a wall-clock test cannot pin down without being slow and
//! flaky. So [`run`] takes a [`Clock`], and the tests drive it with a fake one
//! that advances by however much the test says. A test for "the server survives
//! a ten-second stall" then takes microseconds and gives the same answer every
//! time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tiamat_core::tick::{Accumulator, TICK_DURATION};
use tracing::{info, warn};

/// A source of elapsed time and a way to wait.
///
/// Deliberately elapsed-only. There is no way to ask this for "now", so there
/// is no way for pacing code to accidentally depend on wall-clock time, and no
/// way for a clock adjustment to move the simulation.
pub trait Clock {
    /// Time since the previous call. The first call reports time since start.
    fn tick_elapsed(&mut self) -> Duration;

    /// Waits for approximately `duration`.
    fn sleep(&mut self, duration: Duration);
}

/// The real clock: a monotonic [`Instant`] and a thread sleep.
pub struct MonotonicClock {
    last: Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock {
    /// Starts the clock now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last: Instant::now(),
        }
    }
}

impl Clock for MonotonicClock {
    fn tick_elapsed(&mut self) -> Duration {
        // `Instant` is monotonic by contract on every platform Rust supports,
        // so this can never go backwards — which is the whole reason the
        // accumulator takes durations rather than timestamps.
        let now = Instant::now();
        let elapsed = now.duration_since(self.last);
        self.last = now;
        elapsed
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Where one tick's time went, phase by phase.
///
/// # Why the tick measures itself
///
/// "Eleven of six hundred ticks ran over the 50 ms budget" is a fact nobody can
/// act on. The tick does a dozen different jobs — it serves chunks, relights
/// them, runs every mod's hooks, steps two hundred bodies, moves fluid and
/// writes the world to disk — and which of them spent the 135 ms is the whole
/// question. Without this the answer is a bisect through constants, and a load
/// test that fails on a nightly runner nobody can attach a profiler to says
/// only that something got slower.
///
/// So: a mark at each phase boundary, and a named breakdown whenever a tick
/// runs over. It costs one `Instant::now()` per phase — about a dozen a tick,
/// some tens of nanoseconds — and the `String` is built only for a tick that
/// has already lost its budget.
///
/// # And what is inside a phase
///
/// A phase that does several jobs is still one number, and "fluid 45 ms" was
/// the line that could not be acted on next: a solver whose visits are
/// capped, a broadcast per touched chunk, terrain edits, mod hooks and the
/// tick's relight all sit inside it. So a phase can carry named pieces
/// ([`Phases::part`]), the sizes that explain it ([`Phases::count`]), and the
/// time it spent in the world database ([`Phases::attribute_sql`]), each
/// printed in parentheses after the phase it belongs to.
#[derive(Debug)]
pub struct Phases {
    started: Instant,
    at: Instant,
    spans: Vec<(&'static str, Duration)>,
    /// What a phase was made of, where one phase does several jobs.
    parts: Vec<Part>,
    /// Sizes that say why a phase cost what it did: `(phase, what, how many)`.
    counts: Vec<(&'static str, &'static str, u64)>,
    /// Time in the world database, per phase: `(phase, time, calls)`.
    sql: Vec<(&'static str, Duration, u32)>,
    /// Database calls over [`crate::sqlclock::SLOW_CALL`]: `(phase, what, time)`.
    slow_sql: Vec<(&'static str, &'static str, Duration)>,
    /// The WAL's size in bytes, once a slow call has made it worth reading.
    wal_bytes: Option<u64>,
}

/// One named piece of a phase.
#[derive(Debug, Clone, Copy)]
struct Part {
    phase: &'static str,
    name: &'static str,
    took: Duration,
    /// Whether the over-budget line prints it. A piece the phase's own detail
    /// line already prints is kept for the means and nothing else.
    shown: bool,
}

/// Pieces and database time shorter than this are left out of a line: at one
/// decimal place they would read as `0.0ms`.
const SHOWN_PIECE: Duration = Duration::from_micros(50);

/// A duration as the lines here print one.
fn millis(took: Duration) -> String {
    format!("{:.1}ms", took.as_secs_f64() * 1000.0)
}

impl Default for Phases {
    fn default() -> Self {
        Self::start()
    }
}

impl Phases {
    /// Begins a tick's measurement.
    #[must_use]
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            at: now,
            spans: Vec::with_capacity(16),
            parts: Vec::with_capacity(16),
            counts: Vec::with_capacity(16),
            sql: Vec::with_capacity(8),
            slow_sql: Vec::new(),
            wal_bytes: None,
        }
    }

    /// Begins another tick's measurement, keeping the allocation.
    pub fn restart(&mut self) {
        let now = Instant::now();
        self.started = now;
        self.at = now;
        self.spans.clear();
        self.parts.clear();
        self.counts.clear();
        self.sql.clear();
        self.slow_sql.clear();
        self.wal_bytes = None;
    }

    /// Closes the phase that ends here and names it.
    ///
    /// Every phase is the time since the last mark, so a boundary that is
    /// missed shows up as time attributed to the phase after it rather than as
    /// time that vanished. That is the failure mode worth having: the total
    /// always adds up to the tick.
    pub fn mark(&mut self, phase: &'static str) {
        let now = Instant::now();
        self.spans.push((phase, now.duration_since(self.at)));
        self.at = now;
    }

    /// Records a piece of a phase, printed in parentheses after it.
    ///
    /// Pieces need not add up to their phase — what is not named is the rest
    /// of it — and a piece recorded twice in one tick, once per domain, adds.
    pub fn part(&mut self, phase: &'static str, name: &'static str, took: Duration) {
        self.add_part(phase, name, took, true);
    }

    /// Records a piece of a phase for the means only.
    ///
    /// For a phase whose own detail line already prints it — serving has the
    /// serve line — so the over-budget line does not say it twice, and the
    /// once-a-minute means still carry it.
    pub fn quiet_part(&mut self, phase: &'static str, name: &'static str, took: Duration) {
        self.add_part(phase, name, took, false);
    }

    fn add_part(&mut self, phase: &'static str, name: &'static str, took: Duration, shown: bool) {
        if let Some(part) = self
            .parts
            .iter_mut()
            .find(|part| part.phase == phase && part.name == name)
        {
            part.took += took;
            return;
        }
        self.parts.push(Part {
            phase,
            name,
            took,
            shown,
        });
    }

    /// Records a size that explains a phase: how much it had to do.
    ///
    /// Printed after the phase's pieces. Recorded twice in one tick, it adds.
    pub fn count(&mut self, phase: &'static str, name: &'static str, value: u64) {
        if let Some(count) = self
            .counts
            .iter_mut()
            .find(|(of, what, _)| *of == phase && *what == name)
        {
            count.2 += value;
            return;
        }
        self.counts.push((phase, name, value));
    }

    /// Files each database call under the phase it started in.
    ///
    /// **By when it started, after the fact**, because the calls are made
    /// deep inside the world and the world does not know which phase it is
    /// in. Called once the last phase is marked: a call that started before
    /// the first mark belongs to the first phase and one after the last to
    /// the last, so none is dropped.
    pub fn attribute_sql(&mut self, calls: &[crate::sqlclock::Call]) {
        for call in calls {
            let offset = call.at.saturating_duration_since(self.started);
            let mut end = Duration::ZERO;
            let mut phase = self.spans.last().map_or("unmarked", |(name, _)| *name);
            for (name, took) in &self.spans {
                end += *took;
                if offset < end {
                    phase = name;
                    break;
                }
            }
            match self.sql.iter_mut().find(|(of, ..)| *of == phase) {
                Some(entry) => {
                    entry.1 += call.took;
                    entry.2 += 1;
                }
                None => self.sql.push((phase, call.took, 1)),
            }
            if call.took >= crate::sqlclock::SLOW_CALL {
                self.slow_sql.push((phase, call.what, call.took));
            }
        }
    }

    /// Whether any database call this tick was slow enough to name.
    #[must_use]
    pub fn has_slow_sql(&self) -> bool {
        !self.slow_sql.is_empty()
    }

    /// Records the WAL's size, printed beside the slow calls.
    ///
    /// Asked for by the caller only when there are slow calls to print it
    /// beside, because it is a filesystem call.
    pub fn note_wal(&mut self, bytes: Option<u64>) {
        self.wal_bytes = bytes;
    }

    /// How long the tick has taken so far.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.started.elapsed()
    }

    /// The phases that cost anything, largest first, as one line.
    ///
    /// Everything under 1% of the budget is dropped: a breakdown of twelve
    /// phases where nine are noise is a line nobody reads to the end. A phase
    /// that is shown carries what it was made of in parentheses.
    #[must_use]
    pub fn report(&self) -> String {
        let mut spans: Vec<(&'static str, Duration)> = self
            .spans
            .iter()
            .filter(|(_, took)| took.as_micros() >= 500)
            .copied()
            .collect();
        spans.sort_by_key(|(_, took)| std::cmp::Reverse(*took));
        let named: Vec<String> = spans
            .iter()
            .map(|(phase, took)| {
                let detail = self.detail(phase);
                if detail.is_empty() {
                    format!("{phase} {}", millis(*took))
                } else {
                    format!("{phase} {} ({detail})", millis(*took))
                }
            })
            .collect();
        format!(
            "{:.1}ms total: {}",
            self.total().as_secs_f64() * 1000.0,
            if named.is_empty() {
                "nothing over 0.5ms".to_owned()
            } else {
                named.join(", ")
            }
        )
    }

    /// What one phase was made of: its pieces, its sizes, and its time in the
    /// database, each group separated by a semicolon.
    fn detail(&self, phase: &str) -> String {
        let mut groups = Vec::new();
        let parts: Vec<String> = self
            .parts
            .iter()
            .filter(|part| part.shown && part.phase == phase && part.took >= SHOWN_PIECE)
            .map(|part| format!("{} {}", part.name, millis(part.took)))
            .collect();
        if !parts.is_empty() {
            groups.push(parts.join(", "));
        }
        let counts: Vec<String> = self
            .counts
            .iter()
            .filter(|(of, ..)| *of == phase)
            .map(|(_, name, value)| format!("{value} {name}"))
            .collect();
        if !counts.is_empty() {
            groups.push(counts.join(", "));
        }
        if let Some((_, took, calls)) = self.sql.iter().find(|(of, ..)| *of == phase)
            && *took >= SHOWN_PIECE
        {
            let plural = if *calls == 1 { "" } else { "s" };
            let mut sql = format!("sqlite {} in {calls} call{plural}", millis(*took));
            let slow: Vec<String> = self
                .slow_sql
                .iter()
                .filter(|(of, ..)| *of == phase)
                .map(|(_, what, took)| format!("{what} {}", millis(*took)))
                .collect();
            if !slow.is_empty() {
                sql.push_str(": ");
                sql.push_str(&slow.join(", "));
                if let Some(bytes) = self.wal_bytes {
                    let mib = bytes as f64 / (1024.0 * 1024.0);
                    sql.push_str(&format!(", WAL {mib:.1} MiB"));
                }
            }
            groups.push(sql);
        }
        groups.join("; ")
    }
}

/// Every phase's time summed over many ticks, the quiet ones too.
///
/// # Why a mean over every tick
///
/// [`Phases::report`] explains a tick that lost its budget and says nothing
/// about the nineteen in twenty that did not — and it drops anything under
/// half a millisecond, which is most of a healthy tick. A world that is slowly
/// getting slower shows there first: a phase that creeps from 3 ms to 9 ms puts
/// no tick over until the rest of the tick meets it, and by then the first line
/// about it is the problem already. The soak that found the summary scan had to
/// infer what the under-budget ticks were made of; this is that number, kept.
///
/// **Sums, not means, so two ledgers subtract.** The server keeps one running
/// total ([`Control::phase_ledger`]) and [`PhaseLedger::since`] turns it into
/// whatever window a reader wants — a minute for the log, thirty seconds for a
/// soak.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PhaseLedger {
    ticks: u64,
    /// Every phase and piece as `(phase, piece)`, with `""` for the phase
    /// itself and `"sqlite"` for its database time: the time, and how many
    /// ticks it appeared in.
    time: std::collections::BTreeMap<(&'static str, &'static str), (Duration, u64)>,
    /// Every size a phase noted: the sum, and how many ticks noted it.
    counts: std::collections::BTreeMap<(&'static str, &'static str), (u64, u64)>,
}

impl PhaseLedger {
    /// Adds one tick.
    pub fn add(&mut self, phases: &Phases) {
        self.ticks += 1;
        for (phase, took) in &phases.spans {
            self.add_time(phase, "", *took);
        }
        for part in &phases.parts {
            self.add_time(part.phase, part.name, part.took);
        }
        for (phase, took, _) in &phases.sql {
            self.add_time(phase, "sqlite", *took);
        }
        for (phase, name, value) in &phases.counts {
            let entry = self.counts.entry((phase, name)).or_default();
            entry.0 += value;
            entry.1 += 1;
        }
    }

    fn add_time(&mut self, phase: &'static str, piece: &'static str, took: Duration) {
        let entry = self.time.entry((phase, piece)).or_default();
        entry.0 += took;
        entry.1 += 1;
    }

    /// How many ticks this covers.
    #[must_use]
    pub const fn ticks(&self) -> u64 {
        self.ticks
    }

    /// A phase's mean over every tick, including those it did not run in —
    /// its share of the budget.
    #[must_use]
    pub fn mean(&self, phase: &'static str) -> Duration {
        self.part_mean(phase, "")
    }

    /// A piece's mean over every tick: `"sqlite"` for the phase's database
    /// time.
    #[must_use]
    pub fn part_mean(&self, phase: &'static str, piece: &'static str) -> Duration {
        self.time
            .get(&(phase, piece))
            .map_or(Duration::ZERO, |(took, _)| self.per_tick(*took))
    }

    /// How many ticks a phase ran in: a save runs in one of forty.
    #[must_use]
    pub fn appearances(&self, phase: &'static str) -> u64 {
        self.time.get(&(phase, "")).map_or(0, |(_, seen)| *seen)
    }

    /// A size's mean over the ticks that noted it.
    #[must_use]
    pub fn count_mean(&self, phase: &'static str, name: &'static str) -> Option<u64> {
        self.counts
            .get(&(phase, name))
            .filter(|(_, samples)| *samples > 0)
            .map(|(sum, samples)| sum / samples)
    }

    /// The whole tick's mean.
    #[must_use]
    pub fn tick_mean(&self) -> Duration {
        let total = self
            .time
            .iter()
            .filter(|((_, piece), _)| piece.is_empty())
            .map(|(_, (took, _))| *took)
            .sum();
        self.per_tick(total)
    }

    fn per_tick(&self, took: Duration) -> Duration {
        if self.ticks == 0 {
            return Duration::ZERO;
        }
        let nanos = took.as_nanos() / u128::from(self.ticks);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// What this ledger holds that `earlier` did not: the ticks between them.
    #[must_use]
    pub fn since(&self, earlier: &Self) -> Self {
        let mut time = std::collections::BTreeMap::new();
        for (key, (took, seen)) in &self.time {
            let (was, was_seen) = earlier.time.get(key).copied().unwrap_or_default();
            let seen = seen.saturating_sub(was_seen);
            if seen > 0 {
                time.insert(*key, (took.saturating_sub(was), seen));
            }
        }
        let mut counts = std::collections::BTreeMap::new();
        for (key, (sum, samples)) in &self.counts {
            let (was, was_samples) = earlier.counts.get(key).copied().unwrap_or_default();
            let samples = samples.saturating_sub(was_samples);
            if samples > 0 {
                counts.insert(*key, (sum.saturating_sub(was), samples));
            }
        }
        Self {
            ticks: self.ticks.saturating_sub(earlier.ticks),
            time,
            counts,
        }
    }

    /// Every phase's mean, largest first, with its pieces and sizes in
    /// parentheses — the over-budget line's shape, over every tick.
    ///
    /// Two decimal places and nothing dropped above a hundredth of a
    /// millisecond, because the small phases are the point: this is the line
    /// that says what the ticks that did NOT run over were made of.
    #[must_use]
    pub fn line(&self) -> String {
        let shown = Duration::from_micros(5);
        let two = |took: Duration| format!("{:.2}ms", took.as_secs_f64() * 1000.0);
        let mut phases: Vec<(&'static str, Duration)> = self
            .time
            .iter()
            .filter(|((_, piece), _)| piece.is_empty())
            .map(|((phase, _), (took, _))| (*phase, self.per_tick(*took)))
            .filter(|(_, mean)| *mean >= shown)
            .collect();
        phases.sort_by_key(|(_, mean)| std::cmp::Reverse(*mean));
        let named: Vec<String> = phases
            .iter()
            .map(|(phase, mean)| {
                let mut groups = Vec::new();
                let pieces: Vec<String> = self
                    .time
                    .iter()
                    .filter(|((of, piece), _)| {
                        of == phase && !piece.is_empty() && *piece != "sqlite"
                    })
                    .map(|((_, piece), (took, _))| (piece, self.per_tick(*took)))
                    .filter(|(_, mean)| *mean >= shown)
                    .map(|(piece, mean)| format!("{piece} {}", two(mean)))
                    .collect();
                if !pieces.is_empty() {
                    groups.push(pieces.join(", "));
                }
                let counts: Vec<String> = self
                    .counts
                    .iter()
                    .filter(|((of, _), _)| of == phase)
                    .filter(|(_, (_, samples))| *samples > 0)
                    .map(|((_, name), (sum, samples))| format!("{} {name}", sum / samples))
                    .collect();
                if !counts.is_empty() {
                    groups.push(counts.join(", "));
                }
                let sql = self.part_mean(phase, "sqlite");
                if sql >= shown {
                    groups.push(format!("sqlite {}", two(sql)));
                }
                if groups.is_empty() {
                    format!("{phase} {}", two(*mean))
                } else {
                    format!("{phase} {} ({})", two(*mean), groups.join("; "))
                }
            })
            .collect();
        format!(
            "{} a tick over {} ticks: {}",
            two(self.tick_mean()),
            self.ticks,
            if named.is_empty() {
                "nothing".to_owned()
            } else {
                named.join(", ")
            }
        )
    }
}

/// Shared control surface for a running simulation.
///
/// Cloneable and cheap; the network and RCON layers hold one to ask the
/// simulation to stop and to read its progress without touching world state.
#[derive(Debug, Clone, Default)]
pub struct Control {
    inner: Arc<ControlInner>,
}

#[derive(Debug, Default)]
struct ControlInner {
    stop: AtomicBool,
    /// Whether the simulation is suspended. See [`Control::set_paused`].
    paused: AtomicBool,
    tick: AtomicU64,
    dropped: AtomicU64,
    /// Longest single tick observed, in microseconds.
    slowest_micros: AtomicU64,
    /// Ticks that took longer than the 50 ms budget.
    over_budget: AtomicU64,
    /// Set when an operator asks for a save; cleared when the tick performs it.
    save_requested: AtomicBool,
    /// Chunks relit from scratch since start, and chunks currently holding
    /// light. Counted because relighting a chunk that already has light is
    /// invisible in every other measurement — it produces the answer that was
    /// already there, at about 1.4 ms a chunk.
    full_relights: AtomicU64,
    lit_chunks: AtomicU64,
    /// Chunks the unload sweep has taken out of memory since start, and how
    /// many are resident now. Together they say whether a long-running
    /// server's memory is bounded by where its players are, which is the
    /// property the sweep exists for.
    unloaded: AtomicU64,
    resident_chunks: AtomicU64,
    /// Chunks generated by the worldgen workers rather than on the tick.
    generated_off_tick: AtomicU64,
    /// The breakdown of the slowest tick so far, if one has run over budget.
    ///
    /// Kept as text rather than as numbers because the only consumers are a
    /// human reading a log and a load test printing what it just failed on —
    /// and a load test that says which phase blew the budget is one somebody
    /// can act on without reproducing it.
    slowest_phases: std::sync::Mutex<Option<String>>,
    /// How long the tick behind `slowest_phases` took, in microseconds.
    slowest_phase_micros: AtomicU64,
    /// Every tick's phases, summed since start. See [`PhaseLedger`].
    ledger: std::sync::Mutex<PhaseLedger>,
    /// Summary rows the world file holds, and chunks waiting to be written,
    /// as of the last tick: the two numbers that say how far behind the
    /// disk is and how old the world is.
    summary_rows: AtomicU64,
    dirty_chunks: AtomicU64,
    /// Per-tick durations in microseconds, for the macro benchmark.
    ///
    /// A bounded buffer: a server running for a week must not accumulate a
    /// sample per tick forever. Once full it stops recording rather than
    /// evicting, because a benchmark wants the first N ticks of a fixed
    /// workload and a ring buffer would silently measure only the tail.
    samples: std::sync::Mutex<Vec<u32>>,
}

/// How many tick samples are retained.
///
/// 20 Hz for an hour is 72,000, so this holds a benchmark run several times
/// over while costing a few hundred kilobytes.
pub const MAX_TICK_SAMPLES: usize = 100_000;

impl Control {
    /// A fresh, running control handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the simulation to finish the current tick and stop.
    /// Suspends or resumes the simulation.
    ///
    /// **For singleplayer's pause menu**, and only ever set by the client that
    /// owns an embedded server — a hosted server has other people in it and one
    /// of them opening a menu must not stop the world.
    ///
    /// A paused tick loop reads the clock and throws the reading away, so time
    /// spent in a menu is not charged to the simulation as debt. Nothing else
    /// changes: connections stay up, chunks stay resident, and the world simply
    /// does not advance.
    pub fn set_paused(&self, paused: bool) {
        self.inner
            .paused
            .store(paused, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the simulation is suspended.
    #[must_use]
    pub fn paused(&self) -> bool {
        self.inner.paused.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn stop(&self) {
        // Release so that everything the caller did before asking to stop is
        // visible to the simulation thread when it observes the flag.
        self.inner.stop.store(true, Ordering::Release);
    }

    /// Whether a stop has been requested.
    #[must_use]
    pub fn stopping(&self) -> bool {
        self.inner.stop.load(Ordering::Acquire)
    }

    /// The number of the next tick to run.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.inner.tick.load(Ordering::Relaxed)
    }

    /// Ticks abandoned to the catch-up cap since start.
    ///
    /// The single most useful number for diagnosing a struggling server: if
    /// this is climbing, the machine is not keeping up with 20 Hz.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Records that a chunk was lit from scratch.
    pub fn note_full_relight(&self) {
        self.inner.full_relights.fetch_add(1, Ordering::Relaxed);
    }

    /// Records chunks the worldgen workers generated for the tick.
    pub fn note_generated_off_tick(&self, chunks: usize) {
        self.inner
            .generated_off_tick
            .fetch_add(chunks as u64, Ordering::Relaxed);
    }

    /// Chunks generated off the tick since start — see `worldgen::Pool`.
    #[must_use]
    pub fn generated_off_tick(&self) -> u64 {
        self.inner.generated_off_tick.load(Ordering::Relaxed)
    }

    /// Records chunks the unload sweep took out of memory this tick.
    pub fn note_unloaded(&self, chunks: usize) {
        self.inner
            .unloaded
            .fetch_add(chunks as u64, Ordering::Relaxed);
    }

    /// Chunks the unload sweep has taken out of memory since start.
    #[must_use]
    pub fn unloaded(&self) -> u64 {
        self.inner.unloaded.load(Ordering::Relaxed)
    }

    /// Records how many chunks the world is holding, at the end of a tick.
    pub fn note_resident_chunks(&self, chunks: usize) {
        self.inner
            .resident_chunks
            .store(chunks as u64, Ordering::Relaxed);
    }

    /// Chunks in memory as of the last tick.
    #[must_use]
    pub fn resident_chunks(&self) -> u64 {
        self.inner.resident_chunks.load(Ordering::Relaxed)
    }

    /// Records how many chunks currently hold light.
    pub fn note_lit_chunks(&self, chunks: usize) {
        self.inner
            .lit_chunks
            .store(chunks as u64, Ordering::Relaxed);
    }

    /// Chunks lit from scratch since start.
    ///
    /// Against [`Control::lit_chunks`] this says whether the server is doing
    /// the same work twice: nothing forgets a chunk in a short run, so a
    /// relight count above the number of lit chunks is duplicated work.
    #[must_use]
    pub fn full_relights(&self) -> u64 {
        self.inner.full_relights.load(Ordering::Relaxed)
    }

    /// Chunks currently holding light.
    #[must_use]
    pub fn lit_chunks(&self) -> u64 {
        self.inner.lit_chunks.load(Ordering::Relaxed)
    }

    /// Every recorded tick duration, in microseconds, clearing the buffer.
    ///
    /// Taken rather than borrowed so a benchmark can sample a phase and then
    /// start a fresh one.
    #[must_use]
    pub fn take_tick_samples(&self) -> Vec<u32> {
        self.inner
            .samples
            .lock()
            .map(|mut samples| std::mem::take(&mut *samples))
            .unwrap_or_default()
    }

    /// How many tick samples are currently held.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.inner
            .samples
            .lock()
            .map(|samples| samples.len())
            .unwrap_or(0)
    }

    /// The longest single tick observed, in microseconds.
    ///
    /// Measured around the simulation step alone, not the sleep — this is how
    /// long the server spent *working*, which is the number that says whether
    /// there is headroom left.
    #[must_use]
    pub fn slowest_tick_micros(&self) -> u64 {
        self.inner.slowest_micros.load(Ordering::Relaxed)
    }

    /// Asks the simulation to write dirty chunks on its next tick.
    ///
    /// A request rather than a call: the simulation owns the database, and a
    /// save performed from an RCON task would mean two threads writing chunks
    /// — the exact thing `world.rs` exists to prevent.
    pub fn request_save(&self) {
        self.inner.save_requested.store(true, Ordering::Release);
    }

    /// Takes the save request, if there is one.
    ///
    /// Clears the flag, so a request is honoured once rather than every tick
    /// thereafter.
    #[must_use]
    pub fn take_save_request(&self) -> bool {
        self.inner.save_requested.swap(false, Ordering::AcqRel)
    }

    /// Records what an over-budget tick spent its time on, if it is the worst.
    ///
    /// Worst rather than latest: a run's last slow tick is whichever one
    /// happened to be near the end, and the one worth explaining is the one
    /// that took longest.
    pub fn note_tick_phases(&self, micros: u64, report: &str) {
        // Its own high-water mark rather than `slowest_micros`, which the tick
        // loop has already updated with this very tick by the time anything
        // asks — comparing against it would refuse to record the tick that set
        // it, which is every tick worth recording.
        if micros <= self.inner.slowest_phase_micros.load(Ordering::Relaxed) {
            return;
        }
        self.inner
            .slowest_phase_micros
            .store(micros, Ordering::Relaxed);
        if let Ok(mut held) = self.inner.slowest_phases.lock() {
            *held = Some(report.to_owned());
        }
    }

    /// What the slowest tick spent its time on, if anything recorded it.
    #[must_use]
    pub fn slowest_phases(&self) -> Option<String> {
        self.inner
            .slowest_phases
            .lock()
            .ok()
            .and_then(|held| held.clone())
    }

    /// Adds one tick's phases to the running ledger.
    pub fn note_phases(&self, phases: &Phases) {
        if let Ok(mut ledger) = self.inner.ledger.lock() {
            ledger.add(phases);
        }
    }

    /// Every phase's time since start.
    ///
    /// A running total: subtract an earlier copy with [`PhaseLedger::since`]
    /// for the ticks in between.
    #[must_use]
    pub fn phase_ledger(&self) -> PhaseLedger {
        self.inner
            .ledger
            .lock()
            .map(|ledger| ledger.clone())
            .unwrap_or_default()
    }

    /// Records the world's save backlog and how many summaries it holds.
    pub fn note_world_rows(&self, summary_rows: u64, dirty_chunks: usize) {
        self.inner
            .summary_rows
            .store(summary_rows, Ordering::Relaxed);
        self.inner
            .dirty_chunks
            .store(dirty_chunks as u64, Ordering::Relaxed);
    }

    /// Summary rows the world file held at the end of the last tick.
    #[must_use]
    pub fn summary_rows(&self) -> u64 {
        self.inner.summary_rows.load(Ordering::Relaxed)
    }

    /// Chunks waiting to be written at the end of the last tick.
    #[must_use]
    pub fn dirty_chunks(&self) -> u64 {
        self.inner.dirty_chunks.load(Ordering::Relaxed)
    }

    /// How many ticks ran over the 50 ms budget.
    ///
    /// A tick over budget has not necessarily hurt anyone — the accumulator
    /// absorbs a single slow tick — but a rising count means the server is
    /// living on the catch-up allowance rather than inside its budget.
    #[must_use]
    pub fn over_budget_ticks(&self) -> u64 {
        self.inner.over_budget.load(Ordering::Relaxed)
    }
}

/// Runs the fixed-rate loop until [`Control::stop`] is called.
///
/// `step` is called once per tick with the tick number. It is the only place
/// world state may change.
///
/// Returns the number of ticks run.
pub fn run<C: Clock, F: FnMut(u64)>(clock: &mut C, control: &Control, mut step: F) -> u64 {
    // Charter rule 4: this thread runs simulation floats, so it must be in
    // IEEE default mode. A thread that inherited flush-to-zero or a non-nearest
    // rounding mode — from a driver, an audio library, or a mod's native
    // code — would silently produce a different world. Fail here, loudly, at
    // startup, rather than at the first cross-platform hash mismatch.
    tiamat_core::assert_ieee_mode();

    let mut accumulator = Accumulator::new();
    let mut ran = 0u64;

    // Discard the time between thread spawn and loop entry. Otherwise process
    // startup — opening the world, loading mods — is charged to the simulation
    // as debt, and the server begins life already behind.
    clock.tick_elapsed();

    while !control.stopping() {
        // **Paused: the clock is read and thrown away.**
        //
        // The world not lurching forward on resume is already guaranteed by the
        // accumulator, which drops ticks rather than chasing them — verified by
        // removing this line and watching the test still pass. What reading the
        // clock here actually buys is that a pause is not an INCIDENT: without
        // it every unpause logs "simulation fell behind", stores the menu's
        // worth of ticks in the dropped counter, and makes the one metric that
        // says whether the server is keeping up meaningless for the session.
        if control.paused() {
            clock.tick_elapsed();
            clock.sleep(TICK_DURATION);
            continue;
        }
        let elapsed = clock.tick_elapsed();
        let advance = accumulator.advance(elapsed);

        if advance.dropped > 0 {
            warn!(
                dropped = advance.dropped,
                total_dropped = accumulator.dropped(),
                tick = accumulator.tick(),
                "simulation fell behind; dropped ticks rather than chasing them"
            );
            control
                .inner
                .dropped
                .store(accumulator.dropped(), Ordering::Relaxed);
        }

        if advance.ticks == 0 {
            // Sleep the remainder of the tick, but never so long that a stop
            // request waits on it. A shutdown that takes a whole tick to be
            // noticed is not a problem at 50 ms; capping it keeps that true if
            // the tick rate ever drops.
            clock.sleep(advance.sleep.min(TICK_DURATION));
            continue;
        }

        // Tick numbers are SEQUENTIAL, with no gaps for dropped ticks. A tick
        // is a simulation step, and its number is its index; dropping means the
        // world advanced less than real time did, which is the honest account.
        // Numbering by real time instead would leave holes, and a mod computing
        // an interval from tick numbers would silently get it wrong.
        for _ in 0..advance.ticks {
            // Measured with a real `Instant` rather than through the `Clock`.
            // The clock exists so PACING can be tested without sleeping; how
            // long the work took is a fact about the machine, and faking it
            // would make this number meaningless.
            let started = Instant::now();
            step(ran);
            let elapsed = started.elapsed();
            ran += 1;

            let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
            control
                .inner
                .slowest_micros
                .fetch_max(micros, Ordering::Relaxed);
            if let Ok(mut samples) = control.inner.samples.lock()
                && samples.len() < MAX_TICK_SAMPLES
            {
                samples.push(u32::try_from(micros).unwrap_or(u32::MAX));
            }
            if elapsed > TICK_DURATION {
                control.inner.over_budget.fetch_add(1, Ordering::Relaxed);
            }
        }

        control.inner.tick.store(ran, Ordering::Relaxed);
    }

    info!(
        ticks = ran,
        dropped = accumulator.dropped(),
        "simulation stopped"
    );
    debug_assert_eq!(
        ran,
        accumulator.tick(),
        "the loop's tick count and the accumulator's must not diverge"
    );
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A clock the test drives by hand.
    struct FakeClock {
        /// Elapsed values to return, in order. Repeats the last one forever.
        script: Vec<Duration>,
        index: usize,
        /// Every sleep requested, for asserting on pacing.
        slept: Vec<Duration>,
        /// Stops the loop after this many readings, for tests where no tick is
        /// ever due and nothing else could end it.
        stop_after: Option<(usize, Control)>,
    }

    impl FakeClock {
        fn new(script: Vec<Duration>) -> Self {
            Self {
                script,
                index: 0,
                slept: Vec::new(),
                stop_after: None,
            }
        }

        /// Stops `control` once the loop has asked for time `readings` times.
        fn stopping_after(mut self, readings: usize, control: &Control) -> Self {
            self.stop_after = Some((readings, control.clone()));
            self
        }

        /// A clock that always reports exactly one tick of elapsed time.
        fn real_time() -> Self {
            Self::new(vec![TICK_DURATION])
        }
    }

    impl Clock for FakeClock {
        fn tick_elapsed(&mut self) -> Duration {
            let value = self
                .script
                .get(self.index)
                .copied()
                .or_else(|| self.script.last().copied())
                .unwrap_or(Duration::ZERO);
            self.index += 1;
            if let Some((readings, control)) = &self.stop_after
                && self.index >= *readings
            {
                control.stop();
            }
            value
        }

        fn sleep(&mut self, duration: Duration) {
            self.slept.push(duration);
        }
    }

    #[test]
    fn the_loop_runs_ticks_in_order_and_stops_when_asked() {
        let control = Control::new();
        let mut clock = FakeClock::real_time();
        let seen = Mutex::new(Vec::new());

        let stopper = control.clone();
        let ran = run(&mut clock, &control, |tick| {
            seen.lock().expect("lock").push(tick);
            if tick == 9 {
                stopper.stop();
            }
        });

        assert_eq!(ran, 10);
        let seen = seen.into_inner().expect("lock");
        assert_eq!(
            seen,
            (0..10).collect::<Vec<_>>(),
            "ticks must be sequential"
        );
    }

    #[test]
    fn a_stop_before_the_first_tick_runs_nothing() {
        let control = Control::new();
        control.stop();
        let mut clock = FakeClock::real_time();

        let ran = run(&mut clock, &control, |_| panic!("should not tick"));
        assert_eq!(ran, 0);
    }

    #[test]
    fn startup_time_is_not_charged_as_debt() {
        // The first elapsed reading covers thread spawn and world loading. If
        // it were fed to the accumulator, a server that took two seconds to
        // start would immediately run its catch-up cap and log dropped ticks
        // before doing anything at all.
        let control = Control::new();
        // A huge first reading (startup), then normal frames.
        let mut clock = FakeClock::new(vec![Duration::from_secs(30), TICK_DURATION]);

        let stopper = control.clone();
        let ran = run(&mut clock, &control, move |tick| {
            if tick == 2 {
                stopper.stop();
            }
        });

        assert_eq!(ran, 3);
        assert_eq!(
            control.dropped(),
            0,
            "startup time must not be charged to the simulation"
        );
    }

    #[test]
    fn an_idle_loop_sleeps_rather_than_spinning() {
        let control = Control::new();
        // Frames far shorter than a tick: nothing is ever due, so the loop must
        // sleep every iteration rather than spinning a core at 100%.
        let mut clock = FakeClock::new(vec![Duration::ZERO, Duration::from_millis(5)])
            .stopping_after(6, &control);

        let ran = run(&mut clock, &control, |_| panic!("nothing is due"));

        assert_eq!(ran, 0);
        assert!(!clock.slept.is_empty(), "an idle loop must sleep");
        assert!(
            clock.slept.iter().all(|d| *d <= TICK_DURATION),
            "no sleep may exceed one tick, or shutdown would lag: {:?}",
            clock.slept
        );
        // 5 ms a frame against a 50 ms tick: the sleep should shrink as debt
        // builds, not sit at a constant.
        assert!(
            clock.slept.windows(2).any(|w| w[1] < w[0]),
            "the sleep should shorten as the next tick approaches: {:?}",
            clock.slept
        );
    }

    #[test]
    fn a_stall_drops_ticks_instead_of_chasing_them() {
        let control = Control::new();
        // One ten-second stall, then back to real time.
        let mut clock =
            FakeClock::new(vec![Duration::ZERO, Duration::from_secs(10), TICK_DURATION]);

        let stopper = control.clone();
        let ran = run(&mut clock, &control, move |tick| {
            if tick == 20 {
                stopper.stop();
            }
        });

        assert_eq!(ran, 21);
        assert!(
            control.dropped() > 0,
            "a ten-second stall must drop ticks rather than chase 200 of them"
        );
        assert!(
            control.dropped() < 200,
            "and it must not drop more than were owed"
        );
    }

    #[test]
    fn the_control_handle_reports_progress() {
        let control = Control::new();
        let mut clock = FakeClock::real_time();

        let stopper = control.clone();
        let observer = control.clone();
        run(&mut clock, &control, move |tick| {
            if tick == 4 {
                stopper.stop();
            }
        });

        assert_eq!(observer.tick(), 5);
        assert!(observer.stopping());
    }

    /// A tick's phases with made-up lengths, since `mark` reads the clock.
    fn phases_of(spans: &[(&'static str, u64)]) -> Phases {
        let mut phases = Phases::start();
        for (phase, micros) in spans {
            phases.spans.push((phase, Duration::from_micros(*micros)));
        }
        phases
    }

    /// A database call `offset_micros` into the tick.
    fn call(
        phases: &Phases,
        what: &'static str,
        offset_micros: u64,
        micros: u64,
    ) -> crate::sqlclock::Call {
        crate::sqlclock::Call {
            what,
            at: phases.started + Duration::from_micros(offset_micros),
            took: Duration::from_micros(micros),
        }
    }

    #[test]
    fn a_phase_carries_its_pieces_sizes_and_database_time_in_parentheses() {
        let mut phases = phases_of(&[("fluid", 23_000), ("serving", 10_000), ("mods", 200)]);
        phases.part("fluid", "solver", Duration::from_millis(2));
        phases.part("fluid", "broadcast", Duration::from_millis(1));
        // Twice in one tick, as the per-domain loop records it: it adds.
        phases.part("fluid", "broadcast", Duration::from_millis(1));
        phases.part("fluid", "hooks", Duration::from_micros(10));
        phases.count("fluid", "active", 1200);
        phases.count("fluid", "carried", 30);
        phases.quiet_part("serving", "gen", Duration::from_millis(3));
        let calls = [
            call(&phases, "save fluid", 1_000, 6_000),
            call(&phases, "load fluid", 2_000, 200),
        ];
        phases.attribute_sql(&calls);
        assert!(phases.has_slow_sql());
        phases.note_wal(Some(2 * 1024 * 1024));
        let report = phases.report();
        assert!(
            report.contains(
                "fluid 23.0ms (solver 2.0ms, broadcast 2.0ms; 1200 active, 30 carried; \
                 sqlite 6.2ms in 2 calls: save fluid 6.0ms, WAL 2.0 MiB)"
            ),
            "{report}"
        );
        // A quiet piece is for the means: the serve line already says it.
        assert!(report.contains("serving 10.0ms"), "{report}");
        assert!(!report.contains("gen"), "{report}");
        // Too short to read as anything but 0.0ms.
        assert!(!report.contains("hooks"), "{report}");
        // Still dropped below half a millisecond, pieces or not.
        assert!(!report.contains("mods"), "{report}");
    }

    #[test]
    fn a_database_call_is_filed_under_the_phase_it_started_in() {
        let mut phases = phases_of(&[("serving", 10_000), ("save chunks", 10_000)]);
        let before = crate::sqlclock::Call {
            what: "load chunk",
            at: phases
                .started
                .checked_sub(Duration::from_millis(1))
                .unwrap_or(phases.started),
            took: Duration::from_micros(100),
        };
        let calls = [
            before,
            call(&phases, "load chunk", 5_000, 100),
            call(&phases, "save chunk batch", 15_000, 300),
            // Past the last mark: the last phase's, not dropped.
            call(&phases, "save fluid", 25_000, 50),
        ];
        phases.attribute_sql(&calls);
        assert_eq!(
            phases.sql,
            vec![
                ("serving", Duration::from_micros(200), 2),
                ("save chunks", Duration::from_micros(350), 2),
            ]
        );
        assert!(
            !phases.has_slow_sql(),
            "nothing here is over five milliseconds"
        );
    }

    #[test]
    fn the_ledger_means_count_every_tick_including_the_quiet_ones() {
        // A save runs in one tick of four here: its mean over every tick is a
        // quarter of what it cost when it ran, which is its share of the
        // budget — the number that says whether it is growing.
        let mut ledger = PhaseLedger::default();
        for tick in 0..4 {
            let mut phases = if tick == 0 {
                phases_of(&[("fluid", 4_000), ("save chunks", 8_000)])
            } else {
                phases_of(&[("fluid", 4_000)])
            };
            phases.part("fluid", "solver", Duration::from_millis(1));
            phases.count("fluid", "active", 100 * (tick + 1));
            let calls = [call(&phases, "load fluid", 0, 400)];
            phases.attribute_sql(&calls);
            ledger.add(&phases);
        }
        assert_eq!(ledger.ticks(), 4);
        assert_eq!(ledger.mean("fluid"), Duration::from_millis(4));
        assert_eq!(ledger.mean("save chunks"), Duration::from_millis(2));
        assert_eq!(ledger.appearances("save chunks"), 1);
        assert_eq!(
            ledger.part_mean("fluid", "solver"),
            Duration::from_millis(1)
        );
        assert_eq!(
            ledger.part_mean("fluid", "sqlite"),
            Duration::from_micros(400)
        );
        assert_eq!(ledger.count_mean("fluid", "active"), Some(250));
        assert_eq!(ledger.tick_mean(), Duration::from_millis(6));

        // And a window is a subtraction.
        let earlier = ledger.clone();
        let mut phases = phases_of(&[("fluid", 10_000)]);
        phases.count("fluid", "active", 7);
        ledger.add(&phases);
        let window = ledger.since(&earlier);
        assert_eq!(window.ticks(), 1);
        assert_eq!(window.mean("fluid"), Duration::from_millis(10));
        assert_eq!(window.mean("save chunks"), Duration::ZERO);
        assert_eq!(window.count_mean("fluid", "active"), Some(7));
    }

    #[test]
    fn the_ledger_line_keeps_what_the_over_budget_line_drops() {
        // The small phases are the point of it: the report drops anything
        // under half a millisecond, and this is what a tick that did NOT run
        // over was made of.
        let mut ledger = PhaseLedger::default();
        let mut phases = phases_of(&[("serving", 12_000), ("unload", 100)]);
        phases.quiet_part("serving", "relight", Duration::from_millis(9));
        ledger.add(&phases);
        let line = ledger.line();
        assert_eq!(
            line,
            "12.10ms a tick over 1 ticks: serving 12.00ms (relight 9.00ms), unload 0.10ms"
        );
    }

    #[test]
    fn the_control_handle_keeps_a_running_ledger() {
        let control = Control::new();
        let phases = phases_of(&[("fluid", 1_000)]);
        control.note_phases(&phases);
        control.note_phases(&phases);
        assert_eq!(control.phase_ledger().ticks(), 2);
        control.note_world_rows(47_000, 180);
        assert_eq!(
            (control.summary_rows(), control.dirty_chunks()),
            (47_000, 180)
        );
    }
}
