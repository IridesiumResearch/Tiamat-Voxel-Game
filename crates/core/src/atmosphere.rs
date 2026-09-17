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

/// Where `game.set_sky_modifier` reaches.
///
/// The same seam shape as [`crate::hud::Access`], and for the same reason:
/// who is connected lives above core.
pub trait Access: Send + Sync {
    /// Replaces one player's modifier, or clears it with `None`.
    ///
    /// Returns whether the player was there to tell.
    fn set_sky_modifier(&self, player: PlayerUuid, modifier: Option<SkyModifier>) -> bool;
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
}
