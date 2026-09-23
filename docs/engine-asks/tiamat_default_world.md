# Engine asks from Tiamat Default World

From the `tiamat_default_world` mod (the Spindle; repo `Tiamat_Default_World`,
beside the engine). Kept here rather than in that repo so the engine agent
finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

Filed 2026-09-23, from the designer's fly-round (giant summary slabs beside
the player, a dry river, a water sheet over the Obsidian Barrens) and the
investigation behind the world's performance pass.

## 42. A summary draws its fluid as an opaque slab

**Seen:** a translucent grey-blue sheet standing over the Obsidian
Barrens' lava field. A summary paints a terrain-free block holding fluid
as the block that fluid is drawn as (ask 28, right), but the summary
mesher emits only opaque quads — so a far sea, or standing rain, is a hard
slab that disagrees with the blended, transparent water beside it the
moment detail arrives.

**Why the mod cannot fix it:** the summary pass is the client's; a mod
has no say in how a summary cell is drawn.

**Smallest change:** draw a summary's fluid-family cells in the blended
pass (or tint-and-blend the quad by the material's own alpha). Cosmetic,
and the lowest of these four.

## 41. Near summaries are stuck coarse while detail lags

**Seen:** the "full chunk issue" — flat-topped, chunk-aligned slabs beside
the player in the Alpine Highlands, the Badlands and the Ember Ridge. A
position holds a chunk OR a summary; a summary sent at level 3–5 while the
player was far away keeps being drawn until the full chunk arrives, and
summary UPGRADES queue behind the detail frontier — so when generation is
the bottleneck, the world beside a fast-moving player is 4–16-block slabs
for minutes.

**Why the mod cannot fix it:** serving order and summary levels are the
engine's; the mod can only make generation cheaper (it is, this week).

**Smallest change:** inside the detail radius, let a level-1 summary
(block-resolution, ~14x cheaper than sampled detail) be re-sent AHEAD of
the detail frontier for positions whose full chunk is still parked on the
workers. Slab-world becomes block-world while detail catches up.

## 40. Generation lag is silent

**Seen:** the designer flew a world that was arriving as slabs and the log
said nothing — the over-budget warning fires past 50 ms of tick, which the
worker pool exists to prevent, and the workers' `generate()` is untimed.
The mod's own counters say what was generated, never how long it took or
how deep the queue is.

**Why the mod cannot fix it:** a mod cannot time the engine's workers or
see their backlog.

**Smallest change:** a periodic (or backlog-triggered) line naming the
worker queue depth and the rolling ms/chunk — "generation behind: 1,842
waiting, 41 ms/chunk". One line turns "we might need a performance pass"
from a guess into a number.

## 39. A terraced fill's `within` cannot be built flat

**Seen:** a fully-dressed river bed, bone dry, one day after the world mod
wrapped every terraced fill in a guard for the ask-35 error ("`within`
answers differently at the two heights"). The flattest mask a mod can
build from noise is `stretch = { y = 1000 }` — tall, not flat — so two
heights disagree by a hair, the error fires, the guard skips the chunk,
and the river is quietly dry. The sea's shoreline detail reads height
harder and skips patchily, which is the "weird ocean fill glitch". The
world mod is rebuilding its `within` fields from maps and 2D geometry now,
which works but costs every y-free mask a map or a compromise.

**Why the mod cannot fix it properly:** there is no noise the DSL can
declare that provably does not vary in y, so "flat by construction" is
impossible for any mask that wants coherent noise in it.

**Smallest change:** a `noise2` node — x/z only, no y axis at all, same
streams and determinism — and the ask-35 check passes any program whose
every leaf is y-free. (Alternatively: narrow the ask-35 error to the case
the plane actually misreads, but the 2D node serves worldgen more widely —
it was already on the plan's wish list.)
