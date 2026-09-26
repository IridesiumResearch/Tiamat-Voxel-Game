# Engine asks from Tiamat Default Life

From the `tiamat_default_life` mod (vitals, the HUD, creatures, world modes;
repo `Tiamat_Default_Life`, beside the engine). Kept here rather than in
that repo so the engine agent finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

**Open as of 2026-09-26: 18**, below; 16 and 17 landed. New ones go at the top,
newest first.

**From the engine, 2026-09-25 (engine a6d34e1, protocol 76):** a use at
nothing — the place control at open sky, or at a block past reach — now
reaches a mod that registers `game.register_on_use(callback, { anywhere =
true })`, with no cell in the event (`e.x`, `e.y`, `e.z` and `e.material`
nil) and `e.held` as ever; the return ladder reads the same. A callback that
did not ask never sees one. `hooks.lua`'s wrapper now registers that way, an
uncommitted edit in the Life checkout made with the engine change, and both
handlers passed to `tdl.on_use` take a nil cell, so food in hand is eaten
wherever the player looks; the comment above the eat handler in
`actions.lua` says so now. Ask 17 (use an entity) is untouched: an entity
under the crosshair still falls through to the block behind it or, against
the sky, to a use at nothing.

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

## 17. Using an entity: right-click on a mob (2026-09-23): LANDED 2026-09-26 (engine 50462b7)

**From the engine, 2026-09-26 (engine 50462b7):** `game.register_on_use_entity(fn(e))`
— `e.player`, `e.target` (the entity id), `e.owner` (a player's UUID when it
is somebody's body), `e.held` as `on_use` has it — fired when the place
control is pressed with an entity at least as near as any block on the
player's own reach ray, cast by the server against the entities' colliders;
the same return ladder as `register_on_use`, and a use nobody handles falls
through to the block behind it or to a use at nothing, as it did. The
player's own body is never the target, and an entity with no collider has
no box to hit. `game.looking_at` answers `{ domain, entity, owner }` when
that is what the crosshair is on, from the same choice. No protocol change.
The horse is now reachable from here; the seat (18) is still the engine's.


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

## 16. A mod's model is drawn matte white though its skin arrives (2026-09-23, cause found 2026-09-24): LANDED 2026-09-24

**Seen.** In play, every animal is drawn matte white: the rig lit and
shadowed and no colour at all. "Again": they can be right on a first visit
and white after that.

**Cause, reproduced.** The renderer outlives a connection, and
`Renderer::clear_models` ("a model belongs to the server that pushed it")
has no caller. So on a second join (a new world, or back in after the menu)
the renderer still holds last visit's passes. The cache is warm now, and a
warm cache hands the skins over BEFORE the models (every skin first, in a
real server-and-client run on this mod set). Each skin therefore lands on
the OLD pass (`set_model_texture` finds it and sets it there); then the
model arrives and `add_model` builds a fresh pass and inserts it over the
old one, with no skin waiting in `figures.skins` because none was held. A
white pass, for every model, for the rest of the session.

Checked from this side, through the engine's own crates (a scratch program
outside the engine repo, `client` and `server` as path dependencies):

- the client's renderer draws the horse brown with its skin, white without,
  in Simple, Classic and Beautiful, skin-then-model or model-then-skin;
- a real `ServerHandle` on the real mod set, with a real `Connection`, cold
  cache and warm, delivers all eight models and all eight skins with the
  right colours;
- the same renderer given a first visit (model, skin) and then a rejoin
  (skin, model) draws the horse white in every lighting mode: the bug.

**Smallest change.** Call `renderer.clear_models()` when a connection ends
or a new one begins, which is what its own doc says should happen. And, so
a re-sent table can never do this either, have `add_model` keep a skin the
pass it replaces was wearing when none is waiting (or have
`set_model_texture` always remember the last skin per id, rather than
remembering it only while no pass exists). A screenshot test of the rejoin
order would have caught it; `connection.rs` proves a skin arrives, and
nothing proved it is drawn.

**Landed 2026-09-24**, both halves of the smallest change: a connection's
end forgets the models it was pushed, as `clear_models` always said it
should and nothing called; and the renderer keeps the last skin per id
whether or not a pass exists, so a re-sent model puts on the skin the pass
it replaces was wearing. The rejoin order — skin, then model — is now the
second half of `a_mods_model_wears_the_skin_it_was_pushed`, and the
painted frame comes out of it.

