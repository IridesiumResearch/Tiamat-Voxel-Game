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

**Seen.** Cows and pigs walked at a player's walk, 4.3 yards a second, and
fled at a player's sprint, 5.6: two to four times what an animal should. A mob's `drive` has gaits and nothing
else, `Intent::walk` is normalised, so a shorter drive is not a slower one,
and `set_player_abilities`' `speed` is for players.

**What the mod does now.** A kind names `walk_speed` and `run_speed` in
blocks a second (the cow and pig: 1.1 and 2.8), drives nothing, and sets
its horizontal velocity to the speed times a gain it nudges each tick by
what the body did. Through `phys::step` on flat ground it settles on
exactly the speed asked, steadily (the gain lands on 1/0.7, the ground
friction). It works; it is a controller standing in for a number, and it
bypasses the gaits a mob was meant to use.

**Smallest change.** `speed` on `drive` (or on the entity), the multiplier
`Abilities::speed` already is for players, through `Abilities::tuning`.
