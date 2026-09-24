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

Nothing open: W17 and W18 landed 2026-09-23, below.

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

## W19. The deck's cost on a real card, and a ladder with a cheap rung at the bottom (2026-09-23)

**Asked for.** The designer: "on lower end machines these clouds are causing
some pretty good fps lag. I think we need to work on optimizing. It might be
helpful to have two or three different resolutions of clouds as well with
the lowest resolution being the default."

**Measured.** The engine's probes run at 320 x 240, and on this machine's
card (an RTX 5070 Ti, Vulkan) the deck's share is under the timing noise —
`how_long_the_deck_costs_from_three_views` printed negative "added"
columns. So a probe of Weather's deck as it is registered (cubes of 16,
detail 2, thickness 200, frequency 1/680) at 1920 x 1080, Beautiful, the
median of nine captures, the deck's cost over a bare sky: every tier, the
six skies Weather sends most, three views. Its source is beside this sheet,
`deck-cost-probe-2026-09-23.rs`, written to be appended to
`crates/client/tests/screenshot.rs` (it uses that file's helpers); it was
run there and taken out again. Milliseconds on this card:

| view | Coarse | Normal (default) | Fine |
|---|---|---|---|
| level, clear / cloudy / storm | 0.31 / 0.30 / 0.59 | 0.34 / 0.32 / 0.34 | 1.56 / 1.43 / 1.49 |
| 45° up, clear / cloudy / storm | 0.49 / 0.44 / 0.53 | 0.60 / 0.59 / 0.85 | 3.52 / 3.07 / 3.47 |
| above the deck, clear / cloudy / storm / mega | 0.94 / 0.99 / 1.69 / 1.74 | 1.21 / 1.34 / 2.26 / 2.44 | 8.01 / 8.63 / 14.74 / 16.21 |

(The full table, rain and mega included, is in Weather's plan 10.19.)

**What it says.**

1. **`Fine` is five to seven times `Normal`** in every view: full
   resolution is four times the pixels and half-size cubes twice the steps,
   and above a storm it is 14.7 ms here — a card twenty times slower spends
   a third of a second on the sky. Anyone who chose the top of the slider
   because it said "fine" is the fps report.
2. **`Coarse` is barely cheaper than `Normal`** — 0.94 against 1.21 above
   the deck, 0.49 against 0.60 at 45° — because `reach()` gives it 6 km
   against Normal's 4: the double-size cubes' saving is spent on distance.
   The ladder as it stands is Off, about-the-same, about-the-same,
   six-times, and the default is on the second rung.
3. **The worst view is from above**, where every ray crosses the whole slab:
   twice the level cost at every tier, and a storm's kilometre of slab
   doubles it again (2.26 against 1.21 at Normal, 14.7 against 8.0 at Fine).
   A player who flies is the second fps report.
4. Weather's own knobs are modest and are turned (its 10.19): cubes 16 → 24
   saves a seventh; surface detail saves nothing; thickness a tenth; the
   heaps' size nothing.

**The ask.** Three rungs spaced about four times apart, the bottom one the
default:

- **`Low` (default)**: quarter resolution (`resolution_divisor` 4 — the
  half-size target already exists, this is its size), cubes at 2x, reach
  **3 km** rather than 6 (a coarse tier should be cheaper, not further), no
  detail rind, and the self-shadow off (`sun_shadow` is six more column
  lookups per hit in modes 1 and 2, and at four pixels a texel nobody sees
  the rim it darkens). Expected a quarter of today's Normal or better.
- **`Medium`**: today's Normal — half resolution, 1x cubes, 4 km.
- **`High`**: today's Fine, full resolution — with the slider's text saying
  what it costs, since it is the one that hurts.

Two more that cost nothing to look at while there: the slab from above is
marched at the same step as from the side, and a ray going DOWN through a
storm's kilometre could take its steps at the coarse cube until it is inside
the low interval, since nothing lives in the anvil's empty middle; and a
map cell that says every genus is zero over a column could skip
`column_at` before the heap search rather than after the sheet's tests.
Neither is asked for; the ladder is.

**Acceptance.** `how_long_weathers_deck_costs_by_knob` (or the probe's
successor) at 1920 x 1080 on real hardware: `Low` at or under a quarter of
today's `Normal` in all three views; `Medium` within ten percent of today's
`Normal`; `High` no worse than today's `Fine`; a fresh config draws at `Low`.
The pictures at all three, for the eye: `Low` should still read as the
designer's cubes, because the cube's edge is what survives a coarse texel.

## W20. The big heaps should be bigger than the small ones are (2026-09-23)

**Asked for.** The designer: "the clouds also seem kind of small to me. I
would make the average cloud about 25% bigger and the big clouds about 75%
bigger."

**What the mod can do, and did.** A heap's size is `spacing * (0.30 + 0.38 *
sqrt(strength))` in radius, spacing `0.42 / frequency`, so the one knob a mod
has — `frequency` on `register_clouds` — scales every heap alike. Weather
took it from 1/680 to 1/850: every heap a quarter bigger, 215 to 485 blocks
across on a lattice 357 apart. That is the average. It cannot make the big
ones grow more than the small ones, and the designer asked for exactly that:
big clouds are banks, and a bank is heaps grown into each other.

**The ask.** Let the curve's top end grow: `radius = spacing * (0.30 + 0.53 *
sqrt(strength))`, so the smallest heap is as it was and the largest is 0.83
of the spacing instead of 0.68 — a fifth wider again, which with the mod's
quarter is the big clouds 1.5 times their size before today; 0.60 in place
of 0.53 makes it 0.90 of the spacing and the full 1.75. Either way the
largest heaps overlap their neighbours into banks, which is the look asked
for. Two things follow, both the engine's:

- `reach` in the heap search is `(0.30 + 0.38) * (1 + HEAP_STRETCH) - 0.15`
  cells; it becomes `(0.30 + 0.53) * 1.2 - 0.15 = 0.85` (or 0.93 at 0.60).
  Under 1.0, so the three-by-three neighbourhood still holds every heap that
  can reach a column; at 0.60 it is close enough to the edge that a stretched
  heap's far tip may be missed from a corner cell, so 0.53 is the safe
  figure and 0.60 the one to check against the gate below.
- The self-shadow and the cost of a column scale with how many candidates
  survive the cheap tests; wider heaps survive more of them. Worth a line in
  `how_long_weathers_deck_costs_by_knob`'s successor, whatever it costs.

**Acceptance.** With cover 0.55, seed 4242, the widest heap in the field is
at least 1.45 times as wide as before this change and the narrowest is
unchanged; two neighbouring strong heaps read as one bank with no seam of
sky between them; no heap is clipped at a lattice edge from any of the three
probe views.

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
