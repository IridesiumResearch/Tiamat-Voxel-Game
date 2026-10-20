<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Where the performance goes — state of the engine, 2026-10-12

Measured on the reference machine (Ryzen 7 7800X3D, release builds; a
minimum-spec i5 runs the same code roughly three times slower, per the CI
note in `docs/performance-targets.md`). The GPU side cannot be measured here
and is reasoned from the renderer's structure; the overlay's phases and its
`VRAM` line are the instruments for a real card. Budgets are the charter's:
**50 ms a tick on the server, shared by everything for every player; 16.7 ms a
frame on the client at 60 fps**, of which the remesh budget takes 2 ms.

## The server: one player, the default mods, a sprint over the fen

The fen soak (`crates/bot/tests/fen_soak.rs`, release, the eight default mods,
two minutes of sprint flight then forty seconds standing). Means over every
tick in each window, in milliseconds:

| window | mean tick | serving (of it: relight) | fluid (active cells) | save | sqlite | resident chunks |
|---|---|---|---|---|---|---|
| spawn | 12.8 | 5.1 (4.6) | 4.1 (933) | 0.3 | 0.5 | 842 |
| fly 0–30 s | 12.1 | 10.3 (9.6) | 0.7 (598) | 0.1 | 0.6 | 1,795 |
| fly 30–120 s | 12.9–13.2 | 11.9–12.2 (11.3–11.6) | 0.2 (0) | 0.1 | 0.5 | 2,035–2,100 |
| standing | 3.5 | 1.4 (0.9) | 1.1 (1,042) | 0.1 | 0.4 | 5,326 |

Not one tick went over budget. What the table says:

- **Relighting newly served chunks is the tick while anybody is moving.**
  Nine tenths of a flying tick, a quarter of the budget, for one player. A
  chunk relights in 0.21 ms sealed, 0.33 ms under open sky and 0.49 ms every
  block chiselled (`cargo bench -p tiamat-core --bench light`), so a sprint
  relights about thirty-five chunks a tick. It scales with players: on this
  CPU four or five players sprinting at once would fill the tick with relight
  alone, and that is before a minimum-spec server's factor of three.
- **Everything else is small.** Fluid settles to 0.2 ms once the ponds are at
  rest (4 ms in the first seconds of a spawn, with 933 cells active); a chunk
  save is 0.1–0.3 ms and a batch of a thousand 3.7 ms; SQLite is half a
  millisecond a tick; a light edit is 0.12 ms placed and 0.29 ms broken; a
  dig is free; the random tick is 8 µs; a chunk encodes in 1–17 µs.
- **Worldgen never touched the tick.** It runs on one to four workers, and the
  flight never logged `generation behind`: the pool kept up. The World mod's
  chunk is the single slowest thing in the engine — 50–100 ms on a worker, as
  the targets doc records — but a worker is a core, not the tick. The
  engine's own paths under it: a block-resolution chunk 54 µs, a density
  with caves 0.75 ms, smooth detail 1.1 ms and sampled detail 4.2 ms per 48³,
  the heightmap path 14.5 µs per 256 samples. `fill_3d` still misses its
  2 ms target at one octave (2.65 ms); explicit SIMD was never attempted and
  needs its own determinism argument first.
- **Pathfinding is budgeted, not free.** A default search costs 2.0 ms and a
  maximum one 5.6 ms; the tick shares 8,000 expansions among every mob, so
  about four default searches a tick get through and the rest answer
  `Exhausted`. A steer is two block reads and is unmetered.
- **Memory is bounded.** Resident chunks fell as the player flew (the unload
  sweep working) and rose to 5,300 standing still as the horizon filled in.

## The client: the CPU side, measured

`cargo bench -p client --bench mesher`, one chunk, this machine:

| chunk | unlit scan | lit, as the game runs it |
|---|---|---|
| air | 11 ns (skipped) | 11 ns |
| uniform stone | 0.33 ms | 1.07 ms |
| realistic terrain | 0.26 ms | 0.75 ms |
| every block chiselled | 0.97 ms | 2.90 ms |

The non-splittable start of a job (the grid scan) is 42–200 µs. Against the
2 ms remesh budget that is **two lit terrain chunks a frame here, about one on
a minimum-spec CPU** — 60 to 120 chunks a second, where the server can serve
440 a second (22 a tick). While flying, the client's frame thread is the
streaming bottleneck, which the overlay shows as `queued`. Summaries are
cheaper (sixteen a frame inside 0.5 ms) and bounded. The per-frame fixed
costs are the cull loop over every resident mesh (thousands to tens of
thousands, now one map probe per chunk since the corner-tint cache), one
instance write, and one draw call per visible chunk plus one each for its
glass, fluid, foliage and sprites. Figures were posed and drawn whether or
not they were on screen until today; now they are culled.

## The client: the GPU side, from the renderer's structure

Not measured here — a software rasteriser says nothing about a card — but
the shape is known:

- **Beautiful is the expensive mode**, and it is where the reporter's card is
  pegged. Three shadow cascades at 2048² (48 MiB; the setting's High is
  4096², 192 MiB), every caster drawn three more times to fill them, a
  nine-tap PCF per fragment; an HDR scene target plus a fogged copy, the fog
  pass reconstructing position from depth per pixel, bloom at half
  resolution, a composite; the flicker list looped per fragment up to
  sixteen sources. Classic (the default) has none of the post chain and a
  single generic shadow.
- **There is no occlusion culling.** Only frustum culling of chunks and the
  buried test for summaries. Every chunk under the ground inside the frustum
  is drawn — a cave system under a hill is as many draws as the hill — and
  cutout foliage is alpha-tested within 176 blocks, sprites drawn within 112.
  In mountains and caves this is where fill rate goes.
- **The cloud deck** costs 1.2–4.0 ms a frame by view at Medium (weather
  sheet W27's measurement), the single largest named GPU item outside the
  post chain.
- **VRAM** is now accounted on the overlay. Today's changes (one index buffer
  for every mesh, pool classes at 1.5×, summaries past the fog held unbuilt,
  culled figures) remove the largest avoidable allocations; the gate is the
  `VRAM` line on a real card before and after.

## Corners that can be cut, in order of payoff for effort

1. **Relight off the tick** (server, medium effort). The biggest tick cost,
   and it is per chunk and deterministic: light a chunk on the worker that
   generated it, or on a relight worker, and merge the layer on the tick. It
   would take a sprinting player's tick from 13 ms to about 2 ms and make the
   fifty-player target realistic for movement.
2. **Meshing off the frame thread** (client, medium-high effort). The
   dominant client CPU cost and the streaming bottleneck. The mesher is pure
   and `MeshJob` already carries state across frames; a job needs a snapshot
   of the chunk, its six neighbours and its light. The upload stays on the
   frame. This is also what Distant Horizons does, and it is the change that
   would make frame pacing while flying flat.
3. **Chunk occlusion** (client, medium). A chunk fully enclosed by its
   neighbours' solid faces, or a column cover map from the server's own
   heightmap, would take the underground out of the draw list for a camera
   above ground. Cuts draws and overdraw in exactly the terrain that is slow.
4. **Cheaper defaults for Beautiful** (cheap). Medium shadows at 1024² cost a
   quarter of 2048²'s memory and fill; two cascades instead of three cut the
   caster passes by a third. Quality costs, and the designer's call.
5. **A smaller vertical view** (cheap, a setting). The default 8×12 box is
   about 4,900 chunks; 8×8 is a third fewer resident, meshed and relit
   chunks. The 12 was chosen for seeing a valley floor from a ridge.
6. **The World mod's chunk** (mod side). 50–100 ms a chunk is the slowest
   thing in the engine; sampled detail (4.2 ms per 48³) and multi-octave
   fills are where it goes. Fewer octaves and the smooth detail path where
   sampled is not seen would halve it without an engine change.
7. **Not worth cutting.** Light edits, digging, saves, SQLite, the random
   tick, summary persistence and fluid at rest are all under a percent of the
   budget each. Sub-node resolution itself is not the expensive part of this
   engine, as the verdict said and the chiselled numbers still show.

What this document does not know: the frame time on the reporter's card by
phase, which only the overlay there can say, and the minimum-spec server's
real factor, which only a run on one can.

## Addendum, 2026-10-13: relight off the tick (milestone 0.5.0, P1)

The same soak, the same windows, with the light thread (`crates/server/src/lightwork.rs`):

| window | mean tick, before | mean tick, after | serving (of it: relight) | resident chunks |
|---|---|---|---|---|
| spawn | 12.8 | 3.8 | 0.44 (0) | 1,976 |
| fly 0–30 s | 12.1 | 3.7 | 0.65 (0) | 3,807 |
| fly 30–120 s | 12.9–13.2 | 3.7–3.9 | 0.6–0.7 (0) | 4,292–4,529 |
| standing | 3.5 | 2.4 | 0.30 (0) | 5,789 |

One tick over budget in 2,850, as before. The thread never took longer
than a tick for a tick's batch. What is left of a flying tick is fluid
(0.5–0.9 ms), serving without its relight (0.6–0.7 ms), SQLite (0.7 ms)
and the rest of the phases under 0.4 ms each; the corner that was worth
cutting is cut, and the next largest is the fluid's hooks. Light reaches a
client a few milliseconds later than it did, after the chunk rather than
beside it, which the client always tolerated. The determinism argument is
in the module's documentation: the thread runs the same passes over the
same blocks in the same order, and `a_snapshot_lights_a_chunk_exactly_as_the_world_does`
holds it to that.

## Addendum, 2026-10-20: the WAL checkpoint off the tick (milestone 0.5.0, E1)

Fifty bots for five minutes, release, the reference machine, before and
after `persist::checkpoint` (84e1a4be):

| | ticks | mean | p99 | max | over budget | the worst tick was |
|---|---|---|---|---|---|---|
| before, 2026-10-16 | 6,294 | 2.6 ms | 25.1 ms | 129 ms | 3 | one SQLite summary save, WAL 4.1 MiB |
| before, the runner (nightly 82) | 6,382 | 2.5 ms | 6.2 ms | 136 ms | 4 | one SQLite chunk-save batch, WAL 3.9 MiB |
| after | 6372 | 1.6 ms | 3.2 ms | 6.4 ms | 0 | nothing named |

Every tick over budget in those soaks was `SQLite`'s automatic checkpoint,
which ran inside whichever commit crossed a thousand pages — on the tick.
It is off now on every connection the world opens; a thread named
`checkpoint`, on a connection of its own, folds the log back passively
every five seconds, and the server's `World` starts it at open and stops
it before the closing flush. The tick still records the WAL's size beside
any slow call, so a regression here would name itself. The next named cost
in the state above, the fluid hooks' registry walk, is the survey's second
item.
