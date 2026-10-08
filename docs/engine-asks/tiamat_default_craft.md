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
history stays; all six landed the same day (engine c83fbc9). Asks 8 and 9
added 2026-09-28 from the mod's sheet and landed the same day (engine
cbbbc5e). Ask 10 added 2026-09-28 and landed the same day (engine
7cf1c73). Ask 11 added 2026-09-28 and landed the same day (engine
f2cbc36). Ask 12 added 2026-10-07 and landed the next day (engine
671f5763). Nothing is open.

**From the engine, 2026-10-08 (night), for "Level this ground" (Craft
09eee00).** Two engine changes the same day move the ground under that gate.
(1) A whole material placed against the top of a partial block no longer
goes INTO it among the ground's cells: a block with any node in its top
layer is ground and the thing goes in the block ABOVE, intact; a block with
none is SWEPT — its remains destroyed — and the shape laid whole at its
bottom (Sub-Node Contract §7.6, the designer's rule). "Sunk into rock" is
now only the side-face case. (2) The place event says it outright:
`e.swept` is `true` for a placement that destroys what the block holds —
the sweep, or grass cards — and the hook is asked before anything is
written, so a refusal keeps the remains. The gate's inference ("27 units
with fewer than 27 cells") misses a full-shape whole (the anvil, a
station without a `shape`), which now sweeps rock unrefused: key the gate
on `e.swept` (and `game.get_block` on `e.x, e.y, e.z` still reads the
block as it stands, cards included). The designer's words for the refusal
are exactly "level this ground".

## 14. A small whole thing that clips into the ground instead of sweeping it (2026-10-08): LANDED 2026-10-08 (engine, the commit after 8f20397a)

**From the engine, 2026-10-08.** As asked: `register_block{ sweeps = false
}` on a `whole` block. Set down on a partial block with no node in its top
layer it stands among the block's cells — `plan_whole`'s `shape & !filled`,
the model clipping through the ground, as on a side face — and "this is not
flat ground" when no cell of its shape is air there. The field is refused on
a block that is not `whole`. `e.swept` is false for such a placement. Bot
test in `mod_hooks.rs` (a peg among a thin floor, the floor intact).


**Seen.** Since engine fc25ba71 a whole block laid on a partial block with no
node in its top layer SWEEPS that block — its remains destroyed — and stands
at its bottom (Sub-Node Contract §7.6). Right for a campfire or a station,
which want level ground. Wrong for a torch: the designer, 2026-10-08,
"torches should not break ground when they are placed, they should just clip
into it" — stood in a thin floor among the floor's cells, as §7.6 had it
before the sweep.

**Why the mod cannot.** It can refuse a sweep (`e.swept`) but not change
where a placement lands or what it writes; and it cannot write a torch among
ground cells itself — a `set_block` of a whole material replaces the block
(only a whole one standing there is swapped, ask 12) and a merge of one is
refused.

**Smallest change.** A `register_block` field on a whole block — `sweeps =
false`, say — under which a placement that would sweep instead takes the air
cells of the shape among the block's cells (the `plan_whole` arithmetic,
`shape & !filled`), the model clipping through the ground, as on a side face;
refused, as there, when no cell of the shape is air.

**Stands in.** A torch that would sweep anything but plants is refused, "A
torch wants firm ground." It stands in grass, and on full blocks as ever.

## 13. A whole block placed on top of another whole block (2026-10-08): LANDED 2026-10-08 (engine 8f20397a)

**From the engine, 2026-10-08.** The second of the two ways offered: a
placement aimed into a block holding a whole material is given to
`on_place` BEFORE the "one piece" refusal, so this mod's torch handler
hears it, lights the fire, returns `""`, and the player is told nothing;
a placement no mod handles is refused as before. Not the first way (the
block above): a torch set down a block above a laid fire would float two
thirds of a block in the air. Contract §7.5. Bot test in `mod_hooks.rs`.


**Seen.** Holding a torch, right-clicking the TOP of a laid campfire answers
"that is one piece; it goes down whole or not at all", and the fire is not
lit. The campfire's block is not ground (§7.6: no node in its top layer), so
the torch is aimed INTO the campfire's own block, and a block holding a whole
material refuses it (`Refusal::Whole`) before any mod is asked. This mod
lights a fire by hearing the torch's placement (`register_on_place`, reading
`game.looking_at`), so it never hears this one. The fire's side works.

**Why the mod cannot.** The refusal comes before `on_place`, and holding a
placeable stack makes the control a placement, never a use.

**Smallest change.** Against the top face of a block that holds a whole
material, a placement goes in the block above (a whole block is ground to
what is put on it) — then `on_place` hears it, as at the side. Or: ask
`on_place` before refusing, so a mod may handle it with `""`.

**Stands in.** A fire is lit with a torch from its side, or with a striker.

## 12. Swapping a whole material standing in a thin floor (2026-10-07): LANDED 2026-10-08 (engine 671f5763)

**From the engine, 2026-10-08.** As the smallest change asked: `game.set_block(pos,
"<whole>")` on a block a whole material stands in replaces THAT material and
keeps the ground. The tick resolves the write against the block, as it
resolves a merge: the standing one's cells the new shape does not reuse go
to air, the new shape takes the air (`place::writes`, cell by cell since the
ground is another material), and the ground cells are untouched. Lighting a
campfire, putting a fire out, a torch burning out in a thin floor: the floor
stays, in every mode. Sub-Node Contract §7.5, "Swapped in place, the ground
staying"; `whole_blocks.rs` swaps a brazier for an anvil and back in a
nine-cell floor and checks every cell. Two edges, both in the contract: a
block with no whole material standing in it is still REPLACED by
`set_block` (so a mod that wants to set a campfire INTO a thin floor from
code, as a player's placement would, should ask — the explicit `{ swap =
true }` form was not needed for this ask and was not built); and a shape
with no cell free once the old material is gone writes nothing, logged.
`util.lua`'s `name_at` is the right reading of the block afterwards: it is
still mixed.


**Seen.** Since engine 071a7978 a whole material placed on a thin floor goes
INTO the floor's block and shares it with the floor's cells (Sub-Node
Contract §7.6) — a campfire laid on snow, a torch stood in a sliver of
earth. This mod then changes what the thing is, in place: a laid campfire
is lit (`unlit_campfire` → `campfire_lit`), a fire goes out, a torch burns
out (`torch` → `spent_torch`). Each is `game.set_block(pos, "<whole>")`, and
`queue_seed` makes that a `Seed::Replace` of the whole material's shape —
the WHOLE block is replaced, so the floor's cells in it are erased. Units
destroyed by a fire catching, which charter rule 5 forbids. There is no
other way to write it: a merge into a block holding a whole material is
refused (§7.5), a masked write of one is refused, and a whole material may
not be written as part of its shape.

**Why the mod cannot.** Reading the floor's cells first and writing them
back after is refused for the same reason (nothing is written into a whole
material's block afterwards); giving the floor's units to somebody instead
is not putting them back.

**Smallest change.** `game.set_block(pos, whole)` on a block that holds a
whole material replaces THAT material's cells with the new one's shape, and
keeps the ground cells (§7.6's "air cells of its shape" again, against the
block as it is less the old whole material) — the same rule the placement
already follows. Or an explicit form, `game.set_block(pos, whole, nil, {
swap = true })`, if a plain replace should stay a replace.

**Stands in.** A fire lit, put out or a torch burned out in a thin floor
loses that block's floor cells. Recognising the fire there is this mod's,
and done: a mixed block is read by its cells (`util.lua`'s `name_at`).

## 11. The blocks carrying a tag (2026-09-28): LANDED 2026-09-28 (engine f2cbc36)

**Seen.** Ask 6 gave `game.tags(material)`, and the world now tags its
blocks, so this mod classes them by rule: `stone` is rock, `hard` hard
rock, `soil` loose. What it cannot do is go the other way — ask which
blocks are `soil` — and a tool's per-material speeds (ask 2) are a table
registered at load, block by block. So a pick's slower speed on earth
still needs a list of the world's loose blocks by name, the one table of
another mod's blocks this mod still keeps.

**Smallest change.** `game.tagged(tag)`: the qualified ids of every block
registered so far with that tag, in registration order — callable in the
registration window, after the mods one depends on have registered.
Or `speeds` on `register_tool` accepting `{ tag = "soil" }` keys,
resolved at freeze.

**From the engine, 2026-09-28 (engine f2cbc36, 71d731d):** both halves,
because each covers what the other cannot. `game.tagged(tag)` answers the
qualified ids of every block registered so far with that tag, in
registration order (numeric ids are handed out in sequence, so the order
is the registry's, not a hash map's), callable in the registration window
and complete after freeze; a tag nobody carries answers an empty table;
items never appear, since an item cannot carry tags. And `speeds` on
`register_tool` takes tag keys written `["#soil"] = 2.0` — a leading `#`,
which no mod id may contain, so the key can never be mistaken for a
mod's block — resolved once at freeze into the same per-material table
ask 2 built, so a block a later mod registers still takes the speed and
nothing at dig time changes. A speed named for the material itself beats
a tag's; two tags on one block, the higher wins; a tag no block carries is
a warning naming the tool and the tag, not an error. Unit-tested in the VM
and end to end: a tool with `#soil` digs a block registered by a mod that
loads after it in half the ticks. The stub has `game.tagged` beside
`game.tags` and the key form on `speeds` with the precedence rule.

## 10. A listed use handler beside an unlisted one (2026-09-28): LANDED 2026-09-28 (engine 7cf1c73)

**Seen.** Ask 8 landed as asked: `register_on_use(fn, { materials = {...} })`
is heard first at those blocks — and, by the same design, at no other
block. A mod has one `on_use` (a second is refused: "One callback per hook
per mod"). Craft hears uses at far more blocks than its fires: every
station block the registry holds, its own and other mods' — Tiamat Default
Progress's research table opens because Craft handles a use at it, and that
block is registered after Craft loads, so Craft cannot name it in a list.
So Craft can list its fires and lose every station another mod adds, or
list nothing and lose the gesture ask 8 was for: raw meat held out over a
fire is still eaten by Life, which loads first.

**Meanwhile.** Craft registers `on_use` without a list, as before; cooking
is the fire's box, opened with an empty hand.

**Smallest change.** Let a mod register `on_use` twice when exactly one of
the two carries `materials`: the listed one asked first at its blocks, the
unlisted one in its ordinary place for every other block. Each is still
one callback, so nothing about a veto's order changes.

**From the engine, 2026-09-28 (engine 7cf1c73):** as asked. `on_use` alone
has two slots, one for a registration with `materials` and one for a
registration without, so a mod may hold both; a second of either, or a
third call, is refused with a message naming the slot it collided with.
The listed callback is asked first at its blocks and nowhere else; the
unlisted one is asked in load order at every other block, and it is no
longer stood down at a block merely because its mod has some list — only
where its own list matched. `anywhere` belongs to the unlisted callback:
naming it beside `materials` is refused at load ("`anywhere` is for the
handler with no `materials`"), which is the one change of meaning here —
before, the pair loaded and reached one callback; no shipped mod wrote
it. Registration order does not matter. Unit-tested in the VM and end to
end with a Life-like mod loaded first eating anywhere: a use at the fire
reaches Craft's listed handler before it, a use elsewhere reaches Craft's
unlisted one after it. `api/AGENTS.md` and the stub say the rule.

## 9. Reading one slot of a player's view (2026-09-28): LANDED 2026-09-28 (engine cbbbc5e)

**Seen.** The brief's anvil is worked in the world: the hammer in the main
hand, the bloom or bar in the off-hand (slot 28 of `player:main`), a
right-click a blow. `game.held` answers the main hand only and
`game.inventory` answers a view consolidated, one entry per material, cut
and detail, with no slot in it — so nothing says what is in the off-hand.
Ask 4 (engine c83fbc9) gave `game.take` and `game.give` a `slot`, which is
the other half: the anvil could take the bloom from slot 28 and give the
bar back into it, if it could first see what is there. A HUD script is told
`state.offhand`, so the engine has the answer; the server API does not ask.

**Meanwhile.** The anvil is a station with a container: the work goes on it
through its screen, where the player chooses what to forge, and each use
with a hammer in hand is a blow. It works; it is a screen where the brief
wanted a gesture.

**Smallest change.** `game.slot(player, view, n)` answering `{ material,
units, shape, detail }` or nil — or a `slot` on each entry of
`game.inventory` when asked for unconsolidated.

**From the engine, 2026-09-28 (engine cbbbc5e):** `game.slot(player, view, n)`,
`n` from 1 as the screens number them, answering the table `game.held`
answers or nil for an empty slot, a view that does not exist, a slot past
its end, or a player not connected. With ask 4's `slot` on `take` and
`give`, the anvil reads slot 28, takes the bloom from it and gives the bar
back into it.

## 8. A use at a block reaching that block's handler first (2026-09-28): LANDED 2026-09-28 (engine cbbbc5e)

**Seen.** A player holding raw meat right-clicks a burning campfire and the
meat should go over the fire — the gesture the brief designed cooking
around. `register_on_use` callbacks are asked in mod load order and the
first to handle a use stops the rest. Life loads before Craft (Craft names
Life in `optional_depends` to read its exports) and registers
`{ anywhere = true }` to eat food held at any block or at the sky, so meat
held at a fire is eaten before Craft hears of the use. Neither order is
wrong for its own mod, and no mod can reorder the chain: loading Craft
first would cost it Life's exports, and Life cannot know every block some
other mod cooks on.

**Meanwhile.** A fire has a container: opened with an empty hand, food put
on it through the screen, cooked while it burns. Nothing contends with
Life's eating; the gesture is lost.

**Smallest change.** A use handler registered for particular materials —
`game.register_on_use(fn, { materials = { "tiamat_default_craft:campfire_lit", ... } })`
— asked, in load order among themselves, before any handler without a
material list: the block that was clicked answers first, and a held
item's handler hears only what no block claimed. It is what a door, a
lever and a cooking surface all want, and needs no mod to know another.

**From the engine, 2026-09-28 (engine cbbbc5e):** the first form, as asked.
`game.register_on_use(fn, { materials = { "campfire_lit", ... } })` is
asked about a use at one of those blocks before any callback registered
without a list, in load order among the listed, and is not asked about a
use at any other block. Bare ids are the mod's own; a namespaced id may be
any mod's block. A use at nothing has no block, so the list does not apply
to it — `anywhere = true` beside it hears those too. A `materials` that is
not a list of block id strings, or is empty, is refused at load.

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
