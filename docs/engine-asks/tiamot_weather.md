# Engine asks from Tiamot Weather

From the `tiamot_weather` mod (repo `Tiamot_Default_Weather`, beside the
engine checkout). Kept here, in the engine's `docs/engine-asks/`, so the
engine agent finds every mod's asks in one place and they are versioned with
the engine. Pictures and patches an ask cites are in `tiamot_weather/` beside
this file; W12's are kept there as the record of engine 0d8e857.

Numbered W*n*, continuing the weather sheet: W1–W9 were filed in that repo's
`docs/engine-asks-weather.md` and all of them are built or answered. That
file stays as the history. Each entry says what was seen, why the mod cannot
fix it, and the smallest engine change that would. Newest first. Items are
removed when they land.

## W14. A fluid that does not wash plants away (2026-09-22)

**Seen, reading e4ac3a8 (`washes_away`, World ask 37).** A block that
declares `washes_away` is cleared by ANY fluid running into it. Weather's
puddles are a fluid: `tiamot_weather:rainwater`, a few cells left on open
ground in rain, which spreads and evaporates. Beside the Spindle, every
passable plant (grass, ferns, flowers: `tdw.washes_away` in its
`blocks.lua`) is the obvious thing to declare it on, and the moment it
does, a shower strips the meadows it falls on.

**Not happening yet**, which is why this is filed now rather than as a
bug. The Spindle still washes plants from its own `on_fluid_flow` rule, and
that rule skips the fluids other mods have told it are harmless
(`add_harmless_fluid`, which Weather calls for rainwater). The engine flag
has no such exception, so moving to it loses one.

**Why the mod cannot.** `washes_away` is the plant's declaration and names
no fluid; Weather registers the fluid and cannot reach another mod's
blocks. Weather could stop leaving puddles near plants, but a puddle's
whole point is that it spreads a little, and it cannot know which blocks
some other mod has made washable.

**Smallest change**, either of:

- **On the fluid:** `register_fluid{ ..., washes = false }` (default true),
  a fluid too gentle to sweep a plant. Rain on grass, a trickle of milk. The
  fluid's author knows this and no plant's author has to list every
  fluid in the world.
- **On the block**, mirroring W7's `absorbs.fluid`: `washes_away = { fluid =
  "core:water" }`, or a list, for the fluids that sweep it. Weaker for this
  case: every plant mod has to know about rainwater.

The first is the one asked for.

**Acceptance.** Two tufts that declare `washes_away`; water run into one
clears it, rainwater (declared `washes = false`) run into the other leaves
it standing, and the rainwater stands in its block as water stands in a
tuft today.

## W13. Cloud genera: cumulus, stratocumulus, altocumulus, cumulonimbus (2026-09-19, revised)

**Revised the same day, before any of it was built.** The first version
asked for three kinds (fair, storm, mega storm). The designer then asked
for more: the heaps are "pretty undetailed noise-wise", and the sky wants
"somewhat realistic shapes like stratocumulus, altocumulus and cumulonimbus
approximations". This replaces it. The first version's pictures and patch
stay in `tiamot_weather/` (`cloud-kinds-*`) as the record.

**Why the mod cannot.** The deck's shape is registration-only, one shape
for the whole world; per player `set_clouds` moves `cover`, `darkness` and
`base`. A sheet of stratocumulus, a mackerel sky and an anvil are shapes,
and none of them can be sent.

**Evidence.** A prototype of `clouds.wgsl` against c160b05, rendered in the
screenshot harness (a temporary ignored test, since removed), 960 x 540,
Beautiful, Normal quality, Weather's deck (cell 16, thickness 160, 1/500,
towers 0.2), base 400 over the camera:

![five skies](tiamot_weather/cloud-genera-2026-09-19.png)

Rows: fair cumulus (cover 0.45); stratocumulus (0.75, darkness 0.15);
altocumulus over a few cumulus (0.8 and 0.2); a storm (stratocumulus 0.85,
cumulonimbus 0.6, cumulus 0.2, darkness 0.7); a mega storm (cumulonimbus 1
among cumulus, stratocumulus and altocumulus at 0.3, darkness 0.6).
Columns: level, 30 degrees up, level and turned 90 degrees, and from 480
blocks over the base. The patch is
`tiamot_weather/cloud-genera-prototype-2026-09-19.patch`: a sketch to
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
