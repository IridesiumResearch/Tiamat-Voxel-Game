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
block's cells first, as a dig would. The mod would declare it on every
`passable` plant and drop the sweep. Separately: the flow hook should be
able to read, as the dig, place and punch hooks now can (7579e22); or the
stub should say it cannot.

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

