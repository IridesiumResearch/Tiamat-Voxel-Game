// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What the service thinks the time is.
//!
//! A trait rather than a call to [`std::time::SystemTime`] everywhere because
//! the interesting behaviour of this service is time-shaped: a certificate
//! expires, a released name is held for thirty days, a recovery notice runs for
//! fourteen, and a claim's timestamp may be at most five minutes from the
//! service's clock. Testing those by sleeping is how a test suite becomes slow
//! and flaky, so the bot tests (N6) move the clock instead.

use std::time::{SystemTime, UNIX_EPOCH};

/// A source of Unix seconds.
///
/// `Send + Sync` because one instance is shared by the request threads.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Seconds since the Unix epoch.
    fn now(&self) -> i64;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        // A clock set before 1970 is not something to panic on: the service
        // reads 0, and every signed request then fails its timestamp check —
        // which is the honest answer for a machine whose clock is wrong.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_is_after_the_epoch() {
        assert!(SystemClock.now() > 1_700_000_000);
    }
}
