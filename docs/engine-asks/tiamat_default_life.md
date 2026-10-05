# Engine asks from Tiamat Default Life

From the `tiamat_default_life` mod (vitals, the HUD, creatures, world modes;
repo `Tiamat_Default_Life`, beside the engine). Kept here rather than in
that repo so the engine agent finds every mod's asks in one place.

Numbered as in that repo's `docs/engine-asks.md`, which keeps the history:
everything that landed is recorded there, and only the open asks are here.
Each entry says what was seen, why the mod cannot fix it, and the smallest
engine change that would. Newest first. Items are removed when they land.

**Open as of 2026-10-05: 20**, a creature drawn at its pitch. 16 to 19 landed; 18 and 19 await the eye. New ones
go at the top, newest first.

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

## 20. A creature is drawn level, whatever its pitch (2026-10-05): LANDED 2026-10-05 (engine ed5826d2), awaiting the eye

**Seen.** A spider climbs walls now (mobs.lua, `climbs`): walking into one,
it goes up the face and over the top. The mod pitches it nose up the wall
with `game.set_entity(id, { pitch = math.pi / 2 })` while it climbs, and
back to 0 over the top. It is drawn level all the same: a spider on a
wall is a spider standing on nothing, its legs sticking out of the face,
rising.

**Why the mod cannot fix it.** A creature's figure is placed by its yaw
alone. `crates/client/src/render/skinned.rs` hands the shader
`placement: [figure.yaw, base, 0, 0]`, and `render/mod.rs` builds the
figure's matrix from `Mat4::from_rotation_y(figure.yaw)`. The entity's
`pitch` is kept (`ent::component::Transform.pitch`) and replicated as an
`i8`, and is used for `facing`, but never reaches the figure. A mod has no
other hand on how a model is turned.

**Smallest change.** Turn a mod creature's figure by its pitch as well as
its yaw: `from_rotation_y(yaw) * from_rotation_x(-pitch)`, about the
middle of its collider, so a body pitched a quarter turn up lies against
the face it climbs rather than pivoting off its feet. The pitch is already
on the wire. A player's own figure (and a rider) would want to stay level,
so it could be mod creatures only, or a flag on `register_model` /
`spawn_entity` (`pitches = true`), which would also suit a bird diving or
a fish nosing down. Collision is untouched: the box stays upright, which
is right for a body that is only pressed against a wall.

**Landed.** A figure drawn as a mod's model is turned by its entity's pitch
as well as its yaw, `Ry(yaw) * Rx(-pitch)` about the middle of its collider,
so a body pitched a quarter turn up lies against the face it climbs. Pitch
`+pi/2` is the camera's looking straight up: the model's forward `+Z` goes
onto `+Y`. The engine's humanoid (players), the mount a rider sits on and
model blocks are never pitched; there is no flag and no wire change, and the
box stays upright. **What the mod should do:** nothing new. Set `pitch` with
`game.set_entity` as it does, and a creature that never sets it stays level.
**[H] awaiting the eye:** the spider lies against the wall it climbs, and
levels out over the top.

## 19. A creature is lit as if it stood in the open, wherever it is (2026-09-29): LANDED 2026-09-29 (engine 847a908), awaiting the eye

**Seen.** In play, every creature is lit the same everywhere: a bat hanging
in a pitch-dark cave, a scurrier in a tunnel and a cow in noon sun are
equally bright, so a cave's animals seem to glow against the dark rock round
them, and a mob under a canopy or in a torchlit room is no darker or warmer
than one in a field.

**Why the mod cannot fix it.** How a model is shaded is the client's alone.
`crates/client/src/render/skinned.rs` says it outright: figures are "lit by
the sun, the ambient and the fog and nothing else", and `skinned.wgsl`
shades `albedo * (sun + sky)` from the frame's globals. The light the world
has PROPAGATED to the block the figure stands in (the sky light that is 0
at the bottom of a cave, and the block light a lamp or lava gives), which
every voxel face beside it is lit by, never reaches the figure. A mod has
no hand in the shader, and no knob on an entity for brightness.

**Smallest change.** Light each figure by the light where it stands: when
the client builds a `Figure`, sample the propagated light at the block its
body's centre is in (the same sky and block channels the mesher lights a
face with there), put it on the instance, and in `skinned.wgsl` scale the
sun and sky terms by the sky channel and add the block channel's colour in
place of the flat ambient, the way a voxel face is lit. A figure in the
open looks exactly as it does now; one in a cave goes dark, and one by lava
glows orange. The same would serve the engine's own humanoid (other
players), which has the same flat lighting.

**From the engine, 2026-09-29 (engine 847a908).** Landed as asked, and
for the engine's own rig too: other players, and the player's own body in
third person. Nothing for the mod to do and nothing on the wire: the
light was already sent, and the client now reads it where each figure
stands.

- **Where it is read.** At the feet, the middle and the head, from the
  entity's collider, and the brightest of the three a channel: light is
  stored a block and a body is rarely in one. Never above the head, so a
  scurrier in a tunnel a block high is not lit by the sky over the floor
  above it. A creature registered with no collider is read at its feet.
- **How it is lit.** The sun and the sky reach a figure as far as the
  sky reaches where it stands. A lamp or lava lights it in its own
  colour, with the falloff a wall beside it has. Where neither reaches,
  it has the floor the rock round it has: dark as the cave is dark, and
  never black.
- **In the open it is exactly as it was**, by day and by night.
- **What it does not do.** A figure is one light over its whole body: a
  cow half in a doorway is lit as its brighter half. And a dropped item
  or a block held in a hand is a prop, drawn by another pass, and is
  still lit as before, so a dark figure in a cave holds a bright block.
  That pass is being rewritten for the cut of several materials (UI 18)
  and gets the same light when that has landed.

Tests: `a_figure_is_lit_by_the_light_where_it_stands`, the pixels a
figure covers in the open, in the dark, half shaded and beside a warm
lamp, in all three lighting modes; and
`a_body_is_lit_by_the_brightest_of_the_blocks_it_stands_in`, the
sampling. What it looks like is the designer's ([H]): a bat in a dark
cave against the rock, a cow at noon, and a creature beside lava or a
torch.

## 18. Riding: a player seated on an entity, driving it (2026-09-23): LANDED 2026-09-30 (engine 7974abc), awaiting the eye

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

**From the engine, 2026-09-28 — still open, and how it will be built.**
Not landed with the day's batch because it is the one ask that changes
what a player's body IS, and that reaches the client's prediction
(charter rule 2: one simulation, mirrored). The shape the engine will
take, so the mod can plan against it: `game.mount(player, entity, { seat })`,
`game.dismount(player)`, `game.mounted(player)` as asked; the mount is a
field on the server's body (`PlayerSim.riding: Option<(EntityId, seat)>`),
and while it is set the player's intent drives the ENTITY through the
same `phys::step` with the entity's own tuning and collider, the body is
placed at the mount's transform plus the seat every tick, and a
`PlayerState` carries the mount's id so the client predicts the mount's
body with the same function — which is a protocol bump (80: 77 went to
the weather's bolt and the dialog tooltip on 2026-09-28, 78 to the
cave fog the same evening, 79 to the click on a dialog's button on
2026-09-29, and 0.2.2 ships at whatever main carries when it is cut). Sneak dismounts, an `on_dismount` hook (an observation,
like `on_player_move`) says where they land, and a despawned mount drops
its rider on the tick it goes. Order of work: server seat and drive with
a bot test (a bot mounts a scarecrow, walks, the scarecrow moves and the
bot rides), then the wire and the client's prediction, then the camera.

**From the engine, 2026-09-30 (engine 7974abc, protocol 80).** Landed, as
asked and as planned above, with the differences written out below.

```lua
game.mount(player, entity, { seat = { x = 0, y = 1.6, z = -0.3 }, sneak_dismounts = true })
    --> true, or nil and a reason
game.dismount(player)      --> whether they were riding
game.mounted(player)       --> the entity's id, or nil
game.register_on_dismount(function(e)
    -- e.player, e.entity, e.reason, e.domain, e.x, e.y, e.z (where they were put)
    -- e.reason is "sneak", "dismount", "gone" or "leave"
end)
```

- **The seat** is in blocks from the mount's feet, measured as if the
  mount faced north, and it turns with the mount. An axis left out is 0.
  With no seat at all the rider is put on top of the mount's collider.
- **The keys drive the mount**, at the mount's `speed` and with its
  collider, through the same step every creature takes, and the client
  predicts it: five seconds at a gallop came back with no correction at
  all. The mount faces where the rider looks, so the keys work with no
  code in the mod; the mount's own `drive` and any yaw the mod writes are
  ignored while it is ridden.
- **Refusals are `nil` and a reason, never an error**, because each can
  happen to a mod doing nothing wrong: `"not connected"`, `"no such
  entity"`, `"a player"`, `"no collider"` (or one over 16 blocks),
  `"another domain"`, `"ridden"` (one rider to a mount) and `"already
  riding"` (one mount to a rider; asking again for the mount already
  ridden moves the seat and keeps the ride). A mistake in the call itself
  is an error: a seat that is not numbers, an option nobody knows.
- **Ways off.** Sneak, unless the seat was given `sneak_dismounts =
  false`, when sneak walks the mount at a crawl instead. `game.dismount`.
  The mount despawned, which drops the rider in the same call, or gone
  any other way. The player leaving. Each is one `on_dismount`, after the
  creatures have moved, and the rider is put at the mount's feet.
- **Where they land is yours**, by calling `game.move_player` inside
  `on_dismount`: it reaches the client with the same tick's state. The
  hook stays an observation and returns nothing.
- **`game.move_player` on a rider ends the ride** where it put them, and
  `game.push_player` on a rider pushes the mount. Otherwise both would
  have done nothing.
- **A rider's abilities stay theirs.** A player who may not sprint still
  gallops: the pace is the horse's. Only flight is taken from a rider.
- **Nothing about a ride is saved.** Leaving gets off, a shutdown is
  everybody leaving, the mount is saved as the creature it always was,
  and a player comes back on foot.

What it costs: fifty riders on fifty creatures are 0.02 to 0.04 ms a
tick, 0.04% to 0.07% of the 50 ms budget, which is less than the same
hundred bodies walking apart, because a ridden pair takes one step.

Not there: a sitting pose. The rider's figure stands on the seat in the
idle clip until a model has a clip for it. And two things the work
turned up in the engine that are not riding's and are not fixed: a
creature creeping less than a twelfth of a cell a tick is never sent
again to a client that already has it, and a bot script that walks
after it has slept files its inputs under ticks the server has passed.

For the eye ([H]): right-click a creature whose mod mounts it, walk,
sprint and jump, look round so the mount turns and the seat swings with
it, V for third person, sneak to get off, and have the mod despawn the
creature under you. Another player should see you on it.

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

