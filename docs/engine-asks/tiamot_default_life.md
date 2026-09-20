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

Nothing open. Step 2 of ask 0 (a texture on a mod's model) landed
2026-09-20, with the placement it needed: an entity wearing a mod's model was
never put in the world at all, so the cow was on the GPU and not in the frame.
