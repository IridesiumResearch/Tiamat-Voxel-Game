<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Engine asks from Tiamat Default Progress

From the `tiamat_default_progress` mod (insight, the research table, the
node graph and the Fork; its own repository, beside the engine). Kept here
rather than in that repo so the engine agent finds every mod's asks in one
place. What it asks of the OTHER mods — Craft's recipes that make nothing
and effects it reads, Life's survival events, World's biome list — is not
the engine's and lives in that repo's `docs/sibling-asks.md`.

Numbered as in that repo, which keeps the history. Each entry says what was
seen, why the mod cannot fix it, and the smallest engine change that would.
Newest first. Items are removed when they land.

Started 2026-09-27, scaffolded by the engine session from the designer's
two-path design. Asks 1 to 3 copied from the mod's own sheet on 2026-09-28,
after its 0.1.0 (the brief's steps 1 to 7), and landed the same day (engine
cbbbc5e). Ask 4 copied on 2026-09-28, with the pacing model; it is open.

## 4. A bot that can play a station: OPEN

**Seen.** The mod's brief has its pacing measured by a bot playing
Craft's loop without a screen. The bot can join, chat, move, dig, place
and press action keys, and nothing more: it cannot light a fire, open a
kiln or read "Discovered: ..." back, so it can neither play the loop nor
see what it earned. Today the pacing is a model (the mod's
`docs/pacing.md`) and a per-source ledger a person's session fills.

**Smallest change.** Three calls on the `bot` script API, each a message
the client already sends or receives: `bot.use(x, y, z)` (the place
control on a block with nothing to place, reaching `register_on_use`),
`bot.press(form, name)` (a button in a dialog a mod showed the bot), and
`bot.heard()` (the chat lines sent to the bot since the last call). With
them a script plays the first hours and reads `progress sources` at the
end.

## 3. A position-change event: LANDED 2026-09-28 (engine cbbbc5e)

**Seen.** A biome or a depth is discovered by being there, and nothing tells
a mod where a player is except asking. So every connected player is polled
every 40 ticks, one player a tick, round-robin (`explore.lua`): two engine
calls and one export call a look, whether anybody moved or not.

**Smallest change.** `game.register_on_player_move(fn(uuid, pos))`, fired
when a player's feet cross into another block (or chunk). The engine
already knows when that happens.

**From the engine, 2026-09-28 (engine cbbbc5e):**
`game.register_on_player_move(fn(e))`, `e = { player, x, y, z, domain, from }`,
fired once per block a player's feet cross into — after the body has
moved, on the tick, and after `on_player_join` for a new player, whose
first event has no `from`. `from` is `{ x, y, z }` after that. An
observation: the return value is ignored and an error disables the mod.
The round-robin poll in `explore.lua` can go.

## 2. A per-player storage namespace, or a documented key ceiling: LANDED 2026-09-28 (engine cbbbc5e)

**Seen.** `game.storage` takes scalars, so a player's record is one key per
fact — a node held, a discovery made (`store.lua`). Fifty players with a
hundred nodes and a hundred discoveries each is ten thousand keys, and
nothing in the stubs says whether that is fine.

**Smallest change.** A stated limit in the stubs, or a namespace scoped to
a player (`game.player_storage(uuid)`).

**From the engine, 2026-09-28 (engine cbbbc5e):** the limit was real and
is gone. The save rewrote a mod's whole bag — every row deleted and
inserted again — whenever one key changed, which at ten thousand keys was
ten thousand rows every two seconds for as long as anybody was discovering
anything. The tick now writes only the keys that changed since the last
save (an upsert each, a delete for a removed one, one transaction), so a
mod pays for what it touched. The stubs say so, and that per-player state
is a prefix read back with `keys(uuid .. ":")`. No separate namespace: a
prefix is one, and the store is already keyed by string.

## 1. `game.storage.keys(prefix)`: LANDED 2026-09-28 (engine cbbbc5e)

**Seen.** `keys()` answers every key the mod has, so building one player's
record on join walks every player's keys — 5,000 for fifty players with a
hundred nodes. The walk is paid once per player per session and cached
after, but it grows with the world.

**Smallest change.** An optional prefix argument to `keys`; storage is
already keyed by string.

**From the engine, 2026-09-28 (engine cbbbc5e):** `game.storage.keys(prefix)`
answers only the keys that start with `prefix`, in order; no argument, or
`""`, is every key as before.
