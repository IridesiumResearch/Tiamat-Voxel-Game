// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A lightning bolt, drawn — weather ask W26.
//!
//! `game.flash` lights the sky for a strike, and nothing was drawn between
//! the cloud and the ground: a storm over the next valley was the sky
//! blinking. Particles are the wrong tool three ways — lit by the world, so
//! a bolt at night comes out grey; placed in axis-aligned boxes, so a jagged
//! line is dozens of bursts; and dropped first under load, which is exactly
//! when a storm is on. So a bolt is its own message and its own pass.
//!
//! # What travels, and what does not
//!
//! Two ends, a seed and a handful of numbers — the PATH never travels. Every
//! client builds it from the seed by midpoint displacement ([`build_path`]),
//! so a bolt with eight forks is the same few dozen bytes as one with none,
//! and everyone watching sees the same bolt: the gate's "two clients given
//! the same seed draw the same path" is a property of this one function,
//! which is why it lives in core, beside the generator it draws from, where
//! the server, the client and the bot all reach it.
//!
//! # The Deterministic Float Subset, in presentation code
//!
//! Charter rule 4 exempts rendering, and this is rendering. But "the same
//! bolt for everyone watching" is a promise ACROSS machines — a player on
//! Windows and one on macOS pointing at the same fork — and the subset is
//! what makes `f32` arithmetic agree across them. Nothing here needs more: a
//! perpendicular is a cross product, a length is a square root, and the
//! randomness is [`Xoshiro256PlusPlus`]'s integers.
//!
//! # Relative to `from`
//!
//! The path is `f32` offsets from the bolt's own top, not world positions.
//! A world coordinate fifty thousand blocks out is exact only in `f64`
//! (charter rule 7), so the client subtracts the camera from `from` in `f64`
//! and adds these small offsets to the difference, as it places everything
//! else it draws.

use crate::detgen::rng::Xoshiro256PlusPlus;
use crate::identity::PlayerUuid;

/// A violet-white, like the designer's reference.
pub const DEFAULT_COLOUR: [f32; 3] = [0.85, 0.8, 1.0];
/// How wide the bright core is when a mod does not say, in blocks.
pub const DEFAULT_WIDTH: f32 = 0.4;
/// How many forks when a mod does not say.
pub const DEFAULT_BRANCHES: u8 = 3;
/// How long a bolt is seen when a mod does not say: under half a second,
/// which is as long as a real one flickers.
pub const DEFAULT_TICKS: u16 = 8;

/// The widest core a bolt may have, in blocks. Wider is a beam, not a bolt.
pub const MAX_LIGHTNING_WIDTH: f32 = 8.0;
/// The most forks a bolt may have.
///
/// **Capped because the client builds them.** A fork is sixteen segments of
/// arithmetic and sixteen instances drawn, and a server that could ask for
/// four billion of them could stall every client in reach with one message.
pub const MAX_LIGHTNING_BRANCHES: u8 = 8;
/// The longest a bolt may be seen: ten seconds.
pub const MAX_LIGHTNING_TICKS: u16 = 200;
/// The farthest `to` may be from `from`, in blocks.
///
/// A storm's base to the ground is a few hundred; cloud to cloud, a few
/// thousand. The cap also keeps every offset in the path small enough that
/// an `f32` holds it to a fraction of a sub-node.
pub const MAX_LIGHTNING_REACH: f64 = 4096.0;
/// The farthest either end may be from the world's centre on any axis, in
/// blocks: the world's own half extent and a bolt's reach beyond it.
///
/// **Bounded, not merely finite.** Two finite ends a few hundred orders of
/// magnitude apart have a distance that is not, and a bolt whose length is
/// infinity would pass the reach clamp as NaN and reach every client in
/// range as a message each one refuses — and a refused message ends the
/// connection. Inside this box every difference and every square is small.
pub const MAX_LIGHTNING_COORDINATE: f64 =
    crate::coords::WORLD_HALF_EXTENT_BLOCKS as f64 + MAX_LIGHTNING_REACH;

/// How many times the trunk is halved: sixty-four segments.
pub const TRUNK_LEVELS: u32 = 6;
/// How many times a fork is halved: sixteen segments.
pub const BRANCH_LEVELS: u32 = 4;
/// The most segments any bolt has: the trunk and every fork at the cap.
pub const MAX_SEGMENTS: usize =
    (1 << TRUNK_LEVELS) + MAX_LIGHTNING_BRANCHES as usize * (1 << BRANCH_LEVELS);

/// How far a midpoint moves off its segment, as a share of the segment's
/// length, at the first halving.
const ROUGHNESS: f32 = 0.15;
/// What each halving keeps of the one before's roughness, so the fine
/// levels jitter a little less than the coarse ones and the bolt reads as a
/// few big bends with a crackle on them, not as noise.
const ROUGHNESS_FALLOFF: f32 = 0.9;
/// How bright a fork is where it leaves the trunk.
const BRANCH_STRENGTH: f32 = 0.5;
/// The least and most of the distance still to go that a fork reaches.
const BRANCH_REACH: (f32, f32) = (0.35, 0.5);
/// How far a fork leans off the bolt, sideways blocks per block along it:
/// between about 27 and 50 degrees.
const BRANCH_LEAN: (f32, f32) = (0.5, 1.2);
/// The deepest share of what was left below its fork that any point of a
/// fork may reach. Under one, so a fork always ends above the ground.
const BRANCH_DEPTH: f32 = 0.8;
/// Where along the trunk's points a fork may leave it: from about a sixth
/// of the way down to about two thirds.
const FORK_FIRST: u64 = 10;
/// See [`FORK_FIRST`].
const FORK_SPAN: u64 = 36;

/// How dark a flicker's dip goes, as a share of the light before it.
const DIP_FLOOR: f32 = 0.15;
/// How far a dip's centre may wander from its even spacing, as a share of
/// the spacing.
const DIP_JITTER: f32 = 0.2;
/// Half a dip's width, as a share of the spacing. With the jitter, two dips
/// never overlap and the first never reaches the start.
const DIP_HALF: f32 = 0.3;
/// How much of the light is gone by the end, so the last return is the
/// dimmest.
const FADE: f32 = 0.4;
/// Mixed into the seed for the flicker, so the envelope's draws are their
/// own stream and never shift the path's.
const FLICKER_STREAM: u64 = 0x6C69_6768_746E_696E;

/// One bolt, as it travels: everything a client needs to draw it.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lightning {
    /// The top, in world blocks: a storm's base.
    pub from: [f64; 3],
    /// Where it strikes, in world blocks: the ground point the mod found.
    pub to: [f64; 3],
    /// What the path is built from. The same seed is the same bolt.
    pub seed: u64,
    /// The light, linear, `0..=2` a channel.
    pub colour: [f32; 3],
    /// The bright core's width, in blocks; the glow is a few times wider.
    pub width: f32,
    /// How many forks leave the trunk.
    pub branches: u8,
    /// How long it is seen, in ticks: full at once, then flickering out.
    pub ticks: u16,
}

impl Lightning {
    /// Whether every number is finite and in range — what a client checks
    /// before trusting one a server sent (charter rule 14).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.from
            .iter()
            .chain(&self.to)
            .all(|value| (-MAX_LIGHTNING_COORDINATE..=MAX_LIGHTNING_COORDINATE).contains(value))
            // The clamp moves `to` by a scale whose rounding can leave the
            // length a hair past the cap; a square block of slack is a
            // million times that and still refuses a bolt that is not one.
            && reach_squared(self.from, self.to) <= MAX_LIGHTNING_REACH * MAX_LIGHTNING_REACH + 1.0
            && self
                .colour
                .iter()
                .all(|c| (0.0..=crate::atmosphere::MAX_CHANNEL).contains(c))
            && (0.0..=MAX_LIGHTNING_WIDTH).contains(&self.width)
            && self.branches <= MAX_LIGHTNING_BRANCHES
            && (1..=MAX_LIGHTNING_TICKS).contains(&self.ticks)
    }
}

/// A bolt, and who sees it: everyone within `radius` of `from` in `domain`,
/// or one of them.
#[derive(Debug, Clone, PartialEq)]
pub struct LightningRequest {
    /// The bolt itself.
    pub lightning: Lightning,
    /// Which domain both ends are in.
    pub domain: String,
    /// How far from `from` it is seen, in blocks — up to the flash's
    /// [`crate::atmosphere::MAX_FLASH_RADIUS`], since a bolt is seen as far
    /// as its flash is.
    pub radius: f32,
    /// One player to send it to, or `None` for everyone in range. Narrows,
    /// never widens, as a flash's does (W28): the mod knows who is under open
    /// sky.
    pub player: Option<PlayerUuid>,
}

/// Clamps a bolt request's numbers into range, never refusing one.
///
/// A number that is not one takes the default; a bolt longer than
/// [`MAX_LIGHTNING_REACH`] keeps its top and its heading and has `to` moved
/// towards `from` until it fits, because the top is the storm the mod
/// placed and the ground point is the one it can most easily be wrong about.
#[must_use]
pub fn sanitise_lightning(mut request: LightningRequest) -> LightningRequest {
    let clamp = |value: f32, low: f32, high: f32, fallback: f32| {
        if value.is_finite() {
            value.clamp(low, high)
        } else {
            fallback
        }
    };
    let bolt = &mut request.lightning;
    let bound = |value: f64| value.clamp(-MAX_LIGHTNING_COORDINATE, MAX_LIGHTNING_COORDINATE);
    for value in &mut bolt.from {
        *value = if value.is_finite() {
            bound(*value)
        } else {
            0.0
        };
    }
    // A `to` that is not a number in one axis is a bolt with no length that
    // way, rather than one reaching for the world origin.
    for (value, top) in bolt.to.iter_mut().zip(bolt.from) {
        *value = if value.is_finite() {
            bound(*value)
        } else {
            top
        };
    }
    let reach = reach_squared(bolt.from, bolt.to).sqrt();
    if reach > MAX_LIGHTNING_REACH {
        let scale = MAX_LIGHTNING_REACH / reach;
        for (value, top) in bolt.to.iter_mut().zip(bolt.from) {
            *value = top + (*value - top) * scale;
        }
    }
    for (channel, fallback) in bolt.colour.iter_mut().zip(DEFAULT_COLOUR) {
        *channel = clamp(*channel, 0.0, crate::atmosphere::MAX_CHANNEL, fallback);
    }
    bolt.width = clamp(bolt.width, 0.0, MAX_LIGHTNING_WIDTH, DEFAULT_WIDTH);
    bolt.branches = bolt.branches.min(MAX_LIGHTNING_BRANCHES);
    bolt.ticks = bolt.ticks.clamp(1, MAX_LIGHTNING_TICKS);
    request.radius = clamp(
        request.radius,
        0.0,
        crate::atmosphere::MAX_FLASH_RADIUS,
        256.0,
    );
    request
}

/// The squared distance between two world points.
fn reach_squared(from: [f64; 3], to: [f64; 3]) -> f64 {
    let offset = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2]
}

/// One straight piece of a bolt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Where it starts, in blocks from the bolt's `from`.
    pub a: [f32; 3],
    /// Where it ends, the same way.
    pub b: [f32; 3],
    /// How bright it is: 1 on the trunk, less on a fork and less again
    /// towards a fork's tip.
    pub strength: f32,
}

/// A bolt's whole shape: the trunk's segments top to bottom, then each
/// fork's from where it leaves the trunk to its tip.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Path {
    /// Every segment, in the order they were built.
    pub segments: Vec<Segment>,
}

/// Builds a bolt's path from its seed.
///
/// **Midpoint displacement.** The trunk runs from `from` to `to` and is
/// halved [`TRUNK_LEVELS`] times, each new midpoint pushed off its segment,
/// square to it, by a seeded share of the segment's length — so the big
/// bends are big and the crackle on them is small. Then `branches` forks
/// leave the trunk partway down at seeded points, lean off at a seeded
/// angle, reach a share of what was left, and are halved
/// [`BRANCH_LEVELS`] times the same way. A fork starts at half the trunk's
/// strength and tapers to nothing at its tip, and no point of it goes deeper
/// than most of the way from its fork to the ground — forks fade out in the
/// air, as in the reference, and only the trunk strikes.
///
/// Same bolt, same path, on every machine: the draws are
/// [`Xoshiro256PlusPlus`]'s and the arithmetic is the Deterministic Float
/// Subset's (see the module docs).
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the offset is under the reach cap, which an f32 holds to a sub-node"
)]
pub fn build_path(bolt: &Lightning) -> Path {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(bolt.seed);
    let span = [
        (bolt.to[0] - bolt.from[0]) as f32,
        (bolt.to[1] - bolt.from[1]) as f32,
        (bolt.to[2] - bolt.from[2]) as f32,
    ];
    let trunk = displace(&mut rng, [0.0; 3], span, TRUNK_LEVELS);
    let mut segments =
        Vec::with_capacity((1 << TRUNK_LEVELS) + usize::from(bolt.branches) * (1 << BRANCH_LEVELS));
    segments.extend(trunk.windows(2).map(|pair| Segment {
        a: pair[0],
        b: pair[1],
        strength: 1.0,
    }));

    let length = norm(span);
    if length <= f32::EPSILON {
        return Path { segments };
    }
    let axis = scale(span, 1.0 / length);
    // **Which way the ground is.** A bolt that falls strikes the ground under
    // it, so a fork must stay above `to`'s height; one that does not fall —
    // cloud to cloud — has no ground, and a fork must only stop short of
    // where the trunk ends.
    let (down, ground) = if span[1] < 0.0 {
        ([0.0, -1.0, 0.0], -span[1])
    } else {
        (axis, length)
    };
    let (across, beside) = perpendiculars(axis);
    for _ in 0..bolt.branches {
        // Every draw is taken whether the fork is kept or not, so a fork
        // that is skipped never moves the ones after it.
        let at = (FORK_FIRST + rng.below(FORK_SPAN)) as usize;
        let reach = BRANCH_REACH.0 + (BRANCH_REACH.1 - BRANCH_REACH.0) * rng.next_f32();
        let lean = BRANCH_LEAN.0 + (BRANCH_LEAN.1 - BRANCH_LEAN.0) * rng.next_f32();
        let sway = [rng.next_f32_signed(), rng.next_f32_signed()];

        let fork = trunk[at.min(trunk.len() - 1)];
        let depth = dot(fork, down);
        let left = ground - depth;
        if left <= 0.0 {
            continue;
        }
        let sideways =
            normalised(add(scale(across, sway[0]), scale(beside, sway[1]))).unwrap_or(across);
        let heading = normalised(add(axis, scale(sideways, lean))).unwrap_or(axis);
        let distance = norm(sub(span, fork)) * reach;
        let end = add(fork, scale(heading, distance));
        let mut points = displace(&mut rng, fork, end, BRANCH_LEVELS);
        let deepest = depth + left * BRANCH_DEPTH;
        for point in &mut points {
            let past = dot(*point, down) - deepest;
            if past > 0.0 {
                *point = sub(*point, scale(down, past));
            }
        }
        let steps = (points.len() - 1) as f32;
        segments.extend(points.windows(2).enumerate().map(|(index, pair)| {
            // Measured at the segment's middle, so the tip is faint rather
            // than a hard zero that draws nothing for its last sixteenth.
            let tip = (index as f32 + 0.5) / steps;
            Segment {
                a: pair[0],
                b: pair[1],
                strength: BRANCH_STRENGTH * (1.0 - tip),
            }
        }));
    }
    Path { segments }
}

/// Halves `start` to `end` `levels` times, pushing each new midpoint off its
/// segment, and returns every point in order, both ends included.
fn displace(
    rng: &mut Xoshiro256PlusPlus,
    start: [f32; 3],
    end: [f32; 3],
    levels: u32,
) -> Vec<[f32; 3]> {
    let mut points = vec![start, end];
    let mut roughness = ROUGHNESS;
    for _ in 0..levels {
        let mut next = Vec::with_capacity(points.len() * 2 - 1);
        for pair in points.windows(2) {
            let (near, far) = (pair[0], pair[1]);
            let along = sub(far, near);
            let (across, beside) = perpendiculars(along);
            let sway = [rng.next_f32_signed(), rng.next_f32_signed()];
            let amount = norm(along) * roughness;
            let middle = scale(add(near, far), 0.5);
            next.push(near);
            next.push(add(
                middle,
                add(
                    scale(across, sway[0] * amount),
                    scale(beside, sway[1] * amount),
                ),
            ));
        }
        next.push(end);
        points = next;
        roughness *= ROUGHNESS_FALLOFF;
    }
    points
}

/// Two unit vectors square to `direction` and to each other.
///
/// **Cross products, not angles**: no trigonometry reaches the path, which
/// is what keeps it inside the subset. The helper axis is whichever of up or
/// east is further from the direction, so the cross product never nears
/// zero; a direction with no length gets any two, since there is nothing to
/// be square to.
fn perpendiculars(direction: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let Some(unit) = normalised(direction) else {
        return ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
    };
    let helper = if unit[1].abs() < 0.9 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let u = normalised(cross(unit, helper)).unwrap_or([1.0, 0.0, 0.0]);
    (u, cross(unit, u))
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], by: f32) -> [f32; 3] {
    [a[0] * by, a[1] * by, a[2] * by]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

/// `a` at unit length, or `None` for one too short to have a direction —
/// never a NaN, which sim-grade code does not make.
fn normalised(a: [f32; 3]) -> Option<[f32; 3]> {
    let length = norm(a);
    (length > f32::EPSILON).then(|| scale(a, 1.0 / length))
}

/// A hash over every coordinate of a path, by its bits.
///
/// FNV-1a over each float's bit pattern, in the order the path holds them:
/// two paths hash alike only if every number in them is the same number,
/// which is the claim "the same bolt on two clients" makes.
#[must_use]
pub fn path_hash(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for segment in &path.segments {
        let values = segment
            .a
            .iter()
            .chain(&segment.b)
            .chain(std::iter::once(&segment.strength));
        for value in values {
            for byte in value.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

/// How bright a bolt is `age_ticks` after it struck: its flicker.
///
/// **Full at once, then two or three dips and returns** — the return
/// strokes that make a real bolt read as lightning and not as a line
/// switched on and off. Each dip falls to [`DIP_FLOOR`] and comes back,
/// evenly spaced with a seeded wander, the light fading a little overall so
/// the last return is the dimmest; at `ticks` it is gone. The count and
/// where each falls come from the seed, so a bolt flickers the same for
/// everyone watching it, and from a stream of their own, so the flicker
/// never moves the path.
#[must_use]
pub fn brightness(bolt: &Lightning, age_ticks: f32) -> f32 {
    let life = f32::from(bolt.ticks);
    // A NaN age is over, not lit.
    if age_ticks.is_nan() || age_ticks >= life {
        return 0.0;
    }
    let age = age_ticks.max(0.0);
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(bolt.seed ^ FLICKER_STREAM);
    let dips = 2 + rng.below(2);
    let spacing = life / (dips + 1) as f32;
    let mut level: f32 = 1.0;
    for dip in 0..dips {
        let centre = spacing * (dip + 1) as f32 + rng.next_f32_signed() * spacing * DIP_JITTER;
        let half = spacing * DIP_HALF;
        let off = (age - centre).abs();
        if off < half {
            level = level.min(DIP_FLOOR + (1.0 - DIP_FLOOR) * off / half);
        }
    }
    level * (1.0 - FADE * age / life)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bolt from a storm's base 300 blocks up to the ground below it.
    fn strike(seed: u64) -> Lightning {
        Lightning {
            from: [1000.0, 380.0, -2000.0],
            to: [1040.0, 80.0, -1970.0],
            seed,
            colour: DEFAULT_COLOUR,
            width: DEFAULT_WIDTH,
            branches: DEFAULT_BRANCHES,
            ticks: DEFAULT_TICKS,
        }
    }

    #[test]
    fn the_defaults_are_a_valid_bolt() {
        assert!(strike(1).is_valid());
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the claim is exact: the same numbers, not nearly them"
    )]
    fn a_bolt_is_clamped_into_the_same_ranges_it_is_checked_against() {
        let wild = LightningRequest {
            lightning: Lightning {
                from: [f64::NAN, 300.0, 0.0],
                to: [9000.0, f64::INFINITY, 0.0],
                seed: 0,
                colour: [f32::NAN, 9.0, -1.0],
                width: f32::INFINITY,
                branches: u8::MAX,
                ticks: 0,
            },
            domain: "d".to_owned(),
            radius: f32::NAN,
            player: None,
        };
        assert!(!wild.lightning.is_valid());
        let tame = sanitise_lightning(wild);
        let bolt = tame.lightning;
        assert!(bolt.is_valid(), "{bolt:?}");
        assert!(
            bolt.from[0].abs() < f64::EPSILON,
            "a top that is not a number is the origin"
        );
        assert!(
            (bolt.to[1] - 300.0).abs() < 1e-9,
            "a `to` that is not a number in an axis has no length that way"
        );
        // Moved towards `from` along its own heading, to the cap.
        let reach = reach_squared(bolt.from, bolt.to).sqrt();
        assert!((reach - MAX_LIGHTNING_REACH).abs() < 1e-6, "{reach}");
        assert!(bolt.to[0] > 0.0 && bolt.to[2].abs() < 1e-9);
        assert!((bolt.colour[0] - DEFAULT_COLOUR[0]).abs() < f32::EPSILON);
        assert!((bolt.colour[1] - crate::atmosphere::MAX_CHANNEL).abs() < f32::EPSILON);
        assert!(bolt.colour[2].abs() < f32::EPSILON);
        assert!((bolt.width - DEFAULT_WIDTH).abs() < f32::EPSILON);
        assert_eq!(bolt.branches, MAX_LIGHTNING_BRANCHES);
        assert_eq!(
            bolt.ticks, 1,
            "a bolt asked for is seen for a tick at least"
        );
        assert!((tame.radius - 256.0).abs() < f32::EPSILON);

        let long = sanitise_lightning(LightningRequest {
            lightning: Lightning {
                ticks: u16::MAX,
                width: 40.0,
                ..strike(3)
            },
            domain: "d".to_owned(),
            radius: 1e9,
            player: None,
        });
        assert_eq!(long.lightning.ticks, MAX_LIGHTNING_TICKS);
        assert!((long.lightning.width - MAX_LIGHTNING_WIDTH).abs() < f32::EPSILON);
        assert!((long.radius - crate::atmosphere::MAX_FLASH_RADIUS).abs() < f32::EPSILON);
        assert_eq!(
            long.lightning.to,
            strike(3).to,
            "a bolt inside the reach is not moved"
        );
    }

    #[test]
    fn ends_too_far_apart_to_measure_are_brought_into_the_world() {
        // Both finite, and their distance is not: without the box the reach
        // clamp would scale an infinity to NaN and send every client in range
        // a message it must refuse.
        let tame = sanitise_lightning(LightningRequest {
            lightning: Lightning {
                from: [1.7e308, 300.0, 0.0],
                to: [-1.7e308, 0.0, 0.0],
                ..strike(8)
            },
            domain: "d".to_owned(),
            radius: 256.0,
            player: None,
        });
        assert!(tame.lightning.is_valid(), "{:?}", tame.lightning);
        assert!((tame.lightning.from[0] - MAX_LIGHTNING_COORDINATE).abs() < 1e-9);
    }

    #[test]
    fn a_bolt_out_of_range_is_not_valid() {
        let good = strike(5);
        let bad = [
            Lightning {
                from: [MAX_LIGHTNING_COORDINATE + 1.0, 300.0, 0.0],
                to: [MAX_LIGHTNING_COORDINATE + 1.0, 0.0, 0.0],
                ..good
            },
            Lightning {
                from: [f64::NAN, 0.0, 0.0],
                ..good
            },
            Lightning {
                to: [0.0, f64::INFINITY, 0.0],
                ..good
            },
            Lightning {
                to: [good.from[0] + 5000.0, good.from[1], good.from[2]],
                ..good
            },
            Lightning {
                colour: [0.5, f32::NAN, 0.5],
                ..good
            },
            Lightning { width: 9.0, ..good },
            Lightning {
                width: -0.1,
                ..good
            },
            Lightning {
                branches: MAX_LIGHTNING_BRANCHES + 1,
                ..good
            },
            Lightning { ticks: 0, ..good },
            Lightning {
                ticks: MAX_LIGHTNING_TICKS + 1,
                ..good
            },
        ];
        for bolt in bad {
            assert!(!bolt.is_valid(), "{bolt:?}");
        }
    }

    #[test]
    fn the_same_seed_builds_the_same_path_and_another_seed_another() {
        let first = path_hash(&build_path(&strike(42)));
        let again = path_hash(&build_path(&strike(42)));
        assert_eq!(first, again, "one seed, two paths");
        let other = path_hash(&build_path(&strike(43)));
        assert_ne!(first, other, "two seeds, one path");
        // The same seed from somewhere else is the same shape: the path is
        // relative to `from`, so where the storm is does not move its forks.
        let moved = Lightning {
            from: [-50_000.0, 380.0, 7.0],
            to: [-49_960.0, 80.0, 37.0],
            ..strike(42)
        };
        assert_eq!(path_hash(&build_path(&moved)), first);
    }

    #[test]
    fn a_seed_past_the_top_bit_is_a_seed_by_its_bits() {
        // The class of bug that made half of all new worlds bare
        // (2026-09-25): a seed past 2^63 rounded through a float is a
        // different seed, and here a different bolt on the client that
        // rounded it.
        let seed = 16_099_289_709_293_836_018_u64;
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the rounding is what is being shown to matter"
        )]
        let rounded = (seed as f64) as u64;
        assert_ne!(seed, rounded);
        let exact = path_hash(&build_path(&strike(seed)));
        assert_eq!(exact, path_hash(&build_path(&strike(seed))));
        assert_ne!(exact, path_hash(&build_path(&strike(rounded))));
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the claim is exact: the same numbers, not nearly them"
    )]
    fn the_trunk_runs_from_the_top_to_the_strike() {
        let bolt = strike(9);
        let path = build_path(&bolt);
        let trunk = &path.segments[..1 << TRUNK_LEVELS];
        assert_eq!(trunk[0].a, [0.0; 3]);
        assert_eq!(trunk[trunk.len() - 1].b, [40.0, -300.0, 30.0]);
        for pair in trunk.windows(2) {
            assert_eq!(pair[0].b, pair[1].a, "the trunk is one line");
        }
        assert!(
            trunk
                .iter()
                .all(|segment| (segment.strength - 1.0).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn forks_fade_and_end_above_the_ground() {
        // Hundreds of bolts, falling straight and slanting, at the cap for
        // forks: no point of any fork reaches the height of the strike.
        for seed in 0..300 {
            for to in [[0.0, 0.0, 0.0], [120.0, 0.0, -80.0], [400.0, 250.0, 0.0]] {
                let bolt = Lightning {
                    from: [0.0, 300.0, 0.0],
                    to,
                    branches: MAX_LIGHTNING_BRANCHES,
                    ..strike(seed)
                };
                let path = build_path(&bolt);
                let ground = (bolt.to[1] - bolt.from[1]) as f32;
                let forks = &path.segments[1 << TRUNK_LEVELS..];
                for segment in forks {
                    assert!(
                        segment.a[1] > ground && segment.b[1] > ground,
                        "seed {seed}: a fork reached the ground at {segment:?}"
                    );
                    assert!(segment.strength < 1.0 && segment.strength > 0.0);
                }
                for fork in forks.chunks(1 << BRANCH_LEVELS) {
                    assert!(
                        fork[fork.len() - 1].strength < fork[0].strength / 4.0,
                        "a fork fades to its tip"
                    );
                }
            }
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the claim is exact: the same numbers, not nearly them"
    )]
    fn a_bolt_has_no_more_segments_than_the_cap_allows() {
        for branches in 0..=MAX_LIGHTNING_BRANCHES {
            let path = build_path(&Lightning {
                branches,
                ..strike(11)
            });
            assert!(path.segments.len() <= MAX_SEGMENTS);
            assert!(
                path.segments.len()
                    <= (1 << TRUNK_LEVELS) + usize::from(branches) * (1 << BRANCH_LEVELS)
            );
        }
        assert_eq!(MAX_SEGMENTS, 64 + 8 * 16);
        // Even for one that clamping could not make sensible: no length at
        // all is sixty-four segments on one point, no forks, and no NaN.
        let point = build_path(&Lightning {
            to: strike(1).from,
            ..strike(1)
        });
        assert_eq!(point.segments.len(), 1 << TRUNK_LEVELS);
        assert!(point.segments.iter().all(|segment| segment.a == [0.0; 3]));
    }

    #[test]
    fn a_bolt_is_full_at_once_flickers_two_or_three_times_and_goes_out() {
        let mut counts = std::collections::BTreeSet::new();
        for seed in 0..64 {
            let bolt = Lightning {
                ticks: 40,
                ..strike(seed)
            };
            assert!((brightness(&bolt, 0.0) - 1.0).abs() < f32::EPSILON);
            assert!(brightness(&bolt, 40.0).abs() < f32::EPSILON);
            assert!(brightness(&bolt, 1000.0).abs() < f32::EPSILON);
            assert!(brightness(&bolt, f32::NAN).abs() < f32::EPSILON);
            // Walk it finely and count the dips.
            let mut dips = 0;
            let mut dark = false;
            let mut darkest = 1.0_f32;
            for step in 0..4000 {
                let level = brightness(&bolt, step as f32 * 0.01);
                darkest = darkest.min(level);
                if level < 0.3 && !dark {
                    dips += 1;
                }
                dark = level < 0.3;
            }
            assert!(darkest < 0.2, "seed {seed}: no dip reached near the floor");
            assert!(
                !dark,
                "seed {seed}: it went out in a dip rather than after a return"
            );
            assert!((2..=3).contains(&dips), "seed {seed}: {dips} dips");
            counts.insert(dips);
            assert_eq!(
                brightness(&bolt, 13.7).to_bits(),
                brightness(&bolt, 13.7).to_bits(),
                "the flicker is the seed's"
            );
        }
        assert_eq!(counts.len(), 2, "both two and three dips occur: {counts:?}");
    }
}
