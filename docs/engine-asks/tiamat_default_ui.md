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

## 20. A mod's settings under its name on the Mods tab (2026-10-07, from the designer): LANDED 2026-10-07 (engine c65269b7), awaiting the eye

**Landed.** In two parts. Part 1 (`befe1921`, by the lead): each enabled mod's
world options sit in a closed "World options" dropdown under its row on the
Mods tab. Part 2 (`c65269b7`): `[[setting]]` in `mod.toml`, with
`[[world_option]]`'s fields (`id`, `name`, `description`, `options`,
`default`; a choice's `default` is one-based in the file, as for world
options, and zero-based as the value) and `register_setting`'s meaning. Each
enabled mod shows a second closed dropdown, "Settings . N", whose answers
edit the selected local world's `Entry::settings` (or a pending map carried
into a new world's entry), which is what is sent on join and what
`game.setting` answers. A selected server shows nothing there; its table
arrives on join. The server registers a manifest setting with the VM before
the mod's `init.lua`, so the `ModSettings` table, `SetSetting` and
`game.setting` treat it exactly like a registered one; no protocol change.

*What the mod should do:* declare `[[setting]]` in `mod.toml` for anything the
start screen should show; keep `register_setting` for the rest; never both
(one id declared both ways fails the mod's load, naming the id).

*[H]:* on the Mods tab each mod shows its world options and its settings
under its row; a setting changed there is what `game.setting` answers in the
world (try `core_ui:show_cost`, under the reference mods).


**Wanted.** On the pre-game menu's Mods tab, each mod's own settings under
its row: what it lets a player choose, where they choose which mods to run,
before any world is open. Today the tab is a box, a name and a description a
mod, and nothing a mod offers can be set there.

**Why a mod cannot do it.** The start screen is the client's own, drawn
before any server exists, and no mod's Lua runs on it (AGENTS.md, "A look for
the engine's own screens"). What it can know about a mod is its `mod.toml`.
So:

- **World options** (`[[world_option]]`) are already known there, and drawn,
  but under the seed box (`front.rs`, `world_option_rows`), apart from the
  mod they belong to.
- **Player settings** (`game.register_setting`) are declared in Lua, so the
  start screen cannot know they exist. Their answers are already the
  client's, per world (`launcher::Entry::settings`), and can only be changed
  in game.

**Smallest change, in two parts.**

1. *World options under their mod.* Draw each enabled mod's world options
   indented under its row on the Mods tab, from the same `self.world_options`
   the seed box edits, so there is one answer and two places to see it (or
   move them there outright). For a selected existing world they show its
   stored answers, read-only, with "fixed for this world"; for a new world
   they are editable. Nothing in the manifest or the protocol changes.
2. *Player settings declared in the manifest.* `[[setting]]` in `mod.toml`,
   the same fields `[[world_option]]` has (`id`, `name`, `description`,
   `options`, `default`), and the same meaning `register_setting` has: a
   checkbox without `options`, a dropdown with them, answered per world and
   per server. The Mods tab draws them under the mod for the selected world
   (into that `Entry`'s `settings`) or the new one, and `game.setting` answers
   them in game exactly as it answers a registered one. `register_setting`
   stays; one id declared in both is refused at load, so a mod has one
   declaration. A remote server's settings could show what the client was
   told on its last join, if that is worth caching.

Part 1 alone answers "my world's options are under my mod"; part 2 is what
"settings" means to a player. This mod declares neither today, so nothing
here waits on it; it is asked for the mods that do (Weather registers a
setting today) and for every mod that will.

## 19. A dialog cannot be built from another mod's exported widgets (2026-09-30): LANDED 2026-09-30 (engine 71bf067)

Relayed by the designer from this mod's work on Magic's U-M2: "the
underlying U-M2 problem is that `game.show_dialog` can't read read-only
tables that came from another mod. I documented a workaround rather than
fixing it. The real fix would be the engine reading those tables properly."
The workaround is `docs/exports.md`'s rule that a tree shown with
`game.show_dialog` must be plain tables, and `util.plain` in callers.

**What was wrong.** A table crossing between mods is a read-only view: an
empty table whose contents are served through `__index`, `__len` and
`__pairs`. The engine read a dialog tree raw, as `lua_next` and
`lua_rawgeti` do, so a view's own entries were all it saw — none. Fields
read by name came through, which is why it half worked; every list the
exporting mod had built itself (a page's `children`, a dropdown's
`options`) came out empty. An exported page arrived as one bare column.

**From the engine, 2026-09-30 (engine 71bf067, no protocol change).**
`game.show_dialog` and `game.update_dialog` now read what each view stands
for, wherever it sits in the tree: as the root, as a child of a plain
table, inside a list the other mod made, views of views. Nothing is
written, and nothing a mod could not already read through the view is
read. A tree with no view in it is read exactly as before, the same table,
not a copy. So:

```lua
local ui = game.exports("tiamat_default_ui")
game.show_dialog{ player = uuid, form = "book", tree = ui.page("Mutus Liber") }
```

works as written. `util.plain` and the plain-tables rule in
`docs/exports.md` can go; nothing breaks if they stay.
`crates/core/tests/mods.rs`,
`a_dialog_built_from_another_mods_exported_widgets_is_shown_whole`, is the
proof: without the change that page came out as one node, with it three.

## 18. A cut of several materials (2026-09-29, from the designer): LANDED 2026-09-30 (engine ca0f919..03d968d, protocol 81), awaiting the eye

Asked by the designer of the engine session, not by this mod: "in the
shape crafter it is important that there is a way to build a shape out
of all the different materials in the inventory", and, asked which was
meant, both. A row for every material carried is this mod's new crafter
and ask 17. The other half is ONE cut made of SEVERAL materials, each
cell its own: a stone stair with an oak tread. That is the engine's to
make possible and is written here so the crafter can be planned against
it.

**What exists already.** The world holds such blocks and always has: two
cuts of different materials placed into one block make one, and breaking
it pays out each material's cells. What is missing is the item, and an
editor that keeps each cell's material.

**The shape it will take.**

```lua
-- The editor, in several-material mode: 27 cells, one-based, cell i - 1
-- being x + 3*y + 9*z, each a material id or 0 for empty. `material` is
-- the BRUSH: a right-click adds a cell of it, a left-click takes the
-- nearest cell off whatever it is made of.
{ type = "shape_editor", name = "cut", shape = mask, material = brush, cells = cells }

-- What it reports: the mask as before, and the cells.
-- event.kind == "chiselled", event.shape, event.cells

-- The item. One unit a cell, each of that cell's material, so the craft
-- takes n * (cells of m) units of every material m and gives n of these.
game.give(player, { cells = cells, count = n })

-- In game.inventory and game.held a cut of several materials reports
-- `cells`, with `material` the lowest id among them and `shape` the mask
-- (the full mask, never absent, when the cut fills the block).
```

An editor sent without `cells` is what it is today, one material
throughout, so nothing this mod does now changes. Choosing a row would
set the brush rather than the whole cube's material; whether a click on
a row still crafts, or a Make button comes back for a cut of several, is
this mod's to decide.

**What does not change.** A cut places as itself whatever tool is held,
all of it or none, and only into air. Breaking what it made pays out
loose material, one stack a material. Identical cuts stack and nothing
else does.

**From the engine, 2026-09-30 (engine ca0f919..03d968d, protocol 81).**
Landed in the shape above, with nothing changed from it. The contract
went first (Sub-Node Contract §9.1, and §7.1/§7.2 for placing), then the
item, saving (player, container and dropped-item formats each gained a
migration step, all in world ids), the wire, placing, Lua, and drawing.

- **Everywhere a stack is drawn, its cells are:** a slot, the cursor, a
  HUD icon, the hand in first person, another player's hand, and an item
  on the ground. A slot's hover names every material in it.
- **The editor keeps its own copy**, so a click lands at once, and takes
  the server's cells only when they differ from what it last sent;
  changing the brush never resets the carving.
- **`game.take` with `cells` takes that exact cut**, and a take that
  names a material and no cells never takes from a cut of several, even
  one that fills the block.
- **The reference crafter (`game/core_ui`) does it end to end** through
  the public API: a "Several materials" checkbox, the dropdown as the
  brush, a cost line naming each material's units, and Make taking them
  and giving the cut, putting back what it took if any material is
  short. Read it for the calls; this mod's crafter is this mod's.

For the eye ([H]): in the reference crafter, tick "Several materials",
paint with two materials chosen in turn, Make, then look at it in a
slot, in the hand, placed, and dropped with Q. Placed and broken, it
must pay out each material's cells.

## 17. Which click pressed a button (2026-09-29): LANDED 2026-09-29 (engine c444967)

The crafter makes a shape from the material row a player clicks: click for
ten, right-click for one, double-click to fill a stack, as a chest works in
the game this is modelled on. It is the difference between a screen of rows
and a screen of rows plus a count picker and two craft buttons.

The mod cannot tell the clicks apart: `"pressed"` carries only `name`. The
client's button already has egui's response, which knows
`secondary_clicked` and `double_clicked`; only the event drops it. (An
`item_slot` reports `click`, but the server moves stacks on it before the
mod hears, so a slot cannot stand in for a button.)

Smallest change: `click` on `"pressed"`: `"left"`, `"right"`, or `"double"`
for the second press of a double-click in place of a second `"left"`, since
egui reports the first half as a click of its own. A right-click on a button
presses it (today it does nothing). A mod that ignores `click` sees exactly
what it sees now. `tab_shapes.lua` already reads `event.click`, treating nil
as a left click, so the crafter works as designed the day this lands and the
native check can then drive all three.

**From the engine, 2026-09-29.** Landed as asked. `event.click` on
`"pressed"` is `"left"`, `"right"` or `"double"`, and is always set, so
`tab_shapes.lua`'s nil case is never taken against this engine and is only
there for an older one. A right-click on a button presses it. A double-click
is two events, `"left"` and then `"double"`, which is what `wanted()` already
assumes: the first half makes ten and the double tops the stack up.

One thing to know about the double: egui decides what a double-click is
(two presses of the primary button on one widget within its own interval and
a few points of each other), and the engine reports what egui says. A redraw
between the two halves does not break it as long as the row keeps its name
and its place, which a row named by its material does; a list that re-sorts
under the pointer would turn the second half into a `"left"` on another row.

The native check can drive all three: `bot.press(form, name, click)` takes
`"left"` (the default), `"right"` or `"double"`, and a script that means a
double-click sends the `"left"` first, as a client does. On the wire it is a
byte on `DialogEvent::Pressed` (`proto::Press`), which is protocol 79; Life 18
(riding) moves to 80. Tests: `a_mod_hears_which_click_pressed_its_button`
(bot, a real server), `a_press_is_its_name_and_then_which_click` (the
encoding, pinned), `a_button_says_which_press_it_had` (the client's reading
of egui).

## 16. The shape editor draws a black cube (2026-09-28): LANDED 2026-09-28 (engine 0921437)

Reported from the window: with a material chosen, the Crafting tab's shape
editor is a solid black block, or a black void, instead of the material's
cells. Nothing the mod sends explains it. The tree carries
`{ type = "shape_editor", shape = game.OCCUPANCY_FULL, material = <the
numeric id game.inventory reported> }`, and the native check asserts both
reach `Widget::ShapeEditor` through the engine's own parser and checker.

On the client, `paint_shape_editor` -> `Icons::paint_cells` ->
`paint_cell_face`, which, when the atlas is registered with egui
(`register_atlas`, `register_native_texture` on `atlas_view`), draws a mesh
sampling that view with a white vertex tint scaled per face. A black cube
means that sample answers black: the wrong texture, a view egui cannot
sample as it expects, or UVs off the tile. A void means no cells at all.
Worth a look: whether an inventory slot's picture, which goes through the
same `Icons`, is right in the same window; if it is, the difference is the
editor's own path.

The mod cannot work around it: the cells are the client's to draw, and the
shape editor is the only widget that shows a cut being carved. Smallest
change: find why the atlas sample is black in the editor, and add a
render test that draws a shape editor with a real atlas and checks the
pixels are not black, since every earlier test took the no-atlas branch.

**From the engine, 2026-09-28 (engine 0921437, 188a983, 1155282):** found
and fixed, and it was never the editor's own path. The editor gives egui
exactly what a slot's picture does — the same atlas, UVs inside the tile,
a white tint shaded per face, 81 faces for a full block — and drawn on a
GPU through egui the two came out wrong alike: egui samples a texture as
its stored bytes and expects a plain RGBA view, but it was handed the
world's sRGB view, so every texel was decoded twice and showed as its own
linear value; dirt (98, 78, 58) became (31, 19, 11) on the top face and
darker on the sides, and coal, basalt and mud went to black — the void.
The slot was as dark, but small on a lighter ground, so the editor got
the blame. Now the atlas carries a second, undecoded view for the
interface (a GL adapter, which cannot view one texture two ways, uploads
a second copy instead), and the render test the ask wanted draws a shape
editor and a slot through egui with a real atlas and checks every visible
face against the tile's colour times its shade, which the old view fails
by a wide margin. Two more things fell out of the same look: stacks,
block deltas and a dialog's `shape_editor.material` left the server in
the session's runtime ids while a client's chunks, table and atlas are
keyed by the world's, so a world reopened under a changed mod set drew
the wrong tile or a void — all three now cross the wire in world ids, a
placement comes back through the same map, and a bot test on a world
made with two mods and reopened with one proves it; and the editor's
turn arrows were glyphs the client's one font lacks, so they are now
ones it has. What it looks like in the window is the designer's ([H]):
the Crafting tab with dirt, stone or coal chosen, the cube in the
material's true colour, and every slot, the hotbar and a carried stack
noticeably lighter than before.

**From the engine, 2026-09-29 (engine eaf0d2e, 0989d3d): the rest of what
that look found.** The wire was one half. The other was inside the
server: the tables the tick reads (hardness, drops, tool speeds, light
given off, let through and dimmed, what a body walks through and slides
on, what the ground drinks and turns into) were keyed by the world's ids
and asked with the session's, which is what a chunk in memory holds. On a
world made by the mod set that opens it the two are the same number, so
nothing showed. On a world reopened with a mod added, removed or loading
in another place, a lamp did not glow, glass was dark, some unrelated
block was walked through, and a block was timed and paid out as another
material. The far horizon was drawn in the wrong materials the same way,
and an item lying on the ground was saved under the session's number and
came back as something else. All keyed one way now: the session's ids in
memory, the world's on disk and on the wire, and nowhere else.

What that means for a mod: **adding a mod to a world that already exists
is safe**, which it was not. It is what every world does the day Magic
and Science are linked in. An existing world rebuilds its horizon once,
the first time it is opened, because the cached one may have been
written the old way. Tests: `crates/bot/tests/divergent_ids.rs`, six of
them through a real bot on a world reopened under another mod set, each
seen to fail before the fix.

Landed so far, asserted by the native check except 11 and 12,
which are the client's own drawing:

- **15**, a tooltip on any dialog node (engine 2655837, protocol v77, drawn
  in 663bb70): `tooltip` is among the fields `screen.lua` copies from another
  mod's tree, and `widgets.tip(widget, text)` answers a copy of any widget
  with one, because a widget the builders hand back reaches its caller
  read-only. Asked for Progress's U5; the native check carries one through.
- **14**, a fixed size for `player:main` (engine 896fb30): `init.lua` calls
  `game.set_main_slots(28)`, so a full pack refuses a pickup rather than
  hiding it in slot 29. `game.give` now answers what did not fit, and the
  crafter takes back any part of a stack that fitted before it refunds the
  material, so a full pack can neither lose units nor make them; the native
  check drives that path.

- **13**, `conflicts` in `mod.toml` (engine 57e5d6f), and the engine's mods
  made secondary the same day: a mod that replaces another names it. Against
  an ordinary mod the set is refused, naming both and the way out; against
  one of the engine's reference mods — every `game/core_*` manifest now
  carries `reference = true` — the fixture stands aside and the replacement
  loads alone, so `--check-mods game` and a local game with no
  `enabled_mods` both work with `core_ui` present, and the check says which
  stood aside for which. Reference mods also load first, lose any lowest-id
  tie, and sit folded away under "Engine reference mods" on the start
  screen. Through `provides` aliases too. The manifest declares
  `conflicts = ["core_ui"]`, and the native check runs the engine's resolver
  on it beside the real `core_ui`.

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
