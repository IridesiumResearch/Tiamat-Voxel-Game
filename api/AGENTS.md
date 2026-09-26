<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: MIT -->

# Writing a Tiamat mod — a brief for AI assistants

**Audience: an AI coding assistant that has been asked to write a mod, and the
person supervising it.** Copy this file to the root of the mod project as
`AGENTS.md` and most assistants will load it automatically. It is MIT, like
everything in `api/`, so it can be vendored anywhere.

Read [`stubs/game.lua`](stubs/game.lua) before writing a line. It is the whole
API — every function, its options table, every field, and why each behaves as it
does — and `scripts/check-stubs.sh` fails the engine's build if a `game.*`
function exists that it does not document. **It cannot fall behind the engine.**
If something is not in there, it does not exist; do not invent it.

---

## What a mod is

A directory with two files:

```
my_mod/
  mod.toml    -- the manifest
  init.lua    -- runs once, at load
```

Dropped into the server's mods directory (`game/` in this repository). Lua 5.4:
you have an integer subtype, `//`, and integer `%`, and you will use all three.

`mod.toml` — `id`, `name` and `version` are required, the rest optional:

```toml
id = "my_mod"           # letters, digits, underscore. Your namespace.
name = "My Mod"
version = "0.1.0"       # semver
depends = ["core >=0.1"]
conflicts = ["core_ui"] # mods this one replaces: the engine refuses to load both
description = "One line."
license = "MIT"
```

`conflicts` is for a mod that replaces another outright — an inventory screen
beside the reference one is two hotbars, not two features. Between two
ordinary mods the set is refused: the server does not start and `--check-mods`
fails, naming both and the way out (`enabled_mods` in the server's config, or
the mod list when a world is made). **Against one of the engine's own reference
mods it is different**: the fixture stands aside and yours loads in its place,
with nothing to disable by hand. It reaches a mod through an alias it
`provides` as well, and a mod that `provides` a reference mod's id puts it aside
the same way.

The mods under `game/core_*` carry `reference = true`. It means "a fixture,
not content": they load before every other mod, lose a tie the lowest id
would otherwise win (the sky, the cloud deck), step aside for a mod that
replaces them, and the start screen folds them away. A mod of yours must not
set it.

Validate without launching the game — this is the fast loop, and it catches
typos, namespace errors and load-order problems in seconds:

```console
cargo run -p server -- --check-mods <mods-dir>
```

It prints the mods that loaded, in dependency order, and every block that
registered. A mod that fails to load is disabled and named; it does not take the
server down.

---

## The six things an assistant gets wrong

These are unusual. An assistant carrying habits from other voxel engines will
violate all six without noticing, and most of them fail quietly.

### 1. Never compute simulation values in Lua

**The single most important rule.** The engine guarantees the same seed produces
bit-identical worlds on Linux, Windows and macOS. That rests on restricting
which floating-point operations run, and it cannot police what happens inside a
script — `x^0.5` in your mod is a platform library call and its last bits differ
between machines.

So ask the engine for whole buffers and hand them to whole-buffer operations:

```lua
-- RIGHT: one native call fills all 256 columns, another consumes it.
local heights = game.noise_heightmap(pos, { octaves = 5, frequency = 0.008, amplitude = 110.0 })
buf:fill_below_heightmap(heights, stone)
```

```lua
-- WRONG: there is no per-sample entry point, by design.
for x = 0, 15 do for z = 0, 15 do
    local h = math.floor(noise(x, z) * 24)   -- does not exist, and could not
end end
```

If you need a trig value, the engine has `game.heading(dx, dz)`. Do not reach
for `math.atan`, `math.sin` or `^`.

The same rule has a quieter form: **if the step already measured it, read it
rather than re-derive it.** `game.entity` reports what the physics actually
acted on this tick, and a mod's own reconstruction is not the same number:

| Read this | Instead of | Because |
|---|---|---|
| `e.submerged` | probing the blocks round a body | a body's box is not a block, and the engine's answer is the one that moved it |
| `e.fell` | watching `velocity.y` for a landing | vertical speed is clamped at terminal velocity, so a fall of forty blocks and one of four hundred land at the same number — and water slows a body before it touches down |
| `e.facing` | `math.sin(e.yaw)` | libm, so two servers throw the same item to different places |

`fell` is blocks, and it is set on the one tick a body lands and zero on every
other, so a fall rule is one `if` in a tick hook with nothing to remember.

### 2. Quantities are integer units, 27 to a block

A block is 3x3x3 sub-nodes. Every inventory quantity, every drop, every cost is
in **units**, stored as integers. Display is `units // 27` blocks plus
`units % 27` nodes. There are no fractional blocks and no special case for
partial ones. `game.inventory` reports each stack as
`{ material, units, blocks, nodes, count, shape, detail }` — `units` and `count`
are different numbers and confusing them is the commonest arithmetic bug here,
and `blocks`/`nodes` are already worked out for you, so do not divide again.

### 3. String IDs are canonical; numbers are per-session

`"core:white"` is the identity. The numeric ids `game.get_block_id` hands back
are **per-session and mean nothing across runs** — never persist one, never
hard-code one, never compare one against a number from somewhere else.
`game.block_of` converts back. Ids you register are namespaced with your mod id
automatically: `game.register_block{ id = "brick" }` gives `my_mod:brick`.

### 4. Registration happens once, then the world freezes

`register_block`, `register_sky`, `register_tool`, `register_domain`,
`register_action`, `register_fluid`, `register_item` and friends work **only
while `init.lua` is running**. After the load phase the registries freeze and
calling one is a hard error. Anything conditional on the world, the player or
the time of day belongs in a hook, not in registration.

Hooks (`register_on_tick`, `register_on_chat`, `register_on_place`,
`register_on_dig_complete`, `register_on_use`, `register_on_generate`, …) are
registered in the window and called for ever after.

**Right-clicking a block with nothing to place is `register_on_use`**, not a
cancelled dig. Picking fruit, opening a door, pulling a lever: the event has the
cell, what it is made of and what is in the hand, `game.get_block` works inside
it, and returning `""` says you handled it. Return `nil` for blocks that are not
yours, so the next mod — and in the end the engine's own "nothing selected"
warning — gets its turn. **Right-clicking at nothing** (open sky, or past
reach) is a use too, heard only by a callback registered with
`{ anywhere = true }`: it comes with no cell — `e.x` and `e.material` nil — and
`e.held` as ever, which is how a meal is eaten wherever the player looks. A
callback that did not ask never sees a use without a cell.

**Between events, `game.looking_at(uuid)` says what a player's crosshair is
on** — the same `{ x, y, z, domain, material }` a use event carries, plus the
face. Use it when you need the answer *now* rather than when somebody pressed
something: a label under the crosshair, a key bound to "interact with what I am
pointing at". It casts the engine's own ray, bounded by the player's reach;
walking one in Lua from `game.entity(body).facing` is per-sample work at the
wrong altitude, and the engine already has the traversal.

**A HUD script's pictures must be registered: `game.register_picture{ file }`.**
A dialog's tree is its own manifest and its pictures are fetched when it
arrives, but a HUD script names a picture only when it draws one, so nothing
had ever fetched the bytes and `hud.image` drew the "not arrived" box for ever.
`register_picture` puts the file in a table the client fetches on join and
answers its content hash in hex; `game.content_hash(file)` answers the hash
alone, for a dialog's `image` or `nine_slice`. Neither hash is pasted by hand.

**The top of a column is one call: `game.surface_at{ x, z, from, depth,
skip_passable, skip_fluid }`.** Snow, rain, a spawn point, anything that has to
find the ground as it is NOW — built on and dug since worldgen, which no
heightmap knows — asks the engine, which walks the column natively and resolves
each chunk once. Do not loop `game.get_block` down a column; that is a VM
crossing per block. `nil` means an unloaded chunk, nothing within `depth`, or
no world (worldgen), which are all "nothing to land on here".

### 5. Worldgen: describe the field, never sample it

The rule above says no per-sample maths. That does not mean no 3D terrain — it
means you describe the expression and the engine evaluates it:

```lua
-- ONCE, in init.lua. Not per chunk.
local field = game.density{
    op = "min",
    a = {                                    -- terrain: noise falling off with height
        op = "sub",
        a = { op = "noise", stream = "terrain", frequency = 0.03, octaves = 3 },
        b = { op = "mul", a = { op = "y" }, b = { op = "const", value = 0.06 } },
    },
    b = {                                    -- caves: tunnels where the noise is near zero
        op = "sub",
        a = { op = "const", value = 0.35 },
        b = { op = "abs", a = { op = "noise", stream = "caves", frequency = 0.05 } },
    },
}

game.register_on_generate(function(buf, pos)
    buf:fill_density(field, stone)           -- solid wherever the field is > 0
end)
```

That is overhangs, arches and caves in one mechanism. Give each noise node a
different `stream` name or your caves will follow your hills exactly. A noise
round in every axis reads as lumps; `stretch = { y = 4 }` on a noise node makes
its features four times as tall without making them wider, which is cliff rock,
and `stretch = { x = 6, z = 6 }` lays them out flat, which is strata. The full
operation list is in the stubs, and it is short on purpose: every operation is
in the deterministic subset, so there is no `pow`, `sin` or `sqrt` and asking
for one is asking to break the cross-platform guarantee.

**Erosion and rivers need a pre-pass, not a density field.** A density field is
a function of one position. Where water goes depends on where the land is
everywhere else, so it cannot be one — it needs a whole field computed once, in
passes that see all of it:

```lua
local function field()
    return game.map{ name = "height", side = 256, scale = 16 }
end

game.register_on_world_init(function()          -- ONCE in a world's life
    local map = field()
    map:noise{ seed = 7, frequency = 0.01, octaves = 4, amplitude = 40.0 }
    local worn = game.map{ name = "worn", side = 256, scale = 16 }
    worn:noise{ seed = 7, frequency = 0.01, octaves = 4, amplitude = 40.0 }
    worn:blur(3)
    map:combine(worn, "min")                    -- valleys cut, peaks kept
end)

game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(field():heightmap(pos), stone)
end)
```

The map is stored with the world, so the pre-pass runs once and every later run
reads it back. **There is no `erode` and there will not be one** — what erosion
looks like is an opinion about what a landscape is, and that is yours. Blur and
`combine("min")` are the primitives it is built from.

**A map and a density field feed each other, which is what makes erosion work
on terrain a heightmap cannot describe.** `map:fill(density)` reads a surface
into a field; `{ op = "map" }` reads a field back into a surface. Without both
you can erode a heightmap and nothing else — a world with a dome, overhangs or
caves could compute an eroded field and have no way to use it:

```lua
game.register_on_world_init(function()
    local land = game.map{ name = "land", side = 256, scale = 16 }
    land:fill(game.density(SURFACE), { y = 0.0, seed = 99 })   -- surface -> field
    local worn = game.map{ name = "worn", side = 256, scale = 16 }
    worn:fill(game.density(SURFACE), { y = 0.0, seed = 99 })
    worn:blur(3)
    land:combine(worn, "min")
end)

game.register_on_generate(function(buf, pos)
    local eroded = game.density{                               -- field -> surface
        op = "sub",
        a = { op = "map", map = game.map{ name = "land", side = 256, scale = 16 } },
        b = { op = "y" },
    }
    buf:fill_density(eroded, stone)
end)
```

`y = 0` is where a field of the form `noise - y` changes sign, so that is its
height. The `map` node takes a COPY of the map as it is when `game.density` is
called — a program reading a live map would generate different terrain after
your next `blur`, and the seam between the two would be permanent and invisible.

**A field says what it cannot be, and the engine skips the rest.** Before every
`fill_density` the engine asks your program what values it could possibly take
over that chunk, and generates nothing at all where the surface cannot reach.
You pay nothing for this and need not call anything. `density:bounds(pos)` is
the same answer, for skipping work of your own:

```lua
if surface:bounds(pos).all_empty then return end     -- nothing here but sky
```

**Pass the `pos` your generator was handed**, not a table you built. It carries
the world seed, and a bound over a box is a fact about one world's field — the
interval that holds for every seed is the one that says the same thing in every
chunk, which is the thing this call exists not to be. A `pos` without a `seed`
is an error rather than a guess.

**This is how you pick a biome without running every biome.** `bounds` answers
`low` and `high` as well as `all_empty` and `all_solid`, so a selector field —
coherent noise, no height term — tells you which biomes a chunk can hold:

```lua
local WARMTH = { op = "noise", stream = "warmth", frequency = 0.001, octaves = 2 }

game.register_on_generate(function(buf, pos)
    local warmth = game.density(WARMTH):bounds(pos)
    if warmth.low > 0.0 then      generate_warm(buf, pos)    -- cold ruled out
    elseif warmth.high < 0.0 then generate_cold(buf, pos)    -- warm ruled out
    else                          generate_both(buf, pos)    -- the chunk straddles
    end
end)
```

**How much it can skip is set by your field, not by the engine**, and the rule
is that a box cannot bound features smaller than itself. Two measurements:

- A terrain field's chunks decided outright — the engine's own skip. The
  surface sits where your noise balances your height term, so the band that
  cannot be decided is the noise amplitude divided by that term's coefficient.
  `noise(11) - y` has **93%** of its chunks decided and runs four times faster;
  `noise(40) - y * 0.08` is 500 blocks of real relief, and none of it can be
  decided. Keep your relief small against your view distance — and note that
  dividing the height term down is the same thing as scaling the relief up.
- A selector field's chunks landing wholly on one side of a threshold, which is
  what the biome skip above turns into work saved: **72%** at frequency 0.001,
  60% at 0.002, 4% at 0.004, and none at all by 0.01. So choose biomes from a
  WIDE field. A selector whose features are smaller than a chunk cannot be
  bounded by one, and you will run every biome in every chunk again.

**Do not write your own version of this by sampling the corners.** Nine samples
over a chunk are not a bound: noise between two samples is not bounded by those
samples, so a chunk whose corners agree can still contain surface, and skipping
it leaves a hole. It fails exactly where feature size drops below sample
spacing, which is to say at your caves and your ore, while a gentle heightmap
survives it — so "I tried it and it looked fine" is not evidence.

**Water is placed at generation or not at all.** The fluid solver conserves
volume — it moves what exists and creates nothing — so there are no sources and
nothing pours a sea into being later:

```lua
game.register_fluid{ id = "water", material = "my_mod:water_block" }
-- ...then, inside on_generate, after the terrain:
buf:fill_fluid_below(0, "my_mod:water")      -- sea level at y = 0
```

Fluid is a layer over the same blocks, not a material. How much goes into a
block is the room the terrain leaves it, out of 27 cells, so a shoreline falls
out of the terrain rather than having to be described.

**Water you place stays where you put it.** Three or more blocks of fluid that
touch, each holding all its block has room for, are a *body* — a river, a pond,
a sea — and **loading a chunk never wakes a body**. That is why a generated
river does not run off down the hillside the moment somebody flies past it, and
it is what the engine can promise instead of the source blocks a conserved model
cannot have. Sub-Node Contract §4.5.

Two things follow that are worth designing around:

- **A body is at rest, not frozen.** Anything that touches it — a player digging
  its bank, a bucket, a block placed in it — wakes it, and from that moment it
  flows like any other water. A river whose banks your terrain never built will
  run the first time a player breaks into it. If you want one that survives
  being dug, give it banks (`fill_fluid_terraced`'s `lip`, or terrain).
- **Loose water still behaves.** One or two blocks is a spill, not a body, and
  so is any block not filled to its capacity. It is woken on load and settles as
  it always did, which is what keeps a bucket honest.

**Lava is water with two fields changed.** There is no lava in the engine and
there will not be — it is content — but the two things that make one are
mechanisms, and both are here:

```lua
game.register_block{
    id = "molten", texture = "lava.png",
    light_emit = { r = 15, g = 8, b = 2 },   -- the glow
}
game.register_fluid{
    id = "lava", material = "my_mod:molten",
    opacity = 1.0,                            -- a surface, not a window
    tick_rate = 4,                            -- and slower than water
}
```

- **`opacity`** is how much of the world behind it a surface of the fluid hides,
  `0.0..=1.0`. The default is `0.72`, which is what every fluid was drawn at
  before the field existed: you can make out a riverbed through it. `1.0` is
  lava. `0.0` is invisible, which is allowed and is not the same as saying
  nothing.
- **A fluid glows with whatever its MATERIAL emits.** There is no `glow` field
  on `register_fluid`, deliberately: a fluid already names a block, and a second
  place to declare brightness is a second thing to keep in step. Put
  `light_emit` on that block and a pool lights the cave it stands in, a flow
  relights as it moves, and a bucket of it lights the room you carry it into.

A block full of lava is AIR in the block store — a block holds terrain and fluid
independently — so the engine looks at the fluid layer on purpose to find this.
It costs nothing in a world whose fluids do not emit: the check is a set that is
empty unless some fluid's material declares `light_emit`. **Emission does not
scale with volume.** One cell of lava in a block glows as brightly as
twenty-seven, for the same reason a lamp chiselled to a sliver is still a lamp —
dimming by how much is left would make the solver a dimmer switch, which is a
game decision and not the engine's.

**Heightmaps still exist and are still right** when a heightmap is what you
mean. `game.noise_heightmap` + `buf:fill_below_heightmap` is 52 us a chunk
against 719 us for terrain-with-caves — fourteen times cheaper. Cheaper still
matters even though generation no longer runs on the tick: a worker is a core,
and a chunk that takes 60 ms is a core for 60 ms.

**Your generator runs off the tick, in a VM of its own.** The server loads your
mod set a second time on worker threads — same mods, same order, same fluid ids,
same maps — and `on_generate`, `register_chunk_tint` and `register_chunk_fog`
run there, never on the simulation thread. This is what lets a chunk cost more
than a tick without the
tick paying for it, and it is why generation is a pure function of `pos` (which
carries the seed), your density programs and your maps:

- **Nothing from the tick is visible.** No players, no entities, no edits, no
  `game.get_block` or `game.get_light` — the worker VM has no world to look at.
  A generator that reads any of those was already producing terrain that
  depended on who was standing where, which the determinism gate forbids.
- **`on_world_init` did not run in the worker.** It ran once, in the world's
  life, on the tick — and what it left behind is the maps, which the worker has.
  State it stored in a Lua table is not there. Put what a generator needs in a
  map or derive it from the seed.
- **Lua state persists per worker, not per world.** A counter you keep between
  `on_generate` calls counts one worker's chunks. Log it as such, or do not rely
  on it; nothing a generator writes to a table reaches your mod on the tick.
- **An error disables your mod everywhere.** A generator that errors in one
  worker is disabled in every worker and on the tick (charter rule 10), so a
  world is never terrain from one VM and air from another.

The reference mods hold to all four without trying, because a generator that
describes a field has no reason to reach for anything else.

### 6. Mods register named actions; the engine owns the keys

A mod never reads a key. It declares an action with `game.register_action` and
responds to `register_on_action`; the player binds it in the settings screen.
There is no key code anywhere in the mod API, and asking for one is asking for
the wrong thing.

---

## Your mod's own options

Do not write a config file the player will never find. Declare what you offer
and the client draws it under your mod's name, in the screen they already open:

```lua
game.register_setting{ id = "nameplates", name = "Show name tags", default = 1 }
game.register_setting{
    id = "difficulty",
    name = "How hard the mimics hit",
    options = { "gentle", "ordinary", "unfair" },
    default = 1,
}

-- and where it matters:
if game.setting(uuid, "my_mod:difficulty") == "unfair" then ... end
```

No `options` is a checkbox; with them it is a dropdown. `game.setting` answers
a boolean or the chosen STRING — never the raw index — so comparing against
`"unfair"` keeps working when you insert an option above it, and it answers your
declared default for a player who has never touched it.

**Answers belong to the world, not to the machine.** A player's choices are
remembered per world and per server, so they are still there when they come
back to that server and do not follow them into the next one. That is the same
rule the mod selection follows and for the same reason.

**An answer arrives with a PLAYER, so a setting cannot shape a world.** Worldgen
has already happened by the time anybody joins — for chunks made before the
first player, it happened with nobody to ask. A setting cannot decide how your
terrain is generated, and one that tried would give a world whose shape depended
on who logged in first.

**Options that shape a world are WORLD options**, and they live in `mod.toml`
rather than in Lua — the start screen has to show them before any mod has run:

```toml
[[world_option]]
id = "biome"
name = "Biome"
description = "One biome everywhere, or the whole spindle."
options = ["spindle", "savanna", "taiga"]
default = 1

[[world_option]]
id = "rivers"
name = "Rivers"
default = 1          # no `options` is a checkbox; default 0 or 1
```

The player picks beside the seed box when they make a world. The answer is
stored in the world file and **never changes for that world**, for the seed's
reason: terrain generated later has to agree with terrain generated before.
Every VM that runs the world — the tick's and every generation worker's —
answers `game.world_option("your_mod:biome")` the same, **from the first line
of `init.lua`**, so you may register differently for one world than another. A
choice answers its TEXT, a toggle a boolean, and a world that chose nothing (or
chose something you have since renamed) gets your declared default. A dedicated
server sets them in `server.toml` under `[world_options]`.

Two things follow: read the option ONCE at load into a local, not per chunk,
because a VM crossing per chunk is the cost the whole design avoids; and design
the defaults to be the world you would ship, because a player who does not
open the dropdown gets them.

Key on the UUID, never the display name (charter rule 13).

### Clouds

`game.register_clouds{ ... }` declares the deck once at load and
`game.set_clouds(uuid, { cover, darkness })` says how much of it one player is
under. The client draws it by marching a ray through a **field**, not by
building cubes — which is why a deck can reach the horizon, drift and change
shape without anything being rebuilt, and why you can fly up through one.

**The sky has four genera, a share each.** `cover` is cumulus, the heaps over
the floor. `stratocumulus` is a low sheet of rounded cells with grooves of sky
between them, `altocumulus` a mid-level mackerel sky of small cloudlets in wave
bands, and `cumulonimbus` towers under spreading anvils, supercells at 1. They
are numbers rather than a kind so that a front arriving blends one sky into the
next, and a genus you leave out is none of it — a mod that only ever sent
`cover` sends exactly the sky it always did.

Two things follow that are worth knowing before you design around them. A
column of the deck is up to **three** intervals — the low cloud, an anvil over
it, and a mid-level layer between — which is what lets a mackerel sky sit under
a storm's anvil and over a heap in one column, and is also the limit: a fourth
lobe cannot be drawn. And `darkness` hangs a dark haze under the deck as well
as greying it, darkest at a cloud's base and least on its tops: that, rather
than `set_precipitation`, is what makes a storm read from outside it, because
precipitation spawns around the player's own camera and cannot draw a curtain
of rain over the next valley.

**A storm over the next valley is `map` on `set_clouds`** — a coarse grid of
cover and darkness laid over the world rather than over the player, sampled
where each ray of the deck passes, with the plain `cover` still answering
outside the grid — and the three genera per cell beside them, each optional,
so a storm over the next valley has its sheet and its anvil from the clear
valley beside it. Up to 16 cells a side; at the 256-block squares a weather mod
tends to evaluate that is four kilometres, which is further than the deck is
drawn. Values are shares of one and travel as bytes. Without it, a front cannot
be watched coming: the sky a player sees is overcast everywhere or nowhere.

The deck shades the ground under it along the sun, in the Classic and
Beautiful lighting modes, so a drifting sky reads as drifting from the ground.
Figures are not shaded by it yet.

The player owns the quality: a cloud setting in their own graphics options
scales the deck's resolution and draw distance, down to off. The server is
never told, and a mod must not assume its clouds are being drawn at all.

### A look for the engine's own screens

**The engine's own screens can wear your look.** The pause screen, the settings
pages and the start screen are the client's, drawn in plain egui, and no Lua
runs on the start screen at all — it is shown before any server exists. So a
look is DATA: a `[theme]` table in your `mod.toml` naming a font, a nine-slice
frame for sheets, one for buttons, and five colours.

```toml
[theme]
font = "fonts/Cinzel.ttf"            # headings and buttons
text_font = "fonts/Spectral.ttf"     # chat, text fields, prose
sheet = "art/frame_iron.png"
button = "art/button_brass.png"

[theme.colours]
text = "#e8dcc0"
heading = "#f0d890"
background = "#1a1512"
button = "#2a2018"
accent = "#b08d57"
```

Every field is optional and anything you leave out stays the client's own, so a
theme that is only a palette is a theme — and a theme cannot make a screen
unreadable, because every part of it is an override with a default underneath.
**Two faces, because a theme's own is usually a display one.** `font` goes on
headings and buttons; `text_font` goes on everything read as sentences. Name
only `font` and it covers both, which is what a one-face theme means — but a
display capital is hard reading for every line anyone says in chat.

**A frame is drawn 18 points deep whatever your art's resolution is**, and your
contents are kept clear of it, so you do not need to pad your own tree to
escape the trim. Draw the border as a third of your image, the rule
`style.nine_slice` uses; the engine scales it to suit. **One theme applies at a time: the last mod in load order that declares
one**, so a mod that depends on another paints over it.

In a world the theme is pushed on join and its files ride the font and picture
pipelines, so they are capped, isolated and fuzzed like any other pushed asset.
On the start screen it is read from the mods installed locally — never from a
cache of the last server's, which would mean decoding bytes a remote server
chose before you had chosen to trust anything.

---

## Terrain that does not look like a texture

Two things, and they are separate mechanisms because they fix different halves
of "it looks artificial".

**Shape: sub-node terrain.** `fill_density` takes a resolution:

```lua
buf:fill_density(field, stone, { detail = "smooth" })   -- no block staircases
buf:fill_density(field, stone, { detail = "sampled" })  -- and fine detail
```

**Strata: one fill, a table of materials.** A field of the form `noise - y` has,
at every point, the value *surface height minus y* — the DEPTH below the surface.
So grass on top, dirt for three blocks and stone below is three bands of that
one field, and `fill_palette` writes them from one evaluation:

```lua
buf:fill_palette(field, {
    { above = 0.0, material = grass },   -- the top block
    { above = 1.0, material = dirt },    -- the next three
    { above = 4.0, material = stone },   -- everything deeper
}, { detail = "smooth" })
```

A value takes the material of the highest band it is above, and a value under
the lowest band leaves the block alone — so `fill_density(field, m)` is the
one-band palette `{ above = 0, material = m }`, and a palette is a pure
replacement for the stack of fills you would otherwise write. **It is also a
pure saving**: each of those fills evaluated the whole field again, so ten
materials cost ten evaluations of the same noise, and the write that follows an
evaluation is a rounding error beside it. Measured on a six-octave field with
ten bands: **11.0 ms as ten fills, 1.5 ms as one palette** at smooth detail
(7.5x), 6.9 ms to 0.6 ms at block resolution (11x), and 19.5 ms to 8.9 ms
sampled (2.2x — there the 27-cell evaluation of each surface block is the cost,
and the palette still pays it once per block on any boundary). The output is
identical, cell for cell, at every resolution: the engine holds itself to that
in a test, on terrain steep enough to make it hard.

At most 16 bands. Thresholds must be distinct numbers; the engine sorts them.

Do NOT reach for `set_subnode` to do this. It writes one cell, and a chunk is
110,592 of them — the whole point of the option above is that the 27x sample
cost happens inside the engine, on the blocks the surface actually crosses.
Measured per chunk on terrain with caves: 729 us at block resolution, 1.08 ms
smooth, 3.98 ms sampled. `set_subnode` is for the handful of cells you place
deliberately, not for terrain.

**What that costs the server, and what the engine does about it.** Your
generator runs on the simulation thread, and the tick is 50 ms shared by
everything (charter rule 18). The engine will spend at most **half of it**
serving chunks — generating, lighting and sending them — and whatever does not
fit waits for the next tick. So an expensive generator does not make the world
stutter; it makes the world arrive more slowly, which is the trade worth having
and the one you can see coming.

The arithmetic is yours to do: at 3.98 ms a chunk, sampled detail fills about
six chunks a tick, and `smooth` at 1.08 ms fills nearly four times as many. A
player at view distance 24 is asking for tens of thousands of chunks, so that
ratio is minutes of waiting rather than a detail. Pick `sampled` where the
surface is worth it and `smooth` where it is not — they can be different fills
in the same generator.

If a server does run over its budget it says so, and it names your share:

```
a tick ran over its budget — 74.5ms total: serving 45.6ms, save chunks 28.7ms
  — serving 1 chunks, 1 summaries, 10 deferred; gen 45.1ms, light 0.5ms
```

`gen` is your generator. `light` is the engine lighting what you generated.
`deferred` is what the budget held back, which is the number that tells you the
server is keeping up with the tick but not with the player.

**Colour: a tint field.** Declare it on the block and the client does the rest:

```lua
game.register_block{
    id = "grass",
    tint = { strength = 0.12, low = {0.85, 1.0, 0.8}, high = {1.0, 0.95, 0.85}, scale = 40 },
}
```

`strength` alone gives tone variation, which is most of what stops a surface
reading as tiling; `low`/`high` add a hue shift across the same field. One field
for every material, keyed on world position, so a hillside varies as a hillside
rather than each block type drifting on its own. It costs nothing for materials
that declare nothing.

**Biome colour: one call, and the engine blends it.** The tint above varies a
material with its SURROUNDINGS. `register_chunk_tint` varies it with the PLACE —
a multiplier for one chunk, from the same field you choose biomes with:

```lua
game.register_chunk_tint(function(pos)
    local warmth = game.density(WARMTH):bounds(pos)
    if warmth.low > 0.0 then return 1.0, 0.85, 0.6 end    -- dry, sandy
    return 0.75, 1.0, 0.8                                  -- cool, green
end)
```

Three things worth knowing before you design around it:

- **The multiplier spans 0 to 2, and 1.0 is the texture's own colour.** Below
  darkens, above BRIGHTENS, and past 2 is clamped. This is worth a moment: a
  ceiling of 1.0 means every biome is at or below the brightness of the texture
  it multiplies, which is why a savanna asking for gold used to read olive-gold
  and a taiga's rust came out muddy. `1.6, 1.3, 0.7` is a gold. The same scale
  `low`/`high` above have always used.
- **It applies to materials that declare a `tint`, and only those.** Declaring
  one is what opts a material into varying with its surroundings, so it is also
  what opts it into varying with the place. You do not say it twice — and stone,
  which declares nothing, is the colour of stone in every biome.
- **The engine blends it; do not try to.** The colour is carried at each chunk's
  four corners, each the mean of the columns meeting there, and blended across.
  Neighbouring chunks agree exactly on the corners they share, so there is no
  seam and no 16-block grid. Returning a hard step between two biomes gives a
  gradient about a chunk wide, which is what a biome edge should look like.
- **You are asked every time a chunk is served, and nothing is stored.** Change
  your palette and the world changes with it, rather than leaving the colour it
  used to be in the ground behind the player. It costs one call per chunk, so
  keep it to a lookup — this is not the place to run your generator again.

One per mod. Every mod with one is asked, in load order; returning `nil` (or
nothing) is no opinion, and where more than one mod answers, the LAST wins. A
mod that depends on the world loads after it, so it colours over the world
where it has something to say and leaves the world's colour everywhere else.
Two opinions are never averaged into a third neither mod meant. (Until
2026-09-17 the first mod to answer at all, `nil` included, decided — a world
mod with a callback silenced every mod after it.)

**Fog by place: `register_chunk_fog`, on the same terms.** The sky's keyframes
set one distance fog for the whole world; a place's own fog — ground mist under
a canopy, murk over a marsh — is a table per chunk column:

```lua
game.register_chunk_fog(function(pos)
    if not rainforest(pos) then return nil end         -- clear air
    return { r = 0.5, g = 0.6, b = 0.5, visibility = 18, top = 64 }
end)
```

`visibility` is how many blocks a player sees into it (95% hidden there), the
colour is its colour in daylight — the engine dims it at night — and `top` makes
it ground fog that thins over a few blocks above that height. The engine blends
columns, and a fog is visible from outside as well as inside, so return what the
PLACE is and let the edges take care of themselves. It runs where the tint does,
in the generation workers.

**Particles: `game.emit_particles`, a burst at a time.** Sea spray, a drip off a
canopy, mist drifting over a floor. You describe a burst — where, how many,
colour, size, lifetime, starting velocity, random `spread`, spawn `area`,
`gravity`, whether it stops at solid ground — and the server sends it to every
player nearby in that domain — or to the one you name in `player` — and each
client animates it alone. It is decoration:
nothing reads a particle back, and bursts are dropped rather than queued for
ever, so a spray that must be seen every tick is a spray to make smaller. Emit
from a tick or a hook, near players (the call returns how many were told), and
never from a generator. Mist is large, faint, slow and `collide = false`; a drip
is small with gravity and `collide = true`.

**A picture on each particle: `texture`.** Give the burst the hash
`game.register_picture` answered and every particle in it draws that picture
instead of the round dot. The picture's own transparency is the shape — a heart
is a heart because of where its pixels are clear — and `colour` still tints it,
so one white picture serves red hearts and grey ones without a second file. It
costs one texture bind per picture per frame, and particles are grouped by
picture before they are drawn, so a scene mixing plain dots and three pictures
is four draws rather than one per particle. A hash whose bytes are still in
flight draws the plain dot until they land: never nothing.

**A row of pictures over an entity: `game.show_over(entity, spec)`.** Health
bars, an "!" over a startled animal, a quest marker. The row is camera-facing,
level, centred over the entity's head, and follows it — where it is comes from
the entity every frame, not from your call, so you hang it once rather than
re-hanging it every tick to keep up with a walking cow.

It is **latest state**: a badge replaces whatever that entity had, so a bar
draining over a second is a call a tick and still costs one message per network
pass. It expires on the client by itself, so there is nothing to take down;
`count = 0` takes it down early, which is what a mob healed back to full wants.
`player` narrows it to the one who hit, as `emit_particles` does. Unlit, so it
is readable at night, and depth-tested, so a mob behind a wall does not
advertise itself through it. An entity that has gone tells nobody and returns
0 — not an error.

---

## Interfaces: what a mod can and cannot do to the look

**Pictures.** `{ type = "image", hash = ... }` in a dialog tree, and
`style.nine_slice` on any widget. Both take a content hash — the same hash the
material table and the sound table use — and the client fetches, decodes and
draws them. A picture that has not arrived yet draws nothing and fills in when
it lands, so do not design around it being there on the first frame.

A **nine-slice's border is a third of the image**, both ways. Draw your frame so
its corners are the outer third and they will keep their size at any box size
while the edges stretch; that is the whole point of a nine-slice and it is why
there is no border argument to get wrong.

Pictures may be up to **2048 pixels on an edge**, and no more than 8 MiB
decoded — which at four bytes a pixel means about 1448² in practice. A frame at
1254² is fine.

**Scrolling.** `{ type = "scroll", children = { ... } }` gives its children
their full height and clips them, and the wheel moves them when the pointer is
inside. That is the answer to a dialog with more in it than fits: put the long
part in a scroll box rather than shrinking the controls, and the controls keep
the size you asked for.

**Slot counts scale with the slot**, so a bigger `item_slot` gets bigger
numbers. You do not set the font.

**Fonts.** Register one and name it in a style:

```lua
game.register_font{ id = "display", file = "fonts/display.ttf" }
-- ...then on any widget that has text:
{ type = "label", text = "Chapter One", font = "my_mod:display", text_size = 24 }
```

Up to **eight fonts per server** and **2 MiB each** — the count cap is about the
client's glyph atlas rather than the files, because what costs is coverage, and
a face with a full CJK range is orders of magnitude more atlas than a Latin one.

A font a client cannot load, or a `font` naming one nothing registered, draws in
the client's own face. A missing file is never a missing screen, so do not
design a dialog that only makes sense in your typeface.

**Ship a font you have the right to ship.** The engine carries no opinion about
your licence and no way to check one; a font in your mod directory is published
with your mod.

---

## Sharing with other mods

Each mod runs in a sealed environment: your globals are yours, and another
mod's `tdl` or `inv` is simply not there. `depends` orders loading and nothing
else. The channel between two mods is an EXPORT:

```lua
-- in tiamat_default_life, in the registration window, once
game.export{
    version = 1,
    humidity = HUMIDITY,                       -- a compiled density: passes through
    biome_under = function(x, z) return biome_at(x, z) end,
}

-- in tiamat_weather, whose mod.toml lists tiamat_default_life in depends
local life = game.exports("tiamat_default_life")   -- nil if absent, or not a dependency
if life then
    local wet = life.humidity:at(x, 0, z, game.world_seed)
    local biome = life.biome_under(x, z)
end
```

What to design around:

- **Only declared dependencies.** `game.exports(id)` answers `nil` unless `id`
  is in your `depends` or `optional_depends`, so load order guarantees the
  table exists when you read it — from the first line of your `init.lua`. It
  is also `nil` for a mod that exported nothing or has been disabled, and the
  four cases are deliberately one answer: handle "not there" and you have
  handled all of them.
- **Read-only, all the way down.** Writing into another mod's exports is an
  error; the tables you get back are views that hand out views. Iterating
  works (`pairs`, `#`).
- **Functions run in their OWNER's sandbox, and faults land on the owner.** An
  exported function that errors disables the mod that wrote it, the call
  answers `nil`, and you carry on — so check for `nil` from any call across
  the boundary. A callback you pass INTO another mod's function is yours: if
  it errors when they call it, you are the one disabled, and they get `nil`.
  Write both sides to be called by code you did not write.
- **A value handed back to its owner is the owner's own.** If you pass a
  table into another mod's function and it later hands it back — as a return
  value, or as an argument to your callback — you get the very table you gave,
  mutable and iterable, not a view of a view. A third mod that receives it
  gets a read-only view of YOUR table, however many hands it passed through.
- **A disabled mod's functions stop answering, even ones you are holding.** A
  function you took from `exports` yesterday answers `nil` the moment its owner
  is disabled, and its code does not run. If it matters whether the other mod
  is still there, ask `game.exports(id)` again — `nil` means it is not.
- **This is how one mod adds to another's screen.** Dialog events go only to
  the mod that opened the dialog, and that does not change. The screen's owner
  exports an `add_button(label, on_click)`; the other mod calls it and passes
  a callback; the owner draws the button and, on the event, calls the
  callback — which runs in the caller's sandbox, with the caller's `game`.
  Nobody has to see anybody's globals.
- **One export per mod, built before you call it.** A second call is an
  error, so gather what you offer into one table. `version = 1` in it costs
  nothing and lets a reader refuse a shape it does not understand.

## Machines: containers a mod can fill

A chest is a container a player drags things into. A **furnace** is one your mod
fills itself, on a tick, whether or not anybody is looking — and that is the
same mechanism with two more calls:

```lua
local FURNACE = "my_mod:furnace:" .. x .. "," .. y .. "," .. z
game.make_container(FURNACE, 3)                 -- fuel, input, output

game.register_on_tick(function()
    local ore = game.container_take(FURNACE, { material = "my_mod:ore", count = 1, slot = 2 })
    if ore > 0 then
        game.container_give(FURNACE, { material = "my_mod:ingot", units = ore, slot = 3 })
    end
end)
```

- **Slots are one-based and worth naming.** `slot = 2` is the input; without it,
  `container_take` would happily consume the ingots sitting in the output.
  `game.container(name)` gives each stack its `slot` back, and leaves empty ones
  out — so `#` counts what is in there, not how big it is.
- **Both answer in UNITS, not true or false.** A container is a fixed size, so a
  partial fit is ordinary: what did not fit was never taken from you. 27 units
  to a block (charter rule 5).
- **Ask `game.container_holder(name)` before breaking one.** `break_container`
  answers an empty list for an empty box and for one it refused because somebody
  has it open, so the holder is how a chest tells "empty" from "in use" and says
  so instead of vanishing under them. `game/core_chest` is the worked example.
- **They work while a player has it open.** An open container lives in that
  player's own inventory, and the engine writes into the slots they are looking
  at, so a machine does not stop while its owner watches it.
- **One callback per hook per mod.** Two `register_on_tick` calls is an error,
  not a merge — put your machines in one tick function.

---

## Building the same thing twice: plans

A village, a dungeon, a ship, somebody's saved house. Do NOT write this as a
loop over `game.get_block` and `game.set_block` holding the result in Lua
tables — the engine has it, in one place, bounded:

```lua
local made = game.plans.capture("hut", { x = 0, y = 8, z = 0 }, { x = 7, y = 12, z = 7 })
if made then                                    -- nil, reason if it could not
    game.plans.stamp("hut", { x = 40, y = 8, z = 40 })
end
```

Four things worth knowing before you design around it:

- **A plan is named, never held.** `capture` saves under a name and every other
  call takes that name back, so a plan survives a restart and a clipboard is
  just a name you reuse.
- **Stamping ADDS.** A plan records only blocks that hold something, so it puts
  a building onto a hillside rather than cutting a box out of it. Clear the
  space yourself if you want the box.
- **`stamp` returns "accepted", not "done".** Big plans are paced across ticks
  by the engine, so a large building appears over a second or two. That is the
  engine protecting the 50 ms tick, and taking the pacing into your own hands is
  not available — nor should you want it.
- **`materials` in the summary is in UNITS**, 27 to a block, which is what an
  inventory counts in. `blocks` is the block count. Check the first against what
  a player is carrying before you stamp, not after.

Limits: 64 blocks on a side, 65,536 filled blocks, and everything you capture
must be in loaded terrain — a capture reaching unloaded chunks is refused whole
rather than coming back with holes you cannot see.

---

## What belongs in your mod, not in the engine

The engine is deliberately small and holds no opinion about what a world is.
What follows is the list of things assistants routinely ask the engine for that
are already yours — and, where the line has moved, the small mechanism the
engine does owe you to make them expressible.

**Biomes are a Lua table.** A registry of names to parameters needs nothing
from the engine. What the engine owes you is a way to tell which biomes a chunk
can hold WITHOUT running each of them, and that is `density:bounds(pos)` on a
selector field — see "A field says what it cannot be" above, which has the
shape and the numbers. Index your own table off the branch it picks.

**Ore placement is already possible, and is NOT per-sample work.** This is the
distinction to get right: the forbidden thing is O(volume) arithmetic in Lua.
Scattering ore is O(ores) — a few dozen `set_block` calls a chunk, chosen from a
seeded stream — and that is ordinary mod code:

```lua
game.register_on_generate(function(buf, pos)
    local rng = game.rng_stream(pos, "ore")
    for _ = 1, 12 do
        local x, y, z = rng:below(16), rng:below(16), rng:below(16)
        buf:set_block(x, y, z, iron)
    end
end)
```

Reading terrain to decide where ore may go is the same shape: bounded, and
proportional to what you place rather than to the volume you place it in.

**Structures that cross a chunk edge: build the neighbourhood, keep your
slice.** A tree, a hut, a ruin is rooted in one place and reaches out from it,
and nothing makes that reach stop at a chunk boundary. The obvious design — write
into your neighbours and let the engine hold those writes until those chunks are
made — is **wrong, and wrong in a way that does not show up in testing**: chunks
are generated in whatever order players walk towards them, so a chunk made
before its neighbour is missing what that neighbour would have contributed and a
chunk made after it has it. Same seed, different world, and only for the players
who approached from the other side.

Do it the other way round. When generating a chunk, run your structure pass for
**every chunk within reach**, and write all of it in world coordinates:

```lua
local REACH = 1                                  -- chunks your biggest structure spans

game.register_on_generate(function(buf, pos)
    for dx = -REACH, REACH do
        for dz = -REACH, REACH do
            local at = { x = pos.x + dx, y = pos.y, z = pos.z + dz, seed = pos.seed }
            local rng = game.rng_stream(at, "trees")   -- that chunk's own trees
            for _ = 1, rng:below(4) do
                local x = at.x * 16 + rng:below(16)
                local z = at.z * 16 + rng:below(16)
                local ground = math.floor(surface:at(x, 0, z, pos.seed))
                for h = 1, 6 do
                    buf:set_world(x, ground + h, z, trunk)   -- outside? dropped
                end
            end
        end
    end
end)
```

Every chunk derives the same structures for the same neighbours, because
`rng_stream` is a pure function of a chunk position and the seed. Each chunk
keeps the slice that lands in it and `set_world` drops the rest — **dropping is
the mechanism, not waste.** The same chunk comes out whatever was generated
before it.

**For anything you place by the dozen, use `buf:scatter`.** It is that pass in
one native call: build each structure once at load as a `game.schematic` (a
list of `{dx, dy, dz, material, mask}`), and the engine draws the candidates
per square of ground, finds each one's surface down its column, checks your
`stand` field there, and stamps the schematic clipped to the chunk. A tree
written block by block is two thousand crossings into the VM per chunk it
overlaps; a forest is a dozen trees a chunk, and a chunk is generated in a
tick's budget.

`density:at(x, y, z, seed)` is the point sample that lets a structure sit on
ground it cannot see: a generator's buffer is write-only, and a structure rooted
in a neighbouring chunk has no buffer to read anyway. It is for choosing WHERE,
a few dozen times a chunk. **Do not loop it over a chunk** — that is 4,096
crossings into the VM for an answer `fill_density` gives in one call, in native
code, with the bounds pruning in front of it. It is the exact cost the opaque
handles exist to prevent, and the engine cannot tell a loop from a list.

Outside a generator — a tick, a join, a dig — there is no `pos` to carry the
seed, so read **`game.world_seed`**: the same number, set in every VM once the
world opens (it is `nil` while your `init.lua` runs, which is before). Aiming a
new player's spawn at the ground is `surface:at(x, 0, z, game.world_seed)`.

`buf:set_subnode_world(x, y, z, material)` is the same write at cell resolution,
where the coordinates count cells rather than blocks — three to a block.

**Decoration that embeds needs the merge write.** A masked `game.set_block`
REPLACES the block — the cells your mask does not name become air — which is
right for something growing into open air and wrong for a rock or a root going
into ground: it ends up standing in a footprint of its own bounding block.

```lua
game.set_block(pos, "my_mod:rock", cells, { merge = true })  -- keeps the turf
game.set_block(pos, "my_mod:rock", cells)                    -- clears the rest
```

A named cell is taken whatever was in it; merging is about the cells you did
NOT name. During generation the buffer already works this way — `set_subnode`
writes one cell and leaves the other twenty-six — so this is the runtime half
of the same rule (Sub-Node Contract §7.4). Merging into a block holding a
different material sends one edit per named cell, which is what it costs to add
a material to a block without erasing the first.

**Glass is a flag, not a shader.** `register_block{ transparent = true }` and
the block's own texture alpha decides how see-through it is. That one flag
changes three things and leaves the rest alone (Sub-Node Contract §8.1):

```lua
game.register_block{
    id = "glass",
    transparent = true,                       -- see through it, and light does
    textures = { all = "textures/glass.png" }, -- the PNG's alpha IS the opacity
}
```

- A face draws where exactly ONE side of it is transparent, so a wall behind a
  window is not a hole and two panes touching do not double up.
- It is drawn in a blended pass after the opaque world.
- Light passes through a whole block of it, so a glass roof does not make a dark
  room.

**Collision does not change — glass is solid.** You cannot walk through a
window, and it holds fluid in.

Two limits worth knowing before you file them as bugs. Panes seen through one
another at an angle are not sorted against each other, which is a deliberate
trade: sorting per quad is per-frame work proportional to the geometry. And only
a WHOLE block of one transparent material passes light — a chiselled or mixed
block holding glass falls back to the ordinary cell rule.

**Leaves are NOT glass — use `cutout`.** This is the one to get right, because
picking the wrong flag gives an artefact rather than a preference:

```lua
game.register_block{
    id = "leaves",
    cutout = true,                             -- see-through in PLACES
    textures = { all = "textures/leaves.png" },
}
```

`transparent` is see-through EVERYWHERE and hides the face between two panes,
so a window does not double up. Foliage needs the opposite: the faces between
two leaf blocks are kept, because those are the leaves you see through the gaps
in the leaves in front of them. Declared `transparent`, a canopy becomes a
hollow shell and its alpha holes look straight through the world at the sky —
which is exactly what it is, since the sky is the frame's clear colour with
nothing drawn over it.

Cutout is drawn alpha-tested with the opaque world, so it writes depth,
occludes itself correctly at every angle, and the sorting limit above does not
apply to it. Light passes as it does through glass — dappled shade is not
expressible, because permeability is yes or no. Collision does not change:
leaves are solid, and whether a player may walk through them is your mod's
opinion to implement.

A block is one or the other. Declaring both is refused at registration rather
than silently resolved.

**Plants want `passable = true` as well.** Every block collided until this
existed, so a two-cell fern was a lip the player stepped up and foliage had to
be shaped around the engine — tufts one cell tall, clumps with gaps. A passable
material stops nothing:

```lua
game.register_block{ id = "grass", cutout = true, passable = true }
```

**Collision only.** The cell still meshes, is still lit, still holds fluid out,
and a ray still stops at it — which is deliberate and is what lets a player aim
at a tuft and break it. A material that reported itself hollow everywhere would
be one nobody could pick.

**Grass is `billboard = true`, not a card of cells.** Building a sprite card out
of cells does not work here and it is worth knowing why, because the geometry
looks like it should: a texture repeats once per BLOCK, so a face one cell
across shows a ninth of the tile — a crop, not a sprite — and a cell is a cube,
so a tuft made of them reads as a little floating box. That is what "sprite
cards are not really a thing" means, and it is correct.

```lua
game.register_block{
    id = "grass",
    billboard = true,   -- drawn as a camera-facing sprite, not as geometry
    passable = true,
    sway = true,
}
```

A RUN of cells in a column is ONE sprite as tall as the run — one cell is a
third of a yard, three is a yard — so you control a plant's size by how many
cells you place, and you never get the same texture stacked on top of itself.
It turns about the vertical axis only, so it never lies over when a player looks
down at it, and the cells stay exactly where they are for collision, light,
fluid and the dig ray. Only the drawing changes.

**Never add `cutout` or `transparent` to a billboard** — `register_block`
refuses it. Those flags are rules about a cell's cube faces, and declared
together the cells used to be drawn as cutout cubes with the sprite lost inside
them. A billboard alpha-tests on its own.

**Place it with `fill_cover`, not with a field.** A density has no idea which
block a sample is in, so a two-cell run written by `fill_density` lands half in
one block and half in the next, and the sampled fill misses most of it in
stripes that follow the contours. `fill_cover` stands a run on every surface the
buffer already holds:

```lua
game.register_on_generate(function(buf, pos)
    buf:fill_density(surface, dirt, { detail = "smooth" })   -- the ground first
    buf:fill_cover(grass, { cells = 2, take = tufts })       -- then what grows on it
    buf:fill_cover(allium, { cells = 6, take = rare })       -- and a two-block flower
end)
```

**Up to three cells a run stays in the block it started in; over three it
carries into the block above, up to nine.** The short case is the rule that
keeps a tuft from being two stacked blocks that highlight and dig apart, and it
is what every grass wants. The tall case is the two-block flower — an allium, a
peony — and there the spill is the point. A tall run is cut at the chunk's
ceiling, one block row in sixteen.

Call it AFTER the fills that make the ground — it reads what they wrote. `take`
is a density sampled at the run's base cell: positive means a run goes there, so
a noise minus a threshold thins the cover, and a term from your terrain field
keeps it off cave floors. Left out, every surface is covered. It costs about a
tenth of what generating the chunk costs, and nothing at all on a chunk of plain
sky or plain rock.

**And `sway = true` makes it move.**

```lua
game.register_block{ id = "grass", cutout = true, passable = true, sway = true }
```

Presentation only: the world does not know the grass is moving, so collision,
lighting and the server's idea of where anything is are untouched, and two
clients at different frame rates disagree about where a leaf is without
disagreeing about anything that matters.

**It bends rather than slides**, because the mesher marks the top edge of each
face and the shader moves only those vertices — greedy meshing spans a plant's
whole height in one quad, so the base staying put gives a linear bend from base
to tip. The motion is the engine's own noise over world position and time, so a
field leans in gusts instead of every plant buzzing on its own.

There is no amplitude to set. What a plant looks like is its texture's business
and its shape's; a knob beside them is a third thing to get wrong.

**Erosion, rivers and blending one biome's TERRAIN into another's are
compositions**, not engine features. Build them from `game.density`'s
arithmetic — a `min` between two surfaces is a coastline, a weighted `add` is a
transition. If a shape genuinely cannot be expressed with the operations that
exist, that is a finding worth reporting: the answer is a new operation with a
determinism argument, not a loop in Lua.

Blending a biome's COLOUR is the exception, and it is an engine feature
(`register_chunk_tint`, above) for a reason you cannot work around: the blend
has to happen where the pixels are, and a mod has no way to reach them.

---

## The sandbox

Server mods run sandboxed for crash isolation: an error disables that mod and is
logged, and the tick keeps going. `os`, `io`, `dofile`, `loadfile`, `package`
and `ffi` are removed, and `_G` does not hand them back.

Client HUD scripts — the ones a server pushes to a player, via
`game.register_hud_script` — are sandboxed much harder still: no `os`, no `io`,
no `require`, no `load`, no `coroutine`, plus instruction and memory caps. A HUD
script draws and nothing else.

---

## A complete mod, start to finish

```lua
-- SPDX-License-Identifier: MIT
local stone = game.get_block_id("core:white")

game.register_block{
    id = "brick",
    name = "Brick",
    hardness = 1.2,
    textures = { all = "textures/brick.png" },   -- `all` is required
}

game.register_domain{ id = "quarry", generator = function(buf, pos)
    local heights = game.noise_heightmap(pos, { octaves = 4, frequency = 0.01, amplitude = 60.0 })
    buf:fill_below_heightmap(heights, stone)
end }

game.register_on_chat(function(event)
    if event.text ~= "quarry" then
        return
    end
    local body = game.player_entity(event.player)
    if body ~= nil then
        game.transfer_entity(body, "my_mod:quarry", { x = 8, y = 96, z = 8 })
    end
    return false          -- swallow the message
end)

game.log("my_mod ready")
```

`docs/fixtures/relief/` in the engine repository is this shape as a real,
runnable file, and `game/` holds larger worked examples — every one of them
written through this API and nothing else, which the build enforces rather than
merely intends. Good ones to read, smallest first: `core_worldgen` (36 lines,
the whole generation path), `core_tools` (118, tools and actions), `core_gear`
(340, items that are not blocks, drops, worn slots).

---

## Limits worth knowing before you design around them

Everything here is true as of 2026-09-18 and is the kind of thing that is
cheaper to read than to discover. None of it is a rule the engine wants; each is
work that has not been done, and each will move.

**A density program may hold 4,096 operations and 16 live buffers.** It was
1,024 and 8 until 2026-09-19, and the world mod's shore programs sat at 985 and
939 with three biomes in them — which is why its reefs lost their tidal gutters
and its cliffs their blowholes. Nothing remembers a subtree it has already
emitted, so a helper called twice is compiled twice: `a * (1 - w) + b * w` emits
`w` twice, and a ring re-emits its radius at every call. Budget for that.
What a program costs is its NOISE reads, not its length: one is about 0.34 ms a
chunk and a thousand arithmetic operations about 0.5 ms, so a noise node is
worth roughly seven hundred arithmetic ones. Spend the room on arithmetic
freely and on noise carefully.

**An open sheet covers the bottom of the screen, and your HUD has to say so.**
The inventory, the pause screen and a mod's dialog are all one sheet: three
quarters of the window's height, four by three, centred. That leaves an eighth
of the window below it, and a HUD with more than a row or two along the bottom
edge does not fit in it — hearts, food and warmth under an open inventory is
what that looks like. Nothing the engine measures would tell it otherwise, so
pass a `reserve` in the table form of `game.register_hud_script`, in the same
virtual pixels your HUD draws in. Sheets rise to clear the tallest reserve any
loaded mod asked for, and shrink only when there is no window left to rise into.

**A widget's `size` is its length along its PARENT's direction**, and a
container now measures its children by what they asked for rather than by what
is in them. Before 2026-09-18 it only honoured `size` on a node that also set
`cross_size`, so a column holding a row with `size = 52` measured that row at
its contents' height, laid it out into that, and squashed everything inside it.
If you have summed your children's sizes and set your own to work around this,
you can stop; the workaround is harmless either way.

**Fluid physics is per fluid.** `tick_rate`, `waterlogs_at` and `evaporates`
are read from the fluid IN a block, so a slow lava beside a quick river and a
puddle that dries beside a sea that does not are both expressible. This was not
so until 2026-09-17: the solver took one set from whichever fluid registered
first, alphabetically, so a reference mod you never thought about could be
setting your sea's speed. Nothing to design around now; it is here so that a
world made before then is understood if its water seems to have changed pace.

**The day is yours to wind.** `game.time_of_day()` reads it and
`game.set_time_of_day(t)` sets it — 0 midnight, 0.25 dawn, 0.5 noon, wrapped
rather than clamped, everybody told at once. That is the bed that ends the
night. One clock for the world; a sky that differs for one player is
`game.set_sky_modifier`.

**Your model can wear a skin: `texture` on `register_model`.** A `.glb` with
no texture is drawn matte white, which is what every model was, and the reader
refuses a `.glb` that embeds its image — ship the PNG beside it and name it.
The model's own UVs are used as they are.

**A canopy shades, if you ask it to: `light_falloff` on `register_block`.**
Leaves are `cutout` and a cutout block passes light the way glass does, so a
forest floor under a whole canopy was as bright as a meadow. `light_falloff = 2`
is two levels lost per block of leaves; three blocks of that put a floor at
about 6 of 15. It is the same number `register_fluid` takes, and 0 — the
default — is exactly what blocks always did.

**Water breaks plants, if you say so: `washes_away` on `register_block`.** A
flood runs straight through a `passable` tuft and stands in the same block, and
your mod cannot see it happen — `on_fluid_flow` reports the flows that were
BLOCKED, and nothing blocked that one. Declare `washes_away` on the plant and
the engine clears the block when fluid enters it. Nothing is dropped; if a
washed plant should leave seeds, spawn them yourself.

**And a fluid can decline to break them: `washes = false` on
`register_fluid`.** Rain is a fluid and its puddles spread, so a shower would
strip the meadow it fell on. The fluid's author sets this, not the plant's —
a plant cannot know about every fluid in the world.

**A floor can be slick: `friction` on `register_block`,** a share of the
ordinary grip from 0 to 1. Ice at 0.1 is slow to start on and glides several
blocks after the keys are let go. It is read per sub-node under the centre of
the feet, it slides mobs as well as players, and the client predicts it, so do
not build ice by pushing bodies from a tick hook — that is the rubber-banding
version.

**A mob's pace is `speed` on the entity** — `spawn_entity{ speed = 0.5 }` or
`set_entity`. A drive's direction is normalised, so a shorter one is not a
slower one; this is the multiplier that makes a cow amble rather than march at
a player's walk. The same number, and the same code, that slows a player.

**A player's movement is yours to limit, and flight yours to grant.**
`game.set_player_abilities(uuid, { fly, speed, sprint, wind_sky })` — a Creative
world where everybody flies, cold that slows, hunger that stops a sprint, and
`wind_sky = false` for a world that means its nights (the engine's sky keys
scrub the client's own clock, which lights a player's night for free). Replaced
whole each call (a field left out is the default again), `fly` is OR-ed with the
operator list, and the client predicts with the same numbers so nobody
rubber-bands. Do not try this with `game.set_entity` on a player's body: the
body is stepped from the player's own inputs and your write is overwritten the
next tick.

**Your sea is drawn at the horizon.** A chunk past the detail radius arrives as
a summary — one material a cell — and until 2026-09-19 a summary held no fluid,
so a generated ocean read as its floor with a hole over it until you walked into
the detail radius. A block holding fluid and no terrain now reads as the block
that fluid is drawn as, which is the same one you named in `register_fluid`.
Nothing to do: place the sea and it is there to the horizon.

**A fluid can take the light out of what passes through it**, with
`light_falloff` on `register_fluid` — levels lost per block, default 0. Zero is
"like air", which is what every fluid was: a block of fluid is air in the block
store, so sunlight fell to a sea floor a hundred blocks down at full strength.
One is a level a block, which also ends daylight's free fall straight down, so a
shaft of water is dark fifteen blocks under the surface and a shaft of air is
not. A passable cell does not displace fluid either, so a plant under water is
saturated rather than standing in a bubble of air.

**Two fluids never mix, and the first one there keeps the space.** A block holds
one fluid and a volume of it, so a move into a block holding a different fluid
is refused: nothing merges, nothing is displaced, nothing is destroyed. Lava
running into water does not hiss, harden or vanish on its own — it stops. The
meeting IS reported to `register_on_fluid_flow`, beside or below, with `meets`
naming the other fluid, so what it MEANS — steam, obsidian, a hiss — is yours to
write from there.

**Ground drinks any fluid unless it names one.** `absorbs = { rate, becomes }`
drinks whatever touches it; `absorbs = { rate, becomes, fluid = "weather:rain" }`
drinks that fluid alone, so the bed that soaks a puddle of rain does not drain
the river it is the bed of. A named fluid nobody registered is one nothing
drinks.

**`everywhere = true` on a loop means every connected player, in every domain**
— unless you name a `player`, which makes it that one player's alone. `play_loop`
and `stop_loop` take `fade_ticks`, and starting a loop that is already running
the same sound MOVES its gain and place over that fade rather than restarting the
clip, so a storm's loudness can follow its intensity. A positioned loop's
loudness and pan follow the listener every frame; its treble is set once, at the
start. A player who joins after a loop started is not told about it — start it
again for them from `register_on_player_join`.

**The sky's keyframes are registration-only; the weather over them is not.**
`register_sky` takes its keyframes in the registration window and the client
interpolates them from the clock. `game.set_sky_modifier(uuid, { intensity,
sky, sky_mix, fog_distance, saturation, ease_ticks })` lays a per-player change
over them at any time — a storm darkens the sun, closes the horizon in and
greys the grade, eased on that player's client — and `nil` puts the plain sky
back. It multiplies and mixes rather than replacing, so it is right at every
hour. `game.flash{ pos, radius, intensity, colour, attack_ticks, decay_ticks }`
is lightning: a moment's light on the sun and sky of everyone in reach, with no
relight. The sun's direction and the keyframes themselves cannot be moved.

**Stars are places, and the sky is per domain.** A keyframe's `stars` (0 to 1)
says how much of the catalog shows at that hour — omit it and none do; the
engine never decides that night means stars. The catalog is `game.stars()`,
two thousand positions derived from the seed on both ends of the wire, so the
star a player sees is the star `game.star_in_view(uuid)` names. A domain
registered with a `position`, or an instance made with
`game.create_domain(template, key, { position = ... })`, sees the sky from
there; `register_sky{ domain = ... }` gives it colours of its own, sent to the
client when a player arrives. Travel is yours: which star has a surface, what
takes you there and what brings you back is a mod's rule, and `game/core_space`
is the smallest one that works.

**A place's fog and tint are asked when a chunk is SERVED, and never again.**
Change what your callback returns and only chunks a player has not loaded yet
will show it — which means the change appears at the edge of the view distance
and never where the player is standing. Good for a world that varies by place,
useless for weather crossing a world somebody is already in.

**There is no surface query.** Nothing keeps a per-column "top" — sunlight asks
one block at a time and caches nothing — so finding the ground under a point
means `game.get_block` down a column, one VM crossing per block. `noise_heightmap`
and `Density:at` are cheap and describe the terrain as GENERATED, which is a
different question the moment somebody digs. If you need the real surface at
runtime, keep the scan short (start just above where you expect ground) and do a
few columns a tick, not a field of them.

**Particles are decoration and are dropped under load.** A burst goes to every
player within `radius` in that domain, or to the one named in `player` — so a
per-player "particles off" setting in your own mod can be honoured. A client
that falls behind loses bursts rather than queuing them, so anything that must
be seen every tick is something to make smaller — and rain is not a burst at
all: `game.set_precipitation(uuid, { rate, size, colour, velocity, area, above,
ease_ticks })` sends the shape once and the player's client spawns it around
its own camera until you send `nil`.

**A mod reaches another mod only through what it exports.** Each mod gets a
fresh sandbox and `game.storage` is private; `game.export` / `game.exports`
(below, under "Sharing with other mods") is the channel, and `depends` is what
opens it. There is no other way in, and copying another mod's constants is the
thing that goes quietly wrong the day they are retuned.

## Before you say it works

- `--check-mods` passes and your mod is listed.
- No `math.sin`, `math.atan`, `^` or any other float maths on a value that
  reaches the world. A density table instead.
- A `game.density` field compiled ONCE at load, not rebuilt per chunk.
- No numeric block id stored, compared to a literal, or written anywhere.
- Nothing registered outside `init.lua`'s first run.
- Quantities in units, and `count` never confused with `units`.
- A hook that can refuse returns the right thing — check the stub, because
  `false`, `nil` and a table mean different things per hook.
- Nothing in your design rests on something in "Limits worth knowing" above —
  or if it does, you know it does and have said so.

## Licensing

The engine is GPL-3.0-only, but [`../LICENSE.EXCEPTION`](../LICENSE.EXCEPTION)
is a formal Additional Permission under GPLv3 §7: work interacting with the
engine solely through the Lua API or the network protocol is not a derivative
work and carries no copyleft obligation. Everything in `api/` is MIT so
vendoring the stubs is unambiguously fine. **Your mod is yours, under whatever
licence you choose.**

---

## Keeping this file honest

It describes behaviour that changes. When the engine changes something a mod
author relies on, this file changes in the same commit — the same rule
`scripts/check-stubs.sh` enforces mechanically for the stubs, applied by hand to
the prose. If it contradicts [`stubs/game.lua`](stubs/game.lua), the stubs are
right and this is stale: fix it.
