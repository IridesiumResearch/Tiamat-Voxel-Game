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
it is lower priority than W10.

## W10. A storm on the horizon: a coarse cover map (deferred from W2, 2026-09-18)

**Seen.** `set_clouds(player, { cover, darkness })` sets one cover for the
whole sky a player sees. A storm over the next valley cannot be seen from the
clear valley beside it: the deck is either overcast everywhere or nowhere,
from that player's point of view. Rain at a distance is a curtain of dark
cloud kilometres away, and `set_precipitation` spawns only around the camera,
so this is the only way a player can watch a front coming.

**Why the mod cannot.** The mod knows the weather over every 256-block square
(`controller.lua`), but `set_clouds` takes a single number per player.

**Smallest change.** Let `set_clouds` take an optional coarse grid of cover
and darkness around the player, which the client samples in the march instead
of the single value:

```lua
game.set_clouds(uuid, {
    cover = 0.55, darkness = 0.0,        -- still the value outside the grid
    map = {
        origin = { x = x0, z = z0 },     -- world blocks, the grid's corner
        cell = 256,                      -- blocks per cell (Weather's square)
        size = 16,                       -- 16 x 16 cells: 4 km on a side
        cover = { ... },                 -- size*size numbers, row-major by z
        darkness = { ... },
    },
    ease_ticks = 600,
})
```

Sent on change like everything else. Weather would recentre the grid on
the player's square, which changes once a player crosses 256 blocks, and fill
it from the squares it already evaluates.

**Acceptance.** Standing under a clear square with a forced storm three
squares east, the east horizon shows a grey deck and the sky overhead does
not.
