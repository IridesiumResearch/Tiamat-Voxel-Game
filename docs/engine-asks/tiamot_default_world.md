# Engine asks from Tiamot Default World

From the `tiamot_default_world` mod (the Spindle; repo `Tiamot_Default_World`,
beside the engine). Kept here rather than in that repo so the engine agent
finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

## 38. A fluid that does not wash plants away (2026-09-22)

**Seen, adopting 37.** `washes_away` (e4ac3a8) is the plant's to declare,
and every fluid that runs into the plant clears it. The world mod declares
it on every `passable` plant, as the contract says to. But the Weather mod's
rainwater is a fluid too: its puddles are laid only on bare whole blocks
(its `ground.lua`), and a storm's puddle is 6 to 12 cells that run on into
the tufts beside it, so every rain would clear the grass round its puddles.
The world mod's own sweep, which this replaced, skipped fluids another mod
had called harmless (`add_harmless_fluid` in its exports); the engine rule
cannot be told.

**Why the mods cannot.** The plant cannot say which fluids wash it, the
rainwater cannot say it washes nothing, and the clearing is the solver's,
so no hook sees it in time to refuse.

**Smallest change.** `washes = false` on `register_fluid` (default true):
a fluid that never sweeps a `washes_away` block. Weather would set it on
rainwater. Or the other way, as `absorbs` names its fluid:
`washes_away = { fluid = ... }` on the block, though that makes every plant
list every fluid; the fluid's switch is the one line.
