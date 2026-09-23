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

## W17. The deck glows from underneath at night (2026-09-23)

**Seen.** The designer, in game: "clouds at night time seem to glow from the
underside. they kind of just need to be dark." With Core Sky's keyframes the
night deck is a blue-lit slab seen from below, over ground that is nearly
black.

**Why.** Two things in the client, and they compound.

1. `prepare_clouds` (`crates/client/src/render/mod.rs`, `clouds::Frame`)
   hands the deck `self.sun_colour` as it is. The terrain is drawn as stored
   sunlight times `sun_intensity` (`world.wgsl`: `input.sun *
   globals.sun_intensity`), and the deck never sees that number. Core Sky's
   night frames are `sun = {0.35, 0.45, 0.80}` at `intensity = 0.08`: the
   ground stands at eight percent and the deck's lit side at a hundred, in
   moonlight blue.
2. At night the sun is UNDER the horizon, and `clouds.wgsl` lights by
   geometry alone: `facing = dot(normal, toward_sun)` is largest on the
   deck's underside, so the warm term (`colour * sun`), the low-sun rim
   (`sun * rim * 0.9`) and the in-scatter (`colour * sun * scatter * 0.55`)
   all land there. Golden hour is backlit on purpose — the sun at the horizon
   lights bases — but nothing stops the same rule once the sun has set.

Nothing on the mod's side reaches either: `register_clouds`'s `colour` and
`shade` are constants for the life of the deck, and `set_sky_modifier`'s
`intensity` multiplies a number the deck does not read.

**The ask**, two lines and a factor —
`tiamat_weather/night-deck-prototype-2026-09-23.patch` beside this sheet (it
is the diff of the two files and has not been run by the mod's author):

- In `prepare_clouds`, `sun: self.sun_colour * self.sun_intensity`,
  component-wise, exactly as the terrain has it. This alone takes the night
  deck from a hundred percent to eight.
- In `fragment_main`, `let horizon = smoothstep(-0.15, 0.0, toward_sun.y);
  let sun_lit = clouds.sun.xyz * horizon;` and the three sun terms read
  `sun_lit` in place of `clouds.sun.xyz`, with the `warm`/`cool` mix also
  scaled by `horizon`. Full at the horizon, so dusk keeps its lit bases; gone
  once the sun is about eight degrees under, so midnight has none. What is
  left at night is `shade * sky`: with Core Sky's `{0.02, 0.03, 0.08}` that is
  dark, which is what was asked for.

Neither changes a daytime frame: `horizon` is 1 whenever the sun is up, and
`sun_intensity` is 0.95–1.0 through the day.

**Acceptance.** At `time = 0.0` with Core Sky, Beautiful, a camera under an
overcast deck: the deck's mean luminance from below is no more than twice the
sky's and no more than its own top's seen from above at the same time. At
`time = 0.73` (golden hour) the bases are still lit warmer than the tops, as
today. The three picture views at `time = 0.0` and `0.73` for the eye.

**W16 landed 2026-09-23**, protocol v74: `map` carries `stratocumulus`,
`altocumulus` and `cumulonimbus` per cell beside `cover` and `darkness`, each
optional and a byte a cell; left out, a genus is none anywhere, so a map
without them is today's map. Inside the grid a cell's five shares replace the
player's own, as the cover did; the slab the march clips to is sized by the
most of each genus anywhere in the sky. The uniform stays at 2 KiB, the five
bytes packed into two words a cell. Gate: a `cumulonimbus` cell two cells out
raises an anvil over the horizon on its side and none on the other, mirrored
with the cell, and a map without the arrays raises none.

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
