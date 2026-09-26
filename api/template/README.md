<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: MIT -->

# {{MOD_NAME}}

A Tiamat mod, started from the engine's template. It registers one of each
kind of thing the API offers — a block, a tool, a sound, an action, a dialog,
an entity — so every part has a worked example beside it. Keep what you need.

## What is here

| File | What |
|---|---|
| `mod.toml` | The manifest: id, name, version, licence, and what this mod depends on or conflicts with. |
| `init.lua` | Runs once at load. Everything is registered here; the hooks it installs run for ever after. |
| `textures/block.png` | The beacon's face, 16 by 16. |
| `sounds/ping.wav` | The beacon's sound. WAV or Ogg Vorbis. |
| `stubs/game.lua` | The whole mod API as editor annotations, vendored from the engine. Documentation and completion in one file. |
| `AGENTS.md` | How to write a mod, for an AI coding assistant and the person supervising it. |
| `.luarc.json` | Points a Lua language server at `stubs/`. |

## Try it

Check it without starting a server — a second, no world left behind:

```sh
server --check-mods <the directory this mod is in>
```

It prints the mods it found in load order and every block they registered;
a mod with a mistake in it is named, with the line.

Then put this directory in the server's mods directory (`mods_path` in the
server's config; `game/` in the engine repository) and start the server. In
the world: dig anything with the hand, place a beacon, use it, and press the
wave key (J unless you moved it) for the dialog.

## Your editor

Any editor with the Lua language server reads `.luarc.json` and gets
completion, signatures and types for every `game.*` call from `stubs/game.lua`.
The stubs are kept in step with the engine by its CI, so when you update the
engine, copy its `api/stubs/game.lua` over yours.

## Where to read next

- `AGENTS.md` — the rules that fail quietly when broken, and the shape of every
  kind of thing a mod can register.
- `stubs/game.lua` — every function, with the reason it behaves as it does.
- The engine repository's `game/` directory — reference mods, each the
  smallest thing that proves one mechanism.

## Licence

The files here are yours, under the licence named in `mod.toml` and in each
file's header. `stubs/game.lua` and `AGENTS.md` are the engine's, under MIT,
so they may be kept in a mod of any licence. The engine's licence exception
says a mod that uses only its scripting API is not a derivative work of it.
