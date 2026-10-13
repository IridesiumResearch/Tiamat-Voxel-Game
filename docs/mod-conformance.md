<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Conformance: what a mod repository's CI runs against the engine

A mod lives in its own repository and is written against the engine's mod
API (API 1.0 from engine 0.5.0; `api/AGENTS.md`, "The API's version"). This
is how that repository proves, on every push, that the mod still loads and
runs on the engine's `main`: `scripts/conform.sh`, run from an engine
checkout, with the mod's directory as its argument.

## What the run does

1. **The default mods beside it.** `scripts/fetch-mods.sh` puts every mod
   `bundle.toml` pins into `game/` at its commit — what a release ships —
   and the mod under test is linked in as `game/<id>` over its pinned copy.
   A mod that depends on another default mod is tested beside the version a
   release would carry.
2. **`server --check-mods game`.** The whole registration window in a
   dry-run VM: manifests, dependency order, every `register_*`, with no world
   touched. Anything that would be disabled fails the run by name.
3. **A real server, and the fixture bot.** A server starts on loopback with
   every mod in `game/` enabled, the bot joins, says something, reads its
   inventory and leaves, and the server is stopped with SIGTERM. The run
   fails if the server did not come up, the bot's script failed, the server
   did not flush on shutdown, or any mod was disabled along the way — at
   load or in a hook.

Exit 0 means all of it held. `CONFORM_SCRIPT` names another bot script to run
in place of the smoke join (`crates/bot/scripts/` has several); `CONFORM_KEEP`
keeps the world and the server log and prints where.

## The workflow

Copied into the mod repository as `.github/workflows/conform.yml`, with the
mod's directory on the last line. The engine is checked out beside the mod
and built in debug, which the cache makes a few minutes after the first run.

```yaml
# SPDX-FileCopyrightText: <the mod's copyright holder>
# SPDX-License-Identifier: GPL-3.0-only

name: conform

on:
  push:
  pull_request:
  schedule:
    # Once a day against the engine's main, so a change there that breaks
    # this mod is seen the day it lands rather than the day someone pushes.
    - cron: "0 5 * * *"
  workflow_dispatch:

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0

jobs:
  conform:
    name: conform against the engine's main
    runs-on: ubuntu-latest
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@v5
        with:
          path: mod

      - uses: actions/checkout@v5
        with:
          repository: IridesiumResearch/Tiamat-Voxel-Game
          path: engine

      # Honours the engine's rust-toolchain.toml.
      - name: Install Rust
        working-directory: engine
        run: rustup show active-toolchain || rustup toolchain install

      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: engine

      - name: Conformance
        working-directory: engine
        run: bash scripts/conform.sh ../mod/mods/<id>
```

A mod whose directory is the repository root passes `../mod` instead.

## What it does not do

It does not run a player: the eye gates on a mod's engine-ask sheet are
still a person's. It does not pin the engine: the run is against `main` on
purpose, so the first engine change that breaks a mod is caught by the mod's
own CI the day it lands. A mod that needs an older engine checks out a tag
in the second step.
