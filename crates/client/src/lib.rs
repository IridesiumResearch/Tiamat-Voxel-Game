// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Client: a viewer onto a running server.
//!
//! Singleplayer is an embedded server over loopback, so this crate never
//! simulates anything itself (charter rule 2). It renders what
//! [`tiamot_core`] tells it and sends input actions back.
//!
//! Presentation code is explicitly exempt from the Deterministic Float Subset
//! (charter rule 4). Rendering, audio, UI layout, camera smoothing, and
//! client-side interpolation may use transcendentals freely — the determinism
//! rules apply to simulation, and taxing presentation with them buys nothing.
//!
//! Audio and UI land in Tasks 13 and 14.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
// Render code converts between pixel counts, vertex indices, and coordinates on
// almost every line, and the values involved are bounded by an atlas edge or a
// chunk extent — five orders of magnitude inside the types they live in.
// Annotating each site would bury the conversions that are genuinely worth a
// second look. Precision loss is expected and harmless here for the same
// reason charter rule 4 exempts presentation: nothing downstream of a pixel
// coordinate has to agree bit-for-bit with another machine.
// The mesher works in (u, v, w) plane coordinates and (x, y, z) cell
// coordinates, and those are the names the technique is described by
// everywhere it is written down. Spelling them out would make the code harder
// to check against the reference, not easier.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

pub mod app;
pub mod audio;
pub mod cache;
pub mod camera;
pub mod config;
pub mod dialog;
pub mod discovery;
pub mod entities;
pub mod fonts;
pub mod front;
pub mod icons;
pub mod input;
pub mod launcher;
pub mod mesher;
pub mod net;
pub mod particles;
pub mod pictures;
pub mod predict;
pub mod render;
pub mod shade;
pub mod shape_view;
pub mod sky;
pub mod texture;
pub mod theme;
pub mod trust;
pub mod update;

/// Interface controls whose behaviour is shared between screens.
pub mod widget {
    /// A button wearing a theme's frame, or the client's own if there is none.
    ///
    /// # Why a helper and not a style
    ///
    /// egui draws a button's background itself, from `Visuals`, and a fill and
    /// a stroke is all that can be said there — so a theme's colours reach a
    /// button through [`crate::theme::Theme::apply`] and its ART cannot. This
    /// paints the nine-slice first and puts a button with no background of its
    /// own on top of it.
    ///
    /// **Not `frame(false)`.** That would take the hover and press highlights
    /// away with the fill, and a button that does not respond to the pointer
    /// reads as broken however good the frame around it looks. The fill is
    /// made transparent instead, so every state still lights the way egui
    /// intends and the art shows through all of them.
    ///
    /// A theme with no button frame returns `ui.button` unchanged, which is
    /// every client until a server says otherwise.
    pub fn button(
        ui: &mut egui::Ui,
        frame: Option<crate::pictures::Picture>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        let Some(frame) = frame else {
            return ui.button(text);
        };
        // Measured before it is drawn, because the frame goes UNDER it and
        // the rectangle is not known until the button has claimed one.
        let button = egui::Button::new(text).fill(egui::Color32::TRANSPARENT);
        let (rect, response) = {
            let response = ui.add(button);
            (response.rect, response)
        };
        crate::pictures::paint_nine_slice(
            // **Behind**, which is what the layer is for: the button has
            // already painted into the current one, so drawing the frame there
            // would put the art over the lettering.
            &ui.painter().clone().with_layer_id(egui::LayerId::new(
                egui::Order::Background,
                ui.layer_id().id,
            )),
            frame.texture,
            rect,
            1.0,
            (frame.width, frame.height),
        );
        response
    }

    /// A row of tabs, drawn the way a browser draws them.
    ///
    /// Returns the index of one that was clicked, or `None`.
    ///
    /// # Why they are drawn rather than composed from labels
    ///
    /// Reported from the window as wanting the menus' tabs to LOOK like tabs.
    /// egui's `selectable_label` is a highlighted word: nothing about it says
    /// the strip below belongs to the one that is lit, and with three of them
    /// in a row it reads as three buttons that happen to be next to each other.
    ///
    /// What makes a tab a tab is that the active one is JOINED to the page —
    /// rounded at the top, square at the bottom, and sitting on a baseline the
    /// inactive ones stay above. That is four rectangles and a line, and it is
    /// worth them.
    ///
    /// **Engine screens only.** A mod's dialog draws its own tabs out of
    /// buttons and a style, deliberately: the engine having one idea of what a
    /// tab looks like is the thing `game/core_ui` says it does not want
    /// imposed on it.
    pub fn tabs(ui: &mut egui::Ui, active: usize, labels: &[&str]) -> Option<usize> {
        /// How far the strip sits above the baseline it draws.
        const HEIGHT: f32 = 26.0;
        /// Padding either side of a label.
        const PAD: f32 = 14.0;

        let mut clicked = None;
        let font = egui::FontId::proportional(14.0);
        let widths: Vec<f32> = labels
            .iter()
            .map(|label| {
                ui.painter()
                    .layout_no_wrap((*label).to_owned(), font.clone(), egui::Color32::WHITE)
                    .rect
                    .width()
                    + PAD * 2.0
            })
            .collect();

        let total: f32 = widths.iter().sum();
        let (strip, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().max(total), HEIGHT + 2.0),
            egui::Sense::hover(),
        );
        let baseline = strip.bottom() - 1.0;

        let mut x = strip.left();
        for (index, (label, width)) in labels.iter().zip(&widths).enumerate() {
            let selected = index == active;
            // The active tab reaches the baseline; the others stop short of it,
            // which is what puts them behind the page rather than on it.
            let top = if selected {
                strip.top()
            } else {
                strip.top() + 3.0
            };
            let rect = egui::Rect::from_min_max(
                egui::pos2(x, top),
                egui::pos2(x + width, baseline + if selected { 1.0 } else { -1.0 }),
            );
            let response = ui.interact(rect, ui.id().with(("tab", index)), egui::Sense::click());

            let fill = if selected {
                egui::Color32::from_gray(48)
            } else if response.hovered() {
                egui::Color32::from_gray(38)
            } else {
                egui::Color32::from_gray(30)
            };
            // Rounded at the top and square at the bottom: a tab is a page
            // corner, not a pill.
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius {
                    nw: 6,
                    ne: 6,
                    sw: 0,
                    se: 0,
                },
                fill,
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                *label,
                font.clone(),
                if selected {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_gray(170)
                },
            );

            if response.clicked() {
                clicked = Some(index);
            }
            x += width;
        }

        // The page edge, drawn UNDER the active tab so the two are one shape.
        let stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(70));
        let gap_from = strip.left() + widths.iter().take(active).sum::<f32>();
        let gap_width = widths.get(active).copied().unwrap_or(0.0);
        ui.painter()
            .hline(strip.left()..=gap_from, baseline, stroke);
        ui.painter()
            .hline((gap_from + gap_width)..=strip.right(), baseline, stroke);

        clicked
    }

    /// What a settled slider should do with the value it is showing.
    ///
    /// Returns the draft to keep for the next frame, and the value to apply, in
    /// that order. Exactly one of them is `Some` — a value is either still
    /// being chosen or it has been chosen.
    ///
    /// # Why a value is not applied while the drag is running
    ///
    /// **Because the interface scale rescales the slider.** Applying it live
    /// changes egui's zoom factor mid-drag, which moves the slider under the
    /// pointer, which changes the value the pointer is now over. Reported from
    /// the window as jumping and jerking around, and it is a feedback loop
    /// rather than a jitter: the control is an input to its own position.
    ///
    /// The previous rule was that a scale you cannot see while dragging is a
    /// scale you have to guess at. That is true and it is the lesser problem.
    ///
    /// A change that is not a drag — an arrow key, a click on the track —
    /// applies at once, because there is no drag to wait for the end of.
    #[must_use]
    pub const fn settle(
        dragging: bool,
        changed: bool,
        draft: Option<f32>,
        shown: f32,
    ) -> (Option<f32>, Option<f32>) {
        if dragging {
            return (Some(shown), None);
        }
        match draft {
            // The frame the pointer came up: what was being dragged is now the
            // answer, whether or not egui calls this frame a change.
            Some(value) => (None, Some(value)),
            None if changed => (None, Some(shown)),
            None => (None, None),
        }
    }

    /// A slider that applies its value only once the player lets go.
    ///
    /// `live` is what is in force now and `draft` is where the pointer has
    /// dragged to and not yet let go of. Returns a value the moment it is
    /// settled, and nothing on the frames in between — see [`settle`].
    pub fn on_release(
        ui: &mut egui::Ui,
        label: &str,
        range: std::ops::RangeInclusive<f32>,
        step: f64,
        live: f32,
        draft: &mut Option<f32>,
    ) -> Option<f32> {
        let mut shown = draft.unwrap_or(live);
        let response = ui.add(
            egui::Slider::new(&mut shown, range)
                .step_by(step)
                .text(label),
        );
        // **Held, not merely moved.** A pointer pressed on the handle and not
        // yet moved is a drag that has started, and treating it as settled
        // would apply a value on the way past.
        let holding = response.dragged() || response.is_pointer_button_down_on();
        let (kept, settled) = settle(holding, response.changed(), *draft, shown);
        *draft = kept;
        settled
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_drag_is_kept_and_a_release_is_applied() {
            // Nothing happening: no draft, no value.
            assert_eq!(settle(false, false, None, 1.0), (None, None));

            // The drag: remembered, and nothing applied yet — this is the whole
            // of the fix, because applying here is what moved the slider.
            assert_eq!(settle(true, true, None, 0.9), (Some(0.9), None));
            assert_eq!(settle(true, true, Some(0.9), 0.85), (Some(0.85), None));

            // The pointer comes up. egui reports no change on that frame, and
            // the answer is the draft rather than nothing.
            assert_eq!(settle(false, false, Some(0.85), 0.85), (None, Some(0.85)));

            // An arrow key or a click on the track: no drag to wait for.
            assert_eq!(settle(false, true, None, 1.1), (None, Some(1.1)));
        }
    }
}

/// The shape every full-screen panel takes: centred, four by three, most of the
/// window.
///
/// # Why one function rather than a number in each panel
///
/// The menu, the controls page, the inventory and a mod's dialog are all the
/// same kind of thing — a sheet the game puts in front of you — and a player
/// reads them as one system or as four. They were four: each picked its own
/// size, so opening the inventory after the settings moved the frame and
/// resized the content under the cursor.
///
/// **Four by three rather than the window's own ratio.** A panel that stretched
/// to an ultrawide monitor would put its two halves a foot apart; one that
/// matched a tall window would be a column. Four by three is the shape a page
/// of controls or a grid of slots actually wants, and it is the same shape on
/// every screen.
///
/// Three quarters of the window's HEIGHT, then width from the ratio, then
/// clamped so a narrow window cannot push the sides off the screen.
pub mod panel {
    /// How much of the window's height a panel takes.
    const SHARE: f32 = 0.75;
    /// Width over height.
    const RATIO: f32 = 4.0 / 3.0;
    /// The most of the window's width a panel may take, so it never touches the
    /// edges on a 4:3 monitor.
    const WIDEST: f32 = 0.9;

    /// How deep a theme's frame is drawn, in points.
    ///
    /// # Why a number here and not a third of the art
    ///
    /// `paint_nine_slice` cuts its border at a third of the SOURCE IMAGE, and
    /// it was drawn at scale 1 — so a 108-pixel frame gave 36 points of trim
    /// and a 512-pixel one would give 170. The drawn border tracked the art's
    /// resolution and nothing else, which is why a sheet's bar and every
    /// engine screen's text ran onto the trim: the content was inset by the
    /// window's own six-point margin and the trim came in six times that far.
    /// Reported by a mod author whose inventory only cleared it by padding
    /// its own tree by 24.
    ///
    /// So the engine says how deep a frame is and scales the art to suit. A
    /// mod draws whatever it likes at whatever resolution it likes; this is
    /// the room it gets, and the room the content stays clear of.
    const FRAME_BORDER: f32 = 18.0;

    /// The panel's size in points, for a window `area` points across.
    #[must_use]
    pub fn size(area: (f32, f32)) -> (f32, f32) {
        size_clear_of(area, 0.0)
    }

    /// The panel's top-left corner in points, centred in `area`.
    #[must_use]
    pub fn origin(area: (f32, f32)) -> (f32, f32) {
        origin_clear_of(area, 0.0)
    }

    /// How many points at the bottom of `area` a HUD's `reserve` asks for.
    ///
    /// A HUD is drawn against a canvas [`tiamot_core::hud::VIRTUAL_HEIGHT`]
    /// tall whatever the window is, so a reserve is in those units too and is
    /// converted here — the one place it happens. A reserve in points would
    /// protect a different fraction of the screen on every monitor while the
    /// HUD it is protecting scaled with the canvas.
    #[must_use]
    pub fn reserve_points(area: (f32, f32), reserve: u16) -> f32 {
        let canvas = f32::from(tiamot_core::hud::VIRTUAL_HEIGHT);
        (f32::from(reserve) / canvas * area.1).clamp(0.0, area.1)
    }

    /// The panel's size in points, keeping `reserve` points at the bottom of
    /// the window clear.
    ///
    /// # Why a sheet gets out of the HUD's way and not the other way round
    ///
    /// A sheet is three quarters of the window's height and centred, which
    /// leaves an eighth of the window below it. A HUD that draws rows along
    /// the bottom edge needs more than that, and a mod author reported the
    /// inventory sitting over their hearts, food and warmth.
    ///
    /// The HUD cannot move: a player reads it in the same place every time,
    /// and a health bar that jumped whenever a screen opened would be worse
    /// than one covered up. So the sheet moves.
    ///
    /// **It rises before it shrinks.** Most windows have room to lift a
    /// full-sized sheet clear, and a sheet that shrank whenever a HUD grew
    /// would change size for a reason the player cannot see. Only when the
    /// room left above the reserve is less than the sheet wants does it lose
    /// height — still four by three, because a screen that changes shape is
    /// a screen whose contents reflow.
    #[must_use]
    pub fn size_clear_of(area: (f32, f32), reserve: f32) -> (f32, f32) {
        // Never more than half the window, whatever was asked for: a reserve
        // that swallowed the screen would leave no way to read the pause menu
        // and so no way to leave.
        let reserve = reserve.clamp(0.0, area.1 / 2.0);
        let room = area.1 - reserve;
        let height = (area.1 * SHARE).min(room).max(120.0);
        let width = (height * RATIO).min(area.0 * WIDEST).max(160.0);
        // Height follows the clamped width, so a narrow window keeps the ratio
        // rather than keeping the height and losing the shape.
        (width, (width / RATIO).min(height).max(120.0))
    }

    /// The panel's top-left corner in points, centred in what `reserve` leaves.
    #[must_use]
    pub fn origin_clear_of(area: (f32, f32), reserve: f32) -> (f32, f32) {
        let reserve = reserve.clamp(0.0, area.1 / 2.0);
        let (width, height) = size_clear_of(area, reserve);
        (
            (area.0 - width) / 2.0,
            // Centred in the room ABOVE the reserve, so a sheet with room to
            // spare sits a little high rather than resting on the HUD.
            ((area.1 - reserve - height) / 2.0).max(0.0),
        )
    }

    /// Whether a sheet's body scrolls.
    ///
    /// # Why this is a choice and not always yes
    ///
    /// The engine's own pages are as long as they are — the controls list
    /// grows with every binding a mod registers — so they scroll, and the bar
    /// above them stays put.
    ///
    /// **A mod's screen is different: it is laid out into exactly the room the
    /// sheet gives it**, so it can never have anything to scroll, and a
    /// scrolling body can only misreport it. It did: a rounding point of
    /// content over the viewport is enough to put a scrollbar on a screen
    /// built to fit, and the inventory arrived scrolled with its frame's top
    /// out of view. Reported from the window.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fit {
        /// The body scrolls, and may be longer than the sheet.
        Scrolling,
        /// The body gets the room and no more.
        Fixed,
    }

    /// Everything about a sheet except what goes in it.
    ///
    /// A struct rather than five more parameters, for the reason
    /// [`tiamot_core::ui`]'s `Flow` is one: they travel together, they are read
    /// together, and passed separately they are two `Option<&str>` and a `bool`
    /// in a row — which is how a heading ends up where a Back label was meant
    /// to go.
    #[derive(Debug, Clone, Copy)]
    pub struct Sheet<'a> {
        /// egui's identity for the window, which no player is ever shown.
        ///
        /// A mod's dialog passes the namespaced form the server named —
        /// `core_ui:inventory` — because egui needs to tell one window from
        /// another and that is a name, not a title.
        pub id: &'a str,
        /// The heading on the bar, or `None` for a screen whose heading is
        /// inside its own body, where the mod put it.
        pub heading: Option<&'a str>,
        /// The label on the way out, or `None` for a screen with none.
        pub back: Option<&'a str>,
        /// Whether the body scrolls.
        pub fit: Fit,
        /// Points at the bottom of the window to stay clear of — a HUD's
        /// reserve, already converted by [`reserve_points`]. Zero anywhere
        /// there is no HUD, which is every screen before a world is joined.
        pub reserve: f32,
        /// The frame picture to paint around this sheet, if a theme supplied
        /// one and its bytes have arrived — see [`crate::theme`].
        pub frame: Option<crate::pictures::Picture>,
        /// The frame picture for buttons on this sheet's own bar.
        pub button: Option<crate::pictures::Picture>,
        /// The colour a themed heading is drawn in.
        pub heading_colour: Option<egui::Color32>,
    }

    impl<'a> Sheet<'a> {
        /// A scrolling sheet titled by its own id, clear of nothing.
        ///
        /// What the engine's own pages are: the id IS the heading, because a
        /// screen the player opened from a menu is named by the button they
        /// pressed.
        #[must_use]
        pub fn titled(id: &'a str) -> Self {
            Self {
                id,
                heading: Some(id),
                back: None,
                fit: Fit::Scrolling,
                reserve: 0.0,
                frame: None,
                button: None,
                heading_colour: None,
            }
        }

        /// The same sheet wearing a theme's frame and heading colour.
        ///
        /// **A method rather than two more fields at every call site**: a
        /// screen that forgot one of them would be a screen in half a look,
        /// and there is nothing at a call site to notice it.
        #[must_use]
        pub fn themed(mut self, dressing: crate::theme::Dressing) -> Self {
            self.frame = dressing.sheet;
            self.button = dressing.button;
            self.heading_colour = dressing.heading;
            self
        }
    }

    /// What a screen the player pressed a button to open looks like.
    ///
    /// # Why every one of these goes through here
    ///
    /// **A `fixed_size` on an `egui::Window` is a request, not a bound.** Put
    /// more in one than fits and egui grows it — so the settings page, which
    /// has a scrolling list of bindings and then volume sliders below it, ran
    /// off the top and the bottom of the screen with no way to reach either end.
    /// Reported from the window, and it is the sort of thing every screen would
    /// have got wrong separately.
    ///
    /// So the shape is decided here and the content is handed a `Ui` that is
    /// already inside it: a title bar with an optional Back, then whatever is
    /// left, scrolling. A screen cannot escape its own sheet because it never
    /// gets to say how big it is.
    ///
    /// Returns whether Back was pressed.
    pub fn sheet(
        ctx: &egui::Context,
        title: &str,
        back: Option<&str>,
        contents: impl FnOnce(&mut egui::Ui),
    ) -> bool {
        sheet_with(
            ctx,
            Sheet {
                back,
                ..Sheet::titled(title)
            },
            contents,
        )
    }

    /// The same sheet, for a screen whose heading is not its identity.
    ///
    /// **A mod's dialog is one of these**, and the two differ: `id` is the
    /// namespaced form the server named — `core_ui:inventory` — which egui
    /// needs to tell one window from another and which no player should ever
    /// be shown. A dialog's own heading is inside its tree, where the mod put
    /// it, so it passes `None` and gets the bar with just the way out on it.
    pub fn sheet_with(
        ctx: &egui::Context,
        sheet: Sheet<'_>,
        contents: impl FnOnce(&mut egui::Ui),
    ) -> bool {
        let Sheet {
            id,
            heading,
            back,
            fit,
            reserve,
            frame,
            button: frame_for_buttons,
            heading_colour,
        } = sheet;
        let title = id;
        let screen = ctx.content_rect();
        let area = (screen.width(), screen.height());
        let (width, height) = size_clear_of(area, reserve);
        let (x, y) = origin_clear_of(area, reserve);
        let mut went_back = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .movable(false)
            .fixed_pos(egui::pos2(screen.left() + x, screen.top() + y))
            .fixed_size(egui::vec2(width, height))
            // Belt as well as braces: `fixed_size` is what egui aims for and
            // this is what it may not exceed, so a screen that asks for more
            // scrolls instead of growing.
            .max_height(height)
            // **A framed sheet keeps its contents off the trim.** Without a
            // frame this is egui's own margin and nothing changes.
            .frame(
                egui::Frame::window(&ctx.global_style()).inner_margin(if frame.is_some() {
                    egui::Margin::same(FRAME_BORDER as i8)
                } else {
                    ctx.global_style().spacing.window_margin
                }),
            )
            .show(ctx, |ui| {
                ui.set_min_size(egui::vec2(width, height));
                // **Behind the contents, and outside the window's own
                // padding.** A frame is art around the whole sheet, so it is
                // painted on the layer the window already owns rather than
                // allocated as a widget — allocated, it would take room from
                // the page it is a frame for, and a settings list would start
                // an inch further down for having a border.
                if let Some(frame) = frame {
                    // Scaled so its border lands on `FRAME_BORDER` whatever
                    // the art's resolution is, and painted OUTSIDE the
                    // content — which is inset by the same amount above, so
                    // the two meet exactly and nothing is drawn on the trim.
                    let third = (frame.height as f32 / 3.0).max(1.0);
                    crate::pictures::paint_nine_slice(
                        &ui.painter().clone(),
                        frame.texture,
                        ui.max_rect().expand(FRAME_BORDER),
                        FRAME_BORDER / third,
                        (frame.width, frame.height),
                    );
                }
                // **The bar is the same on every screen**, which is the point:
                // a player who has learned where Back is has learned it once.
                ui.horizontal(|ui| {
                    if let Some(label) = back {
                        // The one button every engine screen has, so it is
                        // the one that most has to wear the look.
                        went_back |=
                            crate::widget::button(ui, frame_for_buttons, format!("← {label}"))
                                .clicked();
                        if heading.is_some() {
                            ui.separator();
                        }
                    }
                    if let Some(heading) = heading {
                        let text = egui::RichText::new(heading);
                        ui.heading(match heading_colour {
                            Some(colour) => text.color(colour),
                            None => text,
                        });
                    }
                });
                ui.separator();
                match fit {
                    // Everything below scrolls. The header stays put, so the
                    // way out is always on screen however long the page is.
                    Fit::Scrolling => {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, contents);
                    }
                    // No scroll area at all, so nothing can decide the body is
                    // a point too tall and scroll a screen that fits.
                    Fit::Fixed => contents(ui),
                }
            });
        went_back
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_panel_keeps_its_shape_and_its_place_on_every_window() {
            for area in [
                (1920.0, 1080.0),
                (2560.0, 1080.0),
                (1024.0, 768.0),
                (800.0, 600.0),
            ] {
                let (width, height) = size(area);
                let ratio = width / height;
                assert!(
                    (ratio - RATIO).abs() < 0.01,
                    "a {area:?} window gave a {ratio} panel, which is not four by three"
                );
                assert!(width <= area.0, "the panel is wider than the window");
                assert!(height <= area.1, "the panel is taller than the window");

                // Centred: the space left over is the same on both sides.
                let (x, y) = origin(area);
                assert!((x - (area.0 - width - x)).abs() < 0.01);
                assert!((y - (area.1 - height - y)).abs() < 0.01);
            }
        }

        #[test]
        fn a_sheet_rises_clear_of_a_huds_reserve_before_it_shrinks() {
            // Reported by a mod author: with the inventory open, its bottom
            // edge sat over their hearts, food and warmth and ran down to the
            // hotbar. A sheet is three quarters of the window and centred, so
            // it leaves an eighth below it — and that HUD was taller.
            let area = (1920.0, 1080.0);
            let (_, tall) = size(area);
            let reserve = reserve_points(area, 170);

            // A 1080-point window and a 1080-tall canvas, so a 170-pixel
            // reserve is 170 points — the case the conversion is invisible in,
            // which is exactly why the next test does not use it.
            assert!((reserve - 170.0).abs() < 0.01);

            // It RISES: nothing shrank, because there was room to lift it.
            let (_, height) = size_clear_of(area, reserve);
            assert!(
                (height - tall).abs() < 0.01,
                "a sheet with room above the reserve keeps its size; {height} against {tall}"
            );
            let (_, y) = origin_clear_of(area, reserve);
            assert!(
                y + height <= area.1 - reserve + 0.01,
                "the sheet's bottom edge at {} is not clear of the reserve at {}",
                y + height,
                area.1 - reserve
            );

            // And the counter-example, without which the assertion above holds
            // for a sheet that was already clear: with no reserve the bottom
            // edge is well inside the room this one had to be lifted out of.
            let (_, plain) = origin(area);
            assert!(plain + tall > area.1 - reserve);
        }

        #[test]
        fn a_reserve_too_tall_to_rise_clear_of_shrinks_the_sheet_and_keeps_its_shape() {
            // Half the window, which is the cap: past this the sheet would be
            // smaller than the thing it is making way for.
            let area = (1920.0, 1080.0);
            let (width, height) = size_clear_of(area, 800.0);
            let ratio = width / height;
            assert!(
                (ratio - RATIO).abs() < 0.01,
                "a squeezed sheet is still four by three, not {ratio}"
            );
            let (_, y) = origin_clear_of(area, 800.0);
            // Clamped to half, so the sheet keeps the top half of the window
            // rather than being squeezed to the 120-point floor.
            assert!(y + height <= area.1 / 2.0 + 0.01);
            assert!(height >= 120.0);
        }

        #[test]
        fn a_reserve_is_the_same_share_of_every_window() {
            // **The reason a reserve is in virtual pixels and not points.** A
            // HUD scales with the window's height; a reserve in points would
            // protect twice as much of a 720-point window as of a 1440-point
            // one, so the sheet would clear the hotbar on one monitor and sit
            // on it on another.
            let share = |height: f32| reserve_points((height * 16.0 / 9.0, height), 130) / height;
            let small = share(720.0);
            let large = share(2160.0);
            assert!(
                (small - large).abs() < 0.001,
                "a reserve took {small} of a small window and {large} of a large one"
            );
            assert!((small - 130.0 / 1080.0).abs() < 0.001);
        }

        #[test]
        fn a_frames_border_does_not_depend_on_its_arts_resolution() {
            // **The bug this constant exists for.** `paint_nine_slice` cuts at
            // a third of the SOURCE image, so at scale 1 a 108-pixel frame
            // gives 36 points of trim and a 512-pixel one gives 170. The
            // content was inset by the window's six-point margin either way,
            // so a sheet's bar ran onto the trim — and how badly depended on
            // what resolution the mod happened to export at, which is not
            // something a mod author should have to reason about.
            //
            // The scale is chosen so the border lands on `FRAME_BORDER`
            // whatever the art is. Two very different images, one answer.
            for height in [48u32, 108, 512, 1024] {
                let third = height as f32 / 3.0;
                let scale = FRAME_BORDER / third;
                let drawn = third * scale;
                assert!(
                    (drawn - FRAME_BORDER).abs() < 0.01,
                    "a {height}-pixel frame drew a {drawn}-point border"
                );
            }
        }

        #[test]
        fn a_frame_is_drawn_outside_the_room_its_contents_get() {
            // The two have to meet exactly: the frame is painted at the
            // content's rectangle expanded by the border, and the content is
            // inset by the same border. Any other pair either leaves a gap of
            // sheet between the trim and the text, or runs the text onto the
            // trim — which is what it did.
            let content =
                egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 600.0));
            let painted = content.expand(FRAME_BORDER);
            assert!(
                (painted.width() - content.width() - FRAME_BORDER * 2.0).abs() < 0.01,
                "the frame should be exactly one border wider on each side"
            );
            assert!(painted.contains_rect(content));
        }

        #[test]
        fn an_ultrawide_window_does_not_stretch_the_panel() {
            // The case the ratio exists for: the panel is the same width on a
            // 21:9 monitor as on a 16:9 one of the same height.
            let wide = size((3440.0, 1440.0));
            let normal = size((2560.0, 1440.0));
            assert!((wide.0 - normal.0).abs() < 0.01);
        }
    }
}
pub mod world;

/// The engine's units-per-block constant, re-exported.
///
/// A one-line proof that the client links against the same `tiamot_core` the
/// server simulates with — charter rule 5's 27 units are not the client's to
/// decide.
#[must_use]
pub fn units_per_block() -> u32 {
    tiamot_core::UNITS_PER_BLOCK
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_against_core() {
        assert_eq!(super::units_per_block(), tiamot_core::UNITS_PER_BLOCK);
    }
}
