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
}

#[cfg(test)]
mod tests {
    use super::*;

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
