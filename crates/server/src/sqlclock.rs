// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! How long the tick spends inside the world database, call by call.
//!
//! # Why every call is timed where it is made
//!
//! Every read and write of the world file happens on the tick thread, inside
//! whichever phase asked for it: a generated chunk's fluid inside `serving`,
//! a save batch inside `save chunks`, a mob's rows inside `light`. A phase that
//! runs over says that it ran over and not that it was waiting on `SQLite`, and
//! two costs hide exactly there. One is a query whose plan scans a table that
//! grows with the world's age — the summary forget that made every save batch
//! slower the longer a world had been played, invisible until a soak measured
//! it offline. The other is the WAL's automatic checkpoint, which runs inside
//! whichever commit crosses its threshold and so lands on a tick nobody chose.
//!
//! So [`Log::time`] wraps each call, the tick hands the calls to
//! [`crate::sim::Phases::attribute_sql`] at its end, which files each under the
//! phase it started in, and a call slow enough to be either of those is named
//! in the over-budget line beside the size of the WAL.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// A call slow enough to name in the over-budget line.
///
/// A tenth of a tick. An indexed read or a batch of upserts is well under a
/// millisecond; a call past this is a scan, a checkpoint, or a disk that is
/// busy with something else, and each of those is worth a line.
pub const SLOW_CALL: Duration = Duration::from_millis(5);

/// Calls kept between two drains, at most.
///
/// The tick drains every tick and makes a few dozen. The cap is for a `World`
/// used with no tick to drain it — a test, a tool — which must not grow a list
/// for as long as it runs.
const MAX_CALLS: usize = 4096;

/// One call into the world database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    /// What it was, in the words of the line it is reported in.
    pub what: &'static str,
    /// When it started: how the tick finds the phase it belongs to.
    pub at: Instant,
    /// How long it took.
    pub took: Duration,
}

/// The calls made since the tick last asked.
#[derive(Debug, Default)]
pub struct Log {
    calls: Vec<Call>,
    /// Calls past [`MAX_CALLS`]: how many, and their time, filed as one.
    unlisted: Option<Call>,
}

impl Log {
    /// Runs `call`, and records how long it took under `what`.
    ///
    /// Takes the log by its cell rather than by `&mut self` because the callers
    /// are `World` methods that take `&self` — a load is a read — and the borrow
    /// is held only for the push, never across the call, so a call that itself
    /// times another cannot trip over it.
    pub fn time<T>(log: &RefCell<Self>, what: &'static str, call: impl FnOnce() -> T) -> T {
        let at = Instant::now();
        let answer = call();
        let took = at.elapsed();
        // An instrument must never be the thing that stops the tick: a log
        // somebody else is holding misses one entry rather than panicking.
        if let Ok(mut held) = log.try_borrow_mut() {
            held.record(Call { what, at, took });
        }
        answer
    }

    /// Keeps one call, or folds it into the unlisted total once the log is full.
    pub fn record(&mut self, call: Call) {
        if self.calls.len() < MAX_CALLS {
            self.calls.push(call);
            return;
        }
        let unlisted = self.unlisted.get_or_insert(Call {
            what: "unlisted",
            at: call.at,
            took: Duration::ZERO,
        });
        unlisted.took += call.took;
    }

    /// Every call since the last take, oldest first; the unlisted ones last,
    /// as a single entry dated from the first of them.
    pub fn take(&mut self) -> Vec<Call> {
        let mut calls = std::mem::take(&mut self.calls);
        calls.extend(self.unlisted.take());
        calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_call_is_answered_and_recorded_under_its_name() {
        let log = RefCell::new(Log::default());
        let answer = Log::time(&log, "load chunk", || 42);
        assert_eq!(answer, 42, "timing a call changed what it returned");
        let calls = log.borrow_mut().take();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].what, "load chunk");
        assert!(
            log.borrow_mut().take().is_empty(),
            "a take left the calls behind"
        );
    }

    #[test]
    fn a_log_nobody_drains_stops_growing_and_keeps_the_time() {
        // A `World` in a test or a tool has no tick to drain it. What it stops
        // listing it still adds up, so a total built from it is not short.
        let mut log = Log::default();
        let at = Instant::now();
        for _ in 0..MAX_CALLS + 10 {
            log.record(Call {
                what: "save chunk batch",
                at,
                took: Duration::from_micros(3),
            });
        }
        let calls = log.take();
        assert_eq!(calls.len(), MAX_CALLS + 1, "the log grew past its cap");
        let last = calls.last().expect("the unlisted entry");
        assert_eq!(last.what, "unlisted");
        assert_eq!(last.took, Duration::from_micros(30));
    }

    #[test]
    fn a_call_that_times_another_does_not_trip_over_the_log() {
        // Nested timing is legal: the borrow is taken only to push. The outer
        // call is recorded after the inner one, because it ends later.
        let log = RefCell::new(Log::default());
        Log::time(&log, "outer", || Log::time(&log, "inner", || ()));
        let names: Vec<&str> = log.borrow_mut().take().iter().map(|c| c.what).collect();
        assert_eq!(names, ["inner", "outer"]);
    }
}
