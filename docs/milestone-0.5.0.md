<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Milestone 0.5.0 — the finished engine

Decided by the designer on 2026-10-13, after 0.4.2 shipped. This is the plan
of record: what 0.5.0 is, what proves it, in what order the work goes, and
what is deliberately left out. Each item names who does it.

## What 0.5.0 is

The charter says the engine is mechanisms only and the real default game is a
later phase, built as mods on a finished engine. 0.5.0 is the point where the
engine stops moving under the mods: every filed ask is answered (true as of
0.4.2), the performance targets in `docs/performance-targets.md` are met and
measured rather than hoped for, a release is something a stranger can
install without a warning, CI is believed, and the mod API is a frozen 1.0
with a deprecation rule. It is defined by the exit criteria below, not by a
feature list.

## Exit criteria

Every one is checked before the tag, and the release notes say which held.

| # | criterion | proof |
|---|---|---|
| E1 | The swarm load with 50 moving players runs under the 50 ms tick on the reference machine, no tick dropped over a five-minute soak | `swarm_load` with movement, run in the nightly CI soak |
| E2 | A flying session at full detail on the designer's card shows no frame over 33 ms once the world is streamed, and the `VRAM` line stays under the bound in `docs/performance-targets.md` | overlay readings, recorded on the sheet |
| E3 | CI is green on all three platforms for ten consecutive runs of main without a re-run | the Actions history |
| E4 | A fresh Windows machine and a fresh Mac run the downloaded archive with no SmartScreen warning and no quarantine step | installed from the public release by a human |
| E5 | An installed 0.4.x copy updates itself to 0.5.0 and keeps its settings | the designer's machines |
| E6 | Every "awaiting the eye" gate on every engine-ask sheet has been looked at, and either passed or re-filed as a bug | the sheets, with a date beside each |
| E7 | Every LAN pairing (Windows, macOS, Linux, both directions) joins with default names and plays five minutes | the designer's machines |
| E8 | The `api/` stubs and `AGENTS.md` carry a version, a deprecation rule, and a reference section; every default mod's own CI runs `--check-mods` and the bot fixtures against the engine's main | the mod repositories' CI |
| E9 | No invisible wall reported from a `client.log` that the logs cannot explain | the user's reports |

## The work, in order

Sizes are a working estimate for one engineer; "engine" is this repository,
"designer" is a purchase or a judgement only the designer can make, "mods"
is the mod agents in their own repositories.

### 1. Trust — first, because everything after it is measured by CI

| item | who | size |
|---|---|---|
| T1 The badge queue is published once per tick, so a network pass never reads it mid-tick (`show_over` has flaked four times on macOS; the race is real) — **landed 2a7c1db7** | engine | small |
| T2 `placement::chiselled_spares` is understood and fixed, not retried (flaked on Windows twice) — not reproduced on one starved core (10 of 10 passed); the stall now reports every notice and the last messages, so the next CI failure names its cause | engine | small |
| T3 A nightly CI workflow runs the fen soak and the swarm load in release on a real runner and keeps the tick tables as artefacts | engine | small |
| T4 The plan-stamp stall: a plan holding a partial cell of a `whole` material must not wait for ever (`plans.rs`, found landing W35) — **landed fa5ddd51** | engine | small |
| T5 The two unsigned mod-side commits on main are the designer's call: rewrite with sign-off or document the exception once | designer | tiny |
| T6 Invisible walls: act on the first `client.log` that shows one; the reload, the held edits and both logs landed in 0.4.1 | engine, as reports come | unknown |

### 2. Signed releases — in scope by the designer's decision

| item | who | size |
|---|---|---|
| S1 Windows: a code-signing identity. Azure Trusted Signing is the recommended route (a monthly subscription, no hardware token, works with `signtool` in CI); an OV certificate from a CA is the alternative | designer | a purchase and an identity check, days of lead time |
| S2 macOS: an Apple Developer Program membership and a Developer ID Application certificate, exported for CI; notarisation through `notarytool` | designer | a purchase, a day |
| S3 The release workflow signs the Windows executables and the macOS bundle and notarises the bundle, every step gated on the secrets existing so an unsigned build still works; `check-archive.sh` verifies the signatures — **landed 2026-10-13** (`docs/distribution.md` §8a names the secrets); inert until S1 and S2 exist, and its first run on a real certificate is the proof | engine | medium |
| S4 The launcher and the updater verify that a staged update is signed by the same identity before applying it, on top of the manifest's Ed25519 signature | engine | medium |
| S5 Release notes drop the "unsigned binaries" known limit | engine | tiny |

### 3. Performance — the three changes that carry the milestone

| item | who | size |
|---|---|---|
| P1 Relight off the tick: a served chunk is lit on the worker that generated or loaded it, and the tick merges the layer; the tick keeps only light edits. Measured today at nine tenths of a sprinting player's tick | engine | large |
| P2 Meshing off the frame thread: a mesh job takes a snapshot of the chunk, its six neighbours and its light, runs on a worker, and the frame thread only uploads. The streaming bottleneck and the flying frame pacing | engine | large |
| P3 Chunk occlusion: a chunk enclosed by its neighbours' solid faces, or under the column cover, is not drawn for a camera above ground | engine | medium |
| P4 The World mod's chunk at 50–100 ms: fewer octaves, smooth detail where sampled is not seen | mods (World) | medium |
| P5 Beautiful's defaults, if E2 needs them: 1024² cascades, two instead of three | engine, designer's judgement on the look | small |

P1 first: it bounds how many players a server can ever hold, and E1 cannot
be met without it. P2 second: it is what E2 measures. P3 if E2 still fails
on hills and caves.

### 4. The mod API as a 1.0

| item | who | size |
|---|---|---|
| A1 `api/` carries a version and a deprecation rule: a function is marked deprecated for one minor release before it goes, and the stub says so | engine | small |
| A2 `AGENTS.md` gains a reference section, by area, beside the dated "what's new" list; the stubs stay the source of truth | engine | medium |
| A3 A conformance run the mod repositories call from their CI: `tiamat-server --check-mods`, the fixture bot joining with the mod enabled | engine (the script), mods (the workflows) | small |
| A4 Vendored stubs refreshed in every mod at the frozen version | mods | tiny |

### 5. The playtest pass

| item | who | size |
|---|---|---|
| V1 One structured pass over every "awaiting the eye" gate, on all three machines, with the sheets marked | designer | days |
| V2 Every LAN pairing, both directions, default names | designer | half a day |
| V3 The update chain from an installed 0.4.x | designer | an hour |

## Out of scope

Content, progression and game feel (the mods' job, by charter rule 1).
Dynamic lights in Beautiful (assessed in `docs/performance-state-2026-10-12.md`,
affordable, a nicety). Far-view LOD beyond the hold past the fog that landed
in 0.4.1, unless V1 says the far view is what hurts. A protocol
compatibility window (the exact-version handshake stays).

## Order of work

1. T1, T2, T4 (days), then T3.
2. S3 and S4 built while S1 and S2 are being procured, so the first signed
   build happens the day the secrets exist.
3. P1, then P2, then P3 as E2 dictates.
4. A1–A3 once P1–P2 have settled the API they touch.
5. V1–V3, then the tag.

This file is updated as items land, with the commit beside each.
