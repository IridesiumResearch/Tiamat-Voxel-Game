// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A body that lies down to go under something (Life ask 23).
//!
//! Sub-Node Contract §2, "A body may crawl": the box is the player's own
//! width at [`CRAWL_HEIGHT`](super::CRAWL_HEIGHT), taken when the space
//! ahead or overhead is too low to stand in and given back where there is
//! room — never into a ceiling. The decision is a pure function of the body,
//! the inputs and the cells around it, so the client predicts it from the
//! same arguments the server decides it from (charter rule 2: one simulation
//! code path) and nobody rubber-bands at the mouth of a tunnel.
//!
//! Nothing here crawls without a mod's grant — `crawl = true` on
//! `set_player_abilities` — so a world whose mods never ask steps exactly as
//! it did.

use super::{Body, Gait, Intent, Shape, Solid};

/// How far ahead the gap is looked for, in cells, along the way the body is
/// being pushed.
///
/// One cell: the box already reaches 0.9 past the feet, so the probe's front
/// face is 1.9 cells out — the next cell and not the one after it, so a
/// player sneaking toward a low opening lies down one stride before it and
/// not from across the room.
const PROBE: f32 = 1.0;

/// Below this push the body is not going anywhere, so there is no "ahead".
const STILL: f32 = 1e-3;

/// Whether a body crawls this tick.
///
/// `crawling` is what it did last tick; `allowed` is the mod's grant.
///
/// - **Standing**, a body lies down when the standing box is blocked where it
///   is (a ceiling closed over it) and the low one is not, or when it is
///   sneaking toward a gap the standing box cannot enter and the low one can —
///   a gap the step-up would not clear either, so a one-cell lip is still a
///   step and never a crawl.
/// - **Crawling**, it stands when the standing box is clear where it is,
///   unless it is still sneaking into that gap: the body lies down a stride
///   before the opening, and standing back up there would be a bob at every
///   mouth.
/// - **Flying**, it never lies down, and stands only where there is room.
///
/// Deterministic (charter rule 4): a square root, comparisons and cell
/// lookups, in a fixed order.
#[must_use]
pub fn posture(
    solid: &impl Solid,
    body: &Body,
    intent: Intent,
    crawling: bool,
    allowed: bool,
) -> bool {
    if !allowed {
        return false;
    }
    let feet = body.position;
    let standing_blocked = solid.overlaps(&Shape::HUMANOID.aabb(feet));
    if intent.fly {
        return crawling && standing_blocked;
    }
    let gap_ahead = sneaking_at_a_gap(solid, feet, intent);
    if crawling {
        standing_blocked || gap_ahead
    } else {
        (standing_blocked && !solid.overlaps(&Shape::CRAWL.aabb(feet))) || gap_ahead
    }
}

/// Whether the body is being sneaked toward an opening only the low box fits:
/// the standing box one stride ahead is blocked, so is the same box one cell
/// up (the step-up's retry, Contract §2), and the low box there is clear.
fn sneaking_at_a_gap(solid: &impl Solid, feet: [f32; 3], intent: Intent) -> bool {
    if intent.gait != Gait::Sneak {
        return false;
    }
    let [wx, wz] = intent.walk;
    let push = (wx * wx + wz * wz).sqrt();
    if push <= STILL {
        return false;
    }
    let ahead = [
        feet[0] + wx / push * PROBE,
        feet[1],
        feet[2] + wz / push * PROBE,
    ];
    let stepped = [ahead[0], ahead[1] + 1.0, ahead[2]];
    solid.overlaps(&Shape::HUMANOID.aabb(ahead))
        && solid.overlaps(&Shape::HUMANOID.aabb(stepped))
        && !solid.overlaps(&Shape::CRAWL.aabb(ahead))
}

/// The inputs a crawling body is stepped with: the sneak's pace, and no jump —
/// there is nothing to jump into, and the key would otherwise spend the
/// cooldown against the ceiling.
#[must_use]
pub fn crawling_intent(intent: Intent) -> Intent {
    Intent {
        gait: Gait::Sneak,
        jump: false,
        ..intent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A floor under `y = 0` and a slab of ceiling over a stretch of `z`,
    /// whose underside is `gap` cells above the floor.
    struct Tunnel {
        from_z: i32,
        to_z: i32,
        gap: i32,
    }

    impl Solid for Tunnel {
        fn solid(&self, _x: i32, y: i32, z: i32) -> bool {
            y < 0 || (y >= self.gap && y < self.gap + 3 && z >= self.from_z && z < self.to_z)
        }
    }

    /// One block high, from cell 12 to cell 24.
    fn one_block_tunnel() -> Tunnel {
        Tunnel {
            from_z: 12,
            to_z: 24,
            gap: 3,
        }
    }

    fn body_at_z(z: f32) -> Body {
        Body {
            on_ground: true,
            ..Body::at([24.0, 0.0, z])
        }
    }

    fn sneaking_forward() -> Intent {
        Intent {
            walk: [0.0, 1.0],
            jump: false,
            gait: Gait::Sneak,
            fly: false,
        }
    }

    fn walking_forward() -> Intent {
        Intent {
            gait: Gait::Walk,
            ..sneaking_forward()
        }
    }

    fn standing_still() -> Intent {
        Intent {
            walk: [0.0, 0.0],
            ..sneaking_forward()
        }
    }

    #[test]
    fn nothing_crawls_without_the_grant() {
        let tunnel = one_block_tunnel();
        assert!(!posture(
            &tunnel,
            &body_at_z(10.5),
            sneaking_forward(),
            false,
            false
        ));
        // Not even a body already under the ceiling, if the grant went away.
        assert!(!posture(
            &tunnel,
            &body_at_z(16.0),
            standing_still(),
            true,
            false
        ));
    }

    #[test]
    fn sneaking_at_a_one_block_gap_lies_down_a_stride_before_it() {
        let tunnel = one_block_tunnel();
        // The box's front face at 11.4: the probe reaches into cell 12.
        assert!(posture(
            &tunnel,
            &body_at_z(10.5),
            sneaking_forward(),
            false,
            true
        ));
        // From across the room, nothing.
        assert!(!posture(
            &tunnel,
            &body_at_z(6.0),
            sneaking_forward(),
            false,
            true
        ));
        // Walking at it, nothing: a crawl is asked for by sneaking.
        assert!(!posture(
            &tunnel,
            &body_at_z(10.5),
            walking_forward(),
            false,
            true
        ));
        // Standing still beside it, nothing.
        assert!(!posture(
            &tunnel,
            &body_at_z(10.5),
            standing_still(),
            false,
            true
        ));
        // Sneaking away from it, nothing.
        let away = Intent {
            walk: [0.0, -1.0],
            ..sneaking_forward()
        };
        assert!(!posture(&tunnel, &body_at_z(10.5), away, false, true));
    }

    #[test]
    fn a_two_cell_gap_is_not_entered() {
        let low = Tunnel {
            gap: 2,
            ..one_block_tunnel()
        };
        assert!(!posture(
            &low,
            &body_at_z(10.5),
            sneaking_forward(),
            false,
            true
        ));
    }

    #[test]
    fn a_one_cell_lip_is_a_step_and_not_a_crawl() {
        struct Lip;
        impl Solid for Lip {
            fn solid(&self, _x: i32, y: i32, z: i32) -> bool {
                y < 0 || (y == 0 && z == 12)
            }
        }
        assert!(!posture(
            &Lip,
            &body_at_z(10.5),
            sneaking_forward(),
            false,
            true
        ));
    }

    #[test]
    fn a_crawling_body_keeps_lying_while_it_sneaks_into_the_gap() {
        let tunnel = one_block_tunnel();
        assert!(posture(
            &tunnel,
            &body_at_z(10.5),
            sneaking_forward(),
            true,
            true
        ));
    }

    #[test]
    fn a_crawling_body_stays_down_under_the_ceiling_whatever_the_keys_say() {
        let tunnel = one_block_tunnel();
        for intent in [standing_still(), walking_forward(), sneaking_forward()] {
            assert!(posture(&tunnel, &body_at_z(16.0), intent, true, true));
        }
        let sprinting = Intent {
            gait: Gait::Sprint,
            jump: true,
            ..walking_forward()
        };
        assert!(posture(&tunnel, &body_at_z(16.0), sprinting, true, true));
    }

    #[test]
    fn a_crawling_body_stands_where_there_is_room() {
        let tunnel = one_block_tunnel();
        // Out the far side: the box's back face is past the slab.
        assert!(!posture(
            &tunnel,
            &body_at_z(25.0),
            walking_forward(),
            true,
            true
        ));
        assert!(!posture(
            &tunnel,
            &body_at_z(25.0),
            standing_still(),
            true,
            true
        ));
        // Backed out of the mouth, no longer sneaking at it.
        assert!(!posture(
            &tunnel,
            &body_at_z(10.5),
            standing_still(),
            true,
            true
        ));
    }

    #[test]
    fn a_ceiling_closing_over_a_standing_body_lies_it_down() {
        let tunnel = one_block_tunnel();
        assert!(posture(
            &tunnel,
            &body_at_z(16.0),
            standing_still(),
            false,
            true
        ));
    }

    #[test]
    fn a_body_buried_outright_is_left_standing() {
        struct Rock;
        impl Solid for Rock {
            fn solid(&self, _x: i32, _y: i32, _z: i32) -> bool {
                true
            }
        }
        assert!(!posture(
            &Rock,
            &body_at_z(16.0),
            standing_still(),
            false,
            true
        ));
    }

    #[test]
    fn a_flying_body_never_lies_down_and_never_stands_into_a_ceiling() {
        let tunnel = one_block_tunnel();
        let flying = Intent {
            fly: true,
            ..sneaking_forward()
        };
        assert!(!posture(&tunnel, &body_at_z(10.5), flying, false, true));
        assert!(posture(&tunnel, &body_at_z(16.0), flying, true, true));
        assert!(!posture(&tunnel, &body_at_z(25.0), flying, true, true));
    }

    #[test]
    fn crawling_intent_is_the_sneaks_pace_without_a_jump() {
        let asked = Intent {
            walk: [0.3, -0.7],
            jump: true,
            gait: Gait::Sprint,
            fly: false,
        };
        let stepped = crawling_intent(asked);
        assert_eq!(stepped.gait, Gait::Sneak);
        assert!(!stepped.jump);
        assert_eq!(stepped.walk.map(f32::to_bits), asked.walk.map(f32::to_bits));
        assert!(!stepped.fly);
    }

    #[test]
    fn the_low_box_is_the_standing_footprint_under_a_block() {
        assert_eq!(
            Shape::CRAWL.width.to_bits(),
            Shape::HUMANOID.width.to_bits()
        );
        assert_eq!(Shape::of_player(true), Shape::CRAWL);
        assert_eq!(Shape::of_player(false), Shape::HUMANOID);
        let body = Body::at([1.0, 2.0, 3.0]);
        assert!(body.eye_when(true)[1] < body.eye()[1]);
        assert!(body.eye_when(true)[1] < 2.0 + Shape::CRAWL.height);
    }
}
