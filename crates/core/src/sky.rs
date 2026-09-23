// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The star catalog: one source for the sky, the map, and the destination.
//!
//! # Stars are places, not directions
//!
//! **This is a deliberate strengthening of what Task 15c's prompt describes.**
//! That prompt asks for a catalog of *directions as seen from the overworld*,
//! which is enough to draw a skybox and nothing more. Iridesium's decision
//! (2026-08-06) is that every star is somewhere a player can go, so the catalog
//! stores a **position in a universal frame** and a direction is derived from
//! wherever the observer happens to be.
//!
//! The difference matters now rather than later. A catalog of directions has no
//! answer to "what does the sky look like from *there*" — the whole sky would
//! have to be regenerated per world, and two worlds' skies would have no
//! relationship to each other. Positions give that for free: the sky from any
//! point is the same catalog seen from a different place, and travelling
//! between two stars changes the view exactly as it should.
//!
//! # Two coordinate systems, and which is which
//!
//! - **Local** — charter rule 7's `(chunk, local)` pair. Where you are *within*
//!   a world, in blocks, and never accumulated in `f32`.
//! - **Universal** — [`UniversalPos`], in blocks, `i64`. Where a *world* is.
//!   The origin is the centre of the universe and is itself a destination.
//!
//! A player therefore has both at once: a local position in the world they are
//! standing on, and that world's universal position. Nothing here converts
//! between them, because they measure different things — one is where you are,
//! the other is which world you are on.
//!
//! # Determinism
//!
//! Charter rule 4 in full: this is world identity, not a picture. Positions are
//! integers from [`crate::detgen::StreamRng`]; directions are `f64` and `sqrt`,
//! both in the allowed subset. **No trigonometry** — a direction on the unit
//! sphere comes from rejection sampling rather than `sin`/`cos` of two angles,
//! which is both deterministic and better sampling, since the angle method
//! clusters points at the poles.

use crate::detgen::StreamRng;

/// The RNG stream the catalog draws from.
///
/// Named rather than derived from the seed alone so that adding another
/// world-level stream later cannot shift the stars — charter rule 4's
/// stream-per-purpose rule.
pub const STAR_STREAM: &str = "sky:stars";

/// The stream a world's own place in the universe comes from.
pub const WORLD_PLACE_STREAM: &str = "sky:world-position";

/// How many stars a universe has.
///
/// Task 15c asks for a capped catalog and suggests 2048. Fixed rather than
/// configurable: the catalog is universe identity, and a count a server could
/// change would mean two servers on one seed disagreeing about the sky. A mod
/// that wants fewer visible simply draws fewer.
pub const STAR_COUNT: usize = 2048;

/// How far stars spread from the centre, in blocks.
///
/// A block is a yard (charter rule 5), so this is roughly a tenth of a
/// light-year — small for a universe and enormous for a game. `i64` blocks
/// reach about nine hundred light-years before overflowing, so there is room to
/// grow this by four orders of magnitude if travel ever makes it feel cramped.
pub const UNIVERSE_RADIUS: i64 = 1_000_000_000_000_000;

/// The reserved id of the sun.
///
/// Task 15c asks for the sun and moon to be catalog entries with reserved ids
/// rather than special cases. They are not generated — a mod places them —
/// but the ids are held back here so a generated star can never collide.
pub const SUN_ID: u32 = 0;

/// The reserved id of the moon.
pub const MOON_ID: u32 = 1;

/// The first id a generated star may take.
pub const FIRST_STAR_ID: u32 = 16;

/// A position in the universe, in blocks.
///
/// **The centre is `(0, 0, 0)` and is a real place.** Nothing is generated
/// there, which is the point: it is somewhere to go rather than something to
/// look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct UniversalPos {
    /// East–west, in blocks.
    pub x: i64,
    /// Up–down, in blocks.
    pub y: i64,
    /// North–south, in blocks.
    pub z: i64,
}

impl UniversalPos {
    /// The centre of the universe.
    pub const CENTRE: Self = Self { x: 0, y: 0, z: 0 };

    /// A position from its three coordinates.
    #[must_use]
    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }

    /// Distance to another position, in blocks.
    ///
    /// `f64` because the squared distance between two points a light-year apart
    /// overflows `i64` — 10^15 squared is 10^30, and `i64` stops at 9.2 × 10^18.
    /// A `f64` holds the intermediate exactly enough for a distance nobody
    /// measures to the yard, and `sqrt` is in charter rule 4's allowed subset.
    #[must_use]
    pub fn distance_to(self, other: Self) -> f64 {
        let dx = (other.x - self.x) as f64;
        let dy = (other.y - self.y) as f64;
        let dz = (other.z - self.z) as f64;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    /// The unit direction from here to `other`, or `None` for the same point.
    ///
    /// `None` rather than a zero vector or a panic: "which way is here from
    /// here" has no answer, and a caller standing on a star has to decide what
    /// to draw rather than be handed a direction that is quietly wrong.
    #[must_use]
    pub fn direction_to(self, other: Self) -> Option<[f32; 3]> {
        let dx = (other.x - self.x) as f64;
        let dy = (other.y - self.y) as f64;
        let dz = (other.z - self.z) as f64;
        let square = dx * dx + dy * dy + dz * dz;
        if square <= 0.0 {
            return None;
        }
        let length = square.sqrt();
        Some([
            (dx / length) as f32,
            (dy / length) as f32,
            (dz / length) as f32,
        ])
    }
}

/// One star: a place, and what it looks like from far away.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StarRecord {
    /// Stable identifier, unique within a universe.
    ///
    /// Ids below [`FIRST_STAR_ID`] are reserved for bodies a mod places, so a
    /// generated star can never collide with the sun or the moon.
    pub id: u32,
    /// Where it is, in universal blocks.
    pub position: UniversalPos,
    /// Intrinsic brightness, `0.0..=1.0`.
    ///
    /// **Intrinsic, not apparent.** How bright a star *looks* depends on how
    /// far away the observer is, and the observer is not fixed any more — that
    /// is the whole point of storing positions. [`StarRecord::apparent`] does
    /// the falloff.
    pub magnitude: f32,
    /// Colour temperature as a hue mix, `0.0` coolest to `1.0` warmest.
    pub warmth: f32,
}

impl StarRecord {
    /// The unit direction from an observer to this star.
    ///
    /// `None` if the observer is standing on it, which a client should draw as
    /// "no star in the sky" rather than as a star in an arbitrary direction.
    #[must_use]
    pub fn direction_from(&self, observer: UniversalPos) -> Option<[f32; 3]> {
        observer.direction_to(self.position)
    }

    /// How bright this star looks from `observer`, `0.0..=1.0`.
    ///
    /// **Not a plain inverse square**, and the reason is worth stating: real
    /// falloff is `1/d²`, which for any star inside the universe is a number
    /// greater than one and clamps to full brightness. Every star would look
    /// identical from everywhere, which is exactly the failure that storing
    /// positions was supposed to fix.
    ///
    /// `1 / (1 + (d/r)²)` keeps the inverse-square *shape* — brightness falling
    /// with the square of distance — while staying bounded, so it is monotonic
    /// over the whole range instead of saturating across most of it. A star at
    /// the universe's radius keeps about two thirds of its magnitude, and one
    /// you are standing on keeps all of it.
    #[must_use]
    pub fn apparent(&self, observer: UniversalPos) -> f32 {
        let distance = observer.distance_to(self.position);
        if distance <= 0.0 {
            return self.magnitude.clamp(0.0, 1.0);
        }
        let ratio = distance / UNIVERSE_RADIUS as f64;
        let falloff = 1.0 / (1.0 + ratio * ratio);
        (f64::from(self.magnitude) * falloff).clamp(0.0, 1.0) as f32
    }
}

/// Where a world sits in the universe.
///
/// Derived from the world seed, so a world's place is as fixed as its terrain
/// and two servers on one seed are the same world in the same place. **Never
/// the centre**: the centre is somewhere to travel to, and a world that started
/// there would make the destination its own spawn.
#[must_use]
pub fn world_position(world_seed: u64) -> UniversalPos {
    let mut rng = StreamRng::global(world_seed, WORLD_PLACE_STREAM);
    // Half the radius out, so there is universe in every direction from home
    // rather than a wall on one side.
    let position = sample_position(&mut rng, UNIVERSE_RADIUS / 2);
    if position == UniversalPos::CENTRE {
        // Astronomically unlikely and cheap to rule out. A world at the centre
        // would make "travel to the centre of the universe" a journey of zero
        // blocks for whoever rolled it.
        return UniversalPos::new(UNIVERSE_RADIUS / 4, 0, 0);
    }
    position
}

/// Every star in a universe, in a fixed order.
///
/// The same seed gives the same catalog on every platform and in every process
/// — which is the property the sky renderer and any map of it both depend on.
#[must_use]
pub fn star_catalog(world_seed: u64) -> Vec<StarRecord> {
    let mut rng = StreamRng::global(world_seed, STAR_STREAM);
    let mut stars = Vec::with_capacity(STAR_COUNT);

    for index in 0..STAR_COUNT {
        let position = sample_position(&mut rng, UNIVERSE_RADIUS);
        // Squared, so most stars are faint and a few are bright. A uniform
        // magnitude gives a sky that looks printed on.
        let roll = rng.next_f32();
        stars.push(StarRecord {
            id: FIRST_STAR_ID + index as u32,
            position,
            magnitude: roll * roll,
            warmth: rng.next_f32(),
        });
    }
    stars
}

/// A point inside a sphere of `radius` blocks, uniformly distributed.
///
/// **Rejection sampling, because charter rule 4 bans trigonometry.** The
/// textbook alternative — two angles through `sin` and `cos` — calls platform
/// libm and differs between operating systems, which is exactly what the rule
/// exists to prevent. Drawing a point in the cube and rejecting it unless it
/// lands inside the sphere uses only multiplication and comparison.
fn sample_position(rng: &mut StreamRng, radius: i64) -> UniversalPos {
    loop {
        let x = rng.next_f32_signed();
        let y = rng.next_f32_signed();
        let z = rng.next_f32_signed();
        if x * x + y * y + z * z > 1.0 {
            continue;
        }
        // Scaled through `f64`: `f32` has 24 bits of mantissa and the radius has
        // fifty, so multiplying in `f32` would quantise every star onto a coarse
        // lattice — visible as stars in rows once anyone travels far enough to
        // see them from the side.
        let scale = radius as f64;
        return UniversalPos::new(
            (f64::from(x) * scale) as i64,
            (f64::from(y) * scale) as i64,
            (f64::from(z) * scale) as i64,
        );
    }
}

/// Where the sky has turned to at a moment of the day, as `(cos, sin)`.
///
/// The stars wheel with the sun. The client's sun rises in the east at 0.25,
/// stands highest at 0.5 and sets in the west at 0.75, turning about the
/// world's z axis; the catalog's directions turn the same way, so a star that
/// was beside the setting sun is beside it every evening. A full turn a day.
///
/// `detgen::trig` rather than `f32::sin`, because the server answers
/// `game.star_in_view` from this and a mod acts on the answer — it is world
/// state (charter rule 4) — and the client draws from the same table so the
/// star it draws and the star the server names are one star.
#[must_use]
pub fn turn(time_of_day: f32) -> (f32, f32) {
    use crate::detgen::trig;
    let angle = (time_of_day.rem_euclid(1.0) - 0.25) * std::f32::consts::TAU;
    (trig::cos(angle), trig::sin(angle))
}

/// A catalog direction as it appears in a world's sky at a moment of the day.
///
/// Rotates about the world's z axis by the day's [`turn`], the sense the sun
/// moves in: at 0.25 the catalog frame and the world frame coincide.
#[must_use]
pub fn wheeled(direction: [f32; 3], turn: (f32, f32)) -> [f32; 3] {
    let (cos, sin) = turn;
    [
        direction[0] * cos + direction[1] * sin,
        direction[1] * cos - direction[0] * sin,
        direction[2],
    ]
}

/// The inverse of [`wheeled`]: a world direction taken back into the catalog
/// frame, which is how a gaze is matched against the catalog.
#[must_use]
pub fn unwheeled(direction: [f32; 3], turn: (f32, f32)) -> [f32; 3] {
    let (cos, sin) = turn;
    [
        direction[0] * cos - direction[1] * sin,
        direction[1] * cos + direction[0] * sin,
        direction[2],
    ]
}

/// The star nearest to a gaze, and how squarely it is looked at.
///
/// `gaze` is a unit direction in the world's frame and `observer` is where the
/// world is; the answer is the id of the catalog star whose direction from
/// there, wheeled to `time_of_day`, lies closest along the gaze, with the
/// cosine of the angle between them — `1.0` is dead centre. `None` only for an
/// empty catalog or a gaze that is not a direction.
///
/// **This is the sky renderer's arithmetic run backwards**, and it must stay
/// that: the client draws star N along [`StarRecord::direction_from`] turned
/// by [`wheeled`], and a mod asking which star a player is looking at has to
/// be told N — Task 15c's no-desync property.
#[must_use]
pub fn star_in_view(
    catalog: &[StarRecord],
    observer: UniversalPos,
    gaze: [f32; 3],
    time_of_day: f32,
) -> Option<(u32, f32)> {
    let length = (gaze[0] * gaze[0] + gaze[1] * gaze[1] + gaze[2] * gaze[2]).sqrt();
    if !length.is_finite() || length <= 0.0 {
        return None;
    }
    let gaze = unwheeled(
        [gaze[0] / length, gaze[1] / length, gaze[2] / length],
        turn(time_of_day),
    );
    let mut best: Option<(u32, f32)> = None;
    for star in catalog {
        let Some(direction) = star.direction_from(observer) else {
            continue;
        };
        let alignment = direction[0] * gaze[0] + direction[1] * gaze[1] + direction[2] * gaze[2];
        // Strictly greater: the first of two stars in exactly one direction
        // wins, so the answer is a function of the catalog's order and not of
        // anything else.
        if best.is_none_or(|(_, held)| alignment > held) {
            best = Some((star.id, alignment));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_universe() {
        // The property the whole module exists for: one derivation, so the sky
        // and any map of it cannot disagree.
        let a = star_catalog(12_345);
        let b = star_catalog(12_345);
        assert_eq!(a.len(), STAR_COUNT);
        assert_eq!(a, b);
        assert_eq!(world_position(12_345), world_position(12_345));
    }

    #[test]
    fn different_seeds_give_different_universes() {
        // The counter-example: a catalog ignoring its seed would pass above.
        assert_ne!(star_catalog(1), star_catalog(2));
        assert_ne!(world_position(1), world_position(2));
    }

    #[test]
    fn the_centre_of_the_universe_is_empty_and_reachable() {
        // **The destination.** Nothing is generated at the origin, which is
        // what makes it somewhere to go rather than something to look at, and
        // no world starts there.
        for seed in [1u64, 2, 3, 99, 12_345, u64::MAX] {
            assert_ne!(
                world_position(seed),
                UniversalPos::CENTRE,
                "seed {seed} put a world at the centre, which would make the journey zero blocks"
            );
            assert!(
                !star_catalog(seed)
                    .iter()
                    .any(|star| star.position == UniversalPos::CENTRE),
                "seed {seed} generated a star at the centre"
            );
        }
    }

    #[test]
    fn every_star_is_inside_the_universe() {
        for star in star_catalog(7) {
            let distance = UniversalPos::CENTRE.distance_to(star.position);
            assert!(
                distance <= UNIVERSE_RADIUS as f64,
                "a star is {distance} blocks out, past the {UNIVERSE_RADIUS}-block radius"
            );
        }
    }

    #[test]
    fn a_star_looks_brighter_from_closer_to_it() {
        // **The reason positions beat directions.** A catalog of directions has
        // no answer to this at all: the sky would look identical from
        // everywhere, and travelling would change nothing.
        let star = star_catalog(4)
            .into_iter()
            .find(|star| star.magnitude > 0.5)
            .expect("some star is bright");

        let far = UniversalPos::CENTRE;
        let near = UniversalPos::new(
            star.position.x / 2,
            star.position.y / 2,
            star.position.z / 2,
        );
        assert!(
            star.apparent(near) > star.apparent(far),
            "approaching a star did not brighten it: {} then {}",
            star.apparent(far),
            star.apparent(near)
        );
    }

    #[test]
    fn the_sky_from_two_places_is_not_the_same_sky() {
        // Travel has to change the view, or "visitable" means nothing.
        let stars = star_catalog(11);
        let home = world_position(11);

        let differing = stars
            .iter()
            .filter(|star| {
                let (Some(a), Some(b)) = (
                    star.direction_from(home),
                    star.direction_from(UniversalPos::CENTRE),
                ) else {
                    return false;
                };
                // Any noticeable angular difference at all.
                (0..3).any(|axis| (a[axis] - b[axis]).abs() > 0.01)
            })
            .count();
        assert!(
            differing > stars.len() / 2,
            "only {differing} of {} stars moved between two places a light-year apart",
            stars.len()
        );
    }

    #[test]
    fn standing_on_a_star_has_no_direction_rather_than_a_wrong_one() {
        let star = star_catalog(5).into_iter().next().expect("a star");
        assert!(star.direction_from(star.position).is_none());
        // And it is at its brightest, which is the honest answer for a body
        // you are inside: no falloff at all, so its full magnitude.
        assert!((star.apparent(star.position) - star.magnitude).abs() < 1e-6);
    }

    #[test]
    fn every_direction_is_a_unit_vector() {
        let home = world_position(7);
        for star in star_catalog(7) {
            let Some([x, y, z]) = star.direction_from(home) else {
                continue;
            };
            let length_squared = x * x + y * y + z * z;
            assert!(
                (length_squared - 1.0).abs() < 1e-4,
                "direction to star {} has length squared {length_squared}",
                star.id
            );
        }
    }

    #[test]
    fn the_stars_are_spread_over_the_whole_sky() {
        // Rejection sampling is uniform; the angle method it replaces clusters
        // at the poles. Every octant should hold roughly an eighth.
        let mut octants = [0usize; 8];
        for star in star_catalog(99) {
            let position = star.position;
            let index = usize::from(position.x > 0)
                | (usize::from(position.y > 0) << 1)
                | (usize::from(position.z > 0) << 2);
            octants[index] += 1;
        }
        let expected = STAR_COUNT / 8;
        for (index, count) in octants.into_iter().enumerate() {
            assert!(
                count > expected / 2 && count < expected * 2,
                "octant {index} holds {count} stars against about {expected}"
            );
        }
    }

    #[test]
    fn ids_are_unique_and_leave_room_for_the_sun_and_moon() {
        let stars = star_catalog(3);
        let mut ids: Vec<u32> = stars.iter().map(|star| star.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), stars.len(), "two stars share an id");
        assert!(
            stars.iter().all(|star| star.id >= FIRST_STAR_ID),
            "a generated star took an id reserved for a body a mod places"
        );
        const { assert!(SUN_ID < FIRST_STAR_ID && MOON_ID < FIRST_STAR_ID) };
    }

    #[test]
    fn positions_are_not_quantised_onto_a_lattice() {
        // Scaling in `f32` would put every star on a coarse grid: 24 bits of
        // mantissa against a fifty-bit radius. Invisible from home and obvious
        // from anywhere else, which is the worst way for a bug to be found.
        let stars = star_catalog(21);
        let distinct: std::collections::BTreeSet<i64> =
            stars.iter().map(|star| star.position.x).collect();
        assert!(
            distinct.len() > stars.len() * 9 / 10,
            "only {} distinct x coordinates among {} stars; positions are on a lattice",
            distinct.len(),
            stars.len()
        );
    }

    #[test]
    fn wheeling_a_direction_and_unwheeling_it_gives_it_back() {
        let direction = [0.6, 0.0, 0.8];
        for time in [0.0, 0.1, 0.25, 0.5, 0.77, 0.999] {
            let turned = turn(time);
            let there = wheeled(direction, turned);
            let back = unwheeled(there, turned);
            for axis in 0..3 {
                assert!(
                    (back[axis] - direction[axis]).abs() < 1e-5,
                    "time {time}: axis {axis} came back as {} from {}",
                    back[axis],
                    direction[axis]
                );
            }
            let length = (there[0] * there[0] + there[1] * there[1] + there[2] * there[2]).sqrt();
            assert!(
                (length - 1.0).abs() < 1e-5,
                "a turn changed a direction's length"
            );
        }
    }

    #[test]
    fn the_stars_wheel_the_way_the_sun_does() {
        // The client's sun is at east (-x) on the horizon at 0.25 and straight
        // up at 0.5. A star that sits where the dawn sun is must be overhead at
        // noon, or the sky comes apart from its own sun.
        let east = [-1.0, 0.0, 0.0];
        let same = wheeled(east, turn(0.25));
        for axis in 0..3 {
            assert!(
                (same[axis] - east[axis]).abs() < 1e-6,
                "at 0.25 the frames coincide, got {same:?}"
            );
        }
        let noon = wheeled(east, turn(0.5));
        assert!(
            noon[1] > 0.999,
            "the dawn star at noon points {noon:?}, not up"
        );
        let dusk = wheeled(east, turn(0.75));
        assert!(
            dusk[0] > 0.999,
            "the dawn star at dusk points {dusk:?}, not west"
        );
    }

    #[test]
    fn looking_along_a_star_names_that_star() {
        // The no-desync property, run from the catalog's side: the direction
        // the sky draws star N along, handed back as a gaze, answers N.
        let catalog = star_catalog(7);
        let observer = world_position(7);
        for (index, star) in catalog.iter().enumerate().step_by(97) {
            let time = (index as f32 * 0.037).rem_euclid(1.0);
            let drawn = wheeled(star.direction_from(observer).expect("not here"), turn(time));
            let (id, alignment) = star_in_view(&catalog, observer, drawn, time).expect("a star");
            assert_eq!(
                id, star.id,
                "looking along star {} found star {id}",
                star.id
            );
            assert!(alignment > 0.9999, "alignment {alignment}");
        }
    }

    #[test]
    fn a_gaze_that_is_not_a_direction_finds_nothing() {
        let catalog = star_catalog(7);
        assert_eq!(
            star_in_view(&catalog, UniversalPos::CENTRE, [0.0; 3], 0.5),
            None
        );
        assert_eq!(
            star_in_view(&catalog, UniversalPos::CENTRE, [f32::NAN, 0.0, 0.0], 0.5),
            None
        );
        assert_eq!(
            star_in_view(&[], UniversalPos::CENTRE, [0.0, 1.0, 0.0], 0.5),
            None
        );
    }

    #[test]
    fn the_catalog_uses_its_own_stream() {
        // Charter rule 4's stream-per-purpose rule. Sharing a stream would mean
        // adding any other world-level randomness later silently rearranged the
        // sky of every existing world.
        let stars = StreamRng::global(3, STAR_STREAM).next_u64();
        let place = StreamRng::global(3, WORLD_PLACE_STREAM).next_u64();
        assert_ne!(
            stars, place,
            "two named streams gave the same sequence, so the name is not reaching the seed"
        );
    }
}
