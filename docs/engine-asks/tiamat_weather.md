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

## W15, what is left: `Normal` at half resolution (2026-09-23)

The rest of W15 landed 2026-09-23. A cloud pixel is not fogged as terrain:
the pass marks its pixels and mode 3's post chain fogs them as the sky
beside them, and the deck's own haze runs to its reach and takes at most 0.7
of the contrast, with only the last stretch before the reach fading the rest
so the edge is not a line. A base is flat per heap and lifts at its rim; a
heap has its own axis, crown, height and floor, from one more hash. The
self-shadow fades and stops past twice the detail reach, candidate cells are
rejected before they are hashed, and the pixel target is nine. The crown is
a blend of two square roots rather than `pow`, which is the same family of
shapes for a fraction of the cost.

`a_deck_past_the_terrains_fog_is_still_drawn_in_every_mode` is the gate for
the bug: at a 256-block view distance, in every lighting mode, the deck
keeps at least seven tenths of its pixels and half of its contrast. The rest
of the acceptance is a person's to judge, so
`pictures_of_the_deck_for_the_designers_eye` writes Weather's deck from the
four views in every mode at that view distance:

```
TIAMAT_CLOUD_PICTURES=<dir> cargo test -p client --test screenshot \
    pictures_of_the_deck -- --ignored --nocapture
```

**What did not land.** `Quality::resolution_scale` is not wired to
anything: the pass draws into the frame's own target at full size, so the
prototype's "Normal at half resolution" changed a number nobody read, and
the saving that step measured came from the pixel target beside it. Drawing
the deck into a half-size target and resolving it — depth included, so
fluid and particles still sort against it — is its own piece of work, and
it is still owed. The constant's doc says so.

**Cost, measured here, and not the last word.** On Mesa's llvmpipe at
320 x 240, Beautiful, Normal quality, Weather's deck, median of five
interleaved runs, the deck's own cost over a bare sky went from
1.20 / 1.83 / 4.04 ms (level / 30 degrees up / above the deck) to
1.85 / 3.14 / 8.83. The pixel target and the culls pay here too, but the
slab the march clips to grew by the tallest heap and the highest floor —
the prototype kept the old ceiling and clipped the tallest crowns flat from
above; this does not — and a software rasteriser charges for the branches
in the heap search in a way a GPU does not. The designer's own GPU numbers
for this design showed a saving, so the acceptance is theirs to measure;
the constants at the top of `clouds.wgsl` are where the trade sits.

## Nothing open

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
