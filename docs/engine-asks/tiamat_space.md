# Engine asks for a space mod

There is no space mod yet. This sheet exists so that whoever writes one — a
`tiamat_space` beside the other default mods, or a `Tiamat_Default_Space`
repo — finds what the engine already answers, and files what it does not,
in the one place the engine agent looks. Same rules as the other sheets:
each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

Nothing open.

**S1–S4 landed 2026-09-23**, protocol v75 — Task 15c's four asks, found by
building the demonstration mod the task described (`game/core_space`, the
smallest one that works) rather than by guessing:

- **S1, stars in the sky.** `register_sky` keyframes take `stars` (0 to 1);
  the client draws `sky::star_catalog(seed)` — two thousand positions, one
  derivation on both ends — from wherever the player's domain sits, wheeling
  with the day the way the sun does, behind cloud and behind the ground, at
  the frame's own resolution whatever the deck is marched at. None unless a
  keyframe says so: whether a world has stars is content.
- **S2, a sky per domain.** `register_sky{ domain = ... }` names a domain or
  a template (instances inherit); the table is sent again after every domain
  change, seen from where that domain sits. One clock: the world's day is
  the unnamed sky's, whatever a domain sky wrote in its own.
- **S3, where a domain sits.** `register_domain{ position = {x, y, z} }` and
  `create_domain(template, key, { position })`, in universal blocks. An
  instance's place is kept with it across restarts — and so, since the same
  commit, is the instance list itself, which the server had never written.
- **S4, choosing a star by looking.** `game.stars()`, `game.world_position()`,
  `game.look_direction(player)` and `game.star_in_view(player)`, the last
  being the sky renderer's arithmetic run backwards so the star it names is
  the star on screen (`crates/client/tests/screenshot.rs`, the gate that
  unprojects the brightest pixel).

Gates: `crates/bot/tests/space.rs` (a body made at a star has that star's
sky, seen from the star, and is still there after a restart);
`crates/core/src/sky.rs` (looking along a star names that star);
`crates/core/src/script/mlua_vm.rs` `space_tests`.

What a space mod would still want, and where it would go — not asks, since
nothing has asked yet, but the places to look first:

- **An observer that moves.** A sky is seen from one point per domain. A
  sparse `space` domain in which a player flies between stars would want the
  observer to follow them, scaled by the domain's `scale`. That is a per-player
  observer on the sky table, sent when it moves far enough to matter — a few
  lines each side of the wire, once somebody has a ship.
- **A sun and a moon.** Catalog ids 0 and 1 are reserved for bodies a mod
  places; nothing places them yet. The engine's sun is still the client's arc.
- **Impostors for a body on approach.** Task 15c's "billboard to low-res
  mesh". Nothing draws a body from space; the transfer is a cut.
