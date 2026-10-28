<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Tiamat performance audit — 2026-10-26

**Read-only audit. No engine, mod, config, or test file was changed.**
Four subsystem sweeps (server tick, server networking/LAN, client renderer, client meshing/persistence) plus a
verification pass over every claim below. No new benchmarks were run, so each item is tagged:

- **[M]** a number the repository already measured and records in a comment/test/doc;
- **[C]** code-confirmed behaviour (the code does this), with an inferred cost range;
- **[I]** inferred arithmetic from code, not measured.

The existing `docs/performance-state-2026-10-12.md` is good and still largely accurate. This report does not
re-litigate what it already fixed (relight off the tick, WAL checkpoint off the tick, meshing off the frame
thread, view-distance LOD). It is about what is left, and about two places where the current numbers
**overstate how much of the target they actually cover**.

---

## 0. Read this before tuning to the current numbers

### 0.1 The 50-player soak is at view distance 1×1, not the default 8×12

`crates/bot/tests/swarm_load.rs:104` starts the server with
`view_distance: ViewDistance::MINIMUM`, which is **1 horizontal / 1 vertical — nine chunks per player**
(`crates/core/src/interest.rs:100-103`). The shipped default is **8 / 12 — 4,925 chunks per player**
(`interest.rs:91-94`). That is roughly a **500×** difference in interest volume and resident chunks.

The 50-bot result (1.6 ms mean, 3.2 ms p99, Oct 20) is a real result for a tiny box. It does **not** exercise:

- the per-player interest set walk and sort (4,925 vs 9 positions);
- the horizon (about 29k summary candidates vs a few hundred);
- the resident-set unload sweep (50 × 9 vs 50 × 4,925, if players are apart);
- the light handover snapshot (5.8k resident in the fen soak vs potentially 100k+);
- worldgen throughput for a real spread-out population.

All of those are on the server tick or on the runtime threads feeding it, and all scale with the above.
**The 50-player target is not demonstrated at the default view distance.** That is the single most important
caveat in this report. A 10-bot default-view soak with the real World mod would expose more than another
five-minute minimum-view run.

### 0.2 The mesher benchmark measures a path the game never runs

`crates/client/benches/mesher.rs:134-150` and `crates/client/tests/mesh_perf.rs` mesh with `&NoGlass` and
`&NoFluid`, whose `any_*()` flags are all false. The game passes a real `mesher::Sight`
(`crates/client/src/app.rs:2756-2790`) built from the material table; with the default mods it has glass,
cutout foliage, billboards, models and flickers populated.

That matters because `SubNodeGrid::from_chunk_with_fluid` runs **up to seven independent full-grid scans**
when those flags are set (`crates/client/src/mesher.rs:471-478`), each walking 110,592 cells and doing a
`BTreeSet` lookup per non-air cell. The 0.75 ms "lit terrain" figure is therefore a lower bound, not the
game's cost. The current CI mesh gate cannot see the regression this would cause.

---

## 1. Server simulation

Budget: 50 ms/tick for everything, 50 players. Findings are ordered by expected payoff.

### S1. Every in-world player re-sorts the whole 4,925-chunk interest set every 50 ms — even when idle

**Impact:** the repository measured **0.44 ms per warm pass** for this, "most of it the interest set's own
ordering" (`crates/server/src/transport/stream.rs:131-135`). The streaming beat fires for *any* connection
with a streamer, once per tick (`crates/server/src/transport/endpoint.rs:3926-3967`), and calls
`next_needed` unconditionally (`endpoint.rs:4721`). At 50 players × 20 passes/s that is **~0.44 core of
continuous CPU**, on the tokio runtime that also carries the network work, whether the world is idle or
joining.

**Why:** `next_needed` calls `interest::chunks_around` (`stream.rs:397`), which allocates a Vec sized
`view.max_chunks()` and **stable-sorts** ~4,925 positions with a key recomputed per comparison
(`crates/core/src/interest.rs:160-208`). It does this even when `limit == 0` (all in-flight slots full) and
when every chunk is already held.

**Fix (S/M):** cache the sorted interest `Vec` in `Streamer`; rebuild only on `recentre`/`resize` (the horizon
already does exactly this at `stream.rs:826-847`), keep a cursor at the first unheld entry, and return
immediately when nothing is outstanding and `in_flight` is empty. The translation-invariant offsets can be
sorted once per `(h, v)` and translated per recentre, removing the sort entirely.

**Confidence:** high that the code does this; the per-pass number is [M], the ×50 is [I].

### S2. The light handover clones the entire resident chunk map every tick, and makes reads deep-copy

**Impact:** `World::snapshot` is `Arc::new(space.cache.clone())` (`crates/server/src/world.rs:1049-1056`),
called once per domain with any light event (`crates/server/src/lightwork.rs:101-111`), which is almost every
tick while anyone moves (`crates/server/src/handle.rs:6644-6650`). The author measured "a few hundred
microseconds for a few thousand chunks" [M]; that is **0.2–0.3 ms at 5.8k chunks and ~10 ms at 100k+**.

The second-order cost is worse and invisible: the snapshot keeps every chunk's `Arc` alive while the light
thread works. `World::chunk` hands out `&mut Chunk` via `Arc::make_mut` (`world.rs:1108-1133`), so **any**
access through it while a snapshot is alive deep-copies the whole chunk — including read-only helpers
`block_cells`, `subnode` and `block_material` (`world.rs:1574-1603`). Digs, placements, plan stamps and fluid
hooks all use those.

**Fix (M):** hand the light thread only the `Arc`s of the chunks the batch touches plus their six face
neighbours (all a flood can reach). Add immutable read paths through `resident()` for the read-only helpers
(`material_at` at `world.rs:1027-1035` is the pattern).

**Confidence:** high on the mechanism; the snapshot cost is [M], the COW copies are [C]/[I].

### S3. The once-a-second unload sweep is O(resident chunks × players), with a full sort and a `String` clone per candidate

**Impact:** `handle.rs:6601-6624` clears the queue, calls `world.resident_positions` (collect **all** keys
and `sort_unstable_by_key` — `world.rs:1012-1019`), then runs `interest::contains` for **every resident chunk
against every player centre in that domain**, and pushes `(domain.clone(), pos)`. At 5,000 resident and 50
players that is 250k range tests plus a 5k sort once a second — low single-digit milliseconds. At the
default-view spread population (100k+ resident × 50) it is tens of milliseconds in one tick, which is a
guaranteed missed frame every second. The `domain.clone()` is a heap allocation per queued chunk.

**Fix (M):** keep a per-chunk count of interested players (incremented/decremented from the streamer, which
already computes interest off-tick) and unload at zero; or a per-domain spatial hash by 8/16-chunk cell.
Drop the sort for this caller (it exists for random-tick determinism, not unloading).

**Confidence:** high on shape; magnitude [I].

### S4. Entity replication scans every entity once per player per tick, and rebuilds a BTreeMap each time

**Impact:** `handle.rs:6168-6216` calls `Tracker::update` per player, which iterates **all** entities
(`crates/core/src/ent/replicate.rs:320-381`), does a `domain_of` BTreeMap lookup per entity per player
(`handle.rs:6192`), builds a fresh `BTreeMap` of every visible entity (`replicate.rs:328-379`) — one node
allocate/free per visible entity per viewer per tick — and `Sent::of` clones `entity.hands.clone().clone()`
twice (`replicate.rs:205`, `353`, `392`). At 50 players × 2,000 entities that is 100k visits plus ~100k tree
nodes per tick, roughly 1–3 ms and steady allocator churn, with no mods required.

**Fix (M):** a `(domain, chunk) -> entities` index in `Population` (also fixes S5/S9), walk only the chunks in
the viewer's interest box, update `known` in place with `retain` + insert, and add a `hands_serial` counter
(the struct already uses `look_serial` for exactly this).

### S5. Crowd separation is O(N²) over every mob in every domain — and ignores domains

**Impact:** `handle.rs:3626-3656` collects every non-transient entity into one vector and calls
`phys::separate` (`crates/core/src/phys/crowd.rs:92-132`), an all-pairs loop with no distance bucket. 200
mobs = 20k pairs (~0.1 ms); 500 = 125k (~0.5–1 ms); 2,000 = 2M (~8–15 ms). The engine's purpose is mods that
populate the world, so this is a real ceiling.

It is also a correctness bug: `Occupant` has no domain (`crowd.rs:62-81`) and the call mixes players with
every domain's mobs, so a player in a ship is shoved by overworld bodies at identical coordinates. The
replication path already fixed this class with `shares_space` (`handle.rs:6192`).

**Fix (M):** bucket occupants by chunk (interaction radius is ~1 cell), pair only within 27 neighbouring
buckets, and add a domain id to the key.

### S6. Worldgen throughput is the real 50-player ceiling, and summary generation costs a full chunk

**Impact:** `worker_count` is half the cores clamped to 1–4 (`crates/server/src/worldgen.rs:242-249`) — three
workers on the six-core minimum spec. The World mod's chunk is measured at **50–100 ms** [M], so the pool
sustains roughly **30–60 chunks/second**. A default-view world is 4,925 chunks per player; 50 spread players
is ~246k unique chunks, or **68–137 minutes** of generation. Even a single 2,000-chunk join is 35–70 seconds.

Worse, the horizon asks for ~29k summaries per player, and `World::summary` generates a **full chunk** before
encoding the chain (`crates/server/src/world.rs:1288-1304`); `ChunkSource` has no coarse entry point. On a
fresh server that is a full chunk generation per horizon candidate.

**Fix:** measure `TIAMAT_GEN_THREADS = cores-2` (the override already exists, max 16) rather than the
conservative half; add a coarse `ChunkSource` summary entry point so a horizon summary can be a heightmap
rather than a full chunk; pre-generate around spawn; consider a generation LOD. This is the one item that
cannot be fixed by shaving constants.

**Confidence:** high on the arithmetic, [M] on 50–100 ms; the actual number for a given mod set must be
measured.

### S7. Every broadcast is deep-cloned per receiver and encoded per connection

**Impact:** `Shared::broadcast` sends a `Broadcast { message: ServerMessage }` through a
`tokio::sync::broadcast` channel (`endpoint.rs:3246-3269`). That channel clones the message **per receiver**
(`Broadcast` derives `Clone`; `ServerMessage` owns Vecs/Strings), and each connection task then calls
`frame::write`, which runs `proto::encode` into a fresh Vec and two `write_all`s
(`crates/server/src/transport/frame.rs:98-111`; receive loop `endpoint.rs:4159-4205`). At 50 players, one
entity update per tick is 1,000 deep clones + 1,000 encodes/s; 50 players digging is 2,500 block-delta
messages/tick, each cloned and encoded 50 times.

**Fix (M):** send `Arc<EncodedMessage>` (or `Arc<ServerMessage>`) through the broadcast channel and let each
connection write the shared bytes with a prefix; better, batch all of a tick's block deltas into one message.
Both remove the 50× clone/encode. Protocol change for batching, none for the Arc.

### S8. A chunk requested by N players is cloned and encoded N times, and the serve clock is outside the per-request loop

**Impact:** `handle.rs:912-978` checks the serve budget once per finished job, then loops
`for request in requests { serve_one_chunk(...) }` with no clock check. Every call redoes
`world.chunk` + `chunk.clone()` + `db().chunk_blob` (postcard + zstd) at `handle.rs:1129-1137`, and clones +
RLE-encodes the fluid layer at `handle.rs:1215-1250`. Fifty players spawning together request the same
chunks; each is served 50 times. The encode is measured at ~10 µs [M], but the clone and script tint/fog
lookups are per requester, and the unclocked loop can push one tick past the 25 ms serve budget (SERVE_TIME_BUDGET).

**Fix (M):** cache the encoded `Served` per `(domain, pos)` for the fan-out and send the same bytes to every
requester; move the budget check inside the request loop.

### S9. Hot SQL reads are prepared from scratch, summaries open a transaction each, and generated fluid is written one row at a time

**Impact (tick):**

- `load_chunk_in` (`crates/core/src/persist/mod.rs:541-549`), `load_chunk_fluid_in` (`769-781`) and
  `load_summary` (`2291-2306`) use `query_row`/`prepare` without `prepare_cached`, unlike the write paths
  (`652`, `929-939`). 2–3 statements per streamed chunk / summary at 5–20 µs each is ~0.2–0.5 ms/tick.
- `save_summaries` (`persist/mod.rs:2318-2361`) opens its own transaction, runs `SELECT COUNT(*)` twice, and
  prepares an uncached upsert **per summary position**. The horizon budget admits 5 summaries/tick; a burst
  is 1–3 ms.
- `World::adopt_into` writes a generated wet chunk's fluid immediately with one autocommit per statement
  (`world.rs:1197-1203`), unbudgeted and unbatched, while the periodic `save_fluid` path exists to batch
  exactly this.

**Fix (S/M):** `prepare_cached` on the four hot reads; batch a tick's summary chains in one transaction;
push generated fluid onto the dirty-fluid queue and let the existing debounce write it.

### S10. The light thread holds the fluid read lock across a whole relight; the tick holds the fluid write lock across SQLite reads

**Impact:** `lightwork.rs:198-229` keeps `ponds` (read) and `lit` (write) alive through
`chunk_loaded_in`/`edited_in` — 0.21–0.49 ms per chunk [M]. Every tick-side `fluidics.write()` waits behind
it: serve (`handle.rs:1178`, `1217`), edits (`3537`, `3565`), dig (`4536`), place (`5146`), soak/wash
(`6421`, `6454`) and the solver (`6270`). The serve path already instruments the wait as `report.locks`.

In the other direction, `load_domain_fluid` takes `fluidics.write()` and then loops `world.load_fluid` (a
SQLite read) inside the guard (`handle.rs:293-316`); `serve_one_chunk` does the same for one chunk
(`1176-1194`). At 22 arrivals/tick that is several milliseconds of write-lock hold, during which the light
thread's read guard blocks.

**Fix (S):** copy the glow/emission inputs out of the fluid store, drop both guards, relight, then re-take the
lighting write lock to store. On the tick, read the layers first and take the fluid lock only for the inserts.

### S11. Random tick sorts all resident positions every tick — and always samples the same 512

**Impact:** `handle.rs:5364-5389` calls `world.resident_positions` (all keys + full sort) and then breaks
after `MAX_RANDOM_TICK_CHUNKS = 512`. That is 0.3–0.6 ms/tick at 5.8k chunks, 1.5–2.5 ms at 20k, for 512
cells. The docs' "8 µs random tick" measures only `random::cells`, not this pass.

**Bug folded in:** because the positions are sorted and the loop always starts at the front, a world with
more than 512 resident chunks gives **every random tick to the same low-coordinate 512** and none to the
rest, the opposite of the comment at `handle.rs:1375-1382`. Inert today — no mod in `game/` registers a
random tick — but one farming mod turns it on.

**Fix (S):** keep a sorted `Vec<ChunkPos>`/`BTreeSet` alongside the cache and sample 512 from a rotating
offset derived from the tick; no per-tick sort, every chunk gets a turn.

### S12. Smaller tick costs worth folding into the same pass

- `Space::dirty` is a `Vec` with `contains` on every applied edit (`world.rs:1538`; also `1185`, `1923`).
  A 256-block plan stamp against a 2k-entry backlog is ~500k comparisons. Use a dirty flag on the cached
  chunk or a `BTreeSet`.
- `Population::owned_by_mod` builds a `BTreeMap<String, Vec<u64>>` and clones `entity.source` for every
  entity **every tick** (`crates/server/src/ent.rs:564-570`, called at `handle.rs:6089`), even when no mod
  registered an entity step.
- `Population::freeze` scans every entity per unloaded chunk (`ent.rs:457-505`); 32 unloads × N.
- `Population::tick` does two full `ids()` passes per domain and allocates a Vec each (`ent.rs:636-698`).
- The fluid solver's `record_blocked` probes five neighbours per visited block whether or not any mod
  registered `on_fluid_flow` (`crates/core/src/fluid/solver.rs:785-787`, `1122-1165`), and the server then
  takes `fluidics.read()` twice per event and linear-scans `name_of` (`handle.rs:6482-6531`).
- `Wet::occupancy`/`absorbency`/`washes_away` do an uncached chunk `HashMap` probe per call
  (`crates/server/src/fluid.rs:907-960`); `Lit` solved the same problem with a one-entry memo and measured a
  28% win (`crates/server/src/light.rs:655-709`).

Together these are a second tier of 0.1–1 ms items, not a rewrite.

---

## 2. Networking, LAN joins, and cross-server caches

I read "cross-LAN cache clearing" as the LAN discovery/join/asset-cache path plus the chunk cache lifecycle.
The content cache is deliberately content-addressed and shared across servers, so **not clearing it between
worlds is correct** — the same bytes are reused. The costs are re-verifying, re-decoding, re-compressing and
re-sending data that is already on disk.

### N1. Clicking a LAN world blocks the window thread for up to 10 seconds

**Impact:** `Connection::open` calls `runtime.block_on(connect(address, pinning))` (`crates/client/src/net.rs:838`)
with `CONNECT_TIMEOUT = 10 s` (`net.rs:69`, `963`), and it is called from the winit thread at
`crates/client/src/main.rs:1555`. A stale discovery entry (`STALE_MS = 3.5 s`, `crates/core/src/discover.rs:43`)
stays clickable after the host has gone; clicking it freezes the UI for the full timeout with no cancel.

**Fix (S):** create the runtime and start the connect, return immediately, and deliver `Connected`/`Failed`
as events the front screen can draw; add a Cancel button. This is also the right shape for reconnect.

### N2. LAN discovery has a 1 s beacon and no probe

**Impact:** `INTERVAL_MS = 1000` (`discover.rs:37`); the host only sends (`crates/server/src/announce.rs:96-144`),
the client only listens. A world opened to the LAN can take up to a second to appear, and there is no
"is this still there?" round trip. The beacon is tiny; the interval is the latency.

**Fix (S):** 250 ms beacons, or a unicast probe the announcer answers immediately. Cache `worlds()` in the
front screen and refresh at ~4 Hz instead of cloning/sorting the found list every frame
(`crates/client/src/front.rs:842-846`).

### N3. Every cached asset is read from disk and BLAKE3-hashed two or three times per join

**Impact:** the material-table path already reads every texture once into `pending.complete`
(`crates/client/src/net.rs:1355-1359`), then calls `cache.missing(&wanted)` (`net.rs:1360`), and
`missing -> contains -> get` reads and re-hashes each one again (`crates/client/src/cache.rs:111-194`). The
same double pattern appears at `net.rs:1304`, `1754`, `1826`, `1863`, `1914`, `1933`, `2262`, and every
`offer_*` helper calls `cache.get` again. A 128 MB mod pack means 256–384 MB of reads/hashes before the join
can proceed. This is the "cache clearing" path people actually feel on a repeat LAN join.

**Fix (S):** a per-session verified-hit memo in `ContentCache` (`HashMap<ContentHash, Arc<[u8]>>`), consulted
by `contains`/`missing`, and pass the bytes the table loop already read into the `offer_*` helpers. The
on-disk re-hash on first load can stay — it is the security gate; doing it three times is not.

### N4. Nothing decoded survives a reconnect

**Impact:** `emit_materials` decodes every material texture serially on the net task
(`crates/client/src/net.rs:2356-2382`); sounds, fonts, models, pictures and skins are decoded per join
(`net.rs:2180`, `2214`, `2221`, `2282`, `2342`). A new `App` is built per join (`main.rs:1214`), so every
decoded map is dropped when leaving a world. On a large modpack this is a repeated 0.5–2 s CPU cost per
join, and it gates `JoinWorld`.

**Fix (M):** a decoded-asset cache (hash → decoded image/font/clip) owned by `Client` and handed to each
`App`; at minimum decode material textures on `spawn_blocking` in parallel rather than serially.

### N5. Content slices are re-compressed for every client, two per pass

**Impact:** `content.rs:166` runs `zstd::encode_all(slice, 3)` per client per pass; there is no shared
compressed cache. `SLICES_PER_PASS = 2` (`content.rs:63`) and only two slices go out per connection per
50 ms beat (`endpoint.rs:3974-3983`). A 20 MB pack needs ≥2.6 s of pure transfer before the client can
finish joining, and 50 simultaneous joiners re-compress the same slices 50 times.

**Fix (S/M):** cache compressed `(hash, slice index) -> Arc<[u8]>` in the immutable `ContentIndex`; raise the
per-pass budget when the queue is deep (the transfer is still quota-bounded).

### N6. One Vec allocation and two `write_all`s per message

**Impact:** `frame::write` serialises into a fresh Vec and writes prefix and body separately
(`transport/frame.rs:98-111`). At 50 players that is tens of thousands of small allocations and writes per
second. Each beat also re-locks ~12 per-player queues and clones the domain `String` (`endpoint.rs:2470-2475`,
`3982-4124`).

**Fix (M):** a reusable per-connection write buffer appended per beat and flushed once; one per-player
outbox lock drained once; return `&str`/a domain id rather than `String`.

### N7. The content cache never evicts

Not a speed bug, but the flip side of "cache clearing": `ContentCache` has no size or age bound
(`crates/client/src/cache.rs`), so a player who visits many servers accumulates every asset they ever
received for the life of the installation. Worth a cap/LRU at some point.

---

## 3. Client rendering and frame management

Frame budget: 16.7 ms at 60 fps. The per-frame path is `main.rs::frame` (~1618), `App::advance` (~5479),
`Renderer::render` (~3749), then `draw_hud` (~2697).

### C1. Every shadow cascade draws the full union of casters

**Impact:** the biggest single GPU item found. `cull_and_upload` computes one `casts` bool = "any cascade
contains it" (`render/mod.rs:3110-3113`) and pushes one union list (`3139-3141`). `fill_cascades` then loops
that whole list **inside** `shadows.render`'s per-cascade closure (`3665-3673`), so all three cascades are
drawn for every caster. Cascade splits are 0.08/0.28/1.0 × 160 blocks
(`render/shadow.rs:41,55,62`); cascade 0 covers ~12.8 blocks, while the union is dominated by cascade 2's
~160+64-block box. That is roughly **2.7–3× the shadow draw count and shadow vertex work**, on top of the
normal world pass. Inferred 1.5–5 ms GPU in Beautiful at 1080p, and ~3k extra draw encodings/frame.

**Fix (S):** build `casters: [Vec<(ChunkPos, u32)>; CASCADES]` in `cull_and_upload` (one extra
`contains_chunk` per cascade) and have `fill_cascades` iterate `casters[cascade]`; or reject in the closure
with that cascade's frustum. Keep the existing margin for tall geometry outside the split.

### C2. Alpha-tested foliage is fully shaded before the discard

**Impact:** `world.wgsl:1717-1729` calls `surface(...)` — atlas sample with gradients, tint, propagated
light, place fog, sky fog and nine PCF taps — and only then tests `colour.a < 0.5` and discards. Every
discarded foliage fragment, and every occluded one, pays the full shading cost, and `discard` already
forfeits early-Z for the whole draw. Inferred 1–4 ms GPU in a forest/canopy view.

**Fix (S):** sample the atlas once up front, discard on the texel alpha, then shade the surviving fragments
(reusing the sample). Same pixels, a fraction of the work.

### C3. The mesher runs up to seven full-grid classification scans with real material sets

**Impact:** see §0.2. `mesher.rs:471-478` calls `glass_columns`, `cutout_columns`, `billboards_of`,
`sprite_columns_of`, `model_columns_of`, `model_instances_of`, `flicker_sources_of` one after another. Each is
a full 48³ walk; the gates (`any_cutout`, `any_billboard`, …) are **global**, not per chunk, so a chunk with
no leaves still pays the foliage scan. With the default mods glass/foliage/sprites/models/flickers are all
populated, so 5–7 scans fire. Each non-air cell does a `BTreeSet<u16>` lookup in `Sight`
(`mesher.rs:1026-1069`). Order **0.3–1 ms/chunk conservatively; 1–3 ms if all seven scans fire**, against a
measured 0.75 ms bench that uses none of them. On four workers this is throughput, not frame time; in
singleplayer it competes with the embedded server and the frame thread.

**Fix (M):** one dense material-kind mask (`Vec<u8>` indexed by material id, built once per material table)
and **one** walk over `materials` that fills every column set; ideally fold the kind bits into
`fill_terrain` so there is no extra pass. Add a benchmark with a real `Sight`.

### C4. Corner light and occlusion are recomputed ~3.84× per lattice point

**Impact:** `shade_at` is called once per faced cell (`mesher.rs:2583-2594`); each call does four corners,
each with four bilinear `light.at()` samples and three occupancy probes (`shade.rs:192-212`, `233-319`). A
48×48 slice has 49×49 = 2,401 distinct corners but performs 9,216 corner evaluations. Shading is 0.74 ms of
the measured 1.07 ms uniform chunk [M]; a per-slice corner cache should save **~0.3–0.5 ms/chunk**, roughly
halving the lit path.

**Fix (M):** keep a `[(N+1)*(N+1)]` corner cache per `(axis, positive, w)` slice, zeroed once per face
direction, and assemble each cell's `Shade` from four cached corners before `greedy_merge`.

### C5. Quad → vertex expansion is still on the frame thread, and the remesh budget does not bound uploads

**Impact:** `Renderer::set_chunk` calls `mesh.to_buffers()`, `glass_buffers()` and `cutout_buffers()`
(`render/mod.rs:2459-2461`), which expand every quad into four `PackedVertex` on the frame thread
(`mesher.rs:1568-1610`) — while fluid already arrives pre-expanded from the worker. Inferred 0.06 ms for 4k
vertices, 0.3–0.5 ms for a dense/chiselled chunk. `App::remesh` drains **every** finished job and uploads it
before considering `REMESH_TIME_BUDGET` (`app.rs:5158-5169`); a burst of 9–16 finished workers can therefore
blow the frame even though the 2 ms budget was respected on submission.

**Fix (S/M):** expand to `Vec<PackedVertex>` in `MeshJob::finish` on the worker and have `set_chunk` consume
them; cap the drain/upload loop by elapsed time as well as submissions. This also makes the "upload is a
tenth of a millisecond" claim in `meshwork.rs:16-17` true.

### C6. `take_dirty` sorts the entire dirty set every frame

**Impact:** `ChunkStore::take_dirty` (`world.rs:976-1025`) drains the dirty `BTreeSet` into a Vec, sorts
**all** of it by distance with a coordinate tie-break, `split_off(budget)`, then reinserts the overflow. During
a fill/backlog this is thousands of elements every frame, 0.2–1 ms, and it is charged inside `remesh` against
the 2 ms budget — the budget goes on ordering rather than meshing. It also runs after a mode/atlas change that
marks every chunk dirty.

**Fix (S):** `select_nth_unstable_by_key` for the first `budget` (O(n) average), sort only those, leave the
tail in the set; or bucket by distance ring. Keep the deterministic tie-break for the selected set.

### C7. Blob shadows probe the ground ~325 times per body, every frame, for bodies off screen

**Impact:** `App::place_blobs` (`app.rs:6209-6319`) builds a feet list for the player **and every entity**,
then for each body runs `blob_columns`, a 5×5 grid of `ground_below` probes, each walking up to 13 cells
(`app.rs:6276-6318`, `render/mod.rs:4372-4417`). That is ~1 + 25×13 = 326 `Voxels::material` calls per body
per frame. A 30-mob camp is ~10k lookups (~0.4–1.5 ms). Nothing is frustum-culled first.

**Fix (M):** build blobs only for bodies the renderer will draw or that cast (reuse the `Culled::shows_figure`
decision), cap to the nearest ~32, and/or probe one column and smooth.

### C8. Figures and model-block lights are built for everything off screen

**Impact:** `place_entities` (`app.rs:6648-6720`) computes `light_on_body` for every entity, and
`place_model_blocks` (`app.rs:6750-6812`) does seven light lookups per model block plus a `String` clone of
the model id and a fresh `Look` key — all before `prepare_figures` frustum-culls them
(`render/mod.rs:2266-2327`). Thirty mobs + 200 braziers is ~1,500 light lookups and hundreds of allocations
per frame that are thrown away. `prepare_figures` then builds a fresh `BTreeMap<Look, Vec<Figure>>` and
clones `Look` strings every frame.

**Fix (M):** do the cheap distance/frustum decision before building; intern `Look` to a `u32`; keep the
per-frame map as a reused field.

### C9. Culling: no distance early-out before the cascade tests, no spatial index, no occlusion

**Impact:** `cull_and_upload` (`render/mod.rs:3086-3141`) iterates every meshed chunk (~1–2.5k at default:
detail surface + built summaries), computes the camera AABB test **and** three light-frustum tests
unconditionally, then does a BTreeMap probe in each draw pass. Inferred 0.2–0.6 ms CPU/frame. There is still
**no occlusion culling** (P3): a cave system under a hill is as many draws as the hill. `horizon_is_buried`
covers only summary chunks.

**Fix:** distance² reject before the cascade loop (chunks past the shadow range cannot cast); keep a
per-frame candidate list rebuilt only when the camera chunk or mesh set changes; then P3 for the structural
GPU win. The cheap, high-value version of P3 is a server-supplied column cover map / per-chunk solidity so a
camera above ground can drop fully buried detail chunks.

### C10. Visible chunks are drawn in BTreeMap order, not front-to-back

**Impact:** `cull_and_upload` iterates `&self.chunks` (`BTreeMap<ChunkPos, ChunkMesh>`) and the world pass
draws `culled.visible` in that order (`render/mod.rs:3107`, `3853-3862`). Opaque overdraw is therefore
slab-ordered rather than near-to-far. In occluded views, inferred 0.3–1.5 ms GPU.

**Fix (S):** sort `culled.visible` and the cutout/sprite lists by squared camera distance before drawing
(~20 µs for 1k items).

### C11. The HUD clones shared structures and builds its state every frame — even when nothing is shown

**Impact:**

- `main.rs:2722` clones `TileMap`, `main.rs:2726` `items`, `main.rs:2727` `material_names` (a
  `BTreeMap<u16, String>`, one allocation per material), `main.rs:2777` `cards` — **every frame**, even with
  the menu closed, no dialogs and the HUD off. ~0.05–0.2 ms/frame plus allocator pressure.
- `App::run_hud_scripts` builds the full `hud_state` before checking whether the VM has any scripts
  (`app.rs:7157-7168`): it clones carried stack names/details/cells, the whole `hud_values` map, constructs
  `phys::Voxels` and does a voxel raycast (`app.rs:7182-7260`). `draw_hud_scripts` calls it **before** the
  `hud_visible` early return (`main.rs:1921-1928`). ~0.05–0.2 ms/frame with zero scripts.
- `draw_hud` builds `Icons` twice and runs `model_icons` every frame even with no dialogs.
- While the debug overlay is on, `vram_line` scans every chunk's buffer and formats a 12-argument string
  every frame — contaminating the `hud` phase of the very frames being measured. Throttle it to ~4 Hz.

**Fix (S):** `Arc` the material/tile/card maps and clone the handle; early-out the HUD pass when there is
nothing to draw; add `HudVm::is_empty()` and check it before building state; build `Icons` once; throttle
the overlay text.

### C12. Per-frame allocations and the post chain

- `Culled::default()` plus `Vec::with_capacity(self.chunks.len())` allocates a fresh ~200 KB instance Vec
  every frame, and five `Culled` lists re-grow from empty (`render/mod.rs:3105-3136`). Reuse them as
  `Renderer` fields and `clear()`.
- `Post::step` creates a new bind group for each of five post passes every frame (`render/graph.rs:897-939`);
  the views only change on resize, so cache them.
- The particle pass allocates a `Vec<&Sprite>`, sorts the whole live set and builds a `Vec<Instance>` every
  frame (`app.rs:6158-6182`, `render/particle.rs:262-299`); at full precipitation that is ~6k elements and
  ~220 KB plus a sort. Bucket by texture in one pass and reuse a persistent buffer.
- `place_blobs`, `set_blobs`, `set_props`, `set_particles` and `set_lightning` all allocate a fresh Vec per
  call. Small individually; together they are steady allocator churn.
- `build_horizon` calls `fluid_material_ids()` **inside** its per-summary loop (`app.rs:5378`); hoist it.

### C13. Beautiful is expensive by construction; the defaults are the lever

Three 2048² cascades (48 MiB), every caster drawn three times (before C1), a nine-tap PCF, an HDR target
plus fog copy, half-res bloom, a composite, and a cloud deck measured at 1.2–4.0 ms [M]. That is the mode
where the reporter's card is pegged. The options are the shadow quality/size (Medium is already the default,
not High), two cascades instead of three, and the cloud quality. These are designer calls, not engine bugs;
the engine-side wins are C1, C2 and C9.

Longer term, one draw call per chunk per pass does not scale to a 32-chunk horizon. Region/merged meshes or
multi-draw-indirect over a shared vertex arena would cut draw-call overhead, but that is a renderer project,
not a shave.

---

## 4. Client meshing, assets and startup

### A1. Atlas packing happens on the frame thread after the join

`adopt_atlas` (`app.rs:3897-3944`) builds the atlas from every decoded image (`build_atlas`,
`crates/client/src/app.rs:7364-7397`), clones every `Image` into the slot table, resamples and packs, then
uploads. It runs inside `pump_network` on the frame thread. For a few hundred textures this is an inferred
10–50 ms one-frame hitch at exactly the moment the player joins. Decoding (`emit_materials`) is serial on the
net task as well. **Fix (M):** build the CPU atlas on a worker, upload on the frame; parallelise decodes.

### A2. Summary mesh allocations

`mesh_summary` allocates its `solid` and `wet` planes inside the per-axis/per-slice loop
(`mesher.rs:5852-5854`), 6 × width × 2 allocations per summary. At 16 summaries/frame this is 48–192
allocations; hoist the planes out of the loop.

### A3. Mesh worker panic isolation is missing

`meshwork.rs:114-131` calls `work(job)` without `catch_unwind`. A panic unwinds that worker (the jobs Mutex
is not held at the time, so it is not poisoned — an earlier report said it was), `backlog` never decrements
for the lost job, and after enough panics submissions stop at `capacity`. The world stops filling in
silently. **Fix (S):** `catch_unwind` per job, return `Mesh::default()`, log the position.

### A4. `ChunkStore::clear` leaves stale fluid behind

`world.rs:800-807` clears chunks/dirty/urgent/light/summaries/stale but **not** `fluid`, `generation` or
`tints`. It is called on a domain switch (`app.rs:4212`), so the old space's `Arc<FluidLayer>`s survive at
the same coordinates: `has_fluid()` stays true (every chunk pays the fluid mesh path), `fluid_at` answers
another space's milk, and ghost ponds can draw. This is a domain-separation bug of the same class the
server documents at `handle.rs:5732-5743`. **Fix (S):** clear `fluid` (and `fluid_materials`/`generation`
for completeness) there.

### A5. `generation` and `biomes.corners` grow without bound

`ChunkStore::generation` entries are inserted on every mark and never removed by `remove`/`clear`
(`world.rs:934`); `Renderer.biomes.corners` adds one entry per column ever visited and is only cleared by
`Renderer::clear` (`render/mod.rs:3148-3152`, `2642`). A long walk leaves tens of thousands of dead map
entries (a few MB) and makes every frame's corner lookup walk a bigger tree. **Fix (S):** prune on
remove/unload, or bound the caches per frame.

---

## 5. Bugs and broken things

Ordered by current impact, not by how interesting they are.

| # | Bug | Evidence | Impact |
|---|-----|----------|--------|
| B1 | **Unbounded zstd expansion of server-pushed content.** `accept_slice` runs `zstd::decode_all(data)` with no output limit; the only check is `total_len` before the decode. | `crates/client/src/net.rs:2008-2019` | A hostile/broken server can send a small frame that expands to GBs. Charter rule 14 violation. Use `zstd::bulk::decompress(data, remaining)` / a bounded `Decoder`. |
| B2 | **Content for hashes the client never requested is buffered and written to disk.** `pending.partial` has no entry cap and no requested-set check; `cache.put` then persists it. | `net.rs:2021-2043`, `cache.rs:148-177` | Memory/disk DoS from a hostile server. Cap to requested hashes and a per-join byte budget. |
| B3 | **Random tick always samples the same first 512 chunks.** `resident_positions` is sorted; the loop takes the front and breaks. | `handle.rs:5364-5389`; comment at `1375-1382` says the opposite | Latent (no shipping mod registers random tick), but any farming mod silently never ticks most of a >512-chunk world. Rotate the offset. |
| B4 | **Crowd separation ignores domains.** `Occupant` has no domain; one `separate` call mixes players with every domain's mobs. | `crowd.rs:62-81`, `handle.rs:3626-3656` | A player in a ship is shoved by overworld mobs at identical coordinates. Add domain to the key. |
| B5 | **`ChunkStore::clear` leaves stale fluid** (and generation/tints). | `world.rs:800-807`, called at `app.rs:4212` | Ghost ponds / wrong swimming / fluid mesh cost after a domain switch. See A4. |
| B6 | **Serve budget is checked per chunk job, not per request.** | `handle.rs:912-923` vs `944-978` | 50 parked requests for one chunk are served in one unclocked loop; can overshoot the 25 ms serve budget. Move the check inside. |
| B7 | **A mesh-worker panic kills a worker and leaks a backlog slot** (no `catch_unwind`). | `meshwork.rs:114-131` | After enough panics, meshing stops. Fix A3. |
| B8 | **A worldgen `encode_chain` panic escapes the per-job `catch_unwind`**, killing a worker while its key stays in `in_flight` forever. | `worldgen.rs:690-725` | That chunk is `is_generating` for ever; one worker lost. Wrap the whole job body. |
| B9 | **`Transfers::seen` never forgets a hash.** A client whose `accept_slice` failed re-requests; the server ignores it for the session. | `content.rs:119-127` | Join stalls on content that will never be resent. Remove the hash from `seen` when delivery fails, or reset the transfer on reconnect. |
| B10 | **Client event channel is unbounded.** | `net.rs:832` (`mpsc::unbounded_channel`), drained in `pump_network` | A server that floods chat/warnings/chunks, or a frame blocked on the GPU, grows the queue without limit. Use a bounded channel with backpressure/drop policy. |
| B11 | **Random-tick material set only grows** when a mod is disabled at runtime. | `handle.rs:3239` | A disabled mod's materials keep being sampled and `random_ticked` called. |
| B12 | **`MeshInput::neighbours` silently substitutes the centre chunk** if the slot arithmetic drifts (`unwrap_or(13)`). | `world.rs:1158` | Wrong mesh with no symptom. `debug_assert` instead. |
| B13 | **`resolve_nametag` can block the tick** on `online.lock()`, although its own comment says a tick must never wait. | `handle.rs:506-517` | A slow roster lock stalls the tick. Use `try_lock` and skip the label. |
| B14 | **Foliage casts no shadow in Beautiful.** `fill_cascades` draws only `mesh.vertices`; leaf quads live in `mesh.cutout`. | `render/mod.rs:3667-3672` | A canopy shadows only through its trunk. Visual, not perf; confirm intent. |
| B15 | **`Discovery::forget_stale` has no caller.** | `client/src/discovery.rs:164-172` | Dead code; the map is capped at 50, so the leak is bounded. |
| B16 | **The content cache never evicts.** | `cache.rs` | Disk grows for the life of the install. |
| B17 | **The bot harness clones every received message into an unbounded history.** | `bot/src/client.rs:300-313`, `355-360` | The 50-bot child process grows to GBs and can contend with the server process on a runner, distorting soaks. This is a measurement caveat, not a game bug. |

---

## 6. What is already right — do not redo

These were checked and are genuinely done:

- Relight off the tick, with a per-tick event batch and the same passes in the same order.
- WAL checkpoint off the tick, on its own connection.
- Meshing on a worker pool with per-chunk generation staleness; edits still mesh on the frame so no holes.
- Interest/horizon separation, fog-reach hold-back, buried-summary culling, the 500 µs horizon budget.
- One index buffer for every mesh, 1.5× pool classes, buffer pooling with a 64 MB cap, one shared instance
  array per frame, figures frustum-culled before posing.
- Content addressing and hash verification on the way in and out; pure-Rust decoders with pre-decode caps;
  panic-isolated sound/font decodes.
- Batched chunk/fluid/entity saves; capped tick queues; serve/save/light/unload budgets; fixed-rate tick and
  the catch-up cap.
- The client acquires the swapchain image before doing the frame's work (the `211 fps · 103 presented` fix),
  which is a real and non-obvious win.
- Per-column corner-tint caching; glass/cutout/fluid pipeline binds gated on "any in view"; shadow casters
  kept separate from the visible list.

---

## 7. Suggested order of work

**Phase 0 — measure the target, not a proxy (1–2 days)**
1. Add a 10-bot (then 50-bot) soak at `ViewDistance::DEFAULT` with the real World mod; report the same tick
   table. This is the number that matters.
2. Add a mesher benchmark with a real `Sight` (glass + foliage + billboards + models + flickers), and a
   `set_chunk`/upload benchmark; publish both beside the `NoGlass` one.
3. Instrument the streamer pass, unload sweep, light snapshot and replication as named pieces in the
   existing `Phases` ledger, so the default-view soak says which one dominates.
4. Trace a LAN join end to end: discovery → connect → content → JoinWorld → first chunk → horizon. The
   current cost is spread across four systems and nobody has a timeline for it.

**Phase 1 — cheap, low-risk wins (days)**
- S1 interest order cache + idle skip.
- C1 per-cascade caster lists; C2 foliage pre-discard.
- C6 `take_dirty` partial selection.
- C11 HUD gates/Arc handles/overlay throttle.
- N1 non-blocking connect; N2 250 ms beacon; N3 cache memo.
- S9 `prepare_cached` on hot reads.
- S10 drop fluid/light guards before the expensive section.

**Phase 2 — medium changes with the best measured payoff (1–2 weeks)**
- S2 touched-only light snapshot + immutable read path.
- S3 unload spatial index; S4 entity index + in-place tracker; S5 crowd buckets + domain.
- S7/S8 encoded broadcast/chunk bytes + block-delta batching.
- C3 single-pass material classification; C4 corner cache; C5 worker-side vertex expansion.
- C7/C8 pre-cull blob/figure work.
- A1 atlas build off the frame thread; N4 decoded-asset cache.
- N5 compressed slice cache.

**Phase 3 — structural**
- S6 worldgen throughput / coarse summary API / pre-generation (the real 50-player ceiling).
- C9/P3 occlusion culling; C10 draw order; long-term draw-call batching.
- B1/B2/B10 hostile-input bounds (these should not wait for a performance phase).

**Do not** raise `CHUNKS_PER_TICK` on the strength of the current number without re-measuring: its comment
still justifies 22 with a 1.44 ms per-chunk lighting cost that P1 moved off the tick
(`endpoint.rs:1307-1340`). With lighting off the tick the count is probably too low for 50 joining players,
but the single light thread (about 140 chunks/tick of headroom) and the 25 ms serve budget become the binding
constraints, so the two constants have to move together.

---

## 8. Caveats

- No profiler or new benchmark was run; costs marked [I] are arithmetic over code, not measurements. The
  [M] numbers are the repository's own and are cited.
- Per-frame costs vary with scene, resolution, GPU and mod set. The client estimates are for the default
  view and a populated world; the server estimates are shares of the 50 ms tick at 50 players unless stated.
- The order-of-work list is about payoff per unit of risk, not a schedule. Several items touch protocol or
  determinism boundaries (light ordering, broadcast batching) and need their own tests.
- Nothing in the repository was modified to produce this report; it is a standalone audit file.
