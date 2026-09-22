# Engine asks from Tiamot Default Life

From the `tiamot_default_life` mod (vitals, the HUD, creatures, world modes;
repo `Tiamot_Default_Life`, beside the engine). Kept here rather than in
that repo so the engine agent finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

Landed and in use as of 2026-09-20: 0 (15302d1 and the texture step), 1 and 9
(dc3b5ee, 82444e7), 2 (eab4c2d), 3, 4, 5, 7 and 8 (a3db9fa), and 10 and 11
(990bf8a).

Step 2 of ask 0 (a texture on a mod's model) landed 2026-09-20, with the
placement it needed: an entity wearing a mod's model was never put in the
world at all, so the cow was on the GPU and not in the frame.

## 12. `steer_entity` jumps at every rise the physics would climb (2026-09-22)

**Seen, in play.** Cows and pigs hop across ordinary ground. The designer:
"cows and pigs should really not jump unless they are stuck in a hole."

**Why.** `path::steer` jumps when the block half a block ahead is not
`passable` and the block over it is standable. `passable` is "no floor
cells", so a block of smooth terrain holding a single cell of floor, a
third-of-a-block lip, counts as an obstacle and is jumped. But the physics
already climbs exactly that: `step_height` is one cell, and
`a_step_up_of_one_subnode_succeeds_and_two_does_not`. On the Spindle's
smooth ground nearly every rise is one cell, so a steered mob jumps at
nearly every rise.

**What the mod does now.** Walkers no longer use `steer_entity`. They set
`drive` toward the target themselves, and jump only when stuck: trying to
walk and not moving for half a second (a hole, a full block ahead). If
three jumps do not free them, they give up on that target. That works, and
it is a second copy of steering the engine meant every mod not to write.

**Smallest change.** Jump only for a rise the step cannot take: the height
of the floor ahead above the feet, in cells, greater than
`tuning.step_height`. A one-cell lip is walked; two cells or a full block
is jumped, as now. Optionally a `jump = "stuck"` mode, for mods that want
the calmer rule.
