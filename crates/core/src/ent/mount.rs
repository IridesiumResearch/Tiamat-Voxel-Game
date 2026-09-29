// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A player seated on an entity and driving it — Life ask 18.
//!
//! # Why the engine owns it
//!
//! A mount is movement, and a player's movement is the one thing a mod cannot
//! write from outside: their body is stepped from their own inputs and
//! predicted by their own client. Every way a mod could fake a ride — moving
//! the player onto the horse each tick, dragging the horse under the player —
//! fights that prediction and rubber-bands. So the engine seats the player, and
//! while they are seated their keys drive the ENTITY: its pace, its box, and
//! the same [`crate::phys::step_shaped`] every entity already takes.
//!
//! # What both ends must agree on is here
//!
//! The server steps a ridden entity from its rider's intent, and the rider's
//! client predicts that same body with the same function — charter rule 2,
//! one simulation, mirrored. So the rules the two must share are functions in
//! `core` rather than two copies of a habit: the intent a rider's keys become
//! ([`drive`]), the tuning a pace becomes ([`tuning`]), whether this tick's
//! keys get the rider off ([`sneaks_off`]), and where the seat is once the
//! mount has turned ([`seat_offset`]).
//!
//! # Determinism
//!
//! [`seat_offset`] turns a seat by the mount's heading, which is a trig value
//! the simulation genuinely needs: it is where the rider's body is. It reads
//! the committed tables in [`crate::detgen::trig`] and never libm (charter
//! rule 4). Everything else here is a copy or a comparison.

use crate::phys::{Abilities, Gait, Intent, Shape, Tuning};

/// The furthest a seat may be from its mount's feet on any axis, in cells.
///
/// Sixteen blocks, a chunk. A seat is ON the mount; a number past this is a
/// mod's mistake (a seat given in cells where blocks were meant is 3x too far),
/// and it is refused where the mod can see it rather than carrying a rider a
/// chunk away from the thing they are riding.
pub const MAX_SEAT_CELLS: f32 = 48.0;

/// The largest a mount's box may be on either side, in cells — a chunk.
///
/// **A body is swept cell by cell**, so the size of its box is the cost of
/// every step it takes, and a riding player's client steps it too. A server's
/// word for the size is checked against this before a client steps with it
/// (charter rule 14), so the server holds its own mounts to the same bound.
pub const MAX_MOUNT_CELLS: f32 = 48.0;

/// What a mod asks for when it seats somebody.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seat {
    /// Where the rider's feet go, in cells from the mount's feet.
    ///
    /// **Measured as if the mount faced yaw zero** — `+z` ahead of it, `+y`
    /// up — and turned with the mount, so a seat behind the withers stays
    /// behind them as the horse turns. `None` is on top of the mount's box,
    /// over its middle, which is where a mod that names nothing means.
    pub offset: Option<[f32; 3]>,
    /// Whether the sneak key gets the rider off. On unless a mod says not;
    /// with it off, sneak is a gait like any other and drives the mount at a
    /// crawl that will not walk off an edge.
    pub sneak_dismounts: bool,
}

impl Default for Seat {
    fn default() -> Self {
        Self {
            offset: None,
            sneak_dismounts: true,
        }
    }
}

impl Seat {
    /// Where this seat is for a mount of `shape`: the mod's offset, or on top
    /// of the box.
    #[must_use]
    pub fn resolve(&self, shape: Shape) -> [f32; 3] {
        self.offset.unwrap_or([0.0, shape.height, 0.0])
    }
}

/// Whether a seat offset is one the engine will carry a rider at: every axis a
/// number, and within [`MAX_SEAT_CELLS`] of the mount's feet.
#[must_use]
pub fn seat_is_valid(offset: [f32; 3]) -> bool {
    offset
        .iter()
        .all(|axis| axis.is_finite() && axis.abs() <= MAX_SEAT_CELLS)
}

/// Whether a box is one a rider can drive: a real size, and no bigger than
/// [`MAX_MOUNT_CELLS`] on either side.
#[must_use]
pub fn fits(shape: Shape) -> bool {
    let side = |cells: f32| cells.is_finite() && cells > 0.0 && cells <= MAX_MOUNT_CELLS;
    side(shape.width) && side(shape.height)
}

/// Why a mount was refused.
///
/// **None of these is a Lua error**, because every one can happen to a mod
/// that did nothing wrong: two players right-clicking one horse on the same
/// tick, a horse that despawned between the click and the call, a player who
/// left. A Lua error disables the mod that raised it, and a race is not a
/// reason to lose the Life mod. So `game.mount` answers `nil, reason`, which
/// reads as false in a condition — see [`Refusal::reason`] for the words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The player is not connected.
    NotConnected,
    /// The id names nothing live.
    NoSuchEntity,
    /// The entity is a player's body, which their own inputs move.
    Player,
    /// The entity has no box to drive, or one no body can be stepped as.
    NoCollider,
    /// The entity is in another simulation space than the player.
    OtherDomain,
    /// The player is already riding something else.
    AlreadyRiding,
    /// Somebody else is riding it.
    Ridden,
}

impl Refusal {
    /// The word `game.mount` hands back beside its `nil`.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NotConnected => "not connected",
            Self::NoSuchEntity => "no such entity",
            Self::Player => "a player",
            Self::NoCollider => "no collider",
            Self::OtherDomain => "another domain",
            Self::AlreadyRiding => "already riding",
            Self::Ridden => "ridden",
        }
    }
}

/// Why a rider came off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dismount {
    /// They pressed sneak, with a seat that lets it get them off.
    Sneak,
    /// A mod ended it: `game.dismount`, or `game.move_player` moving the
    /// rider somewhere else.
    Asked,
    /// The two parted: the mount was despawned — which is how a mod kills
    /// one — or the mount or the rider was moved into another domain.
    Gone,
    /// The player left the server while riding.
    Left,
}

impl Dismount {
    /// The word an `on_dismount` event carries as `reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sneak => "sneak",
            Self::Asked => "dismount",
            Self::Gone => "gone",
            Self::Left => "leave",
        }
    }
}

/// The intent a rider's keys drive their mount with.
///
/// **Run by both ends**, so a riding client predicts what the server will do
/// rather than what the keys said. Walk, jump and gait go through as they are;
/// flight does not, because a player's permission to fly is theirs and not
/// their horse's. The rider's other abilities stay with the rider for the same
/// reason: the mount moves at its own pace, see [`tuning`].
#[must_use]
pub const fn drive(intent: Intent) -> Intent {
    Intent {
        fly: false,
        ..intent
    }
}

/// The tuning a body with this pace is stepped with.
///
/// **The entity step's own rule**, named so that the server stepping a mount
/// and a client predicting it use one function: `Abilities::tuning` over the
/// entity's `speed`, which returns the base tuning untouched at exactly 1.
#[must_use]
pub fn tuning(speed: f32) -> Tuning {
    Abilities {
        speed,
        ..Abilities::DEFAULT
    }
    .tuning(&Tuning::DEFAULT)
}

/// Whether this tick's keys get the rider off.
///
/// Read from the keys as they arrived, before anything filtered them: sneak is
/// the one gait no ability takes away.
#[must_use]
pub fn sneaks_off(sneak_dismounts: bool, intent: &Intent) -> bool {
    sneak_dismounts && intent.gait == Gait::Sneak
}

/// A seat offset turned to a mount's heading, in cells.
///
/// `yaw` is the mount's own, in radians, counted as a figure counts it: a
/// figure at `yaw` faces `(sin yaw, cos yaw)` — see
/// [`super::component::figure_yaw`]. The seat's `+z` turns to that facing and
/// its `+x` with it, so the whole seat rotates rigidly about the mount's feet;
/// `y` is untouched.
///
/// Through [`crate::detgen::trig`]: this is where the rider's body is, so it
/// has to be the same number on every machine (charter rule 4).
#[must_use]
pub fn seat_offset(offset: [f32; 3], yaw: f32) -> [f32; 3] {
    let [x, y, z] = offset;
    // Nothing to turn, and nothing to round: a seat over the middle of the
    // mount — the common case, and the default — is the same wherever it
    // faces, and skipping the tables keeps it exactly so.
    if x == 0.0 && z == 0.0 {
        return offset;
    }
    let sin = crate::detgen::trig::sin(yaw);
    let cos = crate::detgen::trig::cos(yaw);
    [x * cos + z * sin, y, z * cos - x * sin]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(clippy::float_cmp, reason = "an untouched seat is the same bits")]
    fn a_seat_facing_north_is_where_it_was_asked_for() {
        assert_eq!(seat_offset([1.0, 4.8, -0.9], 0.0), [1.0, 4.8, -0.9]);
    }

    #[test]
    fn a_seat_turns_with_the_mount_it_is_on() {
        // A seat a block ahead of the middle, on a mount turned a quarter:
        // it is still ahead of the mount, which now faces `(sin, cos)` of its
        // yaw — the figure convention, so the rider stays over the withers.
        let quarter = std::f32::consts::FRAC_PI_2;
        let turned = seat_offset([0.0, 4.8, 3.0], quarter);
        let facing = [
            crate::detgen::trig::sin(quarter),
            crate::detgen::trig::cos(quarter),
        ];
        assert!((turned[0] - 3.0 * facing[0]).abs() < 1e-4, "{turned:?}");
        assert!((turned[2] - 3.0 * facing[1]).abs() < 1e-4, "{turned:?}");
        assert!(
            (turned[1] - 4.8).abs() < f32::EPSILON,
            "height turned too: {turned:?}"
        );

        // And rigidly: the distance from the middle is kept at any heading.
        for step in 0..32 {
            let yaw = step as f32 * 0.37 - 5.0;
            let [x, _, z] = seat_offset([1.5, 0.0, -2.0], yaw);
            let length = (x * x + z * z).sqrt();
            assert!(
                (length - 2.5).abs() < 1e-3,
                "at {yaw} the seat is {length} out"
            );
        }
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "an untouched seat is the same bits")]
    fn a_seat_over_the_middle_never_moves_whatever_the_heading() {
        for yaw in [0.0, 1.0, -2.5, 3.0, f32::MAX] {
            assert_eq!(seat_offset([0.0, 4.8, 0.0], yaw), [0.0, 4.8, 0.0]);
        }
    }

    #[test]
    fn a_heading_that_is_not_a_number_does_not_put_one_in_the_seat() {
        // Charter rule 4: the tables answer 0 for a non-finite angle, so the
        // seat is a number whatever a mod wrote into a yaw.
        let seat = seat_offset([1.0, 2.0, 3.0], f32::NAN);
        assert!(seat.iter().all(|axis| axis.is_finite()), "{seat:?}");
    }

    #[test]
    fn a_mount_is_driven_by_everything_but_its_riders_flight() {
        let keys = Intent {
            walk: [0.6, -0.8],
            jump: true,
            gait: Gait::Sprint,
            fly: true,
        };
        assert_eq!(drive(keys), Intent { fly: false, ..keys });
    }

    #[test]
    fn an_ordinary_pace_is_the_ordinary_tuning_bit_for_bit() {
        // The entity step returns the base tuning untouched at exactly one,
        // and a mount must step what an unridden entity steps.
        assert_eq!(tuning(1.0), Tuning::DEFAULT);
        let half = tuning(0.5);
        assert!((half.walk_speed - Tuning::DEFAULT.walk_speed * 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn sneak_gets_off_only_a_seat_that_lets_it() {
        let sneak = Intent {
            gait: Gait::Sneak,
            ..Intent::default()
        };
        assert!(sneaks_off(true, &sneak));
        assert!(!sneaks_off(false, &sneak));
        assert!(!sneaks_off(true, &Intent::default()));
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the values asserted are copied, not computed"
    )]
    fn a_seat_named_nowhere_is_on_top_of_the_box() {
        let horse = Shape {
            width: 4.2,
            height: 4.5,
        };
        assert_eq!(Seat::default().resolve(horse), [0.0, 4.5, 0.0]);
        let named = Seat {
            offset: Some([0.0, 4.8, -0.9]),
            ..Seat::default()
        };
        assert_eq!(named.resolve(horse), [0.0, 4.8, -0.9]);
    }

    #[test]
    fn a_seat_or_a_box_out_of_bounds_is_refused() {
        assert!(seat_is_valid([0.0, MAX_SEAT_CELLS, -MAX_SEAT_CELLS]));
        assert!(!seat_is_valid([0.0, MAX_SEAT_CELLS + 0.5, 0.0]));
        assert!(!seat_is_valid([f32::NAN, 0.0, 0.0]));
        assert!(!seat_is_valid([0.0, f32::INFINITY, 0.0]));

        assert!(fits(Shape::HUMANOID));
        for bad in [0.0, -1.0, MAX_MOUNT_CELLS + 1.0, f32::NAN, f32::INFINITY] {
            assert!(
                !fits(Shape {
                    width: bad,
                    height: 4.0
                }),
                "a box {bad} wide fits"
            );
            assert!(
                !fits(Shape {
                    width: 4.0,
                    height: bad
                }),
                "a box {bad} tall fits"
            );
        }
    }

    #[test]
    fn every_refusal_and_every_way_off_has_a_word() {
        for refusal in [
            Refusal::NotConnected,
            Refusal::NoSuchEntity,
            Refusal::Player,
            Refusal::NoCollider,
            Refusal::OtherDomain,
            Refusal::AlreadyRiding,
            Refusal::Ridden,
        ] {
            assert!(!refusal.reason().is_empty());
        }
        assert_eq!(Dismount::Sneak.as_str(), "sneak");
        assert_eq!(Dismount::Asked.as_str(), "dismount");
        assert_eq!(Dismount::Gone.as_str(), "gone");
        assert_eq!(Dismount::Left.as_str(), "leave");
    }
}
