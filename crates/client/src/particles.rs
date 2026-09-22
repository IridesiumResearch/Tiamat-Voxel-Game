// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The particles a server's mods scatter, animated on this client.
//!
//! # Presentation, so this is the client's own business
//!
//! The server sends a [`Burst`] — where, how many, how they move — and forgets
//! it. Each particle is made here, moves here, and dies here; no two clients
//! agree about where one drop is, and nothing needs them to. Charter rule 4
//! exempts all of it, which is why the scatter comes from a plain generator of
//! the client's own rather than a world stream.
//!
//! # Bounded, because the server is not trusted
//!
//! The protocol has already refused a burst outside `particle::Burst::is_valid`.
//! What is left is the TOTAL: a server sending its sixty-four bursts of 256
//! every frame is within every per-message cap and would still bury a client.
//! [`MAX_LIVE`] is the budget, and a burst arriving over it makes as many as
//! fit and no more — the oldest are not evicted, because a spray that has
//! already started is what the player is looking at.

use tiamot_core::atmosphere::Precipitation;
use tiamot_core::particle::Burst;

/// The most particles alive at once.
///
/// Enough for a storm of spray and a forest's worth of drips together; one
/// instance each is 32 bytes, so the whole budget is a quarter of a megabyte
/// of vertex buffer.
pub const MAX_LIVE: usize = 8192;

/// One particle, in flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Particle {
    /// Where it is, in world blocks. `f64`, for the floating origin's reason:
    /// the camera subtracts, and a spray at the edge of the world stays there.
    pub pos: [f64; 3],
    /// Blocks per second.
    pub velocity: [f32; 3],
    /// Blocks per second per second, downward.
    pub gravity: f32,
    /// Seconds lived, and seconds it gets.
    pub age: f32,
    /// See `age`.
    pub lifetime: f32,
    /// Its colour, already lit, and its opacity at birth.
    pub colour: [f32; 4],
    /// Blocks across.
    pub size: f32,
    /// Whether it dies on reaching a solid cell.
    pub collide: bool,
    /// The registered picture drawn on it, or `None` for a plain disc.
    ///
    /// Carried from the burst — Life ask 15. A hash the client has never
    /// heard of, or one whose bytes have not arrived yet, draws the disc.
    pub texture: Option<tiamot_core::proto::ContentHash>,
}

impl Particle {
    /// Its opacity now: full for the first half of its life, fading to nothing
    /// over the second, so a particle never blinks out.
    #[must_use]
    pub fn opacity(&self) -> f32 {
        let left = 1.0 - self.age / self.lifetime.max(f32::EPSILON);
        self.colour[3] * (left * 2.0).clamp(0.0, 1.0)
    }
}

/// Every particle alive on this client.
#[derive(Debug)]
pub struct System {
    live: Vec<Particle>,
    /// A xorshift state. Presentation only — see the module docs.
    rng: u64,
}

impl Default for System {
    fn default() -> Self {
        Self::new(0x9E37_79B9_7F4A_7C15)
    }
}

impl System {
    /// An empty system whose scatter starts from `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            live: Vec::new(),
            rng: seed.max(1),
        }
    }

    /// The particles alive now.
    #[must_use]
    pub fn live(&self) -> &[Particle] {
        &self.live
    }

    /// Forgets every particle, for a world or a domain that is not this one.
    pub fn clear(&mut self) {
        self.live.clear();
    }

    /// Makes a burst's particles, up to the budget.
    ///
    /// `light` is what the burst's centre is lit by, `0.0..=1.0` a channel,
    /// sampled once for the whole burst: a particle is small and short-lived,
    /// and a light lookup per particle per frame would cost more than the
    /// particle does.
    pub fn spawn(&mut self, burst: &Burst, light: [f32; 3]) {
        let room = MAX_LIVE.saturating_sub(self.live.len());
        let count = usize::from(burst.count).min(room);
        let channel = |index: usize| f32::from(burst.colour[index]) / 255.0;
        let colour = [
            channel(0) * light[0],
            channel(1) * light[1],
            channel(2) * light[2],
            channel(3),
        ];
        for _ in 0..count {
            let offset = [
                f64::from(self.signed() * burst.area[0]),
                f64::from(self.signed() * burst.area[1]),
                f64::from(self.signed() * burst.area[2]),
            ];
            let reach = burst.spread * self.unit();
            let kick = self.direction().map(|axis| axis * reach);
            let lifetime = burst.lifetime * (0.75 + 0.5 * self.unit());
            self.live.push(Particle {
                pos: [
                    burst.pos[0] + offset[0],
                    burst.pos[1] + offset[1],
                    burst.pos[2] + offset[2],
                ],
                velocity: [
                    burst.velocity[0] + kick[0],
                    burst.velocity[1] + kick[1],
                    burst.velocity[2] + kick[2],
                ],
                gravity: burst.gravity,
                age: 0.0,
                // A little variety in how long each lasts, so a burst thins out
                // rather than vanishing on one frame.
                lifetime,
                colour,
                size: burst.size,
                collide: burst.collide,
                texture: burst.texture,
            });
        }
    }

    /// Moves every particle on by `dt` seconds and removes the finished.
    ///
    /// `solid` answers whether a world position is inside a solid cell; a
    /// colliding particle that ends a step inside one is gone. Asked only for
    /// those that collide, so mist costs nothing but its motion.
    pub fn advance(&mut self, dt: f32, solid: impl Fn([f64; 3]) -> bool) {
        let dt = dt.clamp(0.0, 0.25);
        self.live.retain_mut(|particle| {
            particle.age += dt;
            if particle.age >= particle.lifetime {
                return false;
            }
            particle.velocity[1] -= particle.gravity * dt;
            for (axis, speed) in particle.velocity.iter().enumerate() {
                particle.pos[axis] += f64::from(*speed * dt);
            }
            !(particle.collide && solid(particle.pos))
        });
    }

    /// A number in `0.0..1.0`.
    fn unit(&mut self) -> f32 {
        // xorshift64*: presentation, so any decent scatter will do.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let bits = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        bits as f32 / (1u64 << 24) as f32
    }

    /// A number in `-1.0..1.0`.
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    /// A direction inside the unit ball, by rejection: uniform, and no
    /// trigonometry to reach for.
    fn direction(&mut self) -> [f32; 3] {
        loop {
            let candidate = [self.signed(), self.signed(), self.signed()];
            let length = candidate.iter().map(|value| value * value).sum::<f32>();
            if length <= 1.0 {
                return candidate;
            }
        }
    }
}

/// The rain around this client, spawned here from the shape the server sent.
///
/// **The client runs the emitter** — see [`Precipitation`]'s docs for why the
/// server sends one message and not a stream of bursts. The rate eases over
/// the ticks the mod asked for, and `None` eases it to nothing over the last
/// ease before the shape is forgotten. A fractional particle is carried from
/// frame to frame against the frame's own `dt`, not the clamped one the
/// system integrates with, so a hitch neither doubles nor drops the rain.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Emitter {
    /// The shape, kept through the fade to nothing.
    shape: Option<Precipitation>,
    rate_from: f32,
    rate_to: f32,
    elapsed: f32,
    duration: f32,
    /// The fraction of a particle owed from the last frame.
    carry: f32,
}

/// The most of the budget precipitation may hold, so a mod's bursts always
/// have room: a storm never silences a splash.
pub const PRECIPITATION_SHARE: usize = MAX_LIVE * 3 / 4;

impl Emitter {
    /// Sets the rain, or `None` to let it die away.
    pub fn set(&mut self, precipitation: Option<Precipitation>) {
        let ticks = precipitation.map_or(
            self.shape.map_or(0, |shape| shape.ease_ticks),
            |precipitation| precipitation.ease_ticks,
        );
        self.rate_from = self.rate();
        self.rate_to = precipitation.map_or(0.0, |precipitation| precipitation.rate);
        if precipitation.is_some() {
            self.shape = precipitation;
        }
        self.elapsed = 0.0;
        self.duration = tiamot_core::tick::TICK_DURATION.as_secs_f32() * ticks as f32;
    }

    /// Particles a second, now.
    #[must_use]
    pub fn rate(&self) -> f32 {
        if self.duration <= 0.0 || self.elapsed >= self.duration {
            return self.rate_to;
        }
        self.rate_from + (self.rate_to - self.rate_from) * (self.elapsed / self.duration)
    }

    /// Advances by a frame and says how many particles are owed for it, and
    /// what they look like — or `None` when there is no rain.
    ///
    /// `centre` is where the camera is; the box is lifted `above` from it.
    pub fn advance(&mut self, dt: f32, centre: [f64; 3]) -> Option<Burst> {
        let shape = self.shape?;
        self.elapsed += dt.max(0.0);
        self.carry += self.rate() * dt.max(0.0);
        // The engine's own floor: `f32::floor` is on the banned list for
        // the crates the determinism rules cover, and one floor is the same
        // everywhere.
        let owed = tiamot_core::detgen::floor_to_i32(self.carry).max(0);
        self.carry -= owed as f32;
        if self.rate() <= 0.0 && self.elapsed >= self.duration {
            // Died away: forget the shape, so a later `None` eases nothing.
            self.shape = None;
            self.carry = 0.0;
        }
        if owed < 1 {
            return None;
        }
        let count = u16::try_from(owed).unwrap_or(u16::MAX);
        Some(Burst {
            pos: [centre[0], centre[1] + f64::from(shape.above), centre[2]],
            count,
            ..shape.burst
        })
    }

    /// Whether there is any rain, falling or fading.
    #[must_use]
    pub fn is_raining(&self) -> bool {
        self.shape.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(count: u16) -> Burst {
        Burst {
            pos: [100_000.5, 64.0, -7.25],
            count,
            colour: [255, 128, 0, 200],
            size: 0.25,
            lifetime: 2.0,
            velocity: [0.0, 5.0, 0.0],
            spread: 1.0,
            area: [0.5, 0.0, 0.5],
            gravity: 10.0,
            collide: false,
            texture: None,
        }
    }

    #[test]
    fn a_burst_starts_where_it_was_asked_and_scatters_within_its_box() {
        let mut system = System::new(7);
        system.spawn(&burst(64), [1.0, 0.5, 1.0]);
        assert_eq!(system.live().len(), 64);
        for particle in system.live() {
            assert!(
                (particle.pos[0] - 100_000.5).abs() <= 0.5,
                "{:?}",
                particle.pos
            );
            assert!(
                (particle.pos[1] - 64.0).abs() < 1e-9,
                "no area in y: {:?}",
                particle.pos
            );
            let speed = particle
                .velocity
                .map(|v| v)
                .iter()
                .zip([0.0, 5.0, 0.0])
                .map(|(a, b)| (a - b) * (a - b))
                .sum::<f32>();
            assert!(
                speed <= 1.0 + 1e-4,
                "the kick exceeded the spread: {:?}",
                particle.velocity
            );
            // Lit by what the burst stands in: green halved, alpha untouched.
            assert!((particle.colour[1] - 128.0 / 255.0 * 0.5).abs() < 1e-5);
            assert!((particle.colour[3] - 200.0 / 255.0).abs() < 1e-5);
        }
        // And they are not all the same particle.
        let first = system.live()[0].pos;
        assert!(
            system
                .live()
                .iter()
                .any(|particle| (particle.pos[0] - first[0]).abs() > 1e-6)
        );
    }

    #[test]
    fn a_particle_rises_slows_falls_and_fades_out() {
        let mut system = System::new(3);
        let mut one = burst(1);
        one.spread = 0.0;
        one.area = [0.0; 3];
        system.spawn(&one, [1.0; 3]);
        let start = system.live()[0];

        system.advance(0.25, |_| false);
        let rising = system.live()[0];
        assert!(rising.pos[1] > start.pos[1], "it did not rise");
        assert!(
            rising.velocity[1] < start.velocity[1],
            "gravity did nothing"
        );
        assert!(
            (rising.opacity() - start.colour[3]).abs() < 1e-5,
            "faded in its first half"
        );

        for _ in 0..3 {
            system.advance(0.25, |_| false);
        }
        let late = system.live()[0];
        assert!(late.velocity[1] < 0.0, "never started to fall");
        assert!(
            late.opacity() < rising.opacity(),
            "did not fade in its second half"
        );

        for _ in 0..40 {
            system.advance(0.25, |_| false);
        }
        assert!(system.live().is_empty(), "outlived its lifetime");
    }

    #[test]
    fn a_colliding_particle_stops_at_the_floor_and_mist_does_not() {
        // A drip dies on the ground; mist drifts through it. The "ground" is
        // everything under y = 63.
        let floor = |at: [f64; 3]| at[1] < 63.0;
        let mut drip = burst(16);
        drip.velocity = [0.0, -8.0, 0.0];
        drip.lifetime = 30.0;
        let mut mist = drip;
        drip.collide = true;
        mist.collide = false;

        let mut drips = System::new(1);
        drips.spawn(&drip, [1.0; 3]);
        let mut mists = System::new(1);
        mists.spawn(&mist, [1.0; 3]);
        for _ in 0..8 {
            drips.advance(0.1, floor);
            mists.advance(0.1, floor);
        }
        assert!(
            drips.live().is_empty(),
            "{} drips went through the floor",
            drips.live().len()
        );
        assert_eq!(mists.live().len(), 16, "mist was stopped by the floor");
    }

    #[test]
    fn the_budget_holds_however_much_a_server_sends() {
        let mut system = System::new(9);
        for _ in 0..100 {
            system.spawn(&burst(256), [1.0; 3]);
        }
        assert_eq!(system.live().len(), MAX_LIVE);
        system.clear();
        assert!(system.live().is_empty());
    }

    #[test]
    fn rain_is_owed_at_its_rate_eases_and_dies_away() {
        // **Weather ask W4(b).** Nine hundred a second at sixty frames a second
        // is fifteen a frame, and a fraction carried; eased over a second it
        // is half way at half a second; told to stop it fades over the same
        // time and is then gone.
        let shape = Precipitation {
            burst: Burst {
                pos: [0.0; 3],
                count: 0,
                colour: [255; 4],
                size: 0.06,
                lifetime: 1.0,
                velocity: [0.0, -22.0, 0.0],
                spread: 0.3,
                area: [16.0, 3.0, 16.0],
                gravity: 0.0,
                collide: true,
                texture: None,
            },
            rate: 900.0,
            above: 18.0,
            ease_ticks: 0,
        };
        let mut emitter = Emitter::default();
        assert!(
            emitter.advance(1.0 / 60.0, [0.0; 3]).is_none(),
            "no rain yet"
        );
        emitter.set(Some(shape));
        let mut spawned = 0u32;
        for _ in 0..60 {
            if let Some(burst) = emitter.advance(1.0 / 60.0, [0.0, 10.0, 0.0]) {
                spawned += u32::from(burst.count);
                assert!(
                    (burst.pos[1] - 28.0).abs() < 1e-9,
                    "lifted above the camera"
                );
                assert!((burst.velocity[1] + 22.0).abs() < f32::EPSILON);
            }
        }
        assert!(
            (899..=901).contains(&spawned),
            "{spawned} in a second at 900/s"
        );

        let mut emitter = Emitter::default();
        emitter.set(Some(Precipitation {
            ease_ticks: 20,
            ..shape
        }));
        assert!(emitter.rate().abs() < f32::EPSILON, "starts from nothing");
        let _ = emitter.advance(0.5, [0.0; 3]);
        assert!((emitter.rate() - 450.0).abs() < 1.0, "{}", emitter.rate());
        emitter.set(None);
        assert!(
            (emitter.rate() - 450.0).abs() < 1.0,
            "fades from where it was"
        );
        let _ = emitter.advance(0.5, [0.0; 3]);
        assert!((emitter.rate() - 225.0).abs() < 1.0);
        let _ = emitter.advance(1.0, [0.0; 3]);
        assert!(emitter.rate().abs() < f32::EPSILON);
        assert!(!emitter.is_raining(), "and then it is gone");
    }
}
