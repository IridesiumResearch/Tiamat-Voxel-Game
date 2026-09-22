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

## 15. A picture over an entity (2026-09-22)

**Wanted.** Hit a cow and a row of hearts shows over it for a second,
draining. The designer asked for exactly that.

**Why the mod cannot do it properly.** Nothing puts a picture in the world
over an entity: a nametag is text, set only at spawn; the HUD script has
no camera, so it cannot place anything over a thing in the world; a
particle is a flat square of one colour.

**What the mod does now.** Draws each heart in pixels, one particle per
pixel (13 a heart, 65 for a cow), each with no speed, spread or gravity so
it stays put, sent only to the hitter and turned square to them. It works,
and it is 65 messages a blow for what is one picture.

**Smallest change.** Either `texture` on `emit_particles` (a registered
picture, so a heart is one particle), or `game.show_over(entity, { picture,
count, seconds, player })`, a row of icons billboarded over an entity that
follows it. The first is general; the second is what health bars, "!"
over a startled animal and quest markers all are.

## 14. A mob's own speed (2026-09-22)

**Seen.** Cows and pigs walked at a player's walk, 4.3 yards a second:
twice what a grazing animal should. A mob's `drive` has gaits and nothing
else, `Intent::walk` is normalised, so a shorter drive is not a slower one,
and `set_player_abilities`' `speed` is for players.

**What the mod does now.** A kind with `pace = 0.5` pushes on every other
tick and coasts on the rest. Measured through `phys::step` on flat ground:
0.50 of the walk, the body at 0.27 to 0.38 cells a tick, never stopping.
It works; it is a duty cycle standing in for a number.

**Smallest change.** `speed` on `drive` (or on the entity), the multiplier
`Abilities::speed` already is for players, through `Abilities::tuning`.

## 13. A mod's model casts no shadow (2026-09-22)

**Seen, in play.** "The 3d models do not cast shadows." The cow and the pig
float on the ground while the players beside them are anchored by theirs.

**Why.** `Renderer::draw_shadow_casters` draws the engine's own rig
(`self.skinned`) into every cascade, and the models a mod pushed live in
`figures.passes` and are never drawn there. The comment on that very call
says why it matters: a mob with no shadow floats.

**Smallest change.** Draw each pass in `figures.passes` into the cascades
with `skinned_shadow`, as `self.skinned` is. Nothing for a mod to do.

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
