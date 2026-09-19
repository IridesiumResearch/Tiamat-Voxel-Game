# Engine asks from Tiamot Default World

From the `tiamot_default_world` mod (the Spindle; repo `Tiamot_Default_World`,
beside the engine). Kept here rather than in that repo so the engine agent
finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

## 37. Water that runs through a plant does not break it, and the flow hook cannot read the world (2026-09-18)

**Seen, in play.** Water let loose over a meadow (a brook's spill, in the
Flower Forest) ran through the grass cards and stood in the same blocks as
the tufts. The designer: "Grass should probably get broken by water."

**What the mod does now, and why it is a workaround** (rules.lua). A tuft is
a few cells of its block, under the fluid's `waterlogs_at`, so the solver
lets water in and `register_on_fluid_flow` never reports the plant. The
reports it does send are the water's edge pressing sideways on ground
(measured, a flood over a meadow: 1,208 reports, all sideways, none from a
plant's block). And **every `game.get_block` inside that hook returns nil**,
1,208 of 1,208: the hook can queue writes but not read. So the mod notes
each report's place and, on the next tick, sweeps a 7 x 7 round it for a
plant with water in it and clears it. It works (20 wet plants of 102 gone,
the dry 82 kept, 1.4 ms of mod time a tick while the water ran), but it is
a guess about where the plants are from where the water is blocked.

**Smallest change.** A block flag, `washes_away = true`: when the solver
moves fluid INTO a block whose terrain is that block's, it clears the
block's cells first (as a dig would; dropping nothing until ask 11 lands).
The mod would declare it on every `passable` plant and drop the sweep.
Separately: the flow hook should be able to read, as ask 17 asks of the
dig and place hooks; or the stub should say it cannot.

## 36. Walls of water: what the world's data says, and where the rest may be (2026-09-18)

For the engine's own hunt ("chunk walls in large bodies of water ... cannot
find it"). The mod's side, measured headless: every fluid block within 40 of
a landing, 30 under to 10 over the water's surface, checked for a SIDEWAYS
face open to air (no terrain, no fluid) — in the columns either side of every
chunk seam, and as a control the mid-chunk columns — and for water over open
air on y seams and mid-chunk heights.

**The open sea is seamless in the data.** Deep Ocean, Coral-Fringed
Shallows, Kelp Forest, Pack Ice: about 363,000 water blocks, **zero** open
faces anywhere and zero water over air.

**The coasts were not — the mod's fault, fixed (mod, this date).** Every
coast had a dry slope under the sea's level on the land side of the
shoreline (the coast's terrain rises from the sea floor to the land there,
and the sea's `within` stopped at the line), and every river valley near a
sea was cut dry to thirty blocks under it. The sea stood against both: up to
31 blocks of water face, 144 faces on seams and 84 inside chunks in one
81x81 patch. After the fix, zero, at both coasts measured. Lakes show only
the one- and two-block edges of ordinary shorelines.

**So what is left is the client's**, and one place fits "chunk walls in
large water, not reproducible between two loaded chunks": `ABSENT_POLICY =
Absent::Air` (client/src/world.rs). A water column whose neighbour chunk
has not arrived — the streaming frontier, absent AND not summarised — has
its side faces drawn against air, the whole depth of the water in that
chunk: a chunk-wide sheet, flush with the seam, standing until the neighbour
lands. In rock that wall is invisible; in water you look straight at it,
and a sea is where the frontier is in plain view (vertically too: the view
distance ends inside deep water). `Neighbours::summarised` already hides
faces at the LOD boundary; the frontier inside water has no such cover.
`ChunkFluid::solid` counts an unsent block as a wall for the SKIRT, but the
FACE culling goes by the grid's shell, which is `Absent::Air`.

**The code that fills the water**, all through `fill_fluid_terraced`:

- the seas: `seas.lua`, `M.fill` — `level` is `M.fluid_level()` (the level
  map, quantised to the pool's step; no y), `within` is `M.d()` (the sea
  distance map plus two fine noises) plus `WATER_INLAND`; no lip. Called for
  every chunk inside the body from `generate.lua` (`sea_into`), after the
  terrain and structures;
- a biome's pools and rivers: its `fluid` fill (level, within, lip) from
  `generate.lua`, `waters_into`, after the sea;
- the caves' pools and rivers: `biomes/caves.lua`, `M.into`, in chunks of
  solid rock, after the sea.

Every `level` and `within` reads no y (engine-asks 35), so stacked chunk
layers ask the same question of a column and cannot disagree at a y seam:
the measured water-over-air at y seams was zero in the open sea, and the few
at the coast went with the dry slopes.

## 35. A terraced fluid fill reads its fields at y = 0.5, and the docs say the chunk's floor (2026-09-18)

**Half landed (engine ed211d8):** the `level` is now solved at its own
height, a fixed point, and the mod's brook levels lean on that (they ride
the real terrain, `shape.gully_water_level`). **`within` is still read on
the slice at y = 0.5**, and that is what cost the Taiga its pools
(2026-09-18: a 3D basin noise there is another field thirty thousand
blocks down). The rest of this entry is as first filed.

`fill_fluid_terraced` (detgen/buffer.rs) evaluates `level` and `within` on
a region whose `origin_y` is the constant `LEVEL_SLICE = 0.5`: one slice of
the whole world, at world height half a block. The stub says the level "is
read once per column (at the column's centre, on the chunk's lowest layer,
so give it a field that does not read `y`)". The two agree only for a
field that reads no `y` at all; a field that reads it a little — a
province cut that carries the depth band, a noise stretched in y — is
evaluated thirty thousand blocks from where its water goes.

Found building the Underground River: its `within` carried the caves'
depth band, and at y = 0.5 the band is nothing, so no column was ever in
the river and the fill laid no water, with no message. The mod now keeps
every `level` and `within` free of y (caves.lua, `mine_flat`).

The smallest change is to the doc, so the next person knows where the
slice is. Better: evaluate the slice on the chunk's own floor, `y0 + 0.5`,
which is what the doc promises and what a field that reads y expects.

## 30. A program cannot spend the same value twice (2026-09-16)

**The binding constraint on the whole world.** A density program may hold
1,024 operations and eight live buffers, and the shore programs are at
985 (`temperate_shore`) and 939 (`belt_shore`) with three biomes' terms in
them. Nothing in the compiler remembers a subtree it has already emitted,
so every helper the mod calls twice is compiled twice:

- `shape.ring` re-emits the radius — `x*x + z*z`, the wobble noise, the
  fold — at every call, and the mod's own comments already say the radius
  is "three buffers of its own".
- the reef's `out()` is a map read and a clamp, and the shore program
  contains six copies of it;
- the dunes' barchan is one stretched noise read by two clamps, so the
  noise is in the program twice;
- every `a * (1 - w) + b * w` cross-fade emits `w` twice.

What that costs, today, in the world: the reef's tidal gutters had to stop
being a cut in the terrain and become a surface material instead; the
reef's floor is a `max` against the shelf rather than a cross-fade into
it; the Coastal Cliffs' blowholes and their fine rib are still out (2.4,
2026-09-15); and the rim is bare past about 54 km because the chunks there
need the body test as well as the terrain, which is why the mod keeps a
second, thinner family of programs (`P.flank`) with none of the biome
terms in it — with room, that family would not have to exist.

**The smallest change is reuse, not a bigger cap**: hash-cons identical
subtrees when `game.density` compiles, emit each once, and have the
repeats read the buffer it went to. The mod builds the repeats out of the
same node constructors with the same arguments, so structural equality
catches nearly all of them. The honest catch is that a shared value has to
stay live between its uses, so reuse trades operations for buffers — which
is why this ask is really "either more of one or more of the other": more
buffers with spilling, or the cap raised, or both. Any of the three buys
the same thing, which is that a biome can carry the detail its brief asks
for without another one losing some.

## 33. A chunk tint cannot brighten (2026-09-16)

**Seen.** The designer's rule is one grass, one dirt, one lichen, and a
biome that wants them another colour gets it from its chunk tint. The
Savanna's grass should be gold and the Taiga's rust; the one grass is a
mid green, and the tint multiplies it with every channel clamped to 1
(`tint_bytes`), so the best a tint can do is darken green toward olive.

**Why the mod cannot.** A brighter base texture would brighten every biome
that does not tint, and the untinted world is most of it.

**Ask.** Let a chunk tint's channels run above 1 — 0 to 2 in the byte, say,
with 1.0 at 128 — so a tint can lift a channel as well as cut one.

## 32. A cover fill is one block tall (2026-09-16)

The Flower Forest's brief asks for "single- and TWO-block flowers", and
alliums and peonies are the two-block ones. `fill_cover` clamps its run to
`SUBNODES_PER_AXIS` and keeps it inside one block, so the tallest flower
the mod can grow is three cells — a third of the height the brief wants,
and the same height as a poppy, which should be the short one.

Everything else the mod has for standing things on a surface is the
scatter, and a two-block flower through the scatter is a schematic per
flower, a stand program per flower, and a neighbourhood pass per chunk for
what a cover does in one pass — for a plant that grows every few blocks
across a whole ring, that is the wrong tool by an order of magnitude.

The ask is a cover that may run past the block it starts in: `cells`
allowed over three, the run continuing into the block above while the
blocks are empty, and the run stopping where it meets anything. The
engine already finds the surface and already keeps one run to a block, so
this is the same scan with the ceiling lifted.

## 27. Friction per block (2026-09-14)

The Frozen Wastes' crevasses have "slick, near-frictionless blue ice
walls", and ice underfoot should slide. Nothing in `register_block` says how
a body grips a block, so ice walks like stone. A `friction` on the block —
a share of the normal grip, default 1 — read by the movement code for the
block under a body and the block it is pressed against, is the ask.

## 29. A plant's cells displace the water round it (2026-09-15)

"Grass, when under water, should get saturated so it doesn't have an air
bubble around it." A block's fluid volume is twenty-seven cells less the
cells it holds, and a tuft of grass, a water iris, a stand of seagrass or
kelp holds one to three, so the water round a plant is short by that much
and the plant stands in a pocket of air. A passable (or billboard) cell
should not displace fluid: the block would hold twenty-seven of water and
the plant, drawn in it. The mod cannot do it — a fluid and a solid cannot
share a cell in the store — so its land covers keep out of the water
meanwhile, and the sea's own growth stands in pockets.

## 28. Summaries carry no fluid (2026-09-15)

The seas are placed: a third of the disc, in pools. A chunk outside the
detail radius is drawn from its summary, and a summary holds materials
only — so from a hill a sea is its floor, a hole in the world the shape of
the pool, until the player is within the detail radius and the full chunk
with its fluid arrives. A summary that carried "and fluid to this level"
(one level and one fluid per summary would do: a pool is flat) would draw
the sea to the horizon. Seen from the window at the first seas, with the
water inside the detail radius drawn in light mode 3 as one translucent
volume with the chunk seams visible through it, which may be the same
thing seen from inside.

## 26. A deep sea is heavy to load (2026-09-14)

**Seen.** The deep ocean put everywhere, headless, one bot: 213 of the ticks
in ninety seconds ran over budget, and the log put the fluid tick at 45 to
100 ms of each. The sea is 60 to 120 blocks deep, and its trenches 350.

**Why, from the code.** `Fluidics::chunk_loaded` touches every non-empty
block of a loaded chunk's fluid layer ("milk saved mid-flow has to carry on
flowing"), so a chunk of open sea puts 4,096 blocks in the solver's active
set, and a player's view of ocean is hundreds of such chunks. Each is found
settled and dropped, but finding it is the cost.

**Ask.** A generated or saved layer that is known to be at rest should not
be woken whole: only its blocks next to something that can move (air
beside a partial surface, an unloaded neighbour becoming loaded, an edit).
The designer is working in the fluid code now; this is the number to watch.

## 25. Light in water (2026-09-14)

The deep ocean's brief has "total light extinction" on its plains, a
hundred blocks down. Water is `transparent` and a fluid is a layer over air
blocks, so sunlight falls through a hundred blocks of sea at full strength.
A fluid that attenuates the sun — `light_falloff` on `register_fluid`,
levels lost per block of it — would make the deep sea dark and a shallow one
bright, which is the whole of it. Until then the plains are dark only in
that nothing grows on them.

## 24. Shade under a canopy (2026-09-14) — the chunk-arriving half LANDED (engine deba305); foliage still passes light as glass

**Seen.** The rainforest's brief has its canopy block 85-90% of direct
sunlight. Headless, round one spot 85% of the floor has a WHOLE leaf block
somewhere over it, and most of those columns read `sun = 15` at the floor.

**Why, from the code (not changed):**

1. **Foliage passes light the way glass does.** `see_through_from_rules`
   (`crates/server/src/light.rs`, about line 558) puts every `cutout` rule
   in the see-through table — "Contract §8.2: foliage passes light the way
   glass does" — and `Lit::faces` answers `Faces::OPEN` for a uniform block
   of it. Straight-down sun at 15 never attenuates
   (`crates/core/src/light/propagate.rs`, `arriving`), so any depth of
   leaves is open sky. Every leaf node in the world mod is `cutout`.
2. **A chunk loaded ABOVE a lit one never darkens it.**
   `Lighting::chunk_loaded` relights only the new chunk; `relight` clears
   only that chunk and `flood` only brightens; `remove` runs only for
   edits. `sky_reaches` counts an unloaded block above as open sky. The
   floor loads first (it is nearest the player), takes full sun, and keeps
   it when the canopy's chunks arrive. This is a bug whatever foliage does.

**Ask.** (2) first: when a chunk arrives, run the sun channel's removal
from the bottom layer of the new chunk into the chunk below wherever the
new chunk's bottom is darker than 15, and re-flood. For (1), a decision
for the contract: foliage that attenuates the sun by a level or two per
block rather than passing it untouched, so a thick canopy is dim beneath
and a thin one dappled. The mod cannot work round either: an opaque
material inside every clump would be visible through the leaves' holes,
and would still be undone by (2).

## 17. The world cannot be read inside a dig or place hook (2026-09-11)

**Seen.** `game.get_block` returns nil from inside `register_on_dig_complete`
for the very block being dug, with the player standing on it. The block
reader answers through the sight lease (`mlua_vm.rs`, `block_reader`),
which is held only while the tick runs the mods' own callbacks; the dig
hook is asked from the dig path, where the lease is `None` and every
reading is `Unavailable`.

**Why it matters.** A hook that decides by what the block HOLDS — blooms
on a bush, a lock on a door, a nest with eggs — cannot look, and the
event carries one material of a block that may hold three. The mod
decides on the event's material and reads the block a tick later, which
means cancelling a dig it may then find had nothing to pick.

**Ask.** Hold the sight lease across the cancellable hooks, or hand the
dig hook the block's cells. Reads are the only thing wanted; a write from
a veto hook is already refused, and can stay refused.

## 16. A right-click on a block (2026-09-11)

**Wanted.** Picking roses: right-click a bush and it gives a rose or two
and loses its blooms for a while. Right-click is the natural verb for
"use what is in front of you" and the designer asked for it by name.

**Why the mod cannot do it.** Right-click is the place control, and a
placement exists only when the player carries a placeable material:
with an empty hand the client sends nothing, and `register_on_place`
never fires. `register_on_punch` is entities; `register_on_action` has
no target. The mod picks on a DIG for now — a completed dig on a bush
with blooms is cancelled with `""` and handled — which is the same idiom
`tiamot_default_life` forages berries with, but it is not the verb asked
for, and a pick that takes a dig's countdown is slow.

**Ask.** `game.register_on_use(callback)`: fired when the place control
lands on a block and no placement is possible (empty hand, or an item in
it), with `{ player, x, y, z (the cell), material, held }`, before
anything else; the same return ladder as the other hooks, `""` meaning
handled. The client already knows the cell under the crosshair — it is
what it would step across for a placement — so it is one message with
the target and nothing to place.

## 13. A billboard that is also cutout is drawn as cubes (2026-09-11)

**Seen.** Grass declared `cutout = true, billboard = true` was drawn as
cutout CUBES — cell faces showing ninths of the blade tile — with the
sprite lost inside them. In `mesher.rs` (`emit` for one axis) the opaque
set is `solid & !panes & !leaves & !sprites`, as the comment beside it
says ("taken out of every set here"), but `leaves` is the cutout column
as it came and its faces are emitted from that, sprites included.

**Fixed in the mod** by not declaring `cutout` on a billboard: the sprite
pass alpha-tests with `fragment_cutout` on its own, nothing in core reads
the flag, and the client reads it only to build the foliage set.

**Ask.** Either `let leaves = cutout & !sprites` (one line, and the
comment already promises it), or refuse the pair at registration the way
`transparent` and `cutout` are refused together — a billboard has no cube
faces for a culling rule to apply to.

## 11. An item dropped at a position (2026-09-11)

**Wanted.** Water reaching a leaves block breaks it (`rules.lua`, on
`register_on_fluid_flow`), and the designer wants what it was to DROP. A
dig drops through the engine's own rule and a placement can be refused
with the player keeping the material, but a mod that removes a block in a
hook has no way to put its units into the world as a pickup.

**Ask.** `game.drop(position, { material = "mod:block", units = 27 })`: the
same pickup a dig makes, spawned at a position, owned by nobody.

## 7. Summaries of partial blocks read as crosses (2026-09-10) — noted

**Seen.** Beyond the view distance the horizon is drawn from LOD summaries,
and a woodland's trunks — whole blocks minus their corner columns — come
out as "+" shapes floating at canopy height, with small leaf clumps as
lone crosses. Not a mod matter; recorded so it is not chased as one.

