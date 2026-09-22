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
