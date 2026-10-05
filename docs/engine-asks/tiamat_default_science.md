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

**Open as of 2026-10-05: none.** E-S4 landed 2026-10-05; E-S1 and E-S2
landed 2026-09-30; E-S3 answered below.

## E-S4, the domain on place and dig events (2026-10-05): LANDED 2026-10-05 (engine e1becf0a), awaiting the eye

*Wanted:* `domain` on the place event (`register_on_place`) and the dig
events (`register_on_dig_start`, `register_on_dig_complete`), as the use
event already carries it. *Why:* a block placed or dug off the overworld
cannot be told from one at the same coordinates in the overworld. A mod
has to keep each player's domain from `register_on_player_move` and trust
that the move was heard first. This mod does (`tds.domain_of`). Craft does
not, so a station placed on a world at a star, or in the Deep, is named
as if it stood in the overworld (sibling ask C-S8): a terraformer's frame
does not know it is on a body, and two frames, one in each domain at the
same coordinates, would share one box. *Stands in:* this mod's own
`domain_of` for its own records. *Smallest change:* the field, filled
where the use event's is.

**Landed.** Exactly the smallest change: `domain` on the place event and
on both dig events, filled on the server from the space the player is
acting in — the same value the use event carries, and the same one the edit
is applied to. No wire change (hooks run on the server). Stubs and
AGENTS.md say to key placed things on the domain and the coordinates
together and to read them back with `game.get_block{ ..., domain = e.domain
}`. Science can drop `tds.domain_of` for place and dig; Craft (C-S8) can
name a station by `e.domain` without keeping a map of its own. [H]: a
terraformer's frame placed on a body knows it is on the body; two frames at
one set of coordinates in two spaces are two frames.

## E-S3, actions that fire (2026-09-29): ANSWERED 2026-09-30, nothing to build

*Wanted:* `register_on_action` delivering presses, to bind the
*Theatrum* to N and calling the automata home to U. The same as Magic's
E-M3, and the same answer: the stub's "inert until Task 13" was stale and
is gone. Register the action with `default_key = "KeyN"`, and presses and
releases of whatever key the player bound arrive at
`game.register_on_action` as `{ player, id, pressed }`. The stand-ins can
stay as second ways in.

## E-S2, the instance in the generator (2026-09-28): LANDED 2026-09-30 (engine 61b4c3e)

*Wanted:* `pos.domain = "template/key"` in a generator's position. *Why:*
a generator is told `{ x, y, z, seed }` only, so two instances of one
template generate the same world. *Stands in:* the slot trick (that
repo's brief §6.8): every block coordinate lies in −60,000..59,999, so
each body kind's template is a 15 × 15 grid of 8,000-block slots — 225
bodies a kind, 1,125 a world, the cap. Magic's E-M1; the answer retires
the slot trick and its cap.

**From the engine, 2026-09-30 (engine 61b4c3e, no protocol change).** A
generator's `pos` carries `domain`: `"overworld"`, a registered domain's
id, or `"template/key"` for an instance, in every VM that generates,
the generation workers' included. Seed your own streams from it:

```lua
generator = function(buf, pos)
    local key = pos.domain:match("/(.+)$")   -- this instance's key
    -- two instances of one template are now two worlds
end
```

The same seed, domain and chunk give the same bytes, as before. The
slot grid and its cap can go (it was always inside the world, unlike
Magic's offset trick): valid block coordinates are -60,000 to 59,999 on
each axis (`WORLD_HALF_EXTENT_BLOCKS`, `crates/core/src/coords.rs`), so
Inside
the bounds a position is a chunk and an offset within it, so it is exact
everywhere the world exists.

## E-S1, a gravity scale per player (2026-09-28): LANDED 2026-09-30 (engine b0996cb, protocol 83), awaiting the feel

*Wanted:* `set_player_abilities{ gravity = 0.17 }`, or a gravity per
domain, for gravity plating, cavorite soles and low-gravity star bodies.
*Stands in:* an upward `push_player` every tick cancelling part of
gravity. `push_player` is "added, not set" and not documented as
client-predicted, so the mod will try it in a real window for
rubber-banding before it ships.

**From the engine, 2026-09-30 (engine b0996cb, protocol 83).** The
per-player form, as asked first:

```lua
game.set_player_abilities(uuid, { gravity = 0.17 })   -- and speed, fly, ...
```

- A multiplier on the gravity acting on that player's body, default 1.
  `0` floats; over 4 is clamped to 4; negative, NaN, infinite or not a
  number is an error naming the function. Replaced whole on every call
  like the other fields, forgotten when the player leaves.
- **Predicted by the client with the same number**, so a light player
  does not rubber-band: a client test jumping and walking at 0.17 rose
  7.6 blocks with no correction at all. Drop the `push_player` stand-in.
- The jump impulse is unchanged, so a light player jumps higher and
  falls slower; swimming buoyancy scales with it too. Gravity 1 steps
  bit for bit as before.
- Riding: the mount's gravity governs the pair, not the rider's.
  Creatures are untouched.

For the feel ([H]): set 0.17 on join, walk and hold jump; take-off,
apex and landing should be smooth, with no snap. Try 0 and 4, then `nil`.
