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

/// The lines, oldest first, and the state of the box they are typed in.
#[derive(Debug, Default)]
pub struct ChatLog {
    lines: VecDeque<Line>,
    /// Whether the chat box is open.
    pub open: bool,
    /// Whether the box has just opened and should take the keyboard — true
    /// exactly once per opening; see `App::take_chat_focus`.
    pub focus: bool,
    /// What has been typed and not yet sent.
    pub draft: String,
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

/// How wide the box is, in points: the input line's width, and where a
/// long line wraps.
pub const WIDTH: f32 = 420.0;

/// How tall the history may be before it scrolls, in points.
pub const HISTORY_HEIGHT: f32 = 220.0;

/// The most of the box there can be: the history, the input line and the
/// frame around them. What the area is told it may grow to.
const MOST_HEIGHT: f32 = HISTORY_HEIGHT + 60.0;

/// What the box is drawn from.
pub struct Panel<'a> {
    /// The lines to show, oldest first, each with how visible it is.
    pub lines: &'a [(String, f32)],
    /// Whether the input line is open.
    pub open: bool,
    /// The line being typed.
    pub draft: &'a mut String,
    /// Whether the input should take the keyboard this frame.
    pub focus: bool,
}

/// What drawing the box said back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drawn {
    /// Enter was pressed on a line: send it.
    pub send: bool,
    /// Escape was pressed: close the box.
    pub close: bool,
    /// Where the box was drawn.
    pub rect: egui::Rect,
    /// Where the input line was drawn, when the box was open.
    pub input: Option<egui::Rect>,
}

/// Draws the chat box in the bottom-left corner.
///
/// # Why the box is told its size, every frame
///
/// Reported from the window: "the chat seems to shrink a little each time I
/// send a message." An anchored `Area` hands its contents LAST frame's
/// rectangle as this frame's room, and the input line takes the lesser of
/// its own width and that room. Sending closes the box, which shrinks to
/// the few short lines still fading; opening it again gave the input line
/// that narrow room, and the box could only ever grow back when a wider
/// line came along — so it shrank a little with every message, and the
/// history was squeezed into the closed box's height until it scrolled. So
/// the room is set here, once, before anything is laid into it: as wide as
/// the input line, as tall as the history and the line together, and the
/// area shrinks to its contents from there as it always did.
#[must_use]
pub fn draw(ctx: &egui::Context, panel: Panel<'_>) -> Drawn {
    let Panel {
        lines,
        open,
        draft,
        focus,
    } = panel;
    let mut send = false;
    let mut close = false;
    let mut input = None;
    let area = egui::Area::new(egui::Id::new("chat"))
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_black_alpha(140))
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.set_max_size(egui::vec2(WIDTH, MOST_HEIGHT));
                    // Newest at the bottom, which is where a reader's eye is.
                    // Only the last few unless the box is open, so chat does
                    // not cover the world nobody is talking about.
                    let shown = if open { 12 } else { 5 };
                    let skip = lines.len().saturating_sub(shown);
                    egui::ScrollArea::vertical()
                        .max_height(HISTORY_HEIGHT)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for (line, alpha) in lines.iter().skip(skip) {
                                ui.label(
                                    egui::RichText::new(line)
                                        .color(egui::Color32::WHITE.gamma_multiply(*alpha)),
                                );
                            }
                        });
                    if !open {
                        return;
                    }
                    let response = ui.add(
                        egui::TextEdit::singleline(draft)
                            .desired_width(WIDTH)
                            .hint_text("say something"),
                    );
                    input = Some(response.rect);
                    // **Once, on the frame it opens — never every frame.**
                    //
                    // Reported from the window: chat did not work. It took the
                    // keys and never sent, because egui reports a single-line
                    // field's Enter as `lost_focus`, and a field handed focus
                    // back on every frame never loses it. Enter was landing on
                    // a box that immediately took focus again, so the line sat
                    // there being typed into for ever.
                    if focus {
                        response.request_focus();
                    }
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        send = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
        });
    Drawn {
        send,
        close,
        rect: area.response.rect,
        input,
    }
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

    /// Draws the box once into a headless context, with `lines` shown.
    fn frame(
        ctx: &egui::Context,
        lines: &[(String, f32)],
        open: bool,
        draft: &mut String,
    ) -> Drawn {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            ..Default::default()
        };
        let mut drawn = None;
        let _ = ctx.run_ui(raw, |root| {
            let ctx = root.ctx().clone();
            drawn = Some(draw(
                &ctx,
                Panel {
                    lines,
                    open,
                    draft,
                    focus: false,
                },
            ));
        });
        drawn.expect("drawn")
    }

    #[test]
    fn the_box_keeps_its_width_and_height_across_a_send() {
        // **Reported from the window: "the chat seems to shrink a little each
        // time I send a message."** An anchored area hands its contents last
        // frame's rectangle as this frame's room. Sending closes the box to a
        // few short fading lines; reopening it in that narrow, short room gave
        // the input line that width and the history that height.
        let ctx = egui::Context::default();
        crate::app::install_fonts(&ctx);
        let history: Vec<(String, f32)> = (0..14).map(|n| (format!("line {n}"), 1.0)).collect();
        let mut draft = String::new();

        // Open, twice so the area has settled.
        frame(&ctx, &history, true, &mut draft);
        let settled = frame(&ctx, &history, true, &mut draft);
        let input = settled.input.expect("an input line while open");
        assert!(
            (input.width() - WIDTH).abs() < 1.0,
            "the input line should be {WIDTH} wide, was {}",
            input.width()
        );

        // Sent: closed, one short line still fading. The box is small.
        let fading = vec![("hi".to_owned(), 0.6)];
        let closed = frame(&ctx, &fading, false, &mut draft);
        assert!(closed.rect.width() < settled.rect.width() / 2.0);
        assert!(closed.rect.height() < settled.rect.height() / 2.0);

        // Reopened: the very first frame is the full size again, not the
        // closed box's.
        let reopened = frame(&ctx, &history, true, &mut draft);
        let input = reopened.input.expect("an input line while open");
        assert!(
            (input.width() - WIDTH).abs() < 1.0,
            "after a send the input line shrank to {}",
            input.width()
        );
        assert!(
            (reopened.rect.height() - settled.rect.height()).abs() < 1.0,
            "after a send the box was {} tall instead of {}",
            reopened.rect.height(),
            settled.rect.height()
        );
    }
}
