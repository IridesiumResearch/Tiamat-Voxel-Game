// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The chat box's lines, and how long each stays on screen.
//!
//! # A notice is read once, not kept
//!
//! A server's notices — "that is too far away", "this is not flat ground" —
//! arrive as lines of chat from nobody and sit in the same box as what players
//! say. Shown for as long as they were, the last five lines stood at the
//! bottom left for the rest of the session, and a tip from ten minutes ago
//! read as if it were about now. The designer, 2026-10-08: they fade out
//! quickly once a newer message is shown, and a line that has faded is gone.
//!
//! So every line carries the moment it starts to fade: [`HOLD`] after it
//! arrived, left alone, or [`QUICK`] after a newer line arrived, whichever is
//! sooner; the fade itself takes [`FADE`]. The box, closed, draws what is
//! still fading in — [`ChatLog::fading`] — and open it draws the history,
//! [`ChatLog::all`], which keeps the most recent [`MAX_LINES`] as it always
//! did.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The most recent lines kept for the open box.
///
/// A bounded deque rather than a growing list: a session lasting hours on a
/// busy server would otherwise hold every line anybody said.
pub const MAX_LINES: usize = 200;
/// How long a line stays in full before it starts to fade, left alone.
pub const HOLD: Duration = Duration::from_secs(5);
/// How long an older line stays in full once a newer one has arrived.
pub const QUICK: Duration = Duration::from_millis(900);
/// How long the fade itself takes.
pub const FADE: Duration = Duration::from_millis(700);

#[derive(Debug, Clone)]
struct Line {
    text: String,
    /// When the line starts to fade.
    fade_at: Instant,
}

/// The lines, oldest first.
#[derive(Debug, Default)]
pub struct ChatLog {
    lines: VecDeque<Line>,
}

impl ChatLog {
    /// Records a line that arrived at `now`, and brings every older line's
    /// fade forward to [`QUICK`] from now.
    pub fn say(&mut self, text: String, now: Instant) {
        let soon = now + QUICK;
        for line in &mut self.lines {
            line.fade_at = line.fade_at.min(soon);
        }
        self.lines.push_back(Line {
            text,
            fade_at: now + HOLD,
        });
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    /// Every kept line, oldest first: the open box's history.
    pub fn all(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(|line| line.text.as_str())
    }

    /// The lines still on screen at `now` with the box closed, oldest first,
    /// each with how visible it is, `0.0..=1.0`. A line that has faded is
    /// not here.
    #[must_use]
    pub fn fading(&self, now: Instant) -> Vec<(&str, f32)> {
        self.lines
            .iter()
            .filter_map(|line| {
                let alpha = alpha(line.fade_at, now);
                (alpha > 0.0).then_some((line.text.as_str(), alpha))
            })
            .collect()
    }
}

/// How visible a line that starts to fade at `fade_at` is at `now`.
#[must_use]
pub fn alpha(fade_at: Instant, now: Instant) -> f32 {
    if now <= fade_at {
        return 1.0;
    }
    let gone = now.duration_since(fade_at).as_secs_f32() / FADE.as_secs_f32();
    (1.0 - gone).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_left_alone_stays_then_fades_then_goes() {
        let start = Instant::now();
        let mut log = ChatLog::default();
        log.say("that is too far away".to_owned(), start);
        assert_eq!(log.fading(start).len(), 1);
        assert_eq!(
            log.fading(start + HOLD).first().map(|line| line.1),
            Some(1.0)
        );
        let halfway = start + HOLD + FADE / 2;
        let alpha = log.fading(halfway).first().map_or(0.0, |line| line.1);
        assert!(
            alpha > 0.3 && alpha < 0.7,
            "halfway through the fade: {alpha}"
        );
        assert!(
            log.fading(start + HOLD + FADE + Duration::from_millis(1))
                .is_empty(),
            "faded, it is gone from the closed box"
        );
        assert_eq!(log.all().count(), 1, "and still in the history");
    }

    #[test]
    fn a_newer_line_hurries_the_older_ones_off() {
        let start = Instant::now();
        let mut log = ChatLog::default();
        log.say("first".to_owned(), start);
        let later = start + Duration::from_secs(1);
        log.say("second".to_owned(), later);
        // Left alone the first would have stood until HOLD; now it fades
        // QUICK after the second arrived, and the second stands its full
        // HOLD.
        let shown: Vec<(&str, f32)> = log.fading(later + QUICK + FADE + Duration::from_millis(1));
        assert_eq!(shown, vec![("second", 1.0)]);
        assert_eq!(
            log.fading(later + QUICK).len(),
            2,
            "the first is still in full at the cut"
        );
    }

    #[test]
    fn the_history_is_bounded() {
        let start = Instant::now();
        let mut log = ChatLog::default();
        for index in 0..(MAX_LINES + 5) {
            log.say(format!("line {index}"), start);
        }
        assert_eq!(log.all().count(), MAX_LINES);
        assert_eq!(log.all().next(), Some("line 5"));
    }
}
