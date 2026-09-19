# Engine asks from Tiamot Default Life

From the `tiamot_default_life` mod (vitals, the HUD, creatures, world modes;
repo `Tiamot_Default_Life`, beside the engine). Kept here rather than in
that repo so the engine agent finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

Landed and in use as of 2026-09-19: 0 steps 1 and 3 (15302d1), 1 and 9
(dc3b5ee, 82444e7), 2 (eab4c2d), 3, 4, 5, 7 and 8 (a3db9fa).

## 11. `fell` counts a flight down to the ground (2026-09-19)

**Seen.** `fell` is the descent since the feet last left the ground,
settled on the tick they land, and `measure_fall` adds a flying body's
descent to it like any other. So an operator who flies down twenty blocks
and touches the grass lands with `fell = 20`, the same number as somebody
who stepped off a cliff.

**What the mod does now.** It keeps its own last-tick vertical speed and
only hurts a landing that was moving down fast. That is a reconstruction
of "were they flying", which the engine knows exactly: flight is an intent
it steps with.

**Smallest change.** Accrue nothing while the body is flying, so a fall
starts when flight stops. Or say `flying` on the entity table beside
`on_ground` and let the mod decide. The first keeps `fell` meaning "fell".

## 10. The sky keys need a permission; the debug keys only need to move (2026-09-18, revised)

*Revised after the engine's answer. The first draft asked for operator gates
on all of these, which was plumbing for nothing: a control that cannot move
your body or change the world needs no permission.*

**10a. The sky keys are a cheat, and unbinding them is not a fix.**
`engine:time_back` / `time_forward` / `time_resync` wind the player's OWN
sky. Nothing on the server changes, but the client draws stored sunlight
scaled by the sky's intensity, so winding to noon lights the player's
night: seeing in the dark for free, in a survival or a one-life world.
Taking the default keys away does nothing, since anybody can bind them
again. **Ask:** a server permission, decided where flight is: the server
tells the client whether this player may wind the sky, and a client that
may not ignores the action whatever it is bound to.

**10b. The rest just move off the letters.** `teleport_far` /
`teleport_home` shift the render origin for the floating-point check and do
not move the body; `material_row` is singleplayer-only already;
`chunk_borders` draws lines. None is a cheat. They hold Y, H, G and B
(`crates/client/src/input.rs`), which are keys a game wants: the Life mod
had to move its wardrobe off G. **Ask:** defaults on function keys only
(F4 chunk borders, F9 material row, F7/F8 teleports with no letter twins).
No permission.

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
