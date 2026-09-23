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

## W13. Cloud genera: cumulus, stratocumulus, altocumulus, cumulonimbus (2026-09-19, revised)

**Revised the same day, before any of it was built.** The first version
asked for three kinds (fair, storm, mega storm). The designer then asked
for more: the heaps are "pretty undetailed noise-wise", and the sky wants
"somewhat realistic shapes like stratocumulus, altocumulus and cumulonimbus
approximations". This replaces it. The first version's pictures and patch
stay in `tiamat_weather/` (`cloud-kinds-*`) as the record.

**Why the mod cannot.** The deck's shape is registration-only, one shape
for the whole world; per player `set_clouds` moves `cover`, `darkness` and
`base`. A sheet of stratocumulus, a mackerel sky and an anvil are shapes,
and none of them can be sent.

**Evidence.** A prototype of `clouds.wgsl` against c160b05, rendered in the
screenshot harness (a temporary ignored test, since removed), 960 x 540,
Beautiful, Normal quality, Weather's deck (cell 16, thickness 160, 1/500,
towers 0.2), base 400 over the camera:

![five skies](tiamat_weather/cloud-genera-2026-09-19.png)

Rows: fair cumulus (cover 0.45); stratocumulus (0.75, darkness 0.15);
altocumulus over a few cumulus (0.8 and 0.2); a storm (stratocumulus 0.85,
cumulonimbus 0.6, cumulus 0.2, darkness 0.7); a mega storm (cumulonimbus 1
among cumulus, stratocumulus and altocumulus at 0.3, darkness 0.6).
Columns: level, 30 degrees up, level and turned 90 degrees, and from 480
blocks over the base. The patch is
`tiamat_weather/cloud-genera-prototype-2026-09-19.patch`: a sketch to
measure against, not a patch to merge. The three new numbers ride in
`view.yzw` from an environment variable; the real thing wants them in
`Clouds`.

Frame time with the deck, ms, this machine's adapter (a bare sky is 0.6 to
0.8):

| | level | up | side | above |
|---|---|---|---|---|
| cumulus | 1.09 | 1.10 | 0.93 | 1.19 |
| stratocumulus | 0.78 | 0.81 | 0.78 | 0.94 |
| altocumulus | 1.50 | 1.96 | 1.48 | 1.82 |
| storm | 1.53 | 1.93 | 1.57 | 11.27 |
| mega storm | 2.81 | 4.19 | 2.72 | 10.90 |

From the ground everything is within about twice today's deck. From above
a cumulonimbus sky is the expensive one: the slab is a kilometre tall and
every pixel marches it. A separate, tighter bound for the anvils (they are
few and their lattice is known) would bring that down.

### The ask

**Per-player cover per genus, on `set_clouds`, eased with the rest:**

```lua
game.set_clouds(uuid, {
    cover = 0.2,            -- cumulus, as today
    stratocumulus = 0.85,   -- 0..1, default 0
    altocumulus = 0.0,      -- 0..1, default 0
    cumulonimbus = 0.6,     -- 0..1, default 0; at 1, supercells
    darkness = 0.7,
    ease_ticks = 600,
})
```

Numbers, so a front arriving blends one sky into the next. `cover` keeps
meaning cumulus, so a mod written for today's deck is unchanged.

**What each genus is in the prototype:**

- **Every top gets a rind:** about one small cube of low-frequency value
  noise (7 cycles per field unit), near the camera only. At the frequency
  first tried (22) it turned every crown to confetti at 16-block cubes;
  lower and gentler reads as lumps.
- **Cumulus:** W12's heaps, as a bun (`pow(1 - d^2, 0.4)`, fuller over the
  crown than a hemisphere), with florets from Worley cells a third of a
  heap across (`F2 - F1`, rounded) riding the crown and fading at the rim.
- **Stratocumulus:** low and thin (a third of the deck's thickness). Worley
  cells drawn out into rolls across the wind (0.46 by 0.27 field units,
  about 230 by 135 blocks on Weather's deck, so each cell spans a dozen
  cubes and can be round), in patches; each cell a rounded cushion, with
  grooves of sky between that close as cover rises. The cells must be many
  cubes wide: at a quarter this size they were confetti.
- **Altocumulus:** a mid-level layer (2.4 times the deck's thickness over
  the base) of small cloudlets (Worley cells about 70 blocks), lined up in
  wave bands the way a mackerel sky is, in patches. Lens-shaped: they grow
  up more than down. This is the one that needs **a third interval**
  (`Column.mid`), since it can sit under an anvil and over a heap in one
  column. It is also the one the cube size limits most: from the ground it
  reads as a dappled sheet, from above as tiles. A finer grid for this
  layer alone would help; an 8-block grid for the whole deck did not, much.
- **Cumulonimbus:** W13's first-version tower and anvil, on a lattice seven
  heaps wide, more of them and larger as the value rises (at 1, supercells
  of 880 blocks with mammatus). Florets from larger Worley cells on the
  tower's flanks. The anvil's top now thins to nothing at its rim instead
  of ending in a sheer edge.
- **Storm light:** `darkness` weighted by height through the cloud (`1 -
  0.6 * rel`), so bases are darkest and tops keep their sun.

### What Weather does once it lands

Clear: a few cumulus, some altocumulus. Cloudy: cumulus, stratocumulus and
altocumulus. Rain and snow: a thick stratocumulus sheet. Storm and blizzard:
stratocumulus under cumulonimbus. Mega storm (twice a year, built in
Weather 03f1925): cumulonimbus 1.

**Acceptance.** In the harness's four views: a stratocumulus sheet of
rounded cells with sky between; altocumulus as banded cloudlets from the
ground; a cumulonimbus reading as a tower under a spreading anvil; crowns
lumpy rather than smooth arcs. From the ground no genus costs more than
twice today's deck.

## W11. Cloud shadows on the ground (deferred from W2, 2026-09-18)

**Seen.** The deck drifts overhead, but the ground under a cloud is lit
exactly as the ground under clear sky. A patch of shade moving over a
hillside is most of what makes a drifting sky read as drifting from the
ground, and it is in the designer's references (`docs/reference/` in the
weather repo).

**Why the mod cannot.** The deck is marched on the client from a field the
mod never sees, and the sunlight on terrain is the client's own. The mod has
no per-pixel say in either.

**Smallest change.** In the terrain pass, one sample of the same cloud field
along the sun direction from each lit fragment, darkening the sun term by the
deck's density there. Mode 1 can skip it, as it skips other lighting.

**Deferred by agreement** when W2 was built. Filed here so it is not lost;
it is lower priority than W10 and W13.
