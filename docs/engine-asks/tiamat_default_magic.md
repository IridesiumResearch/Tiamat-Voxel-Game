<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Engine asks from Tiamat Default Magic

From the `tiamat_default_magic` mod (the magic tree of the default game's two paths, tiers 3 to 7;
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

**Open as of 2026-09-30: none.** E-M1 and E-M2 landed 2026-09-30 (the
sky awaits the eye); E-M3 answered below.

## E-M3, actions that fire (2026-09-29): ANSWERED 2026-09-30, nothing to build

*Wanted:* `register_on_action` delivering presses, to bind the *Mutus
Liber* to J and the familiars to K. *Why it was asked:* the stub of
`game.register_action` said "Stored now, inert until Task 13".

**From the engine, 2026-09-30.** That line was stale, and is gone from the
stubs. Actions have fired for a long time: register one in the
registration window with `default_key = "KeyJ"`, and every press and
release of whatever key the player bound arrives at
`game.register_on_action` as `{ player, id, pressed }`, both edges, so a
"while held" control works. The reference mods `core_gear`, `core_tools`
and `core_ui` use it. The stand-ins (use the book, `magic book` in chat)
can stay as second ways in.

## E-M2, a sky per instance (2026-09-28): LANDED 2026-09-30 (engine 8275173), awaiting the eye

*Wanted:* a domain instance's sky set at run time, so a woven world has
its own sky without a per-player overlay. *Why:* `register_sky{ domain }`
is per template and registration-only. *Stands in:* Weather's overlay
(Wx-M1), per player, on arrival.

**From the engine, 2026-09-30 (engine 8275173, no protocol change).**

```lua
game.set_domain_sky(id, spec)   --> true for a live domain, false otherwise
game.set_domain_sky(id, nil)    -- back to its template's sky
game.create_domain(template, key, { position = p, sky = spec })
```

- `spec` is `register_sky`'s table: `keyframes` (required) and
  `cave_fog` are the domain's own. `day_length_ticks` and `start_time`
  are accepted and ignored, because the world has one clock; `domain` in
  the table is refused, since the id is the first argument. A bad spec
  is an error naming the function.
- Everyone in the domain gets it at once; anyone arriving later gets it
  on arrival. A sibling instance is untouched.
- Kept with the instance across restarts and removed by
  `destroy_domain`, like its position. At creation it applies only when
  the instance is new, as `position` does.
- It works on any live domain, a registered one or the overworld too,
  and is kept for those as well.

For the eye ([H]): weave a world, set a visibly different sky on it
while standing inside, and see it change at once; have a second player
arrive after and see the same sky.

## E-M1, the instance in the generator (2026-09-28): LANDED 2026-09-30 (engine 61b4c3e)

*Wanted:* `pos.domain = "template/key"` in a generator's position. *Why:*
a generator is told `{ x, y, z, seed }` only, so two instances of one
template generate the same world. *Stands in:* the offset trick (that
repo's brief §6.11): each woven world in its own far slice of the
template's coordinates. Science asks the same (E-S2). The mod asks the
engine to confirm how far out the client stays exact before it builds on
the trick: a slot at `(slot × 2 + 1) × 2^20` is a million blocks out, and
the brief's sky parameter puts players past 2^27.

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
offset trick can go: valid block coordinates are -60,000 to 59,999 on
each axis (`WORLD_HALF_EXTENT_BLOCKS`, `crates/core/src/coords.rs`), so
a slot at `(slot * 2 + 1) * 2^20` blocks was about seventeen times past
the world's edge and never inside it; players past 2^27 likewise. Inside
the bounds a position is a chunk and an offset within it, so it is exact
everywhere the world exists.
