<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Licensing for mod authors

**Short version: your mod is yours. License it however you like, including
commercially and including closed-source.**

This page explains the licensing in plain language. It is a summary and has no
legal effect — [`LICENSE`](LICENSE) and [`LICENSE.EXCEPTION`](LICENSE.EXCEPTION)
are the terms that actually govern.

## Why this needs saying at all

The engine is licensed **GPLv3-only**. Left alone, the GPL is deliberately
sticky: a plausible reading is that anything running inside the engine's process
is a derivative work and must itself be GPL. For a game engine whose entire
purpose is hosting third-party mods, that reading would make the project useless
— nobody can build a commercial mod on a platform where the licence might eat it.

So we don't leave it alone, and we don't settle it with a friendly note in the
README. A README promise is not a licence and cannot be relied on. Instead the
permission ships as a formal **Additional Permission under GPLv3 section 7** in
[`LICENSE.EXCEPTION`](LICENSE.EXCEPTION), granted by Iridesium, the copyright
holder. That is a real grant with legal effect, and it travels with every copy
of the engine.

## What you can do

If your work talks to the engine **only** through the Lua scripting API or the
network protocol, then it is an independent work:

- License it under anything you want — MIT, proprietary, all rights reserved.
- Sell it. Keep the source closed. Ship it on any storefront.
- Distribute it alongside the engine.
- Everything it generates — worlds, assets, save data — is yours too.

This holds even though your Lua runs inside the engine's process. The exception
says so explicitly, precisely because that is the case people worry about.

## What is still covered by the GPL

The exception is about *the boundary*, not about the engine. You are back under
the GPL, in full, if you:

- **Modify the engine.** Patches to anything in `crates/` are GPLv3, and must be
  released as such if you distribute them. This is true even when the change
  exists only to make your mod work.
- **Link against engine code directly** — as a Rust crate, a native shared
  library, or anything else that is not the scripting API or the network
  protocol.
- **Reach past the public surface** into private or unpublished interfaces.

The dividing line is the API, not the file boundary. Go through the front door
and you are independent; go around it and you are not.

## Hit a wall in the API?

Charter rule 1 is that the mod API is the only API — if a mod can't do something
through it, that is an engine bug, not an invitation to fork. **Open an issue.**
Extending the API keeps you on the clean side of the line and everyone else
benefits. Patching the engine to get around it puts your work under the GPL and
leaves you maintaining a fork.

## The default mods have the same shape

The default game — Life, World, Weather and UI — is not part of the engine. Each
is a separate work in its own repository, and each is licensed the same way the
engine is: **GPL-3.0-only, with an Additional Permission of its own** granted by
Iridesium over the mod's *exports* — the tables it publishes with `game.export`,
the events and hooks it offers, and the block, item, creature and sound
identifiers it registers. A mod that adds a screen to UI's interface, gives Life
a creature, or reads Weather's climate is interacting with those mods through
their exports and is independent, whatever licence it chooses.

What the mods' permissions do not cover is the same as for the engine: copying.
Copy a function out of Life, adapt World's terrain generator, lift UI's textures,
and your work contains GPL-covered material and is under the GPL. Write your own
creature system that happens to do something similar, and no permission is
needed at all, because the GPL governs copies, not ideas.

Each mod's `LICENSE.EXCEPTION` names the document that lists its exports. The
engine's own permission does not cover the default mods and theirs do not cover
the engine; a mod that touches both relies on both, and both say yes.

## The matrix

What you want to do, and what governs it. "Independent" means the Additional
Permission applies and your work's licence is your own choice.

| You want to… | Engine (`crates/`, `game/core_*`) | `api/` stubs and docs | A default mod's exports | A default mod's code or assets |
|---|---|---|---|---|
| Call it from your mod through the scripting API or its exports | Independent (engine permission) | MIT | Independent (that mod's permission) | — |
| Depend on it in `mod.toml`, ship your mod alongside it, run in the same process | Independent | MIT | Independent | Independent, as long as nothing of it is inside your mod |
| Copy or adapt its source, scripts, shaders or textures into your work | GPL-3.0-only | MIT: copy freely | — | GPL-3.0-only |
| Modify it and distribute the result | GPL-3.0-only | MIT | — | GPL-3.0-only |
| Reach past the public surface: private interfaces, a Rust plugin, a native library, another mod's local state | GPL-3.0-only | — | GPL-3.0-only | GPL-3.0-only |
| Implement similar functionality yourself, without copying | No permission needed | — | No permission needed | No permission needed |
| Reuse a bundled third-party asset (a font under the OFL, say) | Its own licence | — | — | Its own licence, named in that mod's assets register |

The exception text itself is identified by a version and date — 1.0, 24
September 2026 — and every contribution to the engine or to a default mod is
licensed under the GPL *together with* that permission, so the promise above is
made by every copyright holder in the tree, not by Iridesium alone.

## The `api/` directory is MIT

Everything under [`api/`](api/) — the Lua stubs, type definitions, mod template,
and API documentation — is **MIT** licensed, not GPL. You can copy those files
into your own project freely, including a closed-source one. See
[`api/LICENSE`](api/LICENSE).

## Contributing back

Contributions to the engine are taken under the Developer Certificate of Origin;
you keep your copyright and license your contribution under GPLv3 **together
with the Additional Permission**, so the promise to mod authors holds for your
code too. See [`CONTRIBUTING.md`](CONTRIBUTING.md). Iridesium maintains the text
of the permission; a change to it gets a new version and date.
