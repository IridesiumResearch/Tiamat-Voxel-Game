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

## W16. The cover map has no genera, so a storm over the next valley has no anvil (2026-09-23)

**Seen.** With W13 landed, Weather sends a storm as stratocumulus 0.70 under
cumulonimbus 0.60 at cumulus `cover` 0.40, darkness 0.90 — the sky its plan
(10.10) asked for. From the clear valley beside it, the same storm is 40%
cumulus heaps under a dark haze: `set_clouds{ map }` (W10) carries `cover` and
`darkness` per cell and nothing else, and `weather_at` in `clouds.wgsl` reads
the three genera from the per-player uniform alone, so the sheet and the
towers stand only over the square the player is in. A front cannot be watched
coming as the front it is; the anvil appears when you walk under it.

**Why the mod cannot fix it.** The map is the only per-place channel and it
has two bytes a cell. Sending the storm's whole cover as cumulus (`cover` 1.0
in the cell) would make the distance read overcast — but inside the grid the
cell REPLACES the per-player `cover`, which is what makes the cell overhead
the sky overhead, so the same trick puts a full cumulus deck under the towers
overhead: by W13's own figures (storm 1.5–1.6, supercell 2.9–3.7 of a cumulus
deck) the two costliest genera at once, for nothing the eye can tell apart.
Weather keeps the map cumulus and darkness until this lands.

**The ask.** Three more arrays on `Tiamat.CloudMapSpec`, each optional:
`stratocumulus`, `altocumulus`, `cumulonimbus`, `size * size` shares of one
like `cover`, a byte each on the wire, so a cell is five bytes and a 16 x 16
map 1,280. Left out, a genus is 0 in every cell — so a mod that sends today's
map sends exactly today's sky. `weather_at` answers five shares in place of
two, and each genus reads its share from the cell as cumulus does now, the
per-player value answering outside the grid.

**Acceptance.** A map with `cumulonimbus` 1 in one cell and 0 in the rest
draws an anvil over that cell and none over its neighbours, from a camera two
cells away; a map without the new arrays draws what it drew before, pixel for
pixel; a mod sending the new arrays to an engine without them is refused with
the field named, as a misspelt genus is.

**What Weather does once it lands.** `map_around` sends the five shares per
cell from the same `sky_of` that already computes them, and `/weather clouds`
counts stormy squares by their towers rather than their darkness.

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
