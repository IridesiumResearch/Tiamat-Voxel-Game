# Engine asks from Tiamot Default Life

From the `tiamot_default_life` mod (vitals, the HUD, creatures, world modes;
repo `Tiamot_Default_Life`, beside the engine). Kept here rather than in
that repo so the engine agent finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

Landed and in use as of 2026-09-20: 0 steps 1 and 3 (15302d1), 1 and 9
(dc3b5ee, 82444e7), 2 (eab4c2d), 3, 4, 5, 7 and 8 (a3db9fa), and 10 and 11
(990bf8a).

## 0. A texture on a mod's model (2026-09-11, step 2 of 3)

**Seen.** Step 1 landed and works: the cow is registered with
`game.register_model` and drawn as itself, animated by its own clips. It is
matte white. `skinned.wgsl` draws every figure with a fixed albedo ("the
rig is untextured and skins are a later phase"), and `register_model`
refuses any field but `id`, `file` and `scale`, so a mod has no way to name
a texture at all. The reader already refuses embedded images, which is
right; the model's UVs are there and have nothing to sample.

**What the mod does now.** Ships `models/cow.jpg` beside `models/cow.glb`
and keeps `texture = "models/cow.jpg"` in the creature's definition, not
passed to the engine. Its offline renderer (`tools/render_model.py`, fed by
the engine's own skinning through `model_check`) shows the textured cow is
correct: the UVs, the assembly and every clip.

**Smallest change.** `texture = "models/cow.png"` on `register_model`: one
image inside the mod, pushed by hash as block textures are, sampled by the
mesh's own UVs in the skinned pass. No texture stays matte white, so every
existing call is unchanged. The engine's own humanoid can take the same
path later.
