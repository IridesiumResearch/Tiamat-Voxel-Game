# Engine asks from Tiamat Default Life

From the `tiamat_default_life` mod (vitals, the HUD, creatures, world modes;
repo `Tiamat_Default_Life`, beside the engine). Kept here rather than in
that repo so the engine agent finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

**Open as of 2026-09-23: 16, 17 and 18**, below. New ones go at the top,
newest first.

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

## 18. Riding: a player seated on an entity, driving it (2026-09-23): OPEN

**Wanted.** Right-click a horse and you are on it: sat on its back, the
camera up where a rider's eyes are, your movement keys driving the horse at
the horse's pace, and the use (or sneak) control to get off. The designer
asked for exactly that.

**Why the mod cannot do it.** Nothing seats a player on anything. The
workarounds are all the rubber-banding kind the brief warns against:
`move_player` onto the horse every tick fights the player's own prediction;
`set_entity` on the player's body is overwritten from their inputs the next
tick; dragging the horse under the player with `pos` teleports it and
leaves the player standing inside it with their eyes at their own height.
`set_player_abilities{ speed }` could give the gallop, but not the seat,
the camera or the horse moving as the body.

**Smallest change.** A mount, owned by the engine because it is movement:

```lua
game.mount(player, entity, { seat = { x = 0, y = 1.6, z = 0 } })  -- blocks above the entity's feet
game.dismount(player)                                                -- or nil to ask
game.mounted(player)   --> entity id | nil
```

While mounted, the player's walk/jump/sprint intent drives the ENTITY (its
`speed`, collider and step, predicted on the client as a player's body is),
the player's body rides at the seat and turns with it, and the camera sits
at the seat plus eye height. Sneak dismounts by default, and
`register_on_dismount` (or a field on the event of whatever dismounted
them) lets the mod say where they land. A mount that is despawned or dies
drops its rider.

## 17. Using an entity: right-click on a mob (2026-09-23): OPEN

**Wanted.** The place control on an animal does something: get on a horse,
later milk a cow, shear a sheep, feed a pig.

**Why the mod cannot do it.** `register_on_use` fires only at a BLOCK in
reach, and `register_on_punch` only for the dig control. An entity under
the crosshair is invisible to the place control: the use either falls
through to the block behind it or, against the sky, to nothing at all.
Casting a ray in Lua from `look_direction` against every mob's box is the
per-sample work at the wrong altitude that the brief says `looking_at`
exists to prevent.

**Smallest change.** The punch event's twin, for the place control:

```lua
game.register_on_use_entity(function(e)
    -- e.player, e.target (entity id), e.owner (a player's UUID if it is one),
    -- e.held (what is in the hand, as on_use has it)
    return ""        -- handled: the same return ladder as register_on_use
end)
```

fired when the place control is pressed with an entity nearer than any
block along the player's own reach ray, before `on_use` (which then does not
fire). And `game.looking_at` answering an entity when that is what the
crosshair is on, as `{ entity = id }`, so the between-events question has
the same answer.

## 16. A mod's model is drawn matte white though its skin arrives (2026-09-23): OPEN, a BUG

**Seen.** In play, every animal (cow, pig, sheep, horse, bear, crow, bat)
is drawn matte white. A screenshot of the bear shows the rig lit and
shadowed and no colour at all.

**What was checked, from this side.** Each kind calls
`register_model{ id, file = "models/<kind>.glb", texture =
"models/<kind>.png" }`. The skins are 128x128 8-bit RGB PNGs with no white
in them anywhere, so any sampling at all, even with the wrong UVs, would be
brown; they decode cleanly with png 0.18.1 and
`normalize_to_color8`, the client's own decoder and settings. Every model
and every skin is in the player's content cache under its
`tiamat:content:v1` hash, so the bytes reached the client. The engine's
reader keeps the UVs (`model_check`'s pose dump carries them, and
`tools/render_model.py` paints the rig correctly from them). The client
build was from 2026-09-23, after adf6547.

Reading the engine: `registered_models` keeps `texture`, the server hashes
it by the same `hash_of` as the `.glb`, `offer_model_texture` decodes it
and `set_model_texture` either sets it on the pass or holds it for
`add_model`, and `fragment_main` multiplies by the sample. Nothing wrong
is visible; what is missing is a test. `connection.rs` proves a skin
ARRIVES, and nothing proves one is DRAWN.

**Smallest change.** A screenshot test that pushes a model with a
one-colour skin and reads the colour back off the frame, which will find
where it goes white. If the client printed "has a texture that would not
decode" the player did not see it; if that warning is the cause, it wants
to be louder.
