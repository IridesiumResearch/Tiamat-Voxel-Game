# Engine asks from Tiamot Default World

From the `tiamot_default_world` mod (the Spindle; repo `Tiamot_Default_World`,
beside the engine). Kept here rather than in that repo so the engine agent
finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

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

