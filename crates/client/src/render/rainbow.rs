// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A rainbow, painted on the sky — weather ask W30.
//!
//! # A ring round the point opposite the sun
//!
//! A rainbow is fixed to the sun, not to the world. The primary bow is a ring
//! [`PRIMARY_RADIUS`] degrees round the ANTISOLAR point — straight away from
//! the sun, which is the way its light travels — red outside and violet
//! inside; the faint secondary is a ring [`SECONDARY_RADIUS`] degrees out
//! with the colours the other way round. Since the antisolar point is as far
//! under the horizon as the sun is over it, the bow sinks as the sun climbs,
//! and at 42 degrees it is gone; so it fades out as the sun climbs towards
//! there, and it is hidden with the sun down. Only the half over the horizon
//! is ever drawn, as in life.
//!
//! A mod says only how strong (`game.set_rainbow`); the client eases that
//! ([`crate::sky::EasedRainbow`]) and this module turns it and the frame's
//! sun into the few numbers the sky's shaders draw it from.
//!
//! # Where it is drawn
//!
//! **At the sky's depth**, as the stars are: added to the sky where the cloud
//! pass paints it, so terrain in front of it and the cloud deck in front of
//! it hide it by being in front of it. Modes 1 and 2 add it in `clouds.wgsl`
//! beside the stars, at the frame's own resolution — in the resolve, when the
//! deck is marched smaller, so the bands are not a staircase of blocks. Mode 3
//! fogs the sky from depth in its post chain, and a painted sky pixel comes
//! out as the fog's colour, so there it is added in `post.wgsl` after the
//! fog, on the pixels at the sky's depth that are not cloud. Additive and
//! low, in both: it brightens the sky and never covers it.
//!
//! # Presentation
//!
//! Charter rule 4 exempts rendering, so the angles here are libm's; none of
//! it reaches the tick or the hash gate. [`light_along`] is the shaders'
//! `rainbow_along`, written once more in Rust, which is what the tests hold
//! to the ask's numbers: the shaders and it must say the same thing.

/// Degrees from the antisolar point to the middle of the primary bow.
pub const PRIMARY_RADIUS: f32 = 42.0;

/// Degrees from the antisolar point to the middle of the secondary bow.
pub const SECONDARY_RADIUS: f32 = 51.0;

/// Half the primary bow's width, in degrees: violet at 40.5, red at 43.5.
///
/// A few degrees across, as asked — a little more than the two a real bow's
/// colours spread over, which at a few hundred pixels a side would be a
/// thread.
pub const PRIMARY_HALF_WIDTH: f32 = 1.5;

/// Half the secondary bow's width, in degrees: red at 49, violet at 53.
/// Wider than the primary, as a real one is.
pub const SECONDARY_HALF_WIDTH: f32 = 2.0;

/// The most the primary bow adds to the sky, at intensity 1: low, so the sky
/// shows through every band of it.
pub const BRIGHTNESS: f32 = 0.25;

/// The secondary's share of the primary's brightness: faint, as asked.
pub const SECONDARY_SHARE: f32 = 0.4;

/// The sun's elevation, in degrees, over which the bow fades in as it rises:
/// hidden with the sun down, and never a pop at sunrise.
pub const RISE: f32 = 3.0;

/// The sun's elevation, in degrees, from which the bow fades out as it
/// climbs; it is gone at [`PRIMARY_RADIUS`], where the bow's top sinks under
/// the horizon.
pub const FADE_FROM: f32 = 32.0;

/// How far over the horizon, as the sine of the angle, the bow takes to
/// fade in: about two degrees, so the horizon is a soft edge and not a cut.
pub const HORIZON_EDGE: f32 = 0.035;

/// The rainbow as the sky's shaders read it: `Rainbow` in `clouds.wgsl` and
/// in `post.wgsl`, field for field, appended to each pass's uniform.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniform {
    /// The antisolar point, normalised, in `xyz`; the primary bow's
    /// strength in `w`, zero for none.
    pub antisolar: [f32; 4],
    /// The primary's radius and half-width, then the secondary's, in
    /// radians.
    pub bands: [f32; 4],
    /// The secondary bow's strength in `x`; [`HORIZON_EDGE`] in `y`; two
    /// spare.
    pub light: [f32; 4],
}

impl Uniform {
    /// No rainbow: a strength of zero, which the shaders skip at once.
    pub const NONE: Self = Self {
        antisolar: [0.0, 1.0, 0.0, 0.0],
        bands: [0.0; 4],
        light: [0.0; 4],
    };

    /// The rainbow for a strength a mod asked for, `0.0..=1.0`, and the sun
    /// the frame is drawn with: round the point opposite it, and as strong
    /// as the sun's elevation allows.
    #[must_use]
    pub fn new(intensity: f32, sun_direction: [f32; 3]) -> Self {
        let intensity = if intensity.is_finite() {
            intensity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let strength = intensity * sun_share(sun_direction) * BRIGHTNESS;
        let Some(antisolar) = antisolar(sun_direction) else {
            return Self::NONE;
        };
        if strength <= 0.0 {
            return Self::NONE;
        }
        Self {
            antisolar: [antisolar[0], antisolar[1], antisolar[2], strength],
            bands: [
                PRIMARY_RADIUS.to_radians(),
                PRIMARY_HALF_WIDTH.to_radians(),
                SECONDARY_RADIUS.to_radians(),
                SECONDARY_HALF_WIDTH.to_radians(),
            ],
            light: [strength * SECONDARY_SHARE, HORIZON_EDGE, 0.0, 0.0],
        }
    }

    /// Whether anything is drawn.
    #[must_use]
    pub fn shows(&self) -> bool {
        self.antisolar[3] > 0.0
    }
}

/// The point opposite the sun: the way its light travels, normalised, or
/// `None` for a direction too short to have one.
#[must_use]
pub fn antisolar(sun_direction: [f32; 3]) -> Option<[f32; 3]> {
    let [x, y, z] = sun_direction;
    let length = (x * x + y * y + z * z).sqrt();
    if !length.is_finite() || length < f32::EPSILON {
        return None;
    }
    Some([x / length, y / length, z / length])
}

/// How far over the horizon the sun stands, in degrees: negative under it.
#[expect(
    clippy::disallowed_methods,
    reason = "charter rule 4 exempts rendering from the deterministic float subset; where a \
              rainbow is drawn never reaches the tick or the hash gate"
)]
#[must_use]
pub fn sun_elevation(sun_direction: [f32; 3]) -> f32 {
    // The light travels away from the sun, so the sun is up where the light
    // comes down.
    antisolar(sun_direction).map_or(-90.0, |travel| {
        (-travel[1]).clamp(-1.0, 1.0).asin().to_degrees()
    })
}

/// How much of the bow the sun allows, `0.0..=1.0`: none with the sun down,
/// rising over the first [`RISE`] degrees, all of it until [`FADE_FROM`], and
/// none again by [`PRIMARY_RADIUS`], where the bow has sunk under the
/// horizon.
#[must_use]
pub fn sun_share(sun_direction: [f32; 3]) -> f32 {
    let elevation = sun_elevation(sun_direction);
    let rise = smoothstep(0.0, RISE, elevation);
    let set = 1.0 - smoothstep(FADE_FROM, PRIMARY_RADIUS, elevation);
    rise * set
}

/// One bow's colour at `s` across it — 0 at its inner edge, 1 at its outer —
/// from violet through blue, green, yellow and orange to red, faded at both
/// edges so the bow has no hard rim. Zero outside it. `rainbow_band` in the
/// shaders.
#[must_use]
pub fn band(s: f32) -> [f32; 3] {
    if s <= 0.0 || s >= 1.0 {
        return [0.0; 3];
    }
    let red = bump((s - 0.82) / 0.32) + 0.3 * bump(s / 0.18);
    let green = bump((s - 0.5) / 0.3);
    let blue = bump((s - 0.18) / 0.3);
    let edge = smoothstep(0.0, 0.2, s) * (1.0 - smoothstep(0.8, 1.0, s));
    [red * edge, green * edge, blue * edge]
}

/// What the rainbow adds to the sky along one view direction — the shaders'
/// `rainbow_along`, in Rust: both bows, the primary red outside and the
/// secondary red inside, over the horizon only.
#[expect(
    clippy::disallowed_methods,
    reason = "charter rule 4 exempts rendering from the deterministic float subset; where a \
              rainbow is drawn never reaches the tick or the hash gate"
)]
#[must_use]
pub fn light_along(uniform: &Uniform, direction: [f32; 3]) -> [f32; 3] {
    let strength = uniform.antisolar[3];
    if strength <= 0.0 {
        return [0.0; 3];
    }
    let [ax, ay, az, _] = uniform.antisolar;
    let along = (direction[0] * ax + direction[1] * ay + direction[2] * az).clamp(-1.0, 1.0);
    let angle = along.acos();
    let [primary, primary_half, secondary, secondary_half] = uniform.bands;
    let inner = band((angle - (primary - primary_half)) / (2.0 * primary_half));
    let outer = band(1.0 - (angle - (secondary - secondary_half)) / (2.0 * secondary_half));
    let above = smoothstep(0.0, uniform.light[1], direction[1]);
    let faint = uniform.light[0];
    [
        (inner[0] * strength + outer[0] * faint) * above,
        (inner[1] * strength + outer[1] * faint) * above,
        (inner[2] * strength + outer[2] * faint) * above,
    ]
}

/// `max(0, 1 - x²)`: a smooth hump one wide either side of zero, which is
/// what each of the spectrum's three channels is.
fn bump(x: f32) -> f32 {
    (1.0 - x * x).max(0.0)
}

/// WGSL's `smoothstep`, for the mirror.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sunlight from a sun `elevation` degrees up, due south (at -z), so the
    /// light travels north and down.
    #[expect(
        clippy::disallowed_methods,
        reason = "a test of presentation code, which charter rule 4 exempts"
    )]
    fn sun_at(elevation: f32) -> [f32; 3] {
        let e = elevation.to_radians();
        [0.0, -e.sin(), e.cos()]
    }

    /// A direction `angle` degrees above the antisolar point, straight up the
    /// vertical through it: the top of a ring of that radius.
    #[expect(
        clippy::disallowed_methods,
        reason = "a test of presentation code, which charter rule 4 exempts"
    )]
    fn above_antisolar(sun: f32, angle: f32) -> [f32; 3] {
        let elevation = (angle - sun).to_radians();
        [0.0, elevation.sin(), elevation.cos()]
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    #[test]
    fn the_antisolar_point_is_opposite_the_sun_and_as_far_under_the_horizon() {
        // The bow's centre is where the sunlight goes: straight away from
        // the sun, as far below the horizon as the sun is above it.
        for elevation in [0.0, 15.0, 30.0, 60.0] {
            let sun = sun_at(elevation);
            let point = antisolar([sun[0] * 3.0, sun[1] * 3.0, sun[2] * 3.0]).expect("a point");
            let toward_sun = [-sun[0], -sun[1], -sun[2]];
            assert!(
                (dot(point, toward_sun) + 1.0).abs() < 1e-5,
                "at {elevation} the antisolar point is not opposite the sun: {point:?}"
            );
            assert!((dot(point, point) - 1.0).abs() < 1e-5, "not normalised");
            assert!(
                (sun_elevation(sun) - elevation).abs() < 1e-3,
                "the sun at {elevation} read as {}",
                sun_elevation(sun)
            );
        }
        assert_eq!(antisolar([0.0; 3]), None, "no sun, no point");
        assert_eq!(antisolar([f32::NAN, 0.0, 1.0]), None);
        assert_eq!(Uniform::new(1.0, [0.0; 3]), Uniform::NONE);
    }

    #[test]
    fn the_bows_are_forty_two_and_fifty_one_degrees_out_red_outside_then_inside() {
        // The ask's rings: the primary's brightest light 42 degrees from the
        // antisolar point with red outermost; the secondary's at 51 with the
        // colours the other way round; and nothing between, inside or beyond
        // them.
        let sun = 15.0;
        let uniform = Uniform::new(1.0, sun_at(sun));
        assert!(uniform.shows());
        assert!((uniform.bands[0].to_degrees() - 42.0).abs() < 1e-4);
        assert!((uniform.bands[2].to_degrees() - 51.0).abs() < 1e-4);

        // Walk out from the antisolar point in tenths of a degree, up the
        // vertical through it, and find where each channel peaks.
        let mut peaks = [(0.0_f32, 0.0_f32); 3];
        let mut lit = Vec::new();
        for tenth in 0..900_u16 {
            let angle = f32::from(tenth) / 10.0;
            let light = light_along(&uniform, above_antisolar(sun, angle));
            for channel in 0..3 {
                if light[channel] > peaks[channel].1 {
                    peaks[channel] = (angle, light[channel]);
                }
            }
            if light.iter().any(|&value| value > 1e-4) {
                lit.push(angle);
            }
        }
        // Everything lit is inside one of the two bands.
        for angle in &lit {
            assert!(
                (40.5..=43.5).contains(angle) || (49.0..=53.0).contains(angle),
                "light {angle} degrees out, in neither bow"
            );
        }
        assert!(lit.iter().any(|angle| (41.5..42.5).contains(angle)));
        assert!(lit.iter().any(|angle| (50.5..51.5).contains(angle)));

        // Each channel's own light in the primary band: red furthest out,
        // blue furthest in, and the band's middle at 42.
        let centroid = |channel: usize, from: u16, to: u16| {
            let (mut weight, mut sum) = (0.0, 0.0);
            for tenth in from * 10..=to * 10 {
                let angle = f32::from(tenth) / 10.0;
                let value = light_along(&uniform, above_antisolar(sun, angle))[channel];
                weight += value;
                sum += value * angle;
            }
            sum / weight
        };
        let (red, green, blue) = (
            centroid(0, 40, 44),
            centroid(1, 40, 44),
            centroid(2, 40, 44),
        );
        assert!(
            red > green && green > blue,
            "the primary is not red outside and violet inside: red {red}, green {green}, \
             blue {blue}"
        );
        assert!(
            (green - 42.0).abs() < 0.5,
            "the primary's middle is at {green}"
        );
        let (red, green, blue) = (
            centroid(0, 48, 54),
            centroid(1, 48, 54),
            centroid(2, 48, 54),
        );
        assert!(
            red < green && green < blue,
            "the secondary is not red inside: red {red}, green {green}, blue {blue}"
        );
        assert!(
            (green - 51.0).abs() < 0.5,
            "the secondary's middle is at {green}"
        );

        // And the outermost light of the primary is red, the innermost blue.
        let outer = light_along(&uniform, above_antisolar(sun, 43.2));
        let inner = light_along(&uniform, above_antisolar(sun, 40.8));
        assert!(outer[0] > outer[1] && outer[0] > outer[2], "{outer:?}");
        assert!(inner[2] > inner[0] && inner[2] > inner[1], "{inner:?}");

        // Faint: the secondary's brightest is well under the primary's.
        let primary = light_along(&uniform, above_antisolar(sun, 42.0))[1];
        let secondary = light_along(&uniform, above_antisolar(sun, 51.0))[1];
        assert!(secondary < primary * 0.6, "{secondary} against {primary}");
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "none is exactly none")]
    fn only_the_half_over_the_horizon_is_drawn() {
        // With the sun low the ring crosses the horizon: the part under it
        // is not drawn, and the part over it is.
        let sun = 5.0;
        let uniform = Uniform::new(1.0, sun_at(sun));
        #[expect(
            clippy::disallowed_methods,
            reason = "a test of presentation code, which charter rule 4 exempts"
        )]
        let on_ring = |azimuth: f32| {
            // A point 42 degrees from the antisolar point, turned `azimuth`
            // about it from straight up.
            let (a, r) = (azimuth.to_radians(), 42.0_f32.to_radians());
            let point = antisolar(sun_at(sun)).expect("a point");
            let up = [0.0, sun_at(sun)[2], -sun_at(sun)[1]];
            let side = [1.0, 0.0, 0.0];
            let mut direction = [0.0; 3];
            for axis in 0..3 {
                direction[axis] =
                    point[axis] * r.cos() + (up[axis] * a.cos() + side[axis] * a.sin()) * r.sin();
            }
            direction
        };
        for azimuth in [0.0_f32, 30.0, 60.0, 80.0, 100.0, 140.0, 180.0] {
            let direction = on_ring(azimuth);
            let light = light_along(&uniform, direction);
            if direction[1] > HORIZON_EDGE {
                assert!(light[1] > 0.01, "{azimuth}: over the horizon and dark");
            }
            if direction[1] < 0.0 {
                assert_eq!(light, [0.0; 3], "{azimuth}: drawn under the horizon");
            }
        }
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "none is exactly none")]
    fn the_bow_fades_as_the_sun_climbs_and_is_hidden_with_the_sun_down() {
        // All of it with the sun low, as the gate's fifteen degrees is; less
        // as it climbs past FADE_FROM; none by 42, where the bow's top has
        // sunk under the horizon, nor at the gate's 45; none with the sun on
        // or under the horizon, which is night.
        assert!((sun_share(sun_at(15.0)) - 1.0).abs() < 1e-6);
        assert!((sun_share(sun_at(FADE_FROM)) - 1.0).abs() < 1e-6);
        let mut last = 1.0;
        for elevation in [34.0, 36.0, 38.0, 40.0, 41.5] {
            let share = sun_share(sun_at(elevation));
            assert!(
                share < last && share > 0.0,
                "at {elevation} the bow is {share}, after {last}"
            );
            last = share;
        }
        for elevation in [42.0, 45.0, 60.0, 90.0, 0.0, -1.0, -20.0, -90.0] {
            assert_eq!(sun_share(sun_at(elevation)), 0.0, "at {elevation}");
            assert_eq!(
                Uniform::new(1.0, sun_at(elevation)),
                Uniform::NONE,
                "at {elevation} there is a bow to draw"
            );
        }
        // Rising, not popping, over the first degrees of the day.
        let dawn = sun_share(sun_at(1.0));
        assert!(dawn > 0.0 && dawn < 0.5, "{dawn}");

        // And the strength scales it: none at nothing, half at a half, and
        // a wrong one clamped.
        assert!(!Uniform::new(0.0, sun_at(15.0)).shows());
        let half = Uniform::new(0.5, sun_at(15.0));
        let full = Uniform::new(1.0, sun_at(15.0));
        assert!((half.antisolar[3] * 2.0 - full.antisolar[3]).abs() < 1e-6);
        assert_eq!(Uniform::new(9.0, sun_at(15.0)), full);
        assert_eq!(Uniform::new(f32::NAN, sun_at(15.0)), Uniform::NONE);
    }
}
