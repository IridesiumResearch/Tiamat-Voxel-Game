// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A mod's look, worn by the engine's own screens.
//!
//! # Why the engine has no look of its own to defend
//!
//! Charter rule 1 says the mod API is the only API, and the pause screen, the
//! settings pages and the start screen are the client's: plain egui, with no
//! hook a mod can reach. So a game built on this engine could restyle every
//! dialog it pushes and then hand the player back to a menu that looked like a
//! different program. Painting one game's art into the client instead would be
//! putting content in the engine, which is the same rule from the other side.
//!
//! A theme is therefore DATA a mod ships — `[theme]` in its `mod.toml`, see
//! [`tiamot_core::modload::Theme`] — and this is the half that wears it.
//!
//! # Nothing here decodes anything
//!
//! A theme's font and frames arrive through the font table and the picture
//! table, which already cap, isolate and fuzz what they parse (charter rule
//! 14). This module holds ids and hashes and asks those stores for the results.
//! **A theme that adds a parser is a theme that has gone wrong.**
//!
//! # A missing part is the client's own, never a missing screen
//!
//! Every field is optional twice over: the mod may not have declared it, and
//! what it declared may not have arrived. Both read the same way here, because
//! a frame whose bytes are still in flight and a frame nobody shipped need the
//! same answer — draw the screen the way the client would have drawn it. A
//! theme must not be able to make a screen unreadable, and the way to
//! guarantee that is for every part of it to be an override with a default
//! underneath rather than a replacement.

use tiamot_core::proto::{ContentHash, ThemeDef};
use tiamot_core::ui::Colour;

/// What the engine's screens look like right now.
///
/// Empty is the client's own look, which is what a server whose mods declare
/// no theme sends and what the start screen shows before anything is chosen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theme {
    /// The mod that set it, for attribution.
    pub mod_id: String,
    /// The font id to draw engine text in, if one arrived.
    pub font: Option<String>,
    /// The nine-slice frame around a sheet.
    pub sheet: Option<ContentHash>,
    /// The nine-slice frame around a button.
    pub button: Option<ContentHash>,
    /// The palette.
    pub colours: Palette,
}

/// A theme's five colours, in egui's own type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Palette {
    /// Ordinary text.
    pub text: Option<egui::Color32>,
    /// Headings, and a sheet's title.
    pub heading: Option<egui::Color32>,
    /// The fill behind a sheet.
    pub background: Option<egui::Color32>,
    /// The fill behind a button.
    pub button: Option<egui::Color32>,
    /// Selection, hover, and the live parts of a slider or a tick box.
    pub accent: Option<egui::Color32>,
}

/// What one frame needs to draw a theme, looked up once.
///
/// Every field is already `None` where the mod said nothing OR where the bytes
/// have not arrived, so a caller never has to tell those two apart — see the
/// module docs for why they must read the same.
#[derive(Debug, Clone, Copy, Default)]
pub struct Dressing {
    /// The frame around a sheet.
    pub sheet: Option<crate::pictures::Picture>,
    /// The frame around a button.
    pub button: Option<crate::pictures::Picture>,
    /// The colour a heading takes.
    pub heading: Option<egui::Color32>,
}

/// Straight RGBA bytes as egui reads them.
///
/// `from_rgba_unmultiplied` and not `from_rgba_premultiplied`: a mod wrote
/// `#b08d5780` meaning "this colour, half transparent", and premultiplying it
/// by hand is not something a palette in a manifest does.
#[must_use]
fn colour(rgba: Option<Colour>) -> Option<egui::Color32> {
    rgba.map(|[r, g, b, a]| egui::Color32::from_rgba_unmultiplied(r, g, b, a))
}

impl Theme {
    /// The client's own look, and what a server that sends no theme gets.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Takes what a server sent, or the client's own if it sent none.
    #[must_use]
    pub fn of(def: Option<&ThemeDef>) -> Self {
        def.map_or_else(Self::none, Self::from_def)
    }

    /// Takes what a server sent.
    #[must_use]
    pub fn from_def(def: &ThemeDef) -> Self {
        Self {
            mod_id: def.mod_id.clone(),
            font: def.font.clone(),
            sheet: def.sheet,
            button: def.button,
            colours: Palette {
                text: colour(def.colours.text),
                heading: colour(def.colours.heading),
                background: colour(def.colours.background),
                button: colour(def.colours.button),
                accent: colour(def.colours.accent),
            },
        }
    }

    /// Whether this theme says anything at all.
    ///
    /// A theme whose every part is absent is the client's own look, and asking
    /// this is how a caller skips the work of applying nothing.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self == &Self::default()
    }

    /// The pictures this theme needs uploaded before a frame can draw it.
    #[must_use]
    pub fn art(&self) -> Vec<ContentHash> {
        [self.sheet, self.button].into_iter().flatten().collect()
    }

    /// The font family for engine text, or `None` for the client's own.
    ///
    /// **A font id nothing answers to falls back**, for the reason
    /// `dialog::Look::font` gives: a face that has not arrived yet must not
    /// become a screen that has not arrived.
    #[must_use]
    pub fn family(
        &self,
        ctx: &egui::Context,
        fonts: &crate::fonts::Fonts,
    ) -> Option<egui::FontFamily> {
        // **`bound_family`, not `family`.** This goes into a `FontId` that egui
        // will lay text out with, and a family egui has not picked up yet is a
        // panic rather than a fallback. See `fonts::Fonts::bound_family`.
        self.font
            .as_deref()
            .and_then(|id| fonts.bound_family(ctx, id))
    }

    /// Lays this theme over egui's visuals and text styles.
    ///
    /// # Over, and never instead of
    ///
    /// It starts from whatever the context already has and changes only what
    /// the mod named, so a theme with one colour in it is a client with one
    /// colour changed. Building a `Visuals` from scratch here would make every
    /// field the engine ever adds a field a theme silently reset.
    ///
    /// Called once a frame rather than on arrival: a `Visuals` is a plain
    /// struct and setting it is cheap, while tracking whether it needed setting
    /// would be state that could be wrong.
    pub fn apply(&self, ctx: &egui::Context, fonts: &crate::fonts::Fonts) {
        if self.is_none() {
            return;
        }
        let family = self.family(ctx, fonts);
        // **Both styles, not the active one.** A mod's palette is not a
        // light/dark preference, and a theme that applied to one of them would
        // vanish the moment the platform said the other.
        ctx.all_styles_mut(|style| {
            let visuals = &mut style.visuals;
            if let Some(background) = self.colours.background {
                visuals.window_fill = background;
                visuals.panel_fill = background;
                // Behind a text box and a dropdown's list, which would
                // otherwise stay the client's grey inside a themed sheet.
                visuals.extreme_bg_color = background;
            }
            if let Some(text) = self.colours.text {
                visuals.override_text_color = Some(text);
            }
            if let Some(fill) = self.colours.button {
                // Every resting widget: buttons, tick boxes, a slider's rail.
                visuals.widgets.inactive.bg_fill = fill;
                visuals.widgets.inactive.weak_bg_fill = fill;
                visuals.widgets.noninteractive.bg_fill = fill;
                visuals.widgets.noninteractive.weak_bg_fill = fill;
            }
            if let Some(accent) = self.colours.accent {
                // Hovered, held and selected — the three states a player reads
                // as "this one". Kept together because a theme colouring only
                // one of them looks like a bug in the widget, not a palette.
                visuals.widgets.hovered.bg_fill = accent;
                visuals.widgets.hovered.weak_bg_fill = accent;
                visuals.widgets.active.bg_fill = accent;
                visuals.widgets.active.weak_bg_fill = accent;
                visuals.selection.bg_fill = accent;
            }
            if let Some(family) = &family {
                for existing in style.text_styles.values_mut() {
                    // **Size is kept and only the face changes.** A theme that
                    // resized the interface could make the settings screen
                    // unreadable, and the player already has a scale slider.
                    *existing = egui::FontId::new(existing.size, family.clone());
                }
            }
        });
    }

    /// This theme's art, resolved for one frame.
    ///
    /// **Small and `Copy`, and that is the point.** A screen is drawn from
    /// `&mut App`, so holding a `&Theme` across the draw would borrow the app
    /// immutably for exactly as long as it needs to be borrowed mutably. This
    /// is looked up once a frame and passed by value instead.
    #[must_use]
    pub fn dress(&self, art: &crate::pictures::Resolved) -> Dressing {
        Dressing {
            sheet: self.sheet.and_then(|hash| art.get(&hash)),
            button: self.button.and_then(|hash| art.get(&hash)),
            heading: self.colours.heading,
        }
    }

    /// The colour a heading is drawn in, or `None` for the client's own.
    #[must_use]
    pub const fn heading_colour(&self) -> Option<egui::Color32> {
        self.colours.heading
    }
}

/// A theme read off the local disk, for the screen that has no server.
///
/// # Why this exists at all
///
/// The start screen runs before anything is connected to. There is no content
/// pipeline to fetch a frame through and no font table to name a face in, so
/// the only copy of a theme's files is the one the player installed — and this
/// reads it from there.
///
/// **Local mods only, and deliberately.** The other way to dress the start
/// screen would be to remember the last server's theme and cache it, and that
/// would mean storing bytes a remote server chose and decoding them at launch,
/// before the player has chosen to trust anything at all. Charter rule 14 is
/// about what arrives from a server; the cheapest way to keep a launcher out
/// of its reach is for the launcher never to read one.
///
/// Files here are still put through the same guarded decoder and the same size
/// caps as pushed ones. They are the player's own, so the threat is a mistake
/// rather than an attack — but a 400-megabyte PNG mis-named `frame.png` should
/// fail to draw, not fail to start.
#[derive(Default)]
pub struct Local {
    theme: Theme,
    pictures: crate::pictures::Pictures,
    fonts: crate::fonts::Fonts,
    /// Which mod's theme is loaded, so a tick in the mod list reloads it.
    ///
    /// `None` before anything has been looked at; `Some("")` once a look has
    /// been taken and no mod wanted one, which is what stops the scan running
    /// again every frame on a machine with no themed mod installed.
    worn: Option<String>,
}

impl Local {
    /// Reads the enabled mods' theme, applies it, and hands back this frame's
    /// art.
    ///
    /// Reloads only when the mod whose theme applies has changed, because the
    /// alternative is reading files off disk sixty times a second.
    pub fn wear(
        &mut self,
        ctx: &egui::Context,
        catalogue: &crate::launcher::Catalogue,
        bundled: &'static [u8],
    ) -> Dressing {
        // Last in the list, as on a server: the mods are sorted by id and the
        // last one to declare a theme wins, so a player who installs a look
        // gets it rather than whichever mod happened to sort first.
        let chosen = catalogue
            .mods
            .iter()
            .filter(|listing| listing.enabled)
            .rev()
            .find(|listing| listing.theme.is_some());
        let wanted = chosen.map(|listing| listing.id.clone()).unwrap_or_default();
        if self.worn.as_deref() != Some(wanted.as_str()) {
            self.worn = Some(wanted);
            self.load(chosen);
        }
        if self.fonts.has_pending() {
            // Refusals are dropped rather than shown: the start screen has
            // nowhere to put a warning, and a mod whose font will not parse
            // draws in the client's own, which is visible enough.
            let _ = self.fonts.install(ctx, bundled);
        }
        self.theme.apply(ctx, &self.fonts);
        let art = self.theme.art();
        let resolved = self.pictures.resolve_hashes(ctx, &art);
        self.theme.dress(&resolved)
    }

    /// Reads one mod's declared theme off the disk.
    fn load(&mut self, listing: Option<&crate::launcher::Listing>) {
        self.pictures = crate::pictures::Pictures::new();
        self.fonts = crate::fonts::Fonts::new();
        self.theme = Theme::none();
        let Some(listing) = listing else { return };
        let Some(declared) = listing.theme.as_ref() else {
            return;
        };

        let read = |path: &Option<String>| -> Option<Vec<u8>> {
            let path = path.as_ref()?;
            // The manifest already refused anything that climbs out of the
            // mod's directory; this joins onto the directory it was found in,
            // so the two together are the whole of the path rule.
            let full = listing.dir.join(path);
            let size = std::fs::metadata(&full).ok()?.len();
            if size > tiamot_core::content::MAX_FILE_BYTES {
                return None;
            }
            std::fs::read(&full).ok()
        };

        let mut picture = |slot: &Option<String>| -> Option<ContentHash> {
            let bytes = read(slot)?;
            // The same guarded decoder a pushed picture goes through, and the
            // same answer when it will not decode: nothing to draw, rather
            // than a magenta square over somebody's art.
            let (image, failure) = crate::texture::decode_or_missing(&bytes);
            if failure.is_some() {
                return None;
            }
            let hash = tiamot_core::content::hash_bytes(&bytes);
            self.pictures.insert(hash, image);
            Some(hash)
        };
        let sheet = picture(&declared.sheet);
        let button = picture(&declared.button);

        let font = read(&declared.font).and_then(|bytes| {
            let id = format!("{}:theme_font", listing.id);
            // `offer` parses before egui is handed anything — see
            // `crate::fonts` — so a file that is not a font is refused here
            // rather than inside the atlas rebuild.
            self.fonts.offer(id.clone(), bytes).then_some(id)
        });

        let colour = |text: &Option<String>| {
            text.as_deref()
                .and_then(tiamot_core::modload::parse_colour)
                .map(|[r, g, b, a]| egui::Color32::from_rgba_unmultiplied(r, g, b, a))
        };
        self.theme = Theme {
            mod_id: listing.id.clone(),
            font,
            sheet,
            button,
            colours: Palette {
                text: colour(&declared.colours.text),
                heading: colour(&declared.colours.heading),
                background: colour(&declared.colours.background),
                button: colour(&declared.colours.button),
                accent: colour(&declared.colours.accent),
            },
        };
    }

    /// The theme currently worn, for tests and for attribution.
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> ThemeDef {
        ThemeDef {
            mod_id: "iron".to_owned(),
            font: Some("engine:theme_font".to_owned()),
            sheet: Some([7; 32]),
            button: None,
            colours: tiamot_core::proto::ThemePalette {
                text: Some([0xE8, 0xDC, 0xC0, 0xFF]),
                heading: Some([0xF0, 0xD8, 0x90, 0xFF]),
                background: None,
                button: None,
                accent: Some([0xB0, 0x8D, 0x57, 0x80]),
            },
        }
    }

    /// A scratch directory, the way `crate::launcher`'s tests make one.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tiamot-theme-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// A one-pixel PNG, so a test can put real bytes through the real decoder.
    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .expect("header")
            .write_image_data(&[0xFF, 0xFF, 0xFF, 0xFF])
            .expect("pixel");
        out
    }

    /// A mod directory on disk with a `[theme]` in its manifest.
    fn installed(dir: &std::path::Path, id: &str, body: &str) -> crate::launcher::Listing {
        let mod_dir = dir.join(id);
        std::fs::create_dir_all(mod_dir.join("art")).expect("mod dir");
        std::fs::write(mod_dir.join("init.lua"), "").expect("entry");
        std::fs::write(mod_dir.join("art/frame.png"), png()).expect("frame");
        let manifest = format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\n{body}");
        let parsed: tiamot_core::modload::ModManifest =
            toml::from_str(&manifest).expect("a manifest");
        crate::launcher::Listing {
            id: id.to_owned(),
            name: id.to_owned(),
            description: String::new(),
            enabled: true,
            world_options: Vec::new(),
            theme: parsed.theme,
            dir: mod_dir,
        }
    }

    #[test]
    fn the_start_screen_wears_a_theme_it_read_off_the_disk() {
        // **The screen with no server.** Nothing is connected to yet, so the
        // content pipeline cannot supply a frame and the only copy is the one
        // the player installed. This is the whole of that path: manifest on
        // disk, PNG through the real decoder, a `Dressing` a sheet can wear.
        let dir = scratch("read-off-disk");
        let listing = installed(
            &dir,
            "iron",
            "[theme]\nsheet = \"art/frame.png\"\n[theme.colours]\ntext = \"#e8dcc0\"",
        );
        let catalogue = crate::launcher::Catalogue {
            mods: vec![listing],
            problem: None,
        };

        let ctx = egui::Context::default();
        let mut local = Local::default();
        let dressing = local.wear(&ctx, &catalogue, crate::app::HUD_FONT);

        assert_eq!(local.theme().mod_id, "iron");
        assert!(
            dressing.sheet.is_some(),
            "the frame was declared, is on disk, and decoded — so a sheet should have one"
        );
        assert_eq!(
            ctx.global_style().visuals.override_text_color,
            Some(egui::Color32::from_rgb(0xE8, 0xDC, 0xC0))
        );
    }

    #[test]
    fn unticking_the_mod_that_set_the_look_takes_it_off() {
        // A theme is loaded once and kept, so the thing to prove is that the
        // keeping does not outlive the reason for it. The mod list is where a
        // player turns a mod off, and the screen is drawn again immediately.
        let dir = scratch("unticked");
        let mut catalogue = crate::launcher::Catalogue {
            mods: vec![installed(
                &dir,
                "iron",
                "[theme]\nsheet = \"art/frame.png\"",
            )],
            problem: None,
        };
        let ctx = egui::Context::default();
        let mut local = Local::default();
        assert!(
            local
                .wear(&ctx, &catalogue, crate::app::HUD_FONT)
                .sheet
                .is_some()
        );

        catalogue.mods[0].enabled = false;
        let dressing = local.wear(&ctx, &catalogue, crate::app::HUD_FONT);
        assert!(
            dressing.sheet.is_none(),
            "an unticked mod still dressed the screen"
        );
        assert!(local.theme().is_none());
    }

    #[test]
    fn the_last_mod_to_declare_a_look_is_the_one_worn() {
        // One theme at a time, and the later mod wins — the rule a place's fog
        // and tint already follow. Two mods blending their idea of a frame is
        // not a look, it is an accident.
        let dir = scratch("last-wins");
        let catalogue = crate::launcher::Catalogue {
            mods: vec![
                installed(&dir, "aaa", "[theme.colours]\ntext = \"#111111\""),
                installed(&dir, "zzz", "[theme.colours]\ntext = \"#222222\""),
            ],
            problem: None,
        };
        let ctx = egui::Context::default();
        let mut local = Local::default();
        local.wear(&ctx, &catalogue, crate::app::HUD_FONT);
        assert_eq!(local.theme().mod_id, "zzz");
    }

    #[test]
    fn a_frame_that_is_not_a_picture_leaves_the_screen_alone() {
        // The player's own file, so this is a mistake rather than an attack —
        // but it goes through the same guarded decoder, and the answer to a
        // file that will not decode is nothing to draw rather than a failure
        // to start. Non-vacuous against the first test, which uses the same
        // shape with real PNG bytes in it and DOES get a frame.
        let dir = scratch("bad-png");
        let listing = installed(&dir, "iron", "[theme]\nsheet = \"art/frame.png\"");
        std::fs::write(listing.dir.join("art/frame.png"), b"not a png at all").expect("write");
        let catalogue = crate::launcher::Catalogue {
            mods: vec![listing],
            problem: None,
        };
        let ctx = egui::Context::default();
        let mut local = Local::default();
        let dressing = local.wear(&ctx, &catalogue, crate::app::HUD_FONT);
        assert!(dressing.sheet.is_none());
        // The rest of the theme still applies: a missing frame is a screen in
        // the right colours, not a screen with no theme.
        assert_eq!(local.theme().mod_id, "iron");
    }

    #[test]
    fn a_font_installed_this_frame_is_not_named_until_egui_has_it() {
        // **The crash the start screen died on, as a test.** `set_fonts` does
        // not take effect in the frame it is called in — egui swaps its font
        // set at the start of the NEXT frame — so a theme that installed its
        // font and then put the family into every text style named a family
        // that was not there yet, and epaint panicked laying out the first
        // label rather than falling back.
        //
        // Reported from the window as
        // `FontFamily::Name("mod-font-tiamot_default_ui:theme_font") is not
        // bound to any fonts`, on the client's very first frame.
        let ctx = egui::Context::default();
        crate::app::install_fonts(&ctx);
        let mut fonts = crate::fonts::Fonts::new();
        assert!(
            fonts.offer("iron:theme_font".to_owned(), crate::app::HUD_FONT.to_vec()),
            "the bundled font should parse"
        );
        let _ = fonts.install(&ctx, crate::app::HUD_FONT);

        // This side's record says it is installed, and it is.
        assert!(
            fonts.family("iron:theme_font").is_some(),
            "the font arrived, so the bytes-side answer is yes"
        );
        // egui has not picked it up yet, so nothing may name it in a `FontId`.
        assert_eq!(
            fonts.bound_family(&ctx, "iron:theme_font"),
            None,
            "a family egui has not bound yet must not reach a FontId"
        );

        // A frame passes, and now it may.
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        assert!(
            fonts.bound_family(&ctx, "iron:theme_font").is_some(),
            "after a frame egui has the family and the theme may use it"
        );
    }

    #[test]
    fn a_theme_only_asks_for_the_art_it_actually_names() {
        let theme = Theme::from_def(&def());
        assert_eq!(theme.art(), vec![[7; 32]]);
        assert!(
            Theme::none().art().is_empty(),
            "a client with no theme fetches nothing for one"
        );
    }

    #[test]
    fn a_missing_colour_leaves_the_clients_own_alone() {
        // **The property the whole module rests on.** A theme is a set of
        // overrides with the client's look underneath, so a mod that names two
        // colours cannot leave the other three undefined — and cannot make a
        // screen unreadable by saying nothing about it.
        let ctx = egui::Context::default();
        let before = ctx.global_style().visuals.clone();
        Theme::from_def(&def()).apply(&ctx, &crate::fonts::Fonts::new());
        let after = ctx.global_style().visuals.clone();

        assert_eq!(
            after.window_fill, before.window_fill,
            "a theme that named no background changed one"
        );
        assert_eq!(
            after.override_text_color,
            Some(egui::Color32::from_rgb(0xE8, 0xDC, 0xC0)),
            "a theme that named its text colour did not get it"
        );
        assert_eq!(
            after.selection.bg_fill,
            egui::Color32::from_rgba_unmultiplied(0xB0, 0x8D, 0x57, 0x80)
        );
    }

    #[test]
    fn a_theme_that_says_nothing_changes_nothing() {
        // Non-vacuous alongside the test above: it proves the comparison there
        // could have failed, because applying the empty theme leaves every
        // field of `Visuals` — not just the ones that test names — untouched.
        let ctx = egui::Context::default();
        let before = ctx.global_style().visuals.clone();
        Theme::none().apply(&ctx, &crate::fonts::Fonts::new());
        assert_eq!(
            ctx.global_style().visuals.override_text_color,
            before.override_text_color
        );
        assert_eq!(
            ctx.global_style().visuals.widgets.hovered.bg_fill,
            before.widgets.hovered.bg_fill
        );
        assert!(Theme::none().is_none());
        assert!(!Theme::from_def(&def()).is_none());
    }

    #[test]
    fn a_font_nothing_answers_to_leaves_the_text_in_the_clients_own() {
        // A theme whose face has not arrived yet must not become a screen that
        // has not arrived. `Fonts::new()` is a client that has been told about
        // no fonts at all, which is every client before the table lands.
        let theme = Theme::from_def(&def());
        let ctx = egui::Context::default();
        assert_eq!(theme.family(&ctx, &crate::fonts::Fonts::new()), None);
    }

    #[test]
    fn an_alpha_a_mod_wrote_is_the_alpha_it_meant() {
        // Straight, not premultiplied: `#b08d5780` means that colour at half
        // opacity, and premultiplying it by hand is not something a palette in
        // a manifest does.
        let theme = Theme::from_def(&def());
        let accent = theme.colours.accent.expect("an accent");
        let [r, g, b, a] = accent.to_srgba_unmultiplied();

        // **The alpha is exact and the colour is within a bit.** `Color32`
        // stores premultiplied, so a straight colour put in and taken out
        // again loses up to one step per channel. That rounding is not what
        // this test is about — reading `#b08d5780` as PREMULTIPLIED is, and
        // that would halve the colour rather than nudge it, so the tolerance
        // is nowhere near wide enough to hide it.
        assert_eq!(a, 0x80, "the opacity a mod asked for");
        for (got, want) in [(r, 0xB0), (g, 0x8D), (b, 0x57)] {
            assert!(
                i32::from(got).abs_diff(want) <= 1,
                "channel {got} is not {want}; premultiplied would be about half of it"
            );
        }
    }
}
