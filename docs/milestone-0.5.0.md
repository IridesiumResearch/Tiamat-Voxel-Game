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
| T3 A nightly CI workflow runs the fen soak and the swarm load in release on a real runner and keeps the tick tables as artefacts — **landed 2026-10-13** (`.github/workflows/nightly.yml`: the fen soak over the mods at `bundle.toml`'s commits via `scripts/fetch-mods.sh`, the swarm at twenty bots and at E1's fifty for five minutes, every table an artefact); the first nightly after it is the proof — **run 77, 2026-10-15:** the fifty-bot soak passed on a shared runner, the fen soak flew but wrote its table under `crates/bot/target` (a relative `FEN_OUT`; fixed de64985b), and the 200-mob entity swarm failed its zero-tolerance gate once while passing locally at head and at bf00d4ac with the same numbers — a runner stall, and **run 78 (2026-10-16)** passed it, so a runner stall it was. Run 78 failed the fifty-bot soak instead, exit 101 with no table (the path fix was not yet pushed): reproduced here as the memory gate at 96% of its bound, and traced to the harness — a bot keeps every message it is sent, some 130 a second beside a swarm, so fifty bots for five minutes put a gigabyte into the test process while the server itself went from 18 to 50 MiB and flattened (two-process measurement). The test now runs the bots as a child `bot swarm` process and the gate measures the server; the first nightly after it is the proof | engine | small |
| T4 The plan-stamp stall: a plan holding a partial cell of a `whole` material must not wait for ever (`plans.rs`, found landing W35) — **landed fa5ddd51** | engine | small |
| T5 The unsigned mod-side commits on main (three by 2026-10-12: be39490e, 5787eb7d, 14810aa9) are the designer's call: rewrite with sign-off or document the exception once — **documented 2026-10-15**, the designer's call (`CONTRIBUTING.md`, "One exception, recorded here once") | designer | tiny |
| T6 Invisible walls: act on the first `client.log` that shows one; the reload, the held edits and both logs landed in 0.4.1 | engine, as reports come | unknown |

### 2. Signed releases — in scope by the designer's decision

| item | who | size |
|---|---|---|
| S1 Windows: a code-signing identity. Azure Trusted Signing is the recommended route (a monthly subscription, no hardware token, works with `signtool` in CI); an OV certificate from a CA is the alternative | designer | a purchase and an identity check, days of lead time |
| S2 macOS: an Apple Developer Program membership and a Developer ID Application certificate, exported for CI; notarisation through `notarytool` | designer | a purchase, a day |
| S3 The release workflow signs the Windows executables and the macOS bundle and notarises the bundle, every step gated on the secrets existing so an unsigned build still works; `check-archive.sh` verifies the signatures — **landed 2026-10-13** (`docs/distribution.md` §8a names the secrets); inert until S1 and S2 exist, and its first run on a real certificate is the proof | engine | medium |
| S4 The launcher and the updater verify that a staged update is signed by the same identity before applying it, on top of the manifest's Ed25519 signature — **landed 2026-10-13** (`crates/updater/src/signing.rs`: the system's own tool reads both signatures, the team on macOS and the certificate subject on Windows; an unsigned launcher holds an update to nothing); `SIGNING_REQUIRED` in the release workflow fails a leg that lost its secrets once signing has begun; its first real exercise is the second signed release | engine | medium |
| S5 Release notes drop the "unsigned binaries" known limit | engine | tiny |

### 3. Performance — the three changes that carry the milestone

| item | who | size |
|---|---|---|
| P1 Relight off the tick: a light thread relights every arrival and edit from a per-tick snapshot of the chunks, in tick order; the tick records events and hands them over. Measured before at nine tenths of a sprinting player's tick — **landed 2026-10-13**: the fen soak's flying tick fell from 12–13 ms to 3.7–3.9 ms, the thread kept up (see `docs/performance-state-2026-10-12.md`, addendum) | engine | large |
| P2 Meshing off the frame thread: a mesh job takes a snapshot of the chunk, its neighbours, their light and fluid, runs on a worker pool, and the frame thread only snapshots and uploads; an edit's chunks still mesh on the frame so no hole is ever shown — **landed 2026-10-13** (`crates/client/src/meshwork.rs`; a test holds a worker's mesh equal to the frame's). E2's frame pacing on the designer's card is the proof | engine | large |
| P3 Chunk occlusion: a chunk enclosed by its neighbours' solid faces, or under the column cover, is not drawn for a camera above ground | engine | medium |
| P4 The World mod's chunk at 50–100 ms: fewer octaves, smooth detail where sampled is not seen | mods (World) | medium |
| P5 Beautiful's defaults, if E2 needs them: 1024² cascades, two instead of three | engine, designer's judgement on the look | small |

P1 first: it bounds how many players a server can ever hold, and E1 cannot
be met without it. P2 second: it is what E2 measures. P3 if E2 still fails
on hills and caves.

### 4. The mod API as a 1.0

| item | who | size |
|---|---|---|
| A1 `api/` carries a version and a deprecation rule: a function is marked deprecated for one minor release before it goes, and the stub says so — **landed 2026-10-13**: API 1.0, `game.api_version`, the stubs' header, the rule in `AGENTS.md`; `check-stubs.sh` holds the two numbers equal and every `---@deprecated` to its terms | engine | small |
| A2 `AGENTS.md` gains a reference section, by area, beside the dated "what's new" list; the stubs stay the source of truth — **landed 2026-10-13**: "Reference, by area", generated from the stubs by `scripts/api-reference.py` (fifteen areas, every function and method with its first sentence), `--check` in CI fails by name on a function the areas do not place or the stubs no longer declare | engine | medium |
| A3 A conformance run the mod repositories call from their CI: `tiamat-server --check-mods`, the fixture bot joining with the mod enabled — **engine half landed 2026-10-13**: `scripts/conform.sh <mod dir>` (the pinned default mods beside it, `--check-mods`, a real server with the bot joining, no mod disabled), `docs/mod-conformance.md` has the workflow; the mods' half is asked on every sheet | engine (the script), mods (the workflows) | small |
| A4 Vendored stubs refreshed in every mod at the frozen version — asked on every sheet 2026-10-13 | mods | tiny |

### 5. The playtest pass

| item | who | size |
|---|---|---|
| V1 One structured pass over every "awaiting the eye" gate, on all three machines, with the sheets marked | designer | days |
| V2 Every LAN pairing, both directions, default names — **read by the designer 2026-10-15: fully working on 0.4.2** (E7) | designer | half a day |
| V3 The update chain from an installed 0.4.x | designer | an hour |

### 6. Asks filed after the plan

The plan said every filed ask was answered as of 0.4.2. These arrived after
it and are engine work before the tag, because the mods cannot ship their
part of 0.5.0 without them.

| item | who | size |
|---|---|---|
| L23.2 A swimming player is tagged `SWIM` (Life 23, part 2) — **landed 2026-10-14** (`anim_from_motion` reads the submersion the step measured; the client's own figure the same way) | engine | tiny |
| L23.3 A crawl: `crawl` on `set_player_abilities`, `AnimTag::CRAWL`, the low box, predicted (Life 23, part 3) — **landed 2026-10-14** (`phys::crawl::posture`, Sub-Node Contract §2 "A body may crawl", protocol 91) | engine | medium |
| L23.1 The mod's player models with skins (`register_player_model`, `set_player_model`, `set_player_skin`), drawn by everyone and in third person — **landed 2026-10-14** (protocol 92: `EntityDef.skin`, `EntityLooked`, `PlayerLook`; the client keeps a pass per look) | engine | medium |
| L23.1b A player's own skin, offered by the client at join and passed on by a host who allows it (`allow_custom_skins`, off by default) — **landed 2026-10-14** (protocol 93: `HelloAck.custom_skins`, `ClientMessage::Skin`; the server reads the PNG header and never decodes, `event.skin` on join, `game.custom_skins_allowed()`) | engine | medium |
| N1 Names that stick: `sticky_names` in server.toml — a player wears the first name they joined this server with whatever their client asks for, is told so on joining, and only RCON `rename` changes it (the designer, 2026-10-14: players should stop changing names; the smaller of two answers, the other being a name registry at tiamatengine.com behind `AuthProvider`, held for the milestone after 0.5.0 and to be raised again before 1.0.0) — **landed 2026-10-15** (`session::JoinContext::sticky_names`; off by default, and off on a client's embedded host, which has no admin port to rename from; no protocol change) | engine | small |

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

## Where it stands, 2026-10-16

The three commits held for the push landed (N1, T5, the nightly paths), after
the host's GitHub CLI token was given the `workflow` scope. Nightly run 78,
still on the commit before them, passed the entity swarm — run 77's red was
the runner — and failed the fifty-bot soak on its memory gate, which turned
out to measure the bot harness rather than the server (row T3); the swarm
test now keeps the bots in a child process. E8 moved: every default mod
repository carries the conformance workflow, World's adoption commit still
unpushed. The rest is as on the 15th.

## Where it stands, 2026-10-15 (morning)

The first nightly on the T3 workflow ran: the fifty-bot soak passed, the fen
soak's table was lost to a relative path (fixed de64985b), and the entity
swarm failed its zero-tolerance gate once without reproducing locally at
either end of the diff (row T3). T5 is documented (5049d115). The designer
read every LAN pairing as fully working on 0.4.2 (V2, and E7 with it). N1,
names that stick, landed. Decided by the designer the same morning: the name
registry is the theme of the milestone after 0.5.0, and is raised again
before 1.0.0. What is left is unchanged otherwise: S1, S2, E2, V1 and V3 are
the designer's; E8 has Craft and UI running the conformance workflow,
Progress's adoption committed but not pushed, and five mods to go; P3, P5,
T6 and S5 are the engine's if and when their conditions arrive.

## Where it stands, 2026-10-13 (evening)

Landed in the engine: T1, T2 (instrumented), T3, T4, S3, S4, P1, P2, A1,
A2, A3's script. What is left, by who holds it:

- **Designer.** T5 (the two unsigned mod-side commits). S1 and S2 (the
  certificates; then set the repository variable `SIGNING_REQUIRED` to
  `true` after the first signed release). The E2 reading on the reference
  card with the overlay, which decides P3 and P5. V1, V2, V3, which decide
  E5, E6, E7. The nightly's swarm soak is E1's trend; E1's own reading is
  `SWARM_BOTS=50 SWARM_SECONDS=300 cargo test --release -p bot --test
  swarm_load -- --ignored --nocapture` on the reference machine.
- **Mods.** A3's workflows (`docs/mod-conformance.md`), A4's re-vendor of
  `api/`, P4 in World — asked on every sheet, dated 2026-10-13. E8 holds
  when every default mod's CI runs the conformance workflow.
- **Engine, conditional.** P3 if E2 fails on hills and caves; P5 if E2 needs
  it; T6 on the first `client.log` that shows a wall; S5 and the release
  notes at the tag. E3 is read off the Actions history before the tag.
