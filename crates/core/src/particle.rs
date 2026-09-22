// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Particles: short-lived sprites a mod scatters at a place.
//!
//! # A mechanism for presentation, and nothing more
//!
//! Sea spray from a blowhole, drips off a canopy, mist over a forest floor. The
//! server does not simulate a single particle: a mod describes a BURST — where,
//! how many, what colour, how they move and for how long — the server decides
//! who is close enough to see it, and each client animates its own copy.
//! Nothing reads a particle back, so two clients disagreeing about where one
//! drop is disagree about nothing that matters, and charter rule 4 does not
//! reach them.
//!
//! # Bounded twice
//!
//! [`sanitise`] clamps what a careless mod asks for into ranges a client can
//! draw, the way `sound::sanitise` does. The protocol's validation then REFUSES
//! anything outside the same ranges on the client, because a client does not
//! trust the server it is talking to (charter rule 14): a burst of four billion
//! particles is a hostile message, not a large one.

use serde::{Deserialize, Serialize};

/// The most particles one burst may make.
pub const MAX_PER_BURST: u16 = 256;

/// The most bursts one message may carry.
pub const MAX_BURSTS_PER_MESSAGE: usize = 64;

/// The largest radius a burst is sent over, in blocks.
///
/// Well past anything a particle is visible at; a mod that wants "everyone
/// nearby" gets it, and one that asks for the world gets this.
pub const MAX_RADIUS: f32 = 128.0;

/// The longest a particle may live, in seconds.
pub const MAX_LIFETIME: f32 = 30.0;

/// The largest a particle may be, in blocks across.
pub const MAX_SIZE: f32 = 4.0;

/// The fastest a particle may start, and the strongest gravity it may feel, in
/// blocks per second (per second).
pub const MAX_SPEED: f32 = 64.0;

/// How far from its centre a burst may scatter its particles, in blocks.
pub const MAX_AREA: f32 = 16.0;

/// One burst of particles, as it travels to a client.
///
/// Positions are world blocks in `f64`, like a sound's: the client turns them
/// camera-relative, and a burst at the edge of a 120,000-block world must land
/// where the mod put it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Burst {
    /// The centre of the burst, in world blocks.
    pub pos: [f64; 3],
    /// How many particles.
    pub count: u16,
    /// Their colour and opacity, 0..=255 a channel.
    pub colour: [u8; 4],
    /// How big each is, in blocks across.
    pub size: f32,
    /// How long each lives, in seconds. They fade out over it.
    pub lifetime: f32,
    /// The velocity they all start with, in blocks per second.
    pub velocity: [f32; 3],
    /// How much random velocity each adds to it, in blocks per second, in a
    /// random direction.
    pub spread: f32,
    /// Half the size of the box they start in, in blocks per axis.
    pub area: [f32; 3],
    /// How fast they fall, in blocks per second per second. Negative rises.
    pub gravity: f32,
    /// Whether one dies when it reaches a solid cell. A drip stops at the
    /// floor; mist drifts through a hedge.
    pub collide: bool,
    /// A registered picture drawn on each, or `None` for a plain disc.
    ///
    /// **Life ask 15.** Without it a particle is a flat square of one colour,
    /// so the mod drew a row of hearts over a struck cow pixel by pixel — 13
    /// particles a heart, 65 a blow, for what is one picture.
    ///
    /// A content hash rather than a name, because that is what
    /// `register_picture` hands a mod back and what the client already fetches
    /// and caches by. A hash the client has never heard of draws the plain
    /// disc, which is also what a picture that has not arrived yet does.
    #[serde(default)]
    pub texture: Option<crate::proto::ContentHash>,
}

impl Burst {
    /// Whether every number in it is one a client should accept.
    ///
    /// The protocol's check on hostile input: the same ranges [`sanitise`]
    /// clamps into, so anything a well-behaved server sends passes.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let within =
            |value: f32, low: f32, high: f32| value.is_finite() && (low..=high).contains(&value);
        self.pos.iter().all(|value| value.is_finite())
            && self.count <= MAX_PER_BURST
            && within(self.size, 0.0, MAX_SIZE)
            && within(self.lifetime, 0.0, MAX_LIFETIME)
            && self
                .velocity
                .iter()
                .all(|value| within(*value, -MAX_SPEED, MAX_SPEED))
            && within(self.spread, 0.0, MAX_SPEED)
            && self.area.iter().all(|value| within(*value, 0.0, MAX_AREA))
            && within(self.gravity, -MAX_SPEED, MAX_SPEED)
    }
}

/// The most icons one badge may show.
///
/// A row of hearts over a cow, an "!" over a startled animal, a quest marker.
/// Sixteen is twice the longest health bar anybody draws and still a row that
/// fits over an entity rather than across the screen.
pub const MAX_BADGE_ICONS: u8 = 16;

/// A row of pictures hung over an entity — Life ask 15.
///
/// **Why this is not a burst.** A burst is scattered at a PLACE and each client
/// animates it; a badge is anchored to an ENTITY and follows it, which a burst
/// cannot do — the mod's own workaround re-sprayed the row every tick to keep
/// it over a moving cow. One badge replaces whatever the same entity had, so a
/// draining health bar is a badge a tick, not a queue.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Badge {
    /// The entity it hangs over. It follows that entity, and goes when it does.
    pub entity: u64,
    /// The picture each icon draws — a hash `register_picture` answered.
    ///
    /// Required, unlike a burst's: a badge with no picture would be a row of
    /// blank squares, which is not a thing anybody means to ask for.
    pub picture: crate::proto::ContentHash,
    /// How many icons, laid left to right and centred over the entity.
    pub count: u8,
    /// How long the row stays before it fades, in seconds.
    pub seconds: f32,
    /// How big each icon is, in blocks across.
    pub size: f32,
    /// What the picture is tinted by, as a burst's colour tints its particles.
    pub colour: [u8; 4],
}

impl Badge {
    /// Whether every number in it is one a client should accept.
    ///
    /// The hostile-input check, as [`Burst::is_valid`] is: a badge of four
    /// billion icons is a hostile message rather than a large one.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let within =
            |value: f32, low: f32, high: f32| value.is_finite() && (low..=high).contains(&value);
        self.count <= MAX_BADGE_ICONS
            && within(self.seconds, 0.0, MAX_LIFETIME)
            && within(self.size, 0.0, MAX_SIZE)
    }
}

/// A mod's request: a badge, and who should see it.
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeRequest {
    /// The badge itself.
    pub badge: Badge,
    /// How far away a player may be and still be sent it, in blocks.
    pub radius: f32,
    /// One player to see it, or everyone in reach.
    ///
    /// Narrows, never widens, exactly as [`EmitRequest::player`] does — and
    /// the reason the ask names it: the hitter sees the hearts, not the whole
    /// server.
    pub player: Option<crate::identity::PlayerUuid>,
}

/// Clamps a mod's badge into ranges every client accepts.
#[must_use]
pub fn sanitise_badge(mut request: BadgeRequest) -> BadgeRequest {
    let badge = &mut request.badge;
    badge.count = badge.count.min(MAX_BADGE_ICONS);
    badge.seconds = clamp_finite(badge.seconds, 0.0, MAX_LIFETIME, 2.0);
    badge.size = clamp_finite(badge.size, 0.0, MAX_SIZE, 0.4);
    request.radius = clamp_finite(request.radius, 0.0, MAX_RADIUS, 32.0);
    request
}

/// A mod's request: a burst, and who should see it.
#[derive(Debug, Clone, PartialEq)]
pub struct EmitRequest {
    /// The burst itself.
    pub burst: Burst,
    /// The space it happens in. Players elsewhere are not sent it.
    pub domain: String,
    /// How far away a player may be and still be sent it, in blocks.
    pub radius: f32,
    /// One player to send it to, or everyone in reach.
    ///
    /// **Narrows, never widens.** The domain and the radius still apply; this
    /// takes the players they admit down to one. It is what lets a mod honour
    /// its own "particles off" setting — a burst everybody in reach receives
    /// cannot be turned off for one of them — and what lets rain be emitted
    /// per player rather than per patch of ground, without two players beside
    /// each other each seeing the other's.
    pub player: Option<crate::identity::PlayerUuid>,
}

/// Clamps a mod's numbers into ranges every client accepts.
///
/// **A mod is not hostile, but it is careless**, as `sound::sanitise` says: a
/// `NaN` lifetime or a count of a million deserves a burst that still shows,
/// not an error in the middle of somebody's tick.
#[must_use]
pub fn sanitise(mut request: EmitRequest) -> EmitRequest {
    let burst = &mut request.burst;
    for value in &mut burst.pos {
        if !value.is_finite() {
            *value = 0.0;
        }
    }
    burst.count = burst.count.min(MAX_PER_BURST);
    burst.size = clamp_finite(burst.size, 0.0, MAX_SIZE, 0.1);
    burst.lifetime = clamp_finite(burst.lifetime, 0.0, MAX_LIFETIME, 1.0);
    for value in &mut burst.velocity {
        *value = clamp_finite(*value, -MAX_SPEED, MAX_SPEED, 0.0);
    }
    burst.spread = clamp_finite(burst.spread, 0.0, MAX_SPEED, 0.0);
    for value in &mut burst.area {
        *value = clamp_finite(*value, 0.0, MAX_AREA, 0.0);
    }
    burst.gravity = clamp_finite(burst.gravity, -MAX_SPEED, MAX_SPEED, 0.0);
    request.radius = clamp_finite(request.radius, 0.0, MAX_RADIUS, 32.0);
    request
}

/// Clamps, substituting a default for anything that is not a number.
fn clamp_finite(value: f32, low: f32, high: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        fallback
    }
}

/// Where `game.emit_particles` reaches.
///
/// The same seam as `sound::Access`, for its reason: who is close enough needs
/// every connected player, which lives above `core` (charter rule 3).
pub trait Access: Send + Sync {
    /// Sends a burst to every player in its domain and radius, returning how
    /// many were told. Not a promise anybody SAW it — a client behind a wall,
    /// or with its particle budget full, still counts.
    fn emit(&self, request: &EmitRequest) -> u32;

    /// Hangs a row of pictures over an entity, returning how many players were
    /// told.
    ///
    /// The domain and the position come from the ENTITY rather than from the
    /// mod: a badge over something that has stopped existing is told to
    /// nobody, which is the answer a mod wants and not an error.
    fn show_over(&self, request: &BadgeRequest) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst() -> Burst {
        Burst {
            pos: [1.0, 2.0, 3.0],
            count: 12,
            colour: [255; 4],
            size: 0.2,
            lifetime: 1.5,
            velocity: [0.0, 4.0, 0.0],
            spread: 1.0,
            area: [0.5; 3],
            gravity: 20.0,
            collide: true,
            texture: None,
        }
    }

    #[test]
    fn whatever_a_mod_asks_for_comes_out_as_a_burst_a_client_accepts() {
        // Sanitise and validate are two statements of one set of ranges, and
        // this is what keeps them one: the worst request a mod can make,
        // cleaned, must pass the client's check — or a careless mod would get
        // every player disconnected for a protocol error.
        let wild = EmitRequest {
            burst: Burst {
                pos: [f64::NAN, f64::INFINITY, 5.0],
                count: u16::MAX,
                colour: [1, 2, 3, 4],
                size: f32::NAN,
                lifetime: 1.0e9,
                velocity: [f32::NEG_INFINITY, 1.0e6, -1.0e6],
                spread: -3.0,
                area: [f32::NAN, 99.0, -1.0],
                gravity: f32::INFINITY,
                collide: false,
                texture: None,
            },
            domain: "overworld".to_owned(),
            radius: f32::NAN,
            player: None,
        };
        assert!(
            !wild.burst.is_valid(),
            "the counter-example: raw, it is refused"
        );
        let clean = sanitise(wild);
        assert!(
            clean.burst.is_valid(),
            "sanitised and still refused: {clean:?}"
        );
        assert_eq!(clean.burst.count, MAX_PER_BURST);
        assert!(
            clean
                .burst
                .pos
                .iter()
                .zip([0.0, 0.0, 5.0])
                .all(|(got, want)| (got - want).abs() < f64::EPSILON)
        );
        assert!((clean.radius - 32.0).abs() < f32::EPSILON);
        assert!((clean.burst.lifetime - MAX_LIFETIME).abs() < f32::EPSILON);
    }

    #[test]
    fn a_reasonable_burst_is_untouched() {
        let request = EmitRequest {
            burst: burst(),
            domain: "overworld".to_owned(),
            radius: 48.0,
            player: None,
        };
        assert_eq!(sanitise(request.clone()), request);
        assert!(request.burst.is_valid());
    }
}
