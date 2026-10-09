// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A flicker on a block's light: the client's side of `flicker` on a material.
//!
//! A campfire's light should not sit still. The light itself is propagated on
//! the server and sent as levels (Sub-Node Contract §3), and recomputing it
//! every frame for a flame would be absurd; so the flicker is presentation
//! only, done here and in `world.wgsl` with the same arithmetic: smooth value
//! noise in time, `rate` steps a second, dipping the block light near a
//! source by up to `depth` of itself. The ground and walls dim in the shader;
//! the fire's own model dims through its figure's light, from [`factor`], so
//! the two breathe together.
//!
//! Charter rule 4 does not reach this: nothing here is simulation.

use tiamat_core::BlockPos;

/// A hash of a step to `0..=1`, the shader's `flicker_hash`: integer bit
/// mixing, so no transcendental — the ban on them (charter rule 4) is a
/// crate-wide lint, and a sine-based hash would also differ between the
/// shader's and this side's `sin`.
fn hash(step: i32) -> f32 {
    #[expect(clippy::cast_sign_loss, reason = "the bits of the step, not its value")]
    let mut n = (step as u32).wrapping_mul(0x9E37_79B1);
    n ^= n >> 15;
    n = n.wrapping_mul(0x85EB_CA6B);
    n ^= n >> 13;
    #[expect(
        clippy::cast_precision_loss,
        reason = "sixteen bits of hash into a unit fraction"
    )]
    {
        (n & 0xFFFF) as f32 / 65535.0
    }
}

/// Smooth value noise in `0..=1` over `t`: a new random level each unit,
/// eased between them, so the light breathes rather than strobes.
#[must_use]
pub fn noise(t: f32) -> f32 {
    let step = tiamat_core::detgen::floor_to_i32(t);
    #[expect(clippy::cast_precision_loss, reason = "a step count, small")]
    let f = t - step as f32;
    let s = f * f * (3.0 - 2.0 * f);
    let (a, b) = (hash(step), hash(step.wrapping_add(1)));
    (a + (b - a) * s).clamp(0.0, 1.0)
}

/// How much of its light a source gives at time `t`: `1.0` in full, down to
/// `1.0 - depth`. `rate` is steps a second, `phase` a per-source offset so
/// two fires beside each other do not breathe as one.
#[must_use]
pub fn factor(t: f32, rate: f32, phase: f32, depth: f32) -> f32 {
    let depth = depth.clamp(0.0, 1.0);
    1.0 - depth * noise(t * rate + phase)
}

/// A per-source phase from where it stands, in `0..100`.
#[must_use]
pub fn phase_of(block: BlockPos) -> f32 {
    let mixed = block
        .x
        .wrapping_mul(73_856_093)
        .wrapping_add(block.y.wrapping_mul(19_349_663))
        .wrapping_add(block.z.wrapping_mul(83_492_791));
    #[expect(
        clippy::cast_precision_loss,
        reason = "a hash reduced to a hundred steps of phase"
    )]
    {
        (mixed.rem_euclid(1000)) as f32 / 10.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_factor_stays_between_full_and_the_depth() {
        for step in 0..2000 {
            let t = step as f32 * 0.0137;
            let k = factor(t, 8.0, 3.0, 0.3);
            assert!((0.7..=1.0).contains(&k), "at {t}: {k}");
        }
        assert!(
            (factor(5.0, 8.0, 0.0, 0.0) - 1.0).abs() < f32::EPSILON,
            "no depth, no flicker"
        );
    }

    #[test]
    fn the_noise_breathes_rather_than_strobes() {
        // Two samples a frame apart at eight steps a second differ by a
        // little, never by the whole range.
        let mut worst = 0.0f32;
        for step in 0..5000 {
            let t = step as f32 / 60.0;
            let jump = (noise(t * 8.0) - noise((t + 1.0 / 60.0) * 8.0)).abs();
            worst = worst.max(jump);
        }
        assert!(worst < 0.35, "a frame's jump reached {worst}");
    }

    #[test]
    fn neighbours_have_their_own_phase() {
        let a = phase_of(BlockPos::new(2, 1, 1));
        let b = phase_of(BlockPos::new(3, 1, 1));
        assert!((a - b).abs() > f32::EPSILON, "{a} against {b}");
        assert!((0.0..100.0).contains(&a) && (0.0..100.0).contains(&b));
    }
}
