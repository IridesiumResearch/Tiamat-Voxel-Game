# Engine asks from Tiamat Weather

From the `tiamat_weather` mod (repo `Tiamat_Default_Weather`, beside the
engine checkout). Kept here, in the engine's `docs/engine-asks/`, so the
engine agent finds every mod's asks in one place and they are versioned with
the engine. Pictures and patches an ask cites are in `tiamat_weather/` beside
this file; W12's are kept there as the record of engine 0d8e857.

Numbered W*n*, continuing the weather sheet: W1–W9 were filed in that repo's
`docs/engine-asks-weather.md` and all of them are built or answered. That
file stays as the history. Each entry says what was seen, why the mod cannot
fix it, and the smallest engine change that would. Newest first. Items are
removed when they land.

Open: W29, filed 2026-09-28. W27's first step landed 2026-09-28 with the gate not yet met (see it),
W26 and W25 landed 2026-09-28 awaiting the eye, W28 the same day it was filed,
W24 2026-09-26, W19 to W23 2026-09-24, W17 and W18 the day before, below.

## W29. A cave's fog is the sky's colour (2026-09-28)

**Seen.** The designer: "be sure that the day night cycle does not affect
the colour of cave fog underground". Fog is drawn in `moment.sky`
(`app.rs`, the frame's `set_sky`), the keyframed sky colour, so the fog down
a tunnel is pale blue at noon, orange at dusk and black at midnight, and it
leans with a storm's modifier and a flash's colour too.

**What weather does meanwhile.** A player with no sky over their head (sun
0 at the head, or the shaft rule) gets a sky modifier of `sky = CAVE_SKY,
sky_mix = 1`, one dark colour at every hour, eased at the cave mouth. It
works, but a modifier is the WHOLE sky's colour: deep in a cave, the sky
seen out of a long tunnel's mouth and the daylit ground beyond it fog to
the cave's dark colour, and the switch is one player-wide state decided
twice a second on the server rather than per pixel.

**Why the mod cannot do better.** Fog colour is chosen once per frame for
the whole view; only the client knows, per fragment, how much sky reaches
what it is looking at.

**Ask.** Fog that knows a cave from the sky: blend the fog colour per
fragment by the SKY LIGHT at the fragment (already in the mesh for
lighting), from the sky's colour at sky light 15 to a cave fog colour at 0,
where the cave colour does not follow the clock. Its value is the sky
owner's to set, e.g. `cave_fog = { r, g, b }` on `register_sky` (or on the
modifier, so a mod can change it), defaulting to a dark neutral. Then the
fog down a tunnel is the cave's and the view out of the mouth is the day's,
in the same frame, and weather drops its cave modifier. A flash leans only
the sky-lit part, which keeps lightning out of caves without anyone having
to be told. Gate: a tunnel whose far end opens to daylight, at noon and at
midnight: the fog on the tunnel walls is the same colour in both, and the
daylit ground seen through the mouth is fogged in the sky's colour.

## W28. Thunder and a flash cannot be kept out of a cave (2026-09-28): LANDED 2026-09-28 (engine cbbbc5e)

**Seen.** The designer: "lightning strikes and weather should not affect the
underground". Rain, the storm's sky and its loop are already per player and
go quiet under a roof (`set_precipitation`, `set_sky_modifier`,
`play_loop{ player }`). Lightning is not: `game.flash` is sent to everybody
within `radius`, and so is `game.play_sound`. So a player in a cave under a
storm hears every clap overhead, positioned and unoccluded, and the flash
leans the sky colour, and with it the fog down the tunnel.

**What weather does meanwhile.** A bolt lands only on ground under open sky
(it used to look for ground from 64 blocks over the player, which in a deep
cave was inside the rock), and a bolt that no player under open sky is near
enough to see is neither flashed nor sounded. That silences a player alone
underground, but not one underground beside a friend at the cave mouth.

**Ask.** `player` on `game.flash` and `game.play_sound`, as
`emit_particles` and `play_loop` already have: the mod then sends each
strike to the players who are out under the sky, which it already knows.
(An engine-side rule — "only players whose head has sun" — would also do,
but the mod's canopy and shaft rules would not be in it, so `player` is
the smaller and more useful change.) Gate: two players 20 blocks apart, one
in the open and one under a roof, a flash and a sound addressed to the
first: the second's client receives neither.

**From the engine, 2026-09-28 (engine cbbbc5e):** `player` on `game.flash`
and on `game.play_sound`, a UUID in hex as `emit_particles` takes it. It
narrows and never widens: the domain and the radius still apply, so a
player addressed from out of range is still not told, and the count
returned says so. A `player` that is not a UUID is an error at the call,
not a broadcast. The gate is a bot test each way (`atmosphere.rs`,
`sound.rs`): two players in range of every strike, each strike addressed
to one, the other's client receives nothing.

## W27. The deck still lags; the mod is out of knobs (2026-09-25): STEP 1 LANDED 2026-09-28 (engine f4ceb53), gate not yet met

**Seen.** The designer, on the default rung: the clouds are "a bit too
laggy — let's try to optimize and cut some corners". It is the frame, not
the tick.

**What weather did.** Everything this side has, by plan 10.19's
measurements: cubes of 32 (from 24; about an eighth off), thickness 180
(from 200; 140 measured a tenth off), a nearly cloudless clear sky (0.06
cumulus, 0.08 altocumulus; altocumulus measured 3 to 15% of a clear day).
Surface `detail` measured nothing, and the floor rose a quarter (400 to 500
over the ground) at the designer's asking, which lengthens the ray to the
deck from the ground. That is roughly a fifth, and the rest is the pass.

**Why the mod cannot do more.** What is left is how often and how finely
the pass marches, which is the renderer's.

**Ask — any of these, cheapest first, gated by the W19 probe
(`how_long_weathers_deck_costs_by_knob`) on `Low`:**
- **Temporal amortisation.** March half (or a quarter) of the deck's
  pixels each frame in a checkerboard and reproject the rest from the last
  frame by the camera's motion; the deck moves slowly and is soft-edged,
  which is the case this is made for. Roughly halves `Low` again.
- **Empty-space skipping at the slab.** A coarse occupancy per large cube
  (or per map cell), so a ray crosses a clear stretch in one step rather
  than one per cube; a clear or cloudy sky is mostly empty stretches.
- **Distance LOD.** Past a kilometre or so, march cubes twice the size and
  skip the rind: the cost of the far deck is steps a pixel cannot resolve.
- **A cheaper look from above.** The view from over the deck cost twice any
  other in 10.19; the tops seen from above could stop at the first hit
  with no in-scatter or shadow terms.

Gate: `Low` at 1080p, the six skies and three views, at most half of
today's median, with the storm view from above no worse than today's level
view.

**From the engine, 2026-09-28 — still open, and the order it will be
taken in.** Measured work, not guessed: each step is gated by the W19
probe (`how_long_weathers_deck_costs_by_knob`) on `Low` before the next
is started. First empty-space skipping — a coarse occupancy per map cell
built where the cover map is uploaded, so a ray crosses a clear cell in
one step; it is the cheapest to build and the six skies are mostly clear
stretches. Then distance LOD (double the cube past a kilometre, skip the
rind). Temporal amortisation last, because the checkerboard reprojection
is the one that can show (ghosting under a fast camera) and wants the
designer's eye on it. The cheaper look from above only if the storm view
is still the outlier after those three.

**From the engine, 2026-09-28 (engine f4ceb53, 48f7c81, 84355b5, 5080853,
e619f59):** empty-space skipping, built where the sky is uniform as much as
where a map says a cell is clear — the probe skies have no map, so a
per-map-cell occupancy would have measured nothing. Each frame a small
pass draws an occupancy pyramid: one texel per march cube at every grid
the march can walk (the cube, twice it, four times, up to thirty-two),
each level sized to where its grid is walked and centred on the camera,
holding the heights the field reaches in that column; the march reads the
texel before it asks the field, and a clear cube is a texel read rather
than a column of lattice tests. It is exact by construction — the pass
evaluates the same field at the same point the march would — and the
decisions near a threshold are marked undecided and left to the march, so
a driver that rounds the two entry points differently cannot lose a cube;
`skipping_the_decks_clear_cells_changes_no_pixel` renders the six skies
by three views at Low and Medium, a map, and the camera inside a heap,
and requires the skip-on frame to equal the skip-off frame byte for byte
(51 frames, all equal on llvmpipe). `how_long_the_shipped_deck_costs_on_low`
is the probe, on the deck as the mod registers it.

The gate, on llvmpipe at 1080p Low, the six skies by three views: the
median shot is 0.53 of the day's before (7.62 ms to 4.03 ms; the view
from above 16.0 to 4.9, level 4.6 to 4.0, 45 up 7.2 to 3.6), and the
storm from above is 5.28 ms against the before level view's 4.62 — both
short of the ask's half and no-worse. Not met, and the next rungs are the
designer's: distance LOD past a kilometre and the temporal checkerboard
both change the picture, and the ask put them behind the eye. Two [H]
gates ride with what landed: the exactness test on a real GPU compiler
(Vulkan, DX12 and Metal — run the test named above with
`TIAMAT_REQUIRE_GPU=1`; it must say 0 differ for every frame), and the
probe's numbers on a real card, where the ratio is expected to be better
than a software rasteriser's, whose fixed per-pass cost is what keeps the
level view at 0.87.

## W26. A lightning bolt that is drawn (2026-09-25): LANDED 2026-09-28 (engine 1eccb62), awaiting the eye

**Seen.** A strike is a flash on the sky (`game.flash`, W3), sparks where it
lands and thunder after its distance. Nothing is drawn between the cloud and
the ground, so a storm over the next valley is the sky blinking. The
designer's reference, `tiamat_weather/lightning-reference-2026-09-25.webp`:
thin, jagged, forked violet-white bolts from a storm's base to the ground,
seen from kilometres off through the rain curtain.

**Why the mod cannot draw it.** Particles are the only free-standing thing a
mod can place, and they are the wrong tool three ways: they are lit by the
world, so a bolt at night comes out grey (the reason `flash` exists); a
burst's `area` is an axis-aligned box, so a jagged line is dozens of bursts
of dozens of particles; and bursts go to players within a radius and are
dropped first under load, which is exactly when a storm is on.

**Ask.** `game.lightning{ from, to, seed?, colour?, width?, branches?,
ticks?, radius?, player? }`, sent to every player within `radius` (up to
the flash's 1024) in the domain, or to one. The client builds the path
from `seed`: midpoint displacement from `from` (a cloud base) to `to` (the
ground point the mod found), a few levels deep, with `branches` forks that
split off partway and fade before reaching the ground. Drawn as unlit,
additive, camera-facing ribbons `width` blocks across (default ~0.4, with a
soft glow a few times wider), depth-tested against terrain, over `ticks`
(default ~8): full at once, then flickering out — two or three dips and
returns, which is what reads as a return stroke. Presentation only; nothing
collides or is lit by it beyond what `flash` already does, and a seed makes
it the same bolt for everyone watching. Default colour violet-white, like
the reference: about `{ 0.85, 0.8, 1.0 }`.

Gate: a bolt from y + 300 to the ground at 400 blocks is visible as a line
at night and by day; two clients given the same seed draw the same path; a
player behind a hill does not see the part the hill hides.

**What weather does once it lands.** `fx.lua`'s `bolt()` already finds the
ground point and the cloud floor (`floor_at`), so it calls
`game.lightning{ from = { x, floor, z }, to = at, seed = ... }` beside its
`flash` and each flicker, feature-detected like every call since W2.

**From the engine, 2026-09-28 (engine 1eccb62, e0e47c8, bfe108b, ad8927b,
7820f82, 7a8596a):** `game.lightning{ from, to, seed?, colour?, width?,
branches?, ticks?, radius?, player? }`, as asked, addressed like
`game.flash` (the domain from `from`, `radius` up to the flash's 1024,
`player` narrowing) and answering how many were told. Defaults: colour
{0.85, 0.8, 1.0}, width 0.4, branches 3, ticks 8, radius 256; wrong
numbers are clamped, and a bolt longer than 4096 blocks keeps its top and
heading. A `seed` is read by its bits (a seed past 2^63 arrives whole); an
absent one is picked by the engine so two unseeded bolts differ. The
client builds the path once from the seed — midpoint displacement six
levels deep, the forks splitting off partway, thinner, tapering and ending
above the ground — with integer hashing and the deterministic subset, so
every client watching draws the same bolt (`path_hash`, unit-tested, and a
bot test in which two clients given one bolt hash the same path). Drawn as
unlit additive ribbons, a core and a glow four times wider, with a
floor of a pixel and a half so a bolt at four hundred blocks is still a
line; depth-tested and never written, so a hill hides what it should;
never clipped by the far plane, so a bolt a kilometre off is drawn whole;
over `ticks`, full at once and then two or three seeded dips. The gates as
written are screenshot-tested by day and by night in every lighting mode:
a bolt from y + 300 to the ground at 400 blocks is a line, the same at
1100, and behind a wall nothing of it shows on the wall. On the wire it is
`ServerMessage::Lightning`, appended, and the protocol is 77 from this
commit (0.2.2 ships at 77; the tooltip of UI ask 15 rides the same bump).
How it reads against the reference is the designer's ([H]): a storm over
the next valley, seen through rain. `fx.lua`'s `bolt()` adopts it beside
its `flash_at` and each flicker, feature-detected as every call since W2.

## W24. A settled fluid never evaporates (2026-09-25): LANDED 2026-09-26 (engine 1c475a8)

**From the engine, 2026-09-26 (engine 1c475a8):** a block lying open under a
fluid with `evaporates > 0` now stays on the solver's books until it is
empty or covered — one seeded hash a tick, so its roll comes up at the
fluid's rate rather than once — and is visited on the tick it does. The
gate as asked: a one-cell puddle at `evaporates = 50` is gone within a few
hundred ticks, the same under a lid stays, `evaporates = 0` is untouched
(`solver.rs`, `a_puddle_open_to_the_air_dries_at_its_rate…`). Unloading
drops a puddle from the books and §4.5's wake on reload puts it back. The
small one landed with it: `game.fluid_id(name)` answers a fluid's number
for the session (a bare name is your own), and `game.get_fluid` now names
its `fluid` beside `volume`, so `learn_rain_fluid`'s cell in the sky can
go. The sampler's puddle-clearing is yours to keep or retire. World 43
landed in the same commit: the Spindle may declare
`becomes = "tiamat_weather:damp_dirt"` on its soils; the drying — sampler
and random tick — still skips partial blocks, which is worth a line in the
exports contract before the Spindle switches its soils over.


**Seen.** "Rain creates way too many water sources": a world a few storms
old is spotted with rainwater films that never go, however long it stays
dry. `tiamat_weather:rainwater` is registered with `evaporates = 300`.

**Why.** `evaporates` is rolled inside `settle_one` (solver.rs, rule 4), so
it only comes up on a solver VISIT, and a block is visited only while it is
in the active set. A puddle that has finished spreading changes nothing on
the visit where its roll fails, so nothing re-wakes it, it leaves the set,
and it is never rolled again — `1 / evaporates` is the chance it EVER loses
a cell, not the rate. The tests use `evaporates = 1` or `2`, where the first
roll almost always lands, so they cannot see it. A chunk reload wakes loose
water (§4.5) and gives it one more roll, which is why it is not quite never.

**Why the mod cannot fix it.** It can only clear the puddles it happens to
sample near a player (now done, below); a puddle nobody stands near is
there for good, and every mod with an evaporating fluid meets the same.

**Ask.** A block holding a fluid with `evaporates > 0`, open to the air,
stays scheduled — kept in the active set, or in a separate evaporation set
visited at the fluid's rate — until it is empty or covered. Its cost is
proportional to the evaporable fluid lying open, which is exactly what the
mod asked to have evaporate. Gate: a one-cell puddle of an `evaporates = 50`
fluid on level ground is gone within a few hundred fluid ticks; the same
under a lid is still there; `evaporates = 0` is untouched.

**And a smaller one beside it.** `surface_at` names a fluid by its numeric
per-session id and nothing turns a fluid's NAME into that id, so to tell its
own rainwater from a river the mod writes one cell into an empty sky block,
reads the id back and clears it (`ground.lua`, `learn_rain_fluid`). A
`game.fluid_id(name)`, or the name beside the id in `surface_at`, would
retire that.

**What weather does meanwhile.** Fewer, smaller puddles (one sampled column
in 32, not 8; 4 cells in a storm, not 6), and the ground sampler clears any
rainwater it finds near a player once the rain has been gone half a minute.

## W25. Clouds at sunset: a brown sunward face (2026-09-25): LANDED 2026-09-28 (engine 7020552), awaiting the eye

**Seen.** Towards sunset, under a storm deck, the faces of the clouds that
face the sun are a flat brown-orange slab against violet-grey sides
(designer's screenshot, 19:16 on 2026-09-24): "a strange brown face, it
looks awkward. Just tinting the whole cloud a little would be fine, and
then brightening that face."

**Why.** In `clouds.wgsl` mode >= 1, `lit = mix(cool, warm, sunward^2 *
horizon)` with `warm = colour * sun_lit`: at a low sun `sun` is saturated
orange, so the sunward face takes the sun's hue at full strength, while the
side a few degrees away is `shade * sky`. Then storm `darkness` multiplies
by (0.30, 0.31, 0.38), and dark orange is brown. The hue change sits on a
face boundary, so it reads as a painted face rather than light.

**Why the mod cannot fix it.** `colour` and `shade` are the mod's only
inputs and are fixed at registration; the sun's colour and the mix are the
pass's.

**Ask.** Split the sun's contribution into a tint applied to the whole
cloud and a brightness applied by facing: every face takes a share of the
sun's hue (say `mix(white, sun_hue, 0.35)`, sun_hue being `sun` normalised
to its brightest channel), and the sunward term raises LUMINANCE rather
than swapping hue — `lit = cool_tinted * (1 + k * sunward^2 * horizon)`, or
mixing towards a desaturated `warm`. Under darkness the sunward face then
goes a warm grey, not brown. Gate: at a sun 3 degrees up, with darkness 0.9,
the sunward and side faces of one heap differ in luminance by more than
they differ in hue.

**For the sky's owner, 2026-09-23**, protocol v75 — not an ask, a
capability that landed for the space work and is the sky mod's to use: a
keyframe takes `stars` (0 to 1), how much of the star catalog shows at that
hour. The engine draws none unless a keyframe says so, so the real sky sees
no stars until `tiamat_weather` sets it — `stars = 1.0` on the night
keyframes and `0.0` on the day ones is the whole change; `game/core_sky`
does exactly that as the reference. The catalog wheels with the day as the
sun does, sits behind cloud and behind the ground, and is per domain
(`register_sky{ domain = ... }`). See `docs/engine-asks/tiamat_space.md`.

**W13 landed 2026-09-23**, protocol v73: `stratocumulus`, `altocumulus` and
`cumulonimbus` on `set_clouds`, a share each beside `cover`, with the ask's
own shapes — the low sheet of rolls with grooves between, the mackerel layer
on its own third interval, towers under anvils on a lattice seven heaps wide
with mammatus at 1 — plus the rind on every top, florets on the crowns and
storm light darkest at the base. Each genus is asked only at the heights it
can be, so a storm's kilometre of slab costs a lattice test per empty cell
rather than the whole field. Gates: a sheet with sky between, a layer of many
small cloudlets, an anvil wider than its tower. Cost on llvmpipe from the
ground, as a share of the cumulus deck: stratocumulus 0.8, altocumulus 1.1 to
1.4, storm 1.5 to 1.6, the supercell sky 2.9 to 3.7 — the last outside "about
twice", as the prototype's own table had it (2.81 against 1.09).
`how_long_each_genus_costs_from_the_ground` is the probe.

**W15 landed in full, 2026-09-23.** The bug half and the shapes on the day —
a cloud pixel is not fogged as terrain, the deck's own haze at the deck's
scale, a base flat per heap lifting at its rim, a heap with its own axis,
crown, height and floor, the self-shadow faded and culled, candidate cells
rejected before they are hashed, the pixel target at nine — and the last
step after: **`Normal` and `Coarse` draw the deck at half resolution**, into
a target of their own with its own depth, lifted into the frame one texel to
a block of pixels (nearest, so an edge is a step and not a smear) with the
depth written, so the terrain already drawn keeps its place and the glass,
the fluid and the particles drawn after still sort against the deck. `Fine`
marches every pixel. Gates: at `Normal` every two-by-two block of cloud is
one colour and at `Fine` a third of them are not; a deck under the floor
shows through it at neither. Cost on llvmpipe, Beautiful, Normal, Weather's
deck, the deck's own cost over a bare sky at 320 x 240, level / thirty
degrees up / above the deck: 1.20 / 1.83 / 4.04 ms before any of W15, and
the numbers after are in `how_long_the_deck_costs_from_three_views`.

**W23 landed 2026-09-24.** The designer's references (two pictures, the
same afternoon as W22): crowns that are CLUSTERS of distinct rounded
lobes with grooves between them, not domes wearing a texture. The floret
term rode the crown additively at 0.3, so a crown stayed an arc. Now the
lobes carve as well as ride — `LOBE_RELIEF 0.5`, centred on `LOBE_CUT
0.3`, so the grooves cut into the dome — and near the camera a second
octave of buds a quarter their size (`LOBE_FINE 0.14`, one more Worley
read per column with a crown) rides the lobes, faded by the rind's own
`detail_mix` and centred, so its absence at a distance is not a shorter
cloud and it cannot confetti the horizon; the big lobes alone hold the
outline at a kilometre, W13's lesson kept. The crest the reach tests
bound is now the shared `HEAP_CREST`, so `column_at`, `deck_slab` and the
crown cannot drift apart. Cost on the designer's own card (RTX 5070 Ti,
1080p, Beautiful, Weather's `cell 24, freq 1/850`): the level and 45-up
views moved within noise; the above-deck view at `Medium` +0.25 ms cloudy
and +0.33 ms storm (1.76 / 2.39 ms total), about 2% of a 60 fps frame in
the view that pays most; `High` above +2.4 ms (14.7 total), the
enthusiast rung's to spend. All sixteen cloud gates pass unchanged; the
pictures are the designer's to take.

**W22 landed 2026-09-24.** The deck's cube pattern read as a lattice: the
rind's cycle is about five cubes, so runs of neighbouring columns quantised
to the same shelf and every edge was a clean staircase. The designer asked
for "subtle, gnarly noise detail to break up the clear cell pattern just a
little", and could be "extremely subtle". A second octave of the rind's own
value noise — four times the frequency (29 cycles per field unit, about a
cycle per large cube at Weather's numbers) and a quarter of the amplitude,
centred so it gnarls both ways — folded INTO the rind, so every top and
every ruffled underside that takes the one takes the other and it fades
with distance exactly as the rind does. No new field on `register_clouds`:
the 22-cycle experiment at the rind's own amplitude was confetti, and the
distance between "subtle" and "confetti" is a judgement the engine keeps
rather than a knob it hands out — the same reasoning `sway` records. All
sixteen cloud gates pass unchanged; the pictures are the designer's to
take (`pictures_of_the_deck_for_the_designers_eye`).

**W21 landed 2026-09-24.** Each cloud is decided from the weather at its
own centre, and the column's weather is for the darkness alone. A heap asks
`weather_at` at its own point — the inverse of the `at` mapping, so drift
and evolution are honoured — for its threshold and its strength, after a
cheap reject against the loosest threshold any cell of the sky sets (the
sky's highest cover now rides in `genera.w`); a tower asks at its centre for
whether it stands and how tall, and the section is entered wherever any
cell has a storm rather than where the column does, so an anvil is whole
over a clear cell; the sheet and the layer ask at the lattice point of
their area noise, since `cells` keeps its feature point to itself. Cost:
two fetches per candidate that passes the reach tests, as the ask reckoned;
the ladder probe's numbers after are in the commit.

The gate is not the ask's — a silhouette cannot tell a cut heap from a near
cube's edge, and from above a march column snaps a whole heap's edge to the
same lines a cut would; four silhouette metrics were tried against the old
and new shaders side by side and none told them apart. It measures the
mechanism instead: a map clear
everywhere but a small wet square around the camera, seen straight down.
Past the square's ramp every column's weather is the clear sky's, so decided
per column the deck out there is the clear sky's to the pixel and the
square is a square cloud; decided at its own centre, a heap centred in the
square reaches past the ramp, and so does a tower's anvil. On the old rule
the difference was exactly zero pixels in every sky; on the new it is
thousands where a heap or a tower sits in the square. The designer's two
views are theirs to take again.

**W20 landed 2026-09-24.** The heap curve's top is 0.53: `radius = spacing *
(0.30 + 0.53 * sqrt(strength))`, the floor untouched, the search's reach
`(0.30 + 0.53) * (1 + HEAP_STRETCH) - 0.15 = 0.85` cells, under one — and a
unit test reads all three numbers out of the shader so the reach and the
curve cannot drift apart. Measured straight down from 2,200 blocks, seed
4242, before and after: at cover 0.30 the widest cloud went from 290 to 427
px (two strong neighbours as one bank, 1.47 times) while the smallest real
heaps stayed 22 to 24 px wide, and fifty separate clouds became thirty-four;
at cover 0.55, thirty-five clouds and 169,386 cloud pixels became twenty-two
and 197,350 — fewer clouds and more cloud. The engine's share alone is the
fifth on the big end; with the mod's quarter on `frequency` it is the
designer's 1.5, and 0.60 was not taken: 0.85 leaves the three-by-three room
and 0.93 would not. The gate is
`the_big_heaps_are_bigger_the_small_ones_are_not_and_neighbours_make_a_bank`.

**W19 landed 2026-09-24.** Three rungs, four times apart, the bottom one
the default: **`Low`** at a quarter of the frame on each axis, double cubes,
three kilometres, no rind and — since the self-shadow reaches twice the rind
— no self-shadow; **`Medium`**, what `Normal` was; **`High`**, what `Fine`
was. The old names still read from a config file onto the rung each meant,
so nobody's `normal` changes what it draws, and a fresh config draws at
`Low`. The settings screen has a `Clouds` choice now — it had none, the
setting was a line in the file — with what each rung costs written beside
it. Gates: `Low` is cheaper on every axis and never further; every
four-by-four block of cloud at `Low` is one colour; the deck under the floor
stays behind it at every rung. Weather's probe is in the tree as
`how_long_weathers_deck_costs_by_knob`, on the new rungs, with the frame
from `TIAMAT_PROBE_SIZE`; the 1920 x 1080 numbers on a real card are the
designer's to take, since here there is only a software renderer — its
ratios at 640 x 360 are in the commit. The two things "not asked for" —
coarse steps down through a storm's empty middle, and skipping `column_at`
where a map cell is all zeros — are not done.

**W18 landed 2026-09-23.** Both halves. The map is read **filtered**: it
travels to the shader as two small textures — cover, darkness, stratocumulus
and altocumulus a byte each in one, cumulonimbus in the other — under a
linear, clamped sampler, bilinear between cell centres, so a front is a
gradient a cell wide and the texture unit does the four fetches for the
price of one; the last half cell before the grid's edge fades into the plain
state too, so the grid's own boundary is not a line either. The packed
uniform array is gone. And the deck is **eased on the client** as
`ease_ticks` promised: cover, darkness, the three genera and a floor blend
from wherever they had got to over the ticks the new state names (a floor
appearing steps, having nothing to ease from), a clearing takes as long as
the weather took to arrive, and the map eases cell by cell over the same
ticks — from the previous map on the same grid, up from the plain state
where there was none, down to it when it goes, and at once when the grid
moved or resized, since its cells are not the old ones. Gates: a map dark on
one side, overcast on both, read straight up — the darkness across the seam
crosses a fifth to four fifths over 132 px where a quarter cell is 54, and
never rises back; a nearest read crossed in one grid column. Clear to storm
with `ease_ticks = 600` is not overcast on the next frame, half way at
fifteen seconds and exactly there at thirty; a map re-sent with one cell
darker moves that cell by nothing in a frame and by half in fifteen seconds,
and the cells that did not change do not move. The designer's two views are
theirs to take again.

**W17 landed 2026-09-23.** The deck's sun is the terrain's — the keyframe's
colour times its intensity, as `world.wgsl` has it — and a sun under the
horizon lights nothing: the warm term, the low-sun rim and the in-scatter
read a sun faded by `smoothstep(-0.15, 0.0, toward_sun.y)`, full at the
horizon and gone about eight degrees under it, so dusk keeps its lit bases
and midnight has none. Both lines are the prototype's. Gate, at Core Sky's
midnight from under an overcast `tuned_deck`, Beautiful: the deck from below
is no brighter than twice the sky behind it (0.12 against 0.17) nor than
its own top from above (0.11); at golden hour the bases are still lit warm
(red less blue 0.23, four times brighter than at midnight). One reading in
the ask did not survive measurement: at 0.73 the tops are a touch warmer
than the bases (0.239 to 0.228), and were before this — a sun seven
degrees up faces the tops — so the gate keeps the bases lit rather than
warmer. The pictures are the designer's to take.

**W16 landed 2026-09-23**, protocol v74: `map` carries `stratocumulus`,
`altocumulus` and `cumulonimbus` per cell beside `cover` and `darkness`, each
optional and a byte a cell; left out, a genus is none anywhere, so a map
without them is today's map. Inside the grid a cell's five shares replace the
player's own, as the cover did; the slab the march clips to is sized by the
most of each genus anywhere in the sky. The uniform stays at 2 KiB, the five
bytes packed into two words a cell. Gate: a `cumulonimbus` cell two cells out
raises an anvil over the horizon on its side and none on the other, mirrored
with the cell, and a map without the arrays raises none.

**From the engine, 2026-09-28 (engine 7020552):** the ask's own shape,
in `clouds.wgsl`'s mode-1 lighting. The sun's colour is split into a hue
(the sun normalised to its brightest channel) and a brightness (that
channel). Every face takes a share of the hue — `mix(white, sun_hue,
0.35 * horizon)` over both the cool and the warm term — and the sunward
term now mixes towards `colour * tint * brightness` rather than
`colour * sun`, so what the sunward face gains is luminance over the
shared tint, not a hue swap on a face boundary. Under a storm's
darkness the sunward face goes a warm grey. The shader compiles under
the screenshot suite; the gate — a sun three degrees up, darkness 0.9,
the sunward and side faces of one heap differing more in luminance than
in hue — is the designer's screenshot to take (an [H] gate).

## Landed

**W11 landed 2026-09-23.** The cloud pass draws the deck from straight below
once a frame — a 256-texel map, one texel a large cube, over four kilometres
around the camera, every genus included — and the terrain darkens its sun
term by what stands where each fragment's line to the sun crosses the deck's
floor: one sample, soft at a heap's rim, moving with the drift. Classic and
Beautiful; Simple skips it as it skips the rest of the lighting. The sky's
light is untouched, so a floor under a cloud is shaded and not a cave. Gates:
an overcast deck darkens the floor in every mode but Simple, and a minute of
drift moves the shade. Figures and props are not shaded by it yet — their
shaders have no view of the map — which is the next thing to ask for if a cow
under a cloud looks too bright.
