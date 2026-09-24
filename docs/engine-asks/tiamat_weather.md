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

Nothing open: W19 and W20 landed 2026-09-24, W17 and W18 the day before, below.

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
