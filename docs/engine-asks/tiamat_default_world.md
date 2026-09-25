# Engine asks from Tiamat Default World

From the `tiamat_default_world` mod (the Spindle; repo `Tiamat_Default_World`,
beside the engine). Kept here rather than in that repo so the engine agent
finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

Filed 2026-09-24, found taking up Weather ask W7 (soil that drinks the
rain) in the world mod.

## 43. `absorbs.becomes` cannot name another mod's block

**Seen.** Weather's exports contract (`Tiamat_Default_Weather/docs/
exports-contract.md`, the W7 paragraph) proposes the Spindle declare
`absorbs = { rate = 3, becomes = "tiamat_weather:damp_dirt", fluid =
"tiamat_weather:rainwater" }` on its dirt — the engine's own saturation
chain in place of Weather's material swap. Registration refuses it:
`becomes` goes through `qualify_id` (`mlua_vm.rs`, `block_absorbs`),
which errors on any namespace but the registrant's own, so the contract's
line is a load-time error that disables the whole world mod. The `fluid`
one field over already crosses namespaces — kept as written, resolved at
freeze, and a name nobody registered drinks nothing.

**Why the mod cannot.** The damp materials are Weather's — its textures,
its drying sampler and random tick, its aliases back to the dry ids — and
dampness is weather. The soils are the Spindle's, which is the W7 row's
own reasoning for putting the declaration here. Neither mod can hold both
halves of the chain, and the Spindle registering damp twins of its own
would duplicate the set and leave nothing that dries them.

**Ask.** Give `becomes` the treatment `fluid` already has: a name with a
namespace kept as written and resolved at freeze against the world
material table. The machinery is in place — `absorbency_from_rules`
(`server/src/fluid.rs`) already keys by world id and drops a `becomes`
nobody registered to `None`, ending the chain, which is exactly right for
a world running without the other mod.

**For whoever lands it, one warning.** The soak swap keeps a partial
block's shape (`handle.rs`, "a chiselled step that soaks is still a
step"), and Weather's drying — sampler and random tick both — skips any
block that is not FULL. On sub-node-smoothed terrain a cross-mod
`becomes` will therefore strand permanently damp partial blocks down
every slope rainwater runs, until Weather also dries partial blocks
shape-kept. Worth a line in the contract when this lands. Meanwhile the
Spindle declares its soils as drains (rate and fluid, no `becomes`),
which soaks the puddles and changes no material.

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

## 41. Near summaries are stuck coarse while detail lags: LANDED 2026-09-25 (engine c537df8)

**What landed:** the frontier gate in `Streamer::next_summaries` (engine
9d48673) was holding back every summary past the nearest still-needed
chunk, not only new ones. It now holds back only a position the client
draws nothing at. A coarse summary the client already holds is refined as
the player approaches (level 2 or 3 to level 1 at nine to fifteen chunks),
and a full chunk that has left the detail radius is downgraded, however far
behind the detail is. The worker's whole summary chain is stored on the
first request, so a later, finer request is a database read rather than
another generation of the chunk.

**Not done, and why:** the level-1 re-send inside the detail radius. Inside
the radius the replacement for a slab is the full chunk, and a level-1
summary costs the same generation as that chunk, since `ChunkSource` has no
coarse entry point; the re-send would take a worker slot from the chunk it
stands in for and arrive no sooner. What remains inside the radius is
throughput. The request queue is re-sorted from the player's position every
beat, so a stale first-in-first-out order was not the cause; the detail
lags because a chunk beside a walking player is generated twice, once for
its summary and once in full, on a pool every player shares. Two engine
changes would make it cheaper, and either is a separate ask if the mod
wants it: keeping the chunk built for a summary long enough for the detail
request that follows it, and a level-aware entry point in `ChunkSource` so
a block-resolution summary can be built without the full chunk.

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
