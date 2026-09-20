// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a mod does to one player's sky after the keyframes: weather.
//!
//! # Why a modifier, and why per player
//!
//! `game.register_sky` takes its keyframes in the registration window and the
//! client interpolates them from the clock; nothing a mod does after load
//! could move the sky. A storm arriving under a noon sky read as a sprinkler.
//! A modifier sits OVER the keyframes — a multiplier on intensity, a lerp of
//! the horizon colour, a scale on fog distance and grade saturation — and is
//! eased client-side over the ticks the mod asked for, exactly as the
//! keyframes are. Per player rather than per domain because two players in
//! one domain can stand under different weather.
//!
//! Presentation only and outside every determinism hash, like the keyframes:
//! the client scales stored sunlight at draw time, so a darkened sky costs
//! no relight. Weather ask W1.

use crate::identity::PlayerUuid;

/// The longest ease a mod may ask for: two minutes.
pub const MAX_EASE_TICKS: u32 = 2400;

/// A mod's standing change to one player's sky.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkyModifier {
    /// Multiplies the keyframe's intensity. 1.0 is none.
    pub intensity: f32,
    /// What the horizon and fog colour move towards.
    pub sky: [f32; 3],
    /// How far towards `sky`, `0..=1`. 0.0 is none.
    pub sky_mix: f32,
    /// Multiplies the distance fog's reach; under 1.0 is closer. 1.0 is none.
    pub fog_distance: f32,
    /// Multiplies the keyframe grade's saturation (mode 3). 1.0 is none.
    pub saturation: f32,
    /// How long the client takes to get there, in ticks; 0 is at once.
    pub ease_ticks: u32,
}

impl SkyModifier {
    /// The plain sky: every field at its identity.
    pub const NONE: Self = Self {
        intensity: 1.0,
        sky: [0.0; 3],
        sky_mix: 0.0,
        fog_distance: 1.0,
        saturation: 1.0,
        ease_ticks: 0,
    };

    /// Whether every number is finite and in range — what a client checks
    /// before trusting one a server sent (charter rule 14).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        (0.0..=MAX_INTENSITY).contains(&self.intensity)
            && self.sky.iter().all(|c| (0.0..=MAX_CHANNEL).contains(c))
            && (0.0..=1.0).contains(&self.sky_mix)
            && (MIN_FOG_DISTANCE..=MAX_FOG_DISTANCE).contains(&self.fog_distance)
            && (0.0..=MAX_SATURATION).contains(&self.saturation)
            && self.ease_ticks <= MAX_EASE_TICKS
    }
}

/// The most a modifier may brighten the sun.
pub const MAX_INTENSITY: f32 = 2.0;
/// The brightest a target sky channel may be.
pub const MAX_CHANNEL: f32 = 2.0;
/// The nearest fog may be pulled in: a twentieth of the view.
pub const MIN_FOG_DISTANCE: f32 = 0.05;
/// The furthest fog may be pushed out.
pub const MAX_FOG_DISTANCE: f32 = 4.0;
/// The most saturation may be multiplied, matching the grade's own ceiling.
pub const MAX_SATURATION: f32 = 4.0;

/// Clamps a modifier's numbers into range, with the identity for anything
/// that is not a number. Wrong numbers are clamped; wrong types are the
/// binding's errors.
#[must_use]
pub fn sanitise(mut modifier: SkyModifier) -> SkyModifier {
    let clamp = |value: f32, low: f32, high: f32, fallback: f32| {
        if value.is_finite() {
            value.clamp(low, high)
        } else {
            fallback
        }
    };
    modifier.intensity = clamp(modifier.intensity, 0.0, MAX_INTENSITY, 1.0);
    for channel in &mut modifier.sky {
        *channel = clamp(*channel, 0.0, MAX_CHANNEL, 0.0);
    }
    modifier.sky_mix = clamp(modifier.sky_mix, 0.0, 1.0, 0.0);
    modifier.fog_distance = clamp(
        modifier.fog_distance,
        MIN_FOG_DISTANCE,
        MAX_FOG_DISTANCE,
        1.0,
    );
    modifier.saturation = clamp(modifier.saturation, 0.0, MAX_SATURATION, 1.0);
    modifier.ease_ticks = modifier.ease_ticks.min(MAX_EASE_TICKS);
    modifier
}

/// A flash: lightning, seen.
///
/// **A short additive term on the sun and sky, and nothing else.** A lamp
/// block placed and removed is two relights a strike; a white burst of
/// particles at night is lit by the night and comes out grey; the modifier
/// eases, and a flash must not. So the client adds this to the sun's
/// intensity for a moment, with no relight — which, with thunder delayed by
/// distance, is the whole visible effect of lightning at a distance. Weather
/// ask W3.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Flash {
    /// How much is added to the sun's intensity at the peak. 1.0 is full
    /// daylight from nothing.
    pub intensity: f32,
    /// What the light is; the sun and sky lean towards it for the moment.
    pub colour: [f32; 3],
    /// Ticks from nothing to the peak; 0 is at once.
    pub attack_ticks: u32,
    /// Ticks from the peak back to nothing.
    pub decay_ticks: u32,
}

/// The most a flash may add.
pub const MAX_FLASH: f32 = 4.0;
/// The longest a flash may take to peak: five seconds.
pub const MAX_ATTACK_TICKS: u32 = 100;
/// The longest a flash may take to die: twenty seconds.
pub const MAX_DECAY_TICKS: u32 = 400;
/// How far a flash may be seen from: a thunderstorm's whole sky.
pub const MAX_FLASH_RADIUS: f32 = 1024.0;

impl Flash {
    /// Whether every number is finite and in range (charter rule 14).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        (0.0..=MAX_FLASH).contains(&self.intensity)
            && self.colour.iter().all(|c| (0.0..=MAX_CHANNEL).contains(c))
            && self.attack_ticks <= MAX_ATTACK_TICKS
            && self.decay_ticks <= MAX_DECAY_TICKS
    }
}

/// A flash, and who sees it: everyone within `radius` of `pos` in `domain`.
#[derive(Debug, Clone, PartialEq)]
pub struct FlashRequest {
    /// The flash itself.
    pub flash: Flash,
    /// Where it struck, in world blocks.
    pub pos: [f64; 3],
    /// Which domain.
    pub domain: String,
    /// How far it is seen from.
    pub radius: f32,
}

/// Clamps a flash request's numbers into range.
#[must_use]
pub fn sanitise_flash(mut request: FlashRequest) -> FlashRequest {
    let clamp = |value: f32, low: f32, high: f32, fallback: f32| {
        if value.is_finite() {
            value.clamp(low, high)
        } else {
            fallback
        }
    };
    request.flash.intensity = clamp(request.flash.intensity, 0.0, MAX_FLASH, 1.0);
    for channel in &mut request.flash.colour {
        *channel = clamp(*channel, 0.0, MAX_CHANNEL, 1.0);
    }
    request.flash.attack_ticks = request.flash.attack_ticks.min(MAX_ATTACK_TICKS);
    request.flash.decay_ticks = request.flash.decay_ticks.min(MAX_DECAY_TICKS);
    request.radius = clamp(request.radius, 0.0, MAX_FLASH_RADIUS, 256.0);
    for value in &mut request.pos {
        if !value.is_finite() {
            *value = 0.0;
        }
    }
    request
}

/// Precipitation: an emitter the client runs around its own camera.
///
/// **One message when the weather changes, not a stream of bursts.** Rain is
/// continuous and `emit_particles` is a burst; keeping rain alive around a
/// player from the server meant a burst of hundreds every few ticks per
/// player, for as long as the storm lasted — thousands of messages a minute
/// for something whose parameters change every forty seconds, and the first
/// thing to stutter under load. So the server sends the SHAPE of the rain
/// and the client spawns it: `rate` particles a second, in a box `area`
/// around the camera lifted `above` blocks, each shaped by `burst` (whose
/// `pos` and `count` are the client's to fill). The client eases the rate
/// over `ease_ticks`, and honours its own budget. Weather ask W4(b).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Precipitation {
    /// What each particle is: colour, size, lifetime, velocity, spread,
    /// gravity, collision, and the box (`area`, half extents) it spawns in.
    pub burst: crate::particle::Burst,
    /// Particles a second.
    pub rate: f32,
    /// How far above the camera the box is centred, in blocks.
    pub above: f32,
    /// How long the client takes to bring the rate to this, in ticks.
    pub ease_ticks: u32,
}

/// The most particles a second a mod may ask for.
pub const MAX_RATE: f32 = 4000.0;
/// The highest above the camera the box may be centred.
pub const MAX_ABOVE: f32 = 64.0;

impl Precipitation {
    /// Whether every number is finite and in range (charter rule 14).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.burst.is_valid()
            && (0.0..=MAX_RATE).contains(&self.rate)
            && (0.0..=MAX_ABOVE).contains(&self.above)
            && self.ease_ticks <= MAX_EASE_TICKS
    }
}

/// Clamps a precipitation's numbers into range, the burst through the
/// particle module's own sanitiser.
#[must_use]
pub fn sanitise_precipitation(mut precipitation: Precipitation) -> Precipitation {
    let clamp = |value: f32, low: f32, high: f32, fallback: f32| {
        if value.is_finite() {
            value.clamp(low, high)
        } else {
            fallback
        }
    };
    precipitation.burst = crate::particle::sanitise(crate::particle::EmitRequest {
        burst: precipitation.burst,
        domain: String::new(),
        radius: 0.0,
        player: None,
    })
    .burst;
    precipitation.rate = clamp(precipitation.rate, 0.0, MAX_RATE, 0.0);
    precipitation.above = clamp(precipitation.above, 0.0, MAX_ABOVE, 16.0);
    precipitation.ease_ticks = precipitation.ease_ticks.min(MAX_EASE_TICKS);
    precipitation
}

/// A cloud deck, as a mod declares it once at load.
///
/// # Shape, not geometry
///
/// The client never builds cloud cubes. This describes a FIELD, and the
/// renderer marches a ray through it — which is what makes cube faces exact
/// (a grid march hits flat walls and flat tops by construction), makes drift
/// and evolution free (offset the sample, rather than rebuild a mesh), and
/// makes reaching the horizon a matter of step count rather than of memory.
/// A deck 8 km across at `cell = 8` would be millions of cubes as geometry,
/// rebuilt forever because it evolves; as a field it is this struct.
///
/// # A column is up to TWO intervals, which is what buys the anvil
///
/// At each column of the grid the field gives a bottom and a top, and may
/// give a second pair above them. Two heights rather than one flat base
/// because real cumulus has stepped, blocky undersides and lobes hanging
/// below their neighbours; a second interval because the reference images'
/// hero cloud is a tower that **mushrooms out over its own waist**, and one
/// interval per column — solid from bottom to top and nothing else — cannot
/// represent solid, air, solid.
///
/// The upper lobe is driven by [`CloudLayer::towers`] rather than by a field
/// of its own, so a mod that asks for flat banks pays for neither. **Two is a
/// deliberate stopping point**, not a step toward N: it covers the anvil and
/// the mushroom, which is what the references show, at roughly twice a
/// column's cost rather than the open-ended cost of a full three-dimensional
/// march. A cloud needing three would need the 3-D field.
///
/// # The camera may be anywhere
///
/// Below the deck, inside it, or above it looking down on the tops — a player
/// can fly up through this. The march therefore starts at the camera rather
/// than at the deck's floor, and a camera inside a filled cell sees fog
/// rather than a cube's inside face.
///
/// # Presentation only
///
/// Outside every determinism hash (charter rule 4 exempts rendering), so the
/// client may use fast non-deterministic noise. It is seeded from the world
/// seed all the same, which costs nothing and buys two things: a screenshot
/// of a given sky is reproducible, and two players describing the same cloud
/// agree about it.
///
/// Weather ask W2.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudLayer {
    /// World `y` of the deck's floor, before any per-column bottom lifts it.
    pub base: f32,
    /// Blocks from `base` to the tallest tower's top.
    pub thickness: f32,
    /// Blocks per large cube: the grid the march steps through.
    pub cell: f32,
    /// Small cubes per large-cube edge on the surface. 1 is none.
    pub detail: u8,
    /// The cloud field's horizontal scale, in cycles per block.
    pub frequency: f32,
    /// Octaves of the cloud field.
    pub octaves: u8,
    /// How much taller the highest heaps grow. 0 is flat banks.
    pub towers: f32,
    /// Blocks a second the deck drifts, in x and z.
    pub drift: [f32; 2],
    /// How fast the field changes shape, per second.
    pub evolve: f32,
    /// Lit cloud, before the sun's own colour.
    pub colour: [f32; 3],
    /// The unlit side, before the sky's own colour.
    pub shade: [f32; 3],
}

/// The largest cube a mod may ask for, in blocks.
pub const MAX_CELL: f32 = 64.0;
/// The smallest, below which a march to the horizon has no hope of stepping.
pub const MIN_CELL: f32 = 1.0;
/// The most small cubes per large-cube edge.
pub const MAX_DETAIL: u8 = 4;
/// The most octaves of cloud field.
pub const MAX_OCTAVES: u8 = 6;
/// The tallest deck, in blocks.
pub const MAX_THICKNESS: f32 = 1024.0;
/// The fastest a deck may drift, in blocks a second.
pub const MAX_DRIFT: f32 = 64.0;

impl CloudLayer {
    /// Whether every number is finite and in range (charter rule 14).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.base.is_finite()
            && (0.0..=MAX_THICKNESS).contains(&self.thickness)
            && (MIN_CELL..=MAX_CELL).contains(&self.cell)
            && (1..=MAX_DETAIL).contains(&self.detail)
            && self.frequency.is_finite()
            && self.frequency > 0.0
            && (1..=MAX_OCTAVES).contains(&self.octaves)
            && (0.0..=1.0).contains(&self.towers)
            && self.drift.iter().all(|d| d.abs() <= MAX_DRIFT)
            && self.evolve.is_finite()
            && self.evolve.abs() <= 1.0
            && self.colour.iter().all(|c| (0.0..=MAX_CHANNEL).contains(c))
            && self.shade.iter().all(|c| (0.0..=MAX_CHANNEL).contains(c))
    }
}

/// Clamps a cloud layer's numbers into range.
///
/// **Clamped rather than refused**, like every other registration: a mod that
/// got one number wrong should lose that number, not its whole sky. The
/// protocol refuses anything out of range on the way IN, because a server's
/// word for it is not a mod's.
#[must_use]
pub fn sanitise_clouds(mut layer: CloudLayer) -> CloudLayer {
    let clamp = |value: f32, low: f32, high: f32, fallback: f32| {
        if value.is_finite() {
            value.clamp(low, high)
        } else {
            fallback
        }
    };
    layer.base = clamp(layer.base, -30_000.0, 30_000.0, 256.0);
    layer.thickness = clamp(layer.thickness, 0.0, MAX_THICKNESS, 64.0);
    layer.cell = clamp(layer.cell, MIN_CELL, MAX_CELL, 8.0);
    layer.detail = layer.detail.clamp(1, MAX_DETAIL);
    // A frequency of zero is one cloud over the whole world, which is not a
    // sky; the fallback is the scale the ask's own example uses.
    layer.frequency = if layer.frequency.is_finite() && layer.frequency > 0.0 {
        layer.frequency.min(1.0)
    } else {
        1.0 / 600.0
    };
    layer.octaves = layer.octaves.clamp(1, MAX_OCTAVES);
    layer.towers = clamp(layer.towers, 0.0, 1.0, 0.0);
    for drift in &mut layer.drift {
        *drift = clamp(*drift, -MAX_DRIFT, MAX_DRIFT, 0.0);
    }
    layer.evolve = clamp(layer.evolve, -1.0, 1.0, 0.0);
    for channel in &mut layer.colour {
        *channel = clamp(*channel, 0.0, MAX_CHANNEL, 1.0);
    }
    for channel in &mut layer.shade {
        *channel = clamp(*channel, 0.0, MAX_CHANNEL, 0.5);
    }
    layer
}

/// How much cloud one player is under, and how dark it is.
///
/// Latest state, like [`SkyModifier`]: one message when the weather changes,
/// eased client-side. Per player rather than per domain for the reason the
/// sky modifier is — two players in one domain can stand under different
/// weather. Weather ask W2.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Clouds {
    /// 0 is clear, 1 is overcast.
    pub cover: f32,
    /// 0 is fair-weather white, 1 is storm grey.
    ///
    /// High darkness also hangs a dark haze UNDER the deck, which is what
    /// makes a storm read from outside it. Rain seen at a distance is a
    /// curtain kilometres away, and `set_precipitation` spawns around the
    /// player's own camera by construction — so the storm a player sees over
    /// the next valley is this, not particles.
    pub darkness: f32,
    /// Overrides the registered base for this player, in world `y`.
    pub base: Option<f32>,
    /// How long the client takes to get there, in ticks.
    pub ease_ticks: u32,
}

impl Clouds {
    /// Whether every number is finite and in range (charter rule 14).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        (0.0..=1.0).contains(&self.cover)
            && (0.0..=1.0).contains(&self.darkness)
            && self.base.is_none_or(f32::is_finite)
            && self.ease_ticks <= MAX_EASE_TICKS
    }
}

/// Clamps a cloud state's numbers into range.
#[must_use]
pub fn sanitise_cloud_state(mut clouds: Clouds) -> Clouds {
    let clamp = |value: f32, low: f32, high: f32, fallback: f32| {
        if value.is_finite() {
            value.clamp(low, high)
        } else {
            fallback
        }
    };
    clouds.cover = clamp(clouds.cover, 0.0, 1.0, 0.0);
    clouds.darkness = clamp(clouds.darkness, 0.0, 1.0, 0.0);
    clouds.base = clouds.base.filter(|base| base.is_finite());
    clouds.ease_ticks = clouds.ease_ticks.min(MAX_EASE_TICKS);
    clouds
}

/// The widest a cover map may be, in cells a side.
///
/// Sixteen at the 256-block cells the weather mod evaluates is four
/// kilometres a side, which is past any view distance the engine serves — so a
/// wider one would be describing weather nobody can see. It also fixes the
/// room the client's cloud uniform needs at 2 KiB, which is why it is a
/// constant rather than a preference.
pub const MAX_MAP_SIZE: u8 = 16;

/// Cover and darkness over a patch of the world, rather than over one player.
///
/// **A storm over the next valley** — weather ask W10. [`Clouds`] is one
/// number for the whole sky a player sees, so a front could not be watched
/// coming: the deck was overcast everywhere or nowhere. This is a coarse grid
/// laid over the world, sampled where each ray of the cloud march passes, with
/// the [`Clouds`] values still answering everywhere the grid does not reach.
///
/// **Bytes, not floats.** Cover and darkness are shares of one, a 255th of
/// which is far finer than a sky can show, and a 16x16 grid of pairs is 512
/// bytes on the wire against 2 KiB of `f32`. It also means no float can arrive
/// as a NaN and reach a shader.
///
/// Its own message and its own type rather than a field on [`Clouds`], because
/// `Clouds` is `Copy` and travels through eight places by value; a grid inside
/// it would make every one of those copies kilobytes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudMap {
    /// The world x and z of the grid's corner, in blocks.
    pub origin: [f32; 2],
    /// How many blocks a cell covers.
    pub cell: f32,
    /// How many cells a side. At most [`MAX_MAP_SIZE`].
    pub size: u8,
    /// Cover per cell, row-major by z, `size * size` of them. 0 is clear.
    pub cover: Vec<u8>,
    /// Darkness per cell, the same order. 0 is fair-weather white.
    pub darkness: Vec<u8>,
}

impl CloudMap {
    /// Whether the grid is square, in range, and the size it says it is.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let cells = usize::from(self.size) * usize::from(self.size);
        self.size > 0
            && self.size <= MAX_MAP_SIZE
            && self.origin.iter().all(|value| value.is_finite())
            && self.cell.is_finite()
            && self.cell > 0.0
            && self.cover.len() == cells
            && self.darkness.len() == cells
    }
}

/// Brings a grid a mod got wrong into one that can be drawn, or drops it.
///
/// **Dropped rather than clamped when the SHAPE is wrong**, unlike every
/// number beside it: a grid whose cell count does not match its size is not a
/// grid with a bad number in it, it is a mod that has miscounted, and guessing
/// which cells it meant would draw weather nobody asked for.
#[must_use]
pub fn sanitise_cloud_map(mut map: CloudMap) -> Option<CloudMap> {
    map.size = map.size.min(MAX_MAP_SIZE);
    if !map.cell.is_finite() || map.cell <= 0.0 {
        return None;
    }
    if !map.origin.iter().all(|value| value.is_finite()) {
        return None;
    }
    let cells = usize::from(map.size) * usize::from(map.size);
    if map.size == 0 || map.cover.len() != cells || map.darkness.len() != cells {
        return None;
    }
    Some(map)
}

/// Where `game.set_sky_modifier` and `game.flash` reach.
///
/// The same seam shape as [`crate::hud::Access`], and for the same reason:
/// who is connected, and where, lives above core.
pub trait Access: Send + Sync {
    /// Replaces one player's modifier, or clears it with `None`.
    ///
    /// Returns whether the player was there to tell.
    fn set_sky_modifier(&self, player: PlayerUuid, modifier: Option<SkyModifier>) -> bool;

    /// Shows a flash to everyone in reach, returning how many were told.
    fn flash(&self, request: &FlashRequest) -> u32;

    /// Replaces one player's precipitation, or clears it with `None`.
    ///
    /// Returns whether the player was there to tell.
    fn set_precipitation(&self, player: PlayerUuid, precipitation: Option<Precipitation>) -> bool;

    /// Replaces one player's cloud state, or clears it with `None`.
    ///
    /// Returns whether the player was there to tell.
    fn set_clouds(&self, player: PlayerUuid, clouds: Option<Clouds>) -> bool;

    /// Replaces the cover map one player's sky is drawn from — ask W10.
    ///
    /// Its own call rather than a field on [`Clouds`], for the reason
    /// [`CloudMap`] gives: a grid inside a `Copy` type would be copied by
    /// value everywhere that type goes. Set together with the state by
    /// `game.set_clouds`, so a call that names no map clears one.
    ///
    /// Defaulted, because a VM with no server behind it has no players.
    fn set_cloud_map(&self, player: PlayerUuid, map: Option<CloudMap>) -> bool {
        let _ = (player, map);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer near enough the ask's own example to be recognisable.
    fn fair_weather() -> CloudLayer {
        CloudLayer {
            base: 420.0,
            thickness: 96.0,
            cell: 8.0,
            detail: 2,
            frequency: 1.0 / 600.0,
            octaves: 3,
            towers: 0.25,
            drift: [1.5, 0.4],
            evolve: 1.0 / 2400.0,
            colour: [1.0, 1.0, 1.0],
            shade: [0.42, 0.44, 0.58],
        }
    }

    #[test]
    fn a_cloud_layer_a_mod_got_wrong_is_clamped_into_one_that_can_be_drawn() {
        // **Clamped rather than refused**, like every other registration: a
        // mod that got one number wrong should lose that number and not its
        // whole sky. Every field is wild here, and every one comes back inside
        // the range `is_valid` states — which is the property that makes the
        // two functions one rule rather than two.
        let wild = CloudLayer {
            base: f32::NAN,
            thickness: -40.0,
            // A cell of zero would make a march to the horizon step for ever.
            cell: 0.0,
            detail: 200,
            frequency: 0.0,
            octaves: 0,
            towers: 9.0,
            drift: [f32::INFINITY, -500.0],
            evolve: f32::NAN,
            colour: [9.0, -1.0, f32::NAN],
            shade: [f32::NEG_INFINITY, 2.0, 0.5],
        };
        assert!(!wild.is_valid(), "the fixture must be worth sanitising");
        let tame = sanitise_clouds(wild);
        assert!(tame.is_valid(), "sanitising left {tame:?} out of range");
        assert!(tame.cell >= MIN_CELL, "a zero cell would never step");
        assert!(
            tame.frequency > 0.0,
            "a zero frequency is one cloud for ever"
        );
        assert!(tame.octaves >= 1);

        // Non-vacuous: a layer that was already fine is returned unchanged,
        // so the clamp cannot be passing by flattening everything.
        let fine = fair_weather();
        assert!(fine.is_valid());
        assert_eq!(sanitise_clouds(fine), fine);
    }

    #[test]
    fn a_cover_map_whose_shape_is_wrong_is_dropped_rather_than_guessed_at() {
        // Weather ask W10. Every number a mod hands the engine is clamped; a
        // grid is the exception, because a cell count that disagrees with the
        // size is not a bad number, it is a miscount — and guessing which
        // cells were meant would draw weather nobody asked for.
        let grid = |size: u8, cells: usize| CloudMap {
            origin: [0.0, 0.0],
            cell: 256.0,
            size,
            cover: vec![0; cells],
            darkness: vec![0; cells],
        };
        assert!(sanitise_cloud_map(grid(4, 16)).is_some(), "a square grid");
        assert!(sanitise_cloud_map(grid(4, 15)).is_none(), "one cell short");
        assert!(sanitise_cloud_map(grid(0, 0)).is_none(), "no grid at all");

        // A cell size that is not a length, and an origin that is not a place.
        let mut nonsense = grid(2, 4);
        nonsense.cell = 0.0;
        assert!(sanitise_cloud_map(nonsense).is_none());
        let mut adrift = grid(2, 4);
        adrift.origin = [f32::NAN, 0.0];
        assert!(sanitise_cloud_map(adrift).is_none());

        // Wider than the engine will draw: cut to the cap, which then makes
        // the cell count wrong, so it is refused rather than silently cropped.
        let wide = grid(MAX_MAP_SIZE + 1, usize::from(MAX_MAP_SIZE + 1).pow(2));
        assert!(sanitise_cloud_map(wide).is_none());

        // And the shape the wire checks is the same one.
        assert!(grid(4, 16).is_valid());
        assert!(!grid(4, 15).is_valid());
    }

    #[test]
    fn a_cloud_state_a_mod_got_wrong_is_clamped_too() {
        let wild = Clouds {
            cover: 40.0,
            darkness: f32::NAN,
            base: Some(f32::INFINITY),
            ease_ticks: u32::MAX,
        };
        assert!(!wild.is_valid());
        let tame = sanitise_cloud_state(wild);
        assert!(tame.is_valid(), "sanitising left {tame:?} out of range");
        assert_eq!(
            tame.base, None,
            "a base that is not a number is no override, not an override of nothing"
        );

        let fine = Clouds {
            cover: 0.55,
            darkness: 0.0,
            base: Some(380.0),
            ease_ticks: 600,
        };
        assert!(fine.is_valid());
        assert_eq!(sanitise_cloud_state(fine), fine);
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the values asserted are set, not computed"
    )]
    fn sanitise_and_is_valid_are_two_statements_of_one_set_of_ranges() {
        let wild = SkyModifier {
            intensity: f32::NAN,
            sky: [9.0, -1.0, f32::INFINITY],
            sky_mix: 3.0,
            fog_distance: 0.0,
            saturation: -2.0,
            ease_ticks: u32::MAX,
        };
        assert!(!wild.is_valid());
        let tame = sanitise(wild);
        assert!(tame.is_valid(), "{tame:?}");
        assert_eq!(
            tame.intensity, 1.0,
            "not a number falls back to the identity"
        );
        assert_eq!(tame.sky, [MAX_CHANNEL, 0.0, 0.0]);
        assert_eq!(tame.fog_distance, MIN_FOG_DISTANCE);
        assert!(SkyModifier::NONE.is_valid());
        assert_eq!(sanitise(SkyModifier::NONE), SkyModifier::NONE);
    }

    #[test]
    fn precipitation_is_clamped_into_the_same_ranges_it_is_checked_against() {
        let wild = Precipitation {
            burst: crate::particle::Burst {
                pos: [0.0; 3],
                count: 0,
                colour: [255; 4],
                size: 99.0,
                lifetime: f32::NAN,
                velocity: [0.0, -900.0, 0.0],
                spread: 0.0,
                area: [16.0, 3.0, 16.0],
                gravity: 0.0,
                collide: true,
            },
            rate: f32::INFINITY,
            above: -4.0,
            ease_ticks: u32::MAX,
        };
        assert!(!wild.is_valid());
        let tame = sanitise_precipitation(wild);
        assert!(tame.is_valid(), "{tame:?}");
        assert!(
            tame.rate.abs() < f32::EPSILON,
            "a rate that is not a number is none"
        );
        assert!(tame.above.abs() < f32::EPSILON);
        assert!(tame.burst.velocity[1] >= -crate::particle::MAX_SPEED);
    }

    #[test]
    fn a_flash_is_clamped_into_the_same_ranges_it_is_checked_against() {
        let wild = FlashRequest {
            flash: Flash {
                intensity: 40.0,
                colour: [f32::NAN, 3.0, 0.5],
                attack_ticks: 1000,
                decay_ticks: 1000,
            },
            pos: [f64::NAN, 1.0, 2.0],
            domain: "d".to_owned(),
            radius: f32::INFINITY,
        };
        assert!(!wild.flash.is_valid());
        let tame = sanitise_flash(wild);
        assert!(tame.flash.is_valid(), "{tame:?}");
        assert!((tame.flash.intensity - MAX_FLASH).abs() < f32::EPSILON);
        assert!(
            (tame.flash.colour[0] - 1.0).abs() < f32::EPSILON,
            "a channel that is not a number is white"
        );
        assert_eq!(tame.flash.attack_ticks, MAX_ATTACK_TICKS);
        assert!(
            (tame.radius - 256.0).abs() < f32::EPSILON,
            "a radius that is not a number is the default"
        );
        assert!((tame.pos[0]).abs() < f64::EPSILON);
    }
}
