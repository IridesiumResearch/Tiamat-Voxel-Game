# Engine asks from Tiamat Default Life

From the `tiamat_default_life` mod (vitals, the HUD, creatures, world modes;
repo `Tiamat_Default_Life`, beside the engine). Kept here rather than in
that repo so the engine agent finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

**Every ask on this sheet has landed as of 2026-09-22.** There is nothing open
here. New ones go at the top, newest first, in the shape the ones above had.

Landed and in use: 0 (15302d1 and the texture step), 1 and 9 (dc3b5ee,
82444e7), 2 (eab4c2d), 3, 4, 5, 7 and 8 (a3db9fa), 10 and 11 (990bf8a), 12
(aa77731), 13 (7c0679c), 14 (033f4e6), and 15 (e5c0394 and the badge step).

Step 2 of ask 0 (a texture on a mod's model) landed 2026-09-20, with the
placement it needed: an entity wearing a mod's model was never put in the
world at all, so the cow was on the GPU and not in the frame.

Ask 15 landed in two halves, both of them usable and neither replacing the
other. `texture` on `emit_particles` makes a heart ONE particle instead of
thirteen, and is the general answer — any burst may carry a picture.
`game.show_over(entity, spec)` is the specific one the row of hearts wanted: a
camera-facing row centred over an entity's head that FOLLOWS it, latest-state
per entity, expiring on the client. Health bars, an "!" over a startled
animal and a quest marker are all the second one. Both are documented in
`api/stubs/game.lua` and `api/AGENTS.md`.
