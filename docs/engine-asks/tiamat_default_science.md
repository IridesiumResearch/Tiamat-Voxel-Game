<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Engine asks from Tiamat Default Science

From the `tiamat_default_science` mod (the science tree of the default game's two paths (the design's "tech"), tiers 3 to 7;
its own repository, beside the engine). Kept here rather than in that repo
so the engine agent finds every mod's asks in one place. What it asks of the
OTHER mods (Progress, Craft, Life, the world and the interface) is not the
engine's and lives in that repo's `docs/sibling-asks.md`.

Numbered as in that repo, which keeps the history. Each entry says what was
seen, why the mod cannot fix it, and the smallest engine change that would.
Newest first. Items are removed when they land.

Started 2026-09-29, scaffolded by the engine session from what the
designer's two-path design and the sibling mods had already fixed
(`docs/brief.md` in that repo).

**Open as of 2026-09-30: E-S1 and E-S2**, mirrored from that repo's
`docs/engine-asks.md` (filed 2026-09-28/29); E-S3 answered below.

## E-S3, actions that fire (2026-09-29): ANSWERED 2026-09-30, nothing to build

*Wanted:* `register_on_action` delivering presses, to bind the
*Theatrum* to N and calling the automata home to U. The same as Magic's
E-M3, and the same answer: the stub's "inert until Task 13" was stale and
is gone. Register the action with `default_key = "KeyN"`, and presses and
releases of whatever key the player bound arrive at
`game.register_on_action` as `{ player, id, pressed }`. The stand-ins can
stay as second ways in.

## E-S2, the instance in the generator (2026-09-28): OPEN

*Wanted:* `pos.domain = "template/key"` in a generator's position. *Why:*
a generator is told `{ x, y, z, seed }` only, so two instances of one
template generate the same world. *Stands in:* the slot trick (that
repo's brief §6.8): every block coordinate lies in −60,000..59,999, so
each body kind's template is a 15 × 15 grid of 8,000-block slots — 225
bodies a kind, 1,125 a world, the cap. Magic's E-M1; the answer retires
the slot trick and its cap.

## E-S1, a gravity scale per player (2026-09-28): OPEN

*Wanted:* `set_player_abilities{ gravity = 0.17 }`, or a gravity per
domain, for gravity plating, cavorite soles and low-gravity star bodies.
*Stands in:* an upward `push_player` every tick cancelling part of
gravity. `push_player` is "added, not set" and not documented as
client-predicted, so the mod will try it in a real window for
rubber-banding before it ships.
