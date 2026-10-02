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

**From the engine, 2026-09-26 (engine c0fab33):** the designer quit standing on
a big tree and came back on the ground under it. `player.lua` writes the
exact position in `on_player_leave` and every `SAVE_EVERY` (400 ticks)
otherwise — and on a clean quit the tick never saw the player go, so the
leave hook never ran and the twenty-second-old position on disk was what
came back. The engine's shutdown now runs `on_player_leave` for everybody
still connected, before the storage flush, so the hook's write is what is
saved; the cadence can stay as it is. Not asked, answered anyway: the hour
is now saved with the world too, so a night no longer ends at every launch.

**From the engine, 2026-09-25 (engine 67143e9):** the world seed now reaches Lua
exact, as the integer of its 64 bits, so `pos.seed` and `game.world_seed`
read as a negative number for a seed with its top bit set and come back to
`density:bounds`, `density:at`, `game.rng_stream` and `game.noise_heightmap`
by their bits. Until then such a seed crossed as a float and came back a few
hundred off, so a generator's bounds described a different world from its
fills: half of all new worlds were dirt with no biome claim, with air holes
and solid boxes. `generate.lua`'s float branch in `seed_int` no longer runs
and can go.

## 45. A chunk's fog has a top and no bottom (2026-10-01): LANDED 2026-10-02 (engine 5d20328, protocol 84), awaiting the eye

**Seen.** The designer, in the Veined Silver Gallery (a dark cave, 2.1 to
3.6 km down): "way too much bright yellowish fog. It should be dark and
less thick." The gallery declares no fog. The Fungal Grove Chambers did,
for their spore haze, in the normal caves some 2 km over it, and
`register_chunk_fog` answers once per chunk COLUMN with a `top`: the fog
is full strength everywhere under that top, so the grove's haze filled
every cave below it in the column. The same is true of the surface fogs
(the taiga, the mesa, the cold rim's): every one of them stands in the
caves under its biome.

**Why the mod cannot fix it.** The callback is asked per column, with no
y, and the answer has no lower bound. The mod has taken the grove's fog out
(particles near the player instead), but a surface fog is the surface's
look and cannot go.

**Smallest change.** An optional `bottom` on the answer, the mirror of
`top`: the fog thins below it over the same few blocks it thins above
`top`. A surface fog would give its biome's ground less a margin, a cave's
its storey's floor, and an answer without one would be today's.

**From the engine, 2026-10-02 (engine 5d20328, protocol 84).** As asked:
a `register_chunk_fog` answer takes `bottom`, a block height floored like
`top`, and the fog thins below it over the same blocks it thins above
`top`. Omitted, or not a number, is today's fog exactly; a `bottom` above
its own `top` is lowered to the `top`, so a slip cannot thin a fog inside
its own layer; a non-number faults the answering mod, as a bad `top`
does. The gate as a screenshot test in all three lighting modes: a cave
under a fogged column is red with no `bottom` or one beneath it, and clear
with one twenty blocks above it. For the eye ([H]): once the surface fogs
give a `bottom` of ground less a margin, the Veined Silver Gallery is
clear, and the fog still shows above the margin.

## 44. The sky modifier has no say over the stars (2026-10-01): LANDED 2026-10-02 (engine 5d20328, protocol 84), awaiting the eye

**Seen.** "Below the surface only darkness and stars should be seen in the
sky, and the day/night cycle should not be there." The underside under the
core stack is carved open (the sponge), and from it the sky beyond the
body is the overworld's. `underside.lua` now lays a black modifier over a
player's sky under Spindle 14 km (intensity 0, sky black, mixed all the
way), which ends the day there; but the stars come from the keyframes'
`stars` alone, so under the world they show at night and not by day, the
opposite of the half that was asked for.

**Why the mod cannot fix it.** `set_sky_modifier` takes intensity, sky,
sky_mix, fog_distance and saturation; `register_sky` is registration-only
and per domain, and the underside is the overworld.

**Smallest change.** An optional `stars` (0 to 1) on the modifier, which
replaces the keyframe's value while it is set, eased like the rest. With
it the world asks `stars = 1` under the line.

**From the engine, 2026-10-02 (engine 5d20328, protocol 84).** As asked:
`game.set_sky_modifier(uuid, { ..., stars = 0..1 })`. While the modifier
is set, `stars` replaces the keyframes' value, by day as by night, eased
like the rest; omitted, the keyframes decide; `0` is a say (no stars);
out of range clamps, and a wrong type is an error naming the function. So
`underside.lua`'s black modifier with `stars = 1` gives darkness and stars
under the line, and clearing it eases back to the day's sky. For the eye
([H]): under Spindle 14 km by day, a black sky with stars; climbing out,
the day comes back without a jump.

## 43. `absorbs.becomes` cannot name another mod's block: LANDED 2026-09-26 (engine 1c475a8)

**From the engine, 2026-09-26 (engine 1c475a8):** a `becomes` with a
namespace is kept as written, as `fluid` always was, and resolved at
freeze against the world's table — `absorbency_from_rules` drops a block
nobody registered to `None`, so a world running without Weather has a
chain that ends at the soil rather than a mod that failed to load. A bare
name is still your own. The warning stands and was passed to Weather's
sheet: the soak swap keeps a partial block's shape and Weather's drying
skips partial blocks, so a cross-mod `becomes` on sub-node-smoothed slopes
strands damp partial blocks until Weather dries them shape-kept.


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

## 42. A summary draws its fluid as an opaque slab: LANDED 2026-09-28 (engine 7020552), awaiting the eye

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

**From the engine, 2026-09-28 (engine 7020552):** the summary mesher now
sends a fluid-family cell's faces to the blended fluid pass — the same
vertices and the same per-material opacity the detail water uses, full to
the cell and still — and a solid face under such a cell is exposed through
it, so the bed shows under a far sea as it does under a near one. Two
cells of one sea share no face. Unit-tested on the mesh; what it looks
like over the Obsidian Barrens' lava and a far sea is the designer's to
say (an [H] gate): fly to where the slab was and look for a blended
sheet that agrees with the water beside it when detail arrives.

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

## 40. Generation lag is silent: LANDED 2026-09-28 (engine cbbbc5e)

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

**From the engine, 2026-09-28 (engine cbbbc5e):** the workers now time
`generate()` and the pool keeps the cost of the last 64 chunks. Once a
second, while more chunk requests are parked waiting on the workers than
the pool can hold in flight, the server logs one line at `warn`:
`generation behind: N chunks waiting, M in flight, X ms/chunk`. A walk
the pool keeps up with says nothing. `Pool::recent_cost` and
`Pool::capacity` are there for anything else that wants the numbers.

**From the engine, 2026-09-28, again (engine c55ad43):** the rule above
fired on every walk. One player may have more requests out than a
four-worker pool holds, so "more waiting than the pool can hold" was the
ordinary shape of a stream the pool was keeping up with, and it was
logged at 0.6 ms a chunk as readily as at 45. Counting requests
cannot tell the two apart; where the workers' time goes can. The line now
speaks once the workers have spent at least 90% of their time generating,
with requests waiting, for three looks (seconds) running. The tick refills
the pool once a pass, two jobs a worker, so in practice that is chunks
dearer than about 23 ms asked for without a break; a pool the tick is
pacing, full at every look but idle for most of every tick, says nothing.
Still once a second while it holds, now ending in the workers' share and
how long: `generation behind: N chunks waiting, M in flight, X ms/chunk —
W workers generating P% of the time for S s`. `Pool::busy` is the
workers' generating time, for anything else that wants it.

## 39. A terraced fill's `within` cannot be built flat: LANDED 2026-09-28 (engine 4885f54)

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

**From the engine, 2026-09-28 (engine 4885f54):** `{ op = "noise2", ... }`,
the same fractal sampled on the ground plane with `y` held at zero — one
value per column, the same at every height — so `reads_y` is false by
construction and the ask-35 check passes any program built from it.
Same options as `noise` and no `stretch` (refused, as a `contour`'s is);
a `contour` of the same stream draws its line through this field. The
guard around every terraced fill can go, and the river beds fill.
