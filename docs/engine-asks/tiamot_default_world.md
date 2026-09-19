# Engine asks from Tiamot Default World

From the `tiamot_default_world` mod (the Spindle; repo `Tiamot_Default_World`,
beside the engine). Kept here rather than in that repo so the engine agent
finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

## 37. Water that runs through a plant does not break it (2026-09-18) — the flow hook's reads LANDED (engine a3db9fa)

**Seen, in play.** Water let loose over a meadow (a brook's spill, in the
Flower Forest) ran through the grass cards and stood in the same blocks as
the tufts. The designer: "Grass should probably get broken by water."

**What the mod does now, and why it is a workaround** (rules.lua). A tuft is
a few cells of its block, under the fluid's `waterlogs_at`, so the solver
lets water in and `register_on_fluid_flow` never reports the plant. The
reports it does send are the water's edge pressing sideways on ground
(measured, a flood over a meadow: 1,208 reports, all sideways, none from a
plant's block). So the mod notes each report's place and, on the next tick,
sweeps a 7 x 7 round it for a plant with water in it and clears it. It works
(20 wet plants of 102 gone, the dry 82 kept, 1.4 ms of mod time a tick while
the water ran), but it is a guess about where the plants are from where the
water is blocked.

**Smallest change.** A block flag, `washes_away = true`: when the solver
moves fluid INTO a block whose terrain is that block's, it clears the
block's cells first, as a dig would. The mod would declare it on every
`passable` plant and drop the sweep.

## 27. Friction per block (2026-09-14)

The Frozen Wastes' crevasses have "slick, near-frictionless blue ice
walls", and ice underfoot should slide. Nothing in `register_block` says how
a body grips a block, so ice walks like stone. A `friction` on the block —
a share of the normal grip, default 1 — read by the movement code for the
block under a body and the block it is pressed against, is the ask.

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

