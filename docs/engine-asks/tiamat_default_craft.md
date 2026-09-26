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
