# Engine asks from Tiamot Default UI

From the `tiamot_default_ui` mod (repo `Tiamot_Default_Inventory`, beside the
engine). Kept in two places with the same text: in the engine repo as
`docs/engine-asks/tiamot_default_ui.md`, where the engine agent reads every
mod's asks, and in the mod's repo as `docs/engine-asks.md`, so they are on
GitHub with the mod. Change both together.

What the inventory has needed from the engine, found by building it. Each entry
says what was seen, why the mod cannot fix it, and the smallest engine change
that would. Newest first. Items are removed when they land.

## 11. Resting widgets wear a faint outline, and a text field is not the sheet (2026-09-18)

**Seen, in the window.** Under this theme a text field cannot be found: the
chat input, and a mod's `text_input` (`dialog.rs`).
`Theme::apply` sets `extreme_bg_color`, the fill behind a text field, to
`background`, which is the sheet's own fill, and egui's resting widgets have no
stroke, so the field is the sheet. Tick boxes, dropdowns, sliders and the
unframed glyph buttons in a dialog (`dialog.rs`, the shape editor's arrows)
are only a shade apart from the sheet and read as text, not controls. Buttons
that wear the theme's `button` frame are fine; the rest should match them.

**Smallest change, and no sixth colour.** In `apply`, when `accent` is set:
`widgets.inactive.bg_stroke` and `widgets.noninteractive.bg_stroke` become a
1-point stroke in `accent`. When `button` is set, `extreme_bg_color` takes
`button` rather than `background`, so a field is a shade above the sheet.
Hovered and active keep their own strokes. This mod's accent is `#5c4c30`, a
dark brass that is just visible on `#131619`: subtle, as asked for.
`noninteractive.bg_stroke` also draws egui's separators, which would then be
brass too, and that is wanted.

## 10. A themed sheet's contents clear its frame (2026-09-18)

**Seen, in the window.** Text on the engine's own screens runs onto the ornate
trim, and so does every sheet's bar, Close included. `panel::sheet_with`
paints the frame at `ui.max_rect().expand(window_margin)`, so content starts
about 6 points inside the frame's outer edge. A frame's border is a third of
its image: 36 points for `ornate-panel.png`, whose trim runs about 18 points
deep along the sides and 20 into the corners. The inventory's body is clear
only because it pads itself by 24; the bar above it is not, and nothing on the
pause, settings and start screens is.

**Why the mod cannot fix it.** The frame's placement and the margin are the
client's. Thinner art would need a trim under 6 points deep.

**Smallest change.** With a frame, the content (bar and body) starts a whole
border inside the frame's outer edge: paint the frame at
`max_rect().expand(border)` rather than `expand(window_margin)`, where `border`
is the same third `paint_nine_slice` draws at, and have `size_clear_of` and
`origin_clear_of` leave that much around the sheet so the frame stays inside
the window and above the HUD reserve. With no frame, nothing changes.

**Please grow the frame outward rather than shrinking the room.** The
inventory fits 800x600 with less than 12 points to spare. Taking 30 a side out
of the room breaks it (measured with the native check). If the room must
shrink, say by how much: this mod pads its tree by 24 (`frame_padding`) only to
clear the trim, and would give most of that back once the engine clears it.

## 9. A theme's second face, for text read as sentences (2026-09-18)

**Seen.** `[theme] font` is one face, and `client::theme::Theme::apply` puts it
on every egui text style. This mod's face is Cinzel Decorative, a display
capital: right for the pause screen's buttons and a sheet's title, and hard
reading in chat, a text field, or a settings page's descriptions. Chat is the
worst case: `draw_chat` in `client/src/main.rs` uses the default text style, so
every line anyone says is drawn in small capitals over the world. The theme
also takes the client's own face away from a dialog widget that names no font,
which the mod has worked around by naming its text face on every hint.

**Why the mod cannot fix it.** Chat is the client's and runs no Lua, which is
right. The manifest's `Theme` is `deny_unknown_fields`, so the mod has no way to
name a second face.

**Smallest change.** An optional `text_font` in `[theme]` (the name is yours),
pushed and fetched exactly as `font` is. When it is set, `apply` puts it on
`Body`, `Small` and `Monospace`, and `font` keeps `Heading` and `Button`; chat,
text fields and the settings pages' prose come out in the text face. When it is
not set, nothing changes. This mod would declare
`text_font = "fonts/Spectral-Regular.ttf"`, which it already ships and
registers as `tiamot_default_ui:text` (261 KB, well under `MAX_FONT_BYTES`).
Optionally, a dialog widget that names no font could follow `text_font` too,
so another mod's tab reads the same without naming it.

**Descriptions a size down.** Reported from the window alongside this: the
start screen's secondary lines ("Nothing found yet. A world has to be opened
to the LAN to appear here.", "Each world keeps its own selection…", a mod's
description) are drawn at full body size. They are the `ui.weak(...)` calls:
ten in `front.rs`, two in `main.rs`. Drawing them in `TextStyle::Small`, with
`Small` set to about 85% of `Body` rather than egui's much smaller default,
would set them apart as notes. That is a client change whatever the theme,
and with `text_font` set they would be in the text face too.

## Watched, not asked: Life's status tray and narrow windows (2026-09-18)

Life draws a status tray in the bottom-right corner: a 150-pixel weather
shield with effect names stacked above it, up to about 360 pixels high on the
1080-tall HUD canvas at its fullest. It is deliberately outside Life's
`reserve` (216), which only covers the centre, where sheets are.

Run through the client's `panel::size_clear_of` with the 216 reserve, a sheet
clears the tray by 170 to 270 pixels on 16:9 and 16:10 windows and by about 20
on 4:3 (less the few pixels the theme frame is drawn outside the window). It
reaches the tray only on a window narrower than about 1.28:1: by 14 pixels on
a 5:4 monitor, and more on a window dragged tall and thin, and then only while
the tray is at its fullest.

Not an ask yet. If it matters, the engine-side fix is a reserve that is a
rectangle rather than a bottom band (a HUD saying "keep clear of the bottom
right, 150 by 360"), with sheets narrowing before they overlap it. The mod
cannot move a sheet itself.

Open: 9, 10 and 11. Landed so far, all asserted by the native check:

- **5**, a container measures a child by the `size` it asked for (engine
  26b87d8), and **6**, a screen claims its height once and no longer scrolls
  in a sheet built to hold it (8d83855). The same class of bug, the interface
  measuring something twice and disagreeing with itself. `section` no longer
  sums its children's sizes itself; the README's wardrobe tab fits on the
  engine's own measuring.
- **7**, a HUD reserves the bottom of the screen (b3d237c): `hud.lua` is
  registered with `reserve = 138`, and sheets rise to clear the tallest reserve
  any mod declares.
- **8**, a mod's look for the engine's own screens (88c42c9, 0c33026):
  `[theme]` in `mod.toml` dresses the pause, settings and start screens and
  every sheet, the inventory's included, so the tree no longer draws a frame
  of its own.

- **1**, a table handed back to its owner iterates, and **2**, a disabled
  mod's functions stop — engine 823aac3.
- **4**, a picture a HUD script draws is fetched, and **0**, a mod can ask for
  a content hash — engine 6aae28c, protocol v61. `game.register_picture` puts
  the hotbar's slot frame in the table every client fetches on join and answers
  its hash, which `hotbar.lua` sends to the script with `game.set_hud`;
  `game.content_hash` gives the dialog frames theirs. No hash is written by
  hand anywhere in this mod.
