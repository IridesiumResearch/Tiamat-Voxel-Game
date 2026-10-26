// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A request's deadline, and the one rule that makes a timeout honest: **work
//! that was answered "timed out" never commits.**
//!
//! # Why a race needs settling
//!
//! The transport answers `504` when a request's work has not finished in two
//! seconds, so a client is never left holding a socket. The work itself cannot
//! be killed (Rust has no safe way to stop a thread), so before this module it
//! carried on and could still commit: a claim answered `504` that then
//! succeeded, so the client's retry was told `already_named` (review B11).
//!
//! The fix is a three-state flag shared by the worker that answers and the
//! handler that writes. Both race for it exactly once:
//!
//! - the handler, at its commit, moves it from *running* to *committed* — but
//!   only while the deadline has not passed;
//! - the worker, when its wait runs out, moves it from *running* to
//!   *abandoned*, and only then answers `504`.
//!
//! Whichever compare-and-swap wins decides the answer. An abandoned request's
//! commit is refused, so its transaction is dropped and rolled back; a committed
//! request is never abandoned, so the worker waits for its real answer however
//! late it is (a mail send after the commit is the usual reason).

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Transaction;

/// The work may still commit.
const RUNNING: u8 = 0;
/// The work committed; its answer is the truth and must be waited for.
const COMMITTED: u8 = 1;
/// The worker answered `504`; nothing may commit any more.
const ABANDONED: u8 = 2;

/// One request's deadline and its commit-or-abandon flag.
///
/// Shared between the worker that owns the socket and the thread that runs the
/// handler, behind an `Arc`.
#[derive(Debug)]
pub struct Deadline {
    /// When the request's time is up; `None` never expires (tests that are not
    /// about time).
    at: Option<Instant>,
    state: AtomicU8,
}

impl Deadline {
    /// A deadline `budget` after `start` — the moment the request was taken off
    /// the listener, so time spent waiting for a worker counts against it.
    #[must_use]
    pub fn starting(start: Instant, budget: Duration) -> Self {
        Self {
            // An overflow means a budget no request could exhaust: never.
            at: start.checked_add(budget),
            state: AtomicU8::new(RUNNING),
        }
    }

    /// A deadline that never passes, for tests of the flows that are not about
    /// time.
    #[cfg(test)]
    #[must_use]
    pub fn none() -> Self {
        Self {
            at: None,
            state: AtomicU8::new(RUNNING),
        }
    }

    /// Whether the time is up (whatever the flag says).
    #[must_use]
    pub fn expired(&self) -> bool {
        self.at.is_some_and(|at| Instant::now() >= at)
    }

    /// How long is left, zero once it has passed. A deadline that never passes
    /// reports a day, which is longer than any wait in this crate.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        match self.at {
            Some(at) => at.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(24 * 60 * 60),
        }
    }

    /// Whether the worker has given up on this request.
    #[must_use]
    pub fn abandoned(&self) -> bool {
        self.state.load(Ordering::Acquire) == ABANDONED
    }

    /// Claims the right to commit.
    ///
    /// `true` once the work has committed (or may now do so), and from then on;
    /// `false` when the deadline has passed or the worker already answered,
    /// in which case the caller drops its transaction and reports
    /// [`Deadline`]-late. A late caller also marks the request abandoned, so the
    /// worker cannot then wait for an answer that will never be a commit.
    #[must_use]
    pub fn may_commit(&self) -> bool {
        match self.state.load(Ordering::Acquire) {
            COMMITTED => return true,
            ABANDONED => return false,
            _ => {}
        }
        if self.expired() {
            let _ = self.state.compare_exchange(
                RUNNING,
                ABANDONED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return self.state.load(Ordering::Acquire) == COMMITTED;
        }
        match self
            .state
            .compare_exchange(RUNNING, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(current) => current == COMMITTED,
        }
    }

    /// Gives up on the request, if its work has not committed.
    ///
    /// `true` means the worker may answer `504`: nothing has been committed
    /// and nothing will be. `false` means the work already committed and the
    /// worker must wait for its answer.
    #[must_use]
    pub fn abandon(&self) -> bool {
        match self
            .state
            .compare_exchange(RUNNING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(current) => current == ABANDONED,
        }
    }
}

/// Commits `transaction` if the deadline allows, and rolls it back if not.
///
/// Returns whether it committed. Every write path a client waits on ends here
/// rather than in `Transaction::commit`, which is what makes "a `504` never
/// hides a commit" a property of the crate and not of each handler's care.
///
/// # Errors
///
/// The commit's own SQL failure.
pub fn commit(transaction: Transaction<'_>, deadline: &Deadline) -> rusqlite::Result<bool> {
    if !deadline.may_commit() {
        // Dropping a transaction rolls it back.
        drop(transaction);
        return Ok(false);
    }
    transaction.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_that_has_not_passed_lets_the_work_commit_once_and_for_good() {
        let deadline = Deadline::starting(Instant::now(), Duration::from_secs(60));
        assert!(!deadline.expired());
        assert!(deadline.may_commit());
        // Committed work is never abandoned: the worker waits for its answer.
        assert!(!deadline.abandon());
        assert!(deadline.may_commit(), "a second commit in the same request");
    }

    #[test]
    fn abandoned_work_cannot_commit() {
        let deadline = Deadline::starting(Instant::now(), Duration::from_secs(60));
        assert!(deadline.abandon());
        assert!(deadline.abandoned());
        assert!(!deadline.may_commit());
        assert!(deadline.abandon(), "abandoning twice is still abandoned");
    }

    #[test]
    fn work_that_reaches_its_commit_late_is_refused_and_abandons_itself() {
        let deadline = Deadline::starting(Instant::now(), Duration::ZERO);
        assert!(deadline.expired());
        assert_eq!(deadline.remaining(), Duration::ZERO);
        assert!(!deadline.may_commit());
        assert!(deadline.abandoned());
        assert!(deadline.abandon());
    }

    #[test]
    fn no_deadline_never_expires() {
        let deadline = Deadline::none();
        assert!(!deadline.expired());
        assert!(deadline.remaining() >= Duration::from_secs(60 * 60));
        assert!(deadline.may_commit());
    }

    #[test]
    fn a_late_commit_rolls_its_transaction_back() {
        let mut connection = rusqlite::Connection::open_in_memory().expect("db");
        connection
            .execute_batch("CREATE TABLE t (x INTEGER)")
            .expect("schema");
        let late = Deadline::starting(Instant::now(), Duration::ZERO);
        let transaction = connection.transaction().expect("begin");
        transaction
            .execute("INSERT INTO t (x) VALUES (1)", [])
            .expect("insert");
        assert!(!commit(transaction, &late).expect("commit"));
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 0, "a late transaction was committed");

        let transaction = connection.transaction().expect("begin");
        transaction
            .execute("INSERT INTO t (x) VALUES (1)", [])
            .expect("insert");
        assert!(commit(transaction, &Deadline::none()).expect("commit"));
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 1);
    }

    #[test]
    fn the_worker_and_the_handler_never_both_win() {
        // Many races at the boundary: whatever the interleaving, exactly one of
        // "committed" and "answered 504" happens.
        for _ in 0..200 {
            let deadline = std::sync::Arc::new(Deadline::starting(
                Instant::now(),
                Duration::from_micros(50),
            ));
            let handler = {
                let deadline = std::sync::Arc::clone(&deadline);
                std::thread::spawn(move || deadline.may_commit())
            };
            std::thread::sleep(Duration::from_micros(50));
            let abandoned = deadline.abandon();
            let committed = handler.join().expect("handler");
            assert_ne!(committed, abandoned, "both or neither won");
        }
    }
}
