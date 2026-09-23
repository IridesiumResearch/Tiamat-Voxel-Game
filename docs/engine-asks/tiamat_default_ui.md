# Engine asks from Tiamat Default UI

From the `tiamat_default_ui` mod (repo `Tiamat_Default_Inventory`, beside the
engine). Kept in two places with the same text: in the engine repo as
`docs/engine-asks/tiamat_default_ui.md`, where the engine agent reads every
mod's asks, and in the mod's repo as `docs/engine-asks.md`, so they are on
GitHub with the mod. Change both together.

What the inventory has needed from the engine, found by building it. Each entry
says what was seen, why the mod cannot fix it, and the smallest engine change
that would. Newest first. Items are removed when they land.

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

No open asks. Landed so far, asserted by the native check except 11 and 12,
which are the client's own drawing:

- **13**, `conflicts` in `mod.toml` (engine 57e5d6f): a mod that replaces
  another names it, and a set with both present is refused at resolution —
  the server does not start and `--check-mods` fails, naming both mods and
  the way out (`enabled_mods`, or the mod list when a world is made). Through
  `provides` aliases too. The manifest declares `conflicts = ["core_ui"]`, and
  the native check runs the engine's resolver on it beside the real `core_ui`.

- **12**, descriptions a size down (engine 990bf8a): the start screen's
  secondary lines are `TextStyle::Small`, 85% of body, and a theme changes
  faces but never sizes.

- **9**, a theme's second face (engine 86dcbb9): `[theme] text_font` is
  Spectral, on chat, text fields and prose, and `font` keeps Cinzel on
  headings and buttons. Its other half is 12.
- **10**, a themed sheet's contents clear its frame (86dcbb9): not as asked.
  The engine draws every frame `FRAME_BORDER` (18) points deep whatever the
  art's resolution, and insets the contents by the same, so each dimension of
  the room is 24 smaller. `frame_padding` went from 24 to 8, so the screens
  have more room than before, and the native check and preview use the new
  margin.
- **11**, a faint outline on resting widgets and a text field a shade above the
  sheet (86dcbb9).

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
