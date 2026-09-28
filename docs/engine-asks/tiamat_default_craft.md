<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Engine asks from Tiamat Default Craft

From the `tiamat_default_craft` mod (recipes, a chest, a kiln, cooking and
tools with wear; its own repository, beside the engine). Kept here rather
than in that repo so the engine agent finds every mod's asks in one place.
That repo's `docs/sibling-asks.md` holds what it asks of the OTHER mods —
`add_food` and `add_weapon` from Life, and a heads-up that Life's roadmap
note "X at a campfire cooks" is covered here — which is not the engine's.

Numbered as in that repo, which keeps the history. Each entry says what was
seen, why the mod cannot fix it, and the smallest engine change that would.
Newest first. Items are removed when they land.

Started 2026-09-26, from the mod's first plan, relayed by the designer.
Asks 2 to 7 copied from the mod's own sheet on 2026-09-28, where the
history stays; all six landed the same day (engine c83fbc9). Nothing is
open.

## 7. A drop of another mod's material: LANDED 2026-09-28 (engine c83fbc9)

**Seen.** `register_block{ drops = { ["tiamat_default_world:stone"] = 27 } }`
on the mod's `cracked_stone` was refused ("may not register into
namespace"): a drop is a reference, and the rule for REGISTERING into a
namespace was applied to NAMING one. So the cracked blocks dropped nothing
and `on_dig_complete` gave the digger the rock — straight into the
inventory, never onto the ground where a mod watching drops would see it.

**From the engine, 2026-09-28 (engine c83fbc9):** a `drops` key may name
any qualified id; a bare one is still the mod's own. Names resolve on the
server once every mod has registered, as `absorbs.becomes` does; one nobody
registered is logged and left out, and costs the block nothing else. And,
found while landing it: **`drops` had never been applied at all** — the
field was accepted and sorted, and a dig credited whatever the edit removed
regardless. It applies now, in units per full block, paid as the block
comes apart (see 3). `cracked_stone` can say what it drops, and the
hand-giving in the hook can go.

## 6. A material's tags and hardness, read back: LANDED 2026-09-28 (engine c83fbc9)

**Seen.** Registration was write-only: the world declared its rocks'
hardness and could declare `tags = { "ore" }`, and no mod could read either
back, so the dig classes were a table naming the world's blocks one by one.

**From the engine, 2026-09-28 (engine c83fbc9):** `game.hardness(material)`
answers the registered hardness, the engine default (0.75) for a block that
said nothing, `nil` for a material nobody registered. `game.tags(material)`
answers the registered list in the order the mod wrote it, an empty list
for a block with none, `nil` for a material nobody registered. `tags` on
`register_block` is now kept (it was accepted and dropped on the floor).

## 5. Enumerating containers: LANDED 2026-09-28 (engine c83fbc9)

**Seen.** A kiln burns on the tick whether or not anybody is looking, and
nothing listed the containers that exist, so the mod kept its own index of
station positions in `game.storage` — a second record of a fact the engine
already held.

**From the engine, 2026-09-28 (engine c83fbc9):** `game.containers(prefix)`
— the names of every container that exists starting with `prefix`, in
order; `""` for all of them. The engine already keyed them by name.

## 4. A give into one slot of a player's view: LANDED 2026-09-28 (engine c83fbc9)

**Seen.** A tool's wear belongs in its `detail`, but changing a `detail` is
a take and a give, and the give landed wherever the engine put it — not the
hotbar slot the player was holding — so every dig would have moved the pick
out of their hand. Wear lived in storage under the tool's serial instead.

**From the engine, 2026-09-28 (engine c83fbc9):** `slot` on `game.give`
and `game.take`, one-based, as the container calls have. Into a named slot
the stack goes whole or not at all — an empty slot takes it, one holding
the same material, shape and detail with room merges, anything else answers
`false` — so a take of the pick from the held slot and a give of it back
changed puts it in the hand. `game.take` with a slot takes from that slot
alone.

## 3. A drop that depends on the tool: LANDED 2026-09-28 (engine c83fbc9)

**Seen.** `drops` was fixed at registration; the hook could refuse a dig
but not change its yield, so a block was refused to the wrong tool rather
than yielding less.

**From the engine, 2026-09-28 (engine c83fbc9):** `on_dig_complete` may
answer `{ drops = { ["mod:id"] = units } }` — the shape `register_block`'s
`drops` takes, in units per full block — replacing the block's own rule for
that dig alone. A dig that takes nine cells pays a third, credited as the
block comes apart with the fraction carried between bites, so a whole block
pays exactly what was said. Bare ids are the answering mod's, namespaced
ones any mod's. A table without `drops` is a plain allowance; where several
mods answer, the last wins. A `drops` the engine cannot read is the mod's
error: disabled, and the dig goes ahead on the block's rule. `on_dig_start`
is not read for this — it fires before the block is looked at.

## 2. A tool's speed per material: LANDED 2026-09-28 (engine c83fbc9)

**Seen.** Dig time was the block's hardness over the tool's one
`speed_multiplier`: a bronze pick fast on rock was equally fast on earth.

**From the engine, 2026-09-28 (engine c83fbc9):** `register_tool{ speeds =
{ ["tiamat_default_world:stone"] = 4.5, dirt = 0.5 } }` — a bare name is
the mod's own; each must be positive. A block the tool does not name digs
at `speed_multiplier`; one nobody registered is dropped at load and digs at
the general speed.

## 1. A dig-start hook, so a tool gate refuses at once: LANDED 2026-09-26 (engine ddc4fee)

**Seen.** The engine has no dig class, tier or durability; a mod gates a
block on the tool in hand with a veto, and `on_dig_complete` fires at the
first chip — for a one-cell dig, after the player has waited out the whole
dig. The plan mitigated with a red HUD target line.

**From the engine, 2026-09-26 (engine ddc4fee):**
`game.register_on_dig_start(fn(e))` — the same `DigEvent` and the same
ladder as `on_dig_complete` (`false` refuses, a string refuses with that,
`""` silently, anything else allows), asked on the tick a dig is first seen
and before any of the block comes off; and again when the crosshair moves
to another block, which is a new dig. `on_dig_complete` is still asked at
the first chip, as it was, for a mod that wants the block's state then. The
red line can stay as a hint; the refusal itself now arrives as the player
starts.

## 0. The default tool is the lowest id, so the reference hand wins for ever: LANDED 2026-09-26 (engine ddc4fee)

**Seen.** `core_tools:hand` sorts before anything this mod could call its
hand, so the plan declared `conflicts = ["core_tools"]` to be the hand at
all — which also removes the reference chisel, re-registered here as a
craftable tool.

**From the engine, 2026-09-26 (engine ddc4fee):** the default tool is now
the lowest id among mods that are NOT reference mods, the rule the sky
already used — the engine's own fixture stands aside for a real mod's hand,
whatever the ids. `conflicts = ["core_tools"]` is no longer needed for the
hand; keep it only if the mod wants the reference chisel gone as well.

## Not an ask, answered anyway

Durability, dig classes and tiers stay the mod's (charter scope discipline):
`game.set_tool` and a `detail` on the stack carry wear, and a gate is a
veto. If a plan finds a thing the API cannot express, that is an ask.
