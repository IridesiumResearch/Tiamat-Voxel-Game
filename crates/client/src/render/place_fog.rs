// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A place's own fog: `game.register_chunk_fog`, drawn.
//!
//! # The model, in one paragraph
//!
//! A mod says, per chunk column, what colour the air is, how far a player sees
//! into it, and optionally the height it lies under. That becomes a density
//! `σ = 3 / visibility` per block — so at `visibility` blocks the fog hides
//! 95% of what is behind it — full below `top` and thinning exponentially over
//! [`FALLOFF`] blocks above it, which is ground fog: thick in the valley, clear
//! on the hill. How much of a surface survives is `exp(-τ)`, with `τ` the
//! density integrated along the eye's ray.
//!
//! # Why the integral is closed-form and not marched
//!
//! The vertical profile integrates exactly (see [`height_integral`]), so the
//! only approximation is horizontal: the density along the ray is taken as the
//! mean of the two ENDS — the column the camera is in and the column the
//! fragment is in. That is what makes a fog visible from both sides: standing
//! in a rainforest the near trees fade, and standing outside it the forest
//! still reads as misty, because the far end of every ray into it is foggy. A
//! march would be more exact across a boundary and would cost several lookups
//! a pixel for a difference nobody sees in something this soft. Presentation
//! only, so charter rule 4 has nothing to say about the approximation.
//!
//! # Why a grid in a storage buffer, and not the chunk instance
//!
//! The biome colour rides on each chunk's instance because only terrain reads
//! it. Fog is read by terrain, sprites, fluid, glass and — in mode 3 — by the
//! post pass, which has no instances at all, only a depth buffer. So the field
//! lives in one small grid of chunk columns centred on the camera, and every
//! pass that knows a fragment's camera-relative position samples it the same
//! way. It is rebuilt only when a fog arrives or the camera changes chunk.
//!
//! # Premultiplied, which is what makes filtering it right
//!
//! Each cell holds colour × density and top × density rather than the colour
//! and the top. A column with no fog has density zero and no meaningful colour,
//! and blending an unpremultiplied colour with it would drag the fog towards
//! whatever that meaningless colour was — a dark rim round every foggy biome.
//! Premultiplied, the blend divides the density back out and a clear column
//! contributes nothing but thinning.

#![expect(
    clippy::disallowed_methods,
    reason = "charter rule 4 exempts rendering from the deterministic float subset; a place's fog \
              is drawn on the machine that displays it and never reaches the tick or the hash gate"
)]

use std::collections::BTreeMap;

use tiamat_core::proto::ChunkFog;

use crate::camera::Camera;

use super::Gpu;

/// Chunk columns along each side of the grid.
///
/// 128 covers the widest horizon a player can ask for (`lod::MAX_HORIZON` is
/// 32 chunks out, so 64 across) with room either side; beyond the grid a
/// fragment reads the nearest edge cell, which is the fog of the land at the
/// edge of what anybody could see.
pub const GRID: usize = 128;

/// How many blocks above its `top` a fog thins by a factor of `e`.
///
/// A few blocks: thick enough at the top to read as a layer, gone by the time
/// a player has climbed a hill out of it.
pub const FALLOFF: f32 = 4.0;

/// The `top` a fog with no top is given: far enough above anything that the
/// height term is one everywhere a player can stand.
///
/// A number rather than a flag because it has to blend: at the edge of a
/// fog with a top and one without, the grid interpolates between the two, and
/// an infinity would swallow the other side.
pub const NO_TOP: f32 = 8000.0;

/// The density that hides 95% of a surface at `visibility` blocks.
///
/// `exp(-3)` is 0.0498, which is where "you cannot make it out" is.
#[must_use]
pub fn density(visibility: u16) -> f32 {
    3.0 / f32::from(visibility.max(1))
}

/// The antiderivative of the height profile, for [`mean_height_term`].
///
/// The profile is 1 at and below `top` and `exp(-(y - top) / FALLOFF)` above
/// it, so this is `y - top` below and `FALLOFF · (1 - exp(-(y - top) /
/// FALLOFF))` above — the two meet at zero, which is what makes the mean across
/// the boundary exact rather than piecewise-approximate.
#[must_use]
pub fn height_integral(y: f32, top: f32) -> f32 {
    let above = y - top;
    if above <= 0.0 {
        above
    } else {
        FALLOFF * (1.0 - (-above / FALLOFF).exp())
    }
}

/// The mean of the height profile along a ray from height `from` to `to`.
///
/// Level rays take the profile at their height, because the difference
/// quotient below divides by the climb.
#[must_use]
pub fn mean_height_term(from: f32, to: f32, top: f32) -> f32 {
    let climb = to - from;
    if climb.abs() < 0.01 {
        let above = (from - top).max(0.0);
        return (-above / FALLOFF).exp();
    }
    (height_integral(to, top) - height_integral(from, top)) / climb
}

/// A fog as one grid cell holds it: premultiplied, top relative to `reference`.
///
/// `[r·σ, g·σ, b·σ, σ]` and `[top·σ, 0, 0, 0]`; a clear column is all zeros.
#[must_use]
pub fn cell(fog: Option<&ChunkFog>, reference: i32) -> [[f32; 4]; 2] {
    let Some(fog) = fog else {
        return [[0.0; 4]; 2];
    };
    let sigma = density(fog.visibility);
    let colour = fog.colour.map(|channel| f32::from(channel) / 255.0);
    let top = fog.top.map_or(NO_TOP, |top| {
        ((top - reference) as f32).clamp(-NO_TOP, NO_TOP)
    });
    [
        [
            colour[0] * sigma,
            colour[1] * sigma,
            colour[2] * sigma,
            sigma,
        ],
        [top * sigma, 0.0, 0.0, 0.0],
    ]
}

/// A fog as a place has it once the grid has been filtered: unpremultiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// The fog's colour in daylight.
    pub colour: [f32; 3],
    /// Its density, per block.
    pub density: f32,
    /// The height it lies under, relative to the grid's reference height.
    pub top: f32,
}

impl Sample {
    /// Clear air.
    pub const CLEAR: Self = Self {
        colour: [1.0; 3],
        density: 0.0,
        top: NO_TOP,
    };

    fn from_premultiplied(cell: [[f32; 4]; 2]) -> Self {
        let density = cell[0][3];
        if density <= 1e-6 {
            return Self::CLEAR;
        }
        Self {
            colour: [
                cell[0][0] / density,
                cell[0][1] / density,
                cell[0][2] / density,
            ],
            density,
            top: cell[1][0] / density,
        }
    }
}

/// How much of what is behind a stretch of fog it hides, `0.0..=1.0`, and the
/// colour it hides it behind.
///
/// `here` is the camera's column and `there` the fragment's; `from` and `to`
/// their heights relative to the grid's reference, and `distance` the length
/// of the ray. The same arithmetic as `place_fog` in `world.wgsl`, which is
/// what lets the clear colour and the unit tests agree with the shader.
#[must_use]
pub fn amount(here: Sample, there: Sample, from: f32, to: f32, distance: f32) -> (f32, [f32; 3]) {
    let total = here.density + there.density;
    if total <= 1e-6 {
        return (0.0, [1.0; 3]);
    }
    let weigh = |a: f32, b: f32| (a * here.density + b * there.density) / total;
    let colour = [
        weigh(here.colour[0], there.colour[0]),
        weigh(here.colour[1], there.colour[1]),
        weigh(here.colour[2], there.colour[2]),
    ];
    let top = weigh(here.top, there.top);
    let depth = 0.5 * total * distance * mean_height_term(from, to, top);
    (1.0 - (-depth).exp(), colour)
}

/// The camera's fog, where the camera is, as the shaders are told it.
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct Uniforms {
    /// The fog at the camera, unpremultiplied: colour in `xyz`, density in `w`.
    pub here: [f32; 4],
    /// The grid's `-x-z` corner relative to the camera in `xy`, in blocks; the
    /// camera's fog top in `z` and the camera's height in `w`, both relative to
    /// the grid's reference height.
    pub frame: [f32; 4],
    /// Cells per side in `x`; whether any place has fog in `y` (so a world
    /// without any skips it in a uniform branch); how bright daylight fog is in
    /// `z`, which is the sky's light rather than the mod's, since a fog a mod
    /// described once cannot know what time it is.
    pub grid: [f32; 4],
}

impl Uniforms {
    /// No fog anywhere.
    pub const NONE: Self = Self {
        here: [1.0, 1.0, 1.0, 0.0],
        frame: [0.0, 0.0, NO_TOP, 0.0],
        grid: [GRID as f32, 0.0, 1.0, 0.0],
    };

    /// Whether the shaders will draw any place fog at all.
    #[must_use]
    pub fn any(&self) -> bool {
        self.grid[1] > 0.5
    }

    /// The camera's own fog, as a [`Sample`].
    #[must_use]
    pub const fn here(&self) -> Sample {
        Sample {
            colour: [self.here[0], self.here[1], self.here[2]],
            density: self.here[3],
            top: self.frame[2],
        }
    }
}

/// Every column's fog, and the grid of them around the camera.
pub struct PlaceFog {
    /// What the server said, per chunk column. Kept for the columns out of the
    /// grid, so walking back towards a foggy valley finds it again.
    columns: BTreeMap<(i32, i32), ChunkFog>,
    /// The grid, `2 × GRID × GRID` vec4s, laid out `z * GRID + x`.
    cells: Vec<[f32; 4]>,
    /// The chunk the grid was last centred on, and whether it has to be
    /// rebuilt anyway because a fog changed.
    centre: Option<tiamat_core::ChunkPos>,
    dirty: bool,
    /// Off under water, where the water's own murk is the whole view and a
    /// forest's mist behind it is not something anybody down there sees.
    hidden: bool,
    buffer: wgpu::Buffer,
}

impl PlaceFog {
    /// An empty field and its GPU buffer.
    #[must_use]
    pub fn new(gpu: &Gpu) -> Self {
        let cells = vec![[0.0_f32; 4]; 2 * GRID * GRID];
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("place-fog"),
            size: std::mem::size_of_val(cells.as_slice()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            columns: BTreeMap::new(),
            cells,
            centre: None,
            dirty: false,
            hidden: false,
            buffer,
        }
    }

    /// The buffer the shaders read the grid from.
    #[must_use]
    pub const fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Records what the server said about one column's fog.
    ///
    /// `None` REMOVES a fog: the latest chunk served for a column speaks for
    /// it, and a mod that stopped giving a place fog must be able to clear it.
    pub fn set(&mut self, column: (i32, i32), fog: Option<ChunkFog>) {
        let changed = match fog {
            Some(fog) => self.columns.insert(column, fog) != Some(fog),
            None => self.columns.remove(&column).is_some(),
        };
        self.dirty |= changed;
    }

    /// Whether place fog is drawn at all.
    pub const fn set_visible(&mut self, visible: bool) {
        self.hidden = !visible;
    }

    /// Forgets every fog, for a world or a domain that is not this one.
    pub fn clear(&mut self) {
        self.dirty |= !self.columns.is_empty();
        self.columns.clear();
    }

    /// Rebuilds the grid if it has to be, uploads it, and says where the
    /// camera stands in it.
    pub fn prepare(&mut self, gpu: &Gpu, camera: &Camera, daylight: f32) -> Uniforms {
        if self.hidden {
            return Uniforms::NONE;
        }
        if self.columns.is_empty() {
            self.centre = None;
            self.dirty = false;
            return Uniforms::NONE;
        }
        let at = camera.position.chunk;
        if self.dirty || self.centre != Some(at) {
            self.rebuild(at);
            gpu.queue
                .write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.cells));
            self.centre = Some(at);
            self.dirty = false;
        }
        self.uniforms(camera, daylight)
    }

    /// Fills the grid around `centre`.
    fn rebuild(&mut self, centre: tiamat_core::ChunkPos) {
        let reference = reference_height(centre);
        let half = (GRID / 2) as i32;
        for z in 0..GRID {
            for x in 0..GRID {
                let column = (centre.x - half + x as i32, centre.z - half + z as i32);
                let [colour, top] = cell(self.columns.get(&column), reference);
                let index = 2 * (z * GRID + x);
                self.cells[index] = colour;
                self.cells[index + 1] = top;
            }
        }
    }

    /// Where the camera is relative to the grid, and the fog it stands in.
    fn uniforms(&self, camera: &Camera, daylight: f32) -> Uniforms {
        let Some(centre) = self.centre else {
            return Uniforms::NONE;
        };
        let side = tiamat_core::CHUNK_BLOCKS as f32;
        let half = (GRID / 2) as i32;
        let local = camera.position.local;
        let at = camera.position.chunk;
        // Small numbers only: every term is a chunk DIFFERENCE, never a world
        // coordinate, so nothing here accumulates the error a floating origin
        // exists to avoid.
        let origin_x = (centre.x - half - at.x) as f32 * side - local.x;
        let origin_z = (centre.z - half - at.z) as f32 * side - local.z;
        let height = (at.y - centre.y) as f32 * side + local.y;
        let here = sample(&self.cells, [-origin_x, -origin_z]);
        Uniforms {
            here: [here.colour[0], here.colour[1], here.colour[2], here.density],
            frame: [origin_x, origin_z, here.top, height],
            grid: [GRID as f32, 1.0, daylight, 0.0],
        }
    }
}

/// The height the grid's tops are measured from: the base of the chunk it is
/// centred on, which keeps every top a small number.
const fn reference_height(centre: tiamat_core::ChunkPos) -> i32 {
    centre.y * tiamat_core::CHUNK_BLOCKS as i32
}

/// The grid, filtered at a point `within` blocks of its `-x-z` corner.
///
/// Bilinear between cell CENTRES, clamped to the edge — the same lookup
/// `place_fog_at` does in the shaders.
#[must_use]
pub fn sample(cells: &[[f32; 4]], within: [f32; 2]) -> Sample {
    let side = tiamat_core::CHUNK_BLOCKS as f32;
    let last = (GRID - 1) as f32;
    let p = [
        (within[0] / side - 0.5).clamp(0.0, last),
        (within[1] / side - 0.5).clamp(0.0, last),
    ];
    let base = [p[0].floor(), p[1].floor()];
    let t = [p[0] - base[0], p[1] - base[1]];
    let (x0, z0) = (base[0] as usize, base[1] as usize);
    let (x1, z1) = ((x0 + 1).min(GRID - 1), (z0 + 1).min(GRID - 1));
    let at = |x: usize, z: usize| {
        let index = 2 * (z * GRID + x);
        [cells[index], cells[index + 1]]
    };
    let mix = |a: [[f32; 4]; 2], b: [[f32; 4]; 2], t: f32| {
        let lerp = |a: [f32; 4], b: [f32; 4]| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
        [lerp(a[0], b[0]), lerp(a[1], b[1])]
    };
    let near = mix(at(x0, z0), at(x1, z0), t[0]);
    let far = mix(at(x0, z1), at(x1, z1), t[0]);
    Sample::from_premultiplied(mix(near, far, t[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fog(colour: [u8; 3], visibility: u16, top: Option<i32>) -> ChunkFog {
        ChunkFog {
            colour,
            visibility,
            top,
        }
    }

    fn grid_with(columns: &[((usize, usize), ChunkFog)]) -> Vec<[f32; 4]> {
        let mut cells = vec![[0.0_f32; 4]; 2 * GRID * GRID];
        for ((x, z), fog) in columns {
            let [colour, top] = cell(Some(fog), 0);
            cells[2 * (z * GRID + x)] = colour;
            cells[2 * (z * GRID + x) + 1] = top;
        }
        cells
    }

    #[test]
    fn at_its_visibility_a_fog_hides_nineteen_parts_in_twenty() {
        // What `visibility` means to a mod author, held as a number.
        let thick = Sample {
            colour: [0.5; 3],
            density: density(24),
            top: NO_TOP,
        };
        let (hidden, _) = amount(thick, thick, 0.0, 0.0, 24.0);
        assert!((hidden - 0.95).abs() < 0.001, "hid {hidden}");
        let (half_way, _) = amount(thick, thick, 0.0, 0.0, 12.0);
        assert!(half_way > 0.7 && half_way < 0.8, "half way hid {half_way}");
    }

    #[test]
    fn ground_fog_is_thick_under_its_top_and_thin_above_it() {
        // The rainforest's floor mist: the same level ray, in the fog and five
        // falloffs above it, where the profile is under a hundredth.
        let mist = Sample {
            colour: [0.6; 3],
            density: density(16),
            top: 10.0,
        };
        let (under, _) = amount(mist, mist, 5.0, 5.0, 32.0);
        let (over, _) = amount(mist, mist, 30.0, 30.0, 32.0);
        assert!(under > 0.99, "under the top hid {under}");
        assert!(over < 0.06, "five falloffs over the top hid {over}");

        // A ray climbing out of it crosses the boundary exactly: the mean of
        // the profile between 0 and 20 with a top at 10 is half the climb in
        // full fog plus the integral above.
        let expected = (10.0 + FALLOFF * (1.0 - (-10.0_f32 / FALLOFF).exp())) / 20.0;
        let mean = mean_height_term(0.0, 20.0, 10.0);
        assert!((mean - expected).abs() < 1e-5, "{mean} against {expected}");
        // And the same ray the other way round.
        assert!((mean_height_term(20.0, 0.0, 10.0) - expected).abs() < 1e-5);
    }

    #[test]
    fn a_clear_column_thins_a_fog_without_changing_its_colour() {
        // The premultiplication's reason to exist. Half way between a green
        // fog and clear air the density halves — and the colour stays green,
        // where an unpremultiplied blend would have dragged it half way to the
        // zeros a clear cell holds, a dark rim round every foggy place.
        let cells = grid_with(&[((10, 10), fog([60, 200, 90], 20, Some(40)))]);
        let side = tiamat_core::CHUNK_BLOCKS as f32;
        let centre = |x: f32| (x + 0.5) * side;
        let inside = sample(&cells, [centre(10.0), centre(10.0)]);
        let between = sample(&cells, [centre(10.5), centre(10.0)]);
        let outside = sample(&cells, [centre(11.0), centre(10.0)]);

        assert!((inside.density - density(20)).abs() < 1e-6);
        assert!((between.density - density(20) * 0.5).abs() < 1e-6);
        assert!(outside.density.abs() < f32::EPSILON, "{}", outside.density);
        for (got, want) in between.colour.iter().zip([60.0, 200.0, 90.0]) {
            assert!(
                (got - want / 255.0).abs() < 1e-4,
                "colour {:?}",
                between.colour
            );
        }
        assert!((between.top - 40.0).abs() < 1e-3, "top {}", between.top);
    }

    #[test]
    fn a_fog_is_seen_from_outside_it_and_from_within() {
        // Both ends of the ray count. Standing in clear air looking at a
        // foggy place, and standing in it looking out at clear air, both hide
        // something — and less than standing in it looking at more of it.
        let clear = Sample::CLEAR;
        let foggy = Sample {
            colour: [0.4, 0.5, 0.4],
            density: density(30),
            top: NO_TOP,
        };
        let (into, colour) = amount(clear, foggy, 0.0, 0.0, 40.0);
        let (out_of, _) = amount(foggy, clear, 0.0, 0.0, 40.0);
        let (within, _) = amount(foggy, foggy, 0.0, 0.0, 40.0);
        assert!(
            into > 0.5 && (into - out_of).abs() < 1e-6,
            "{into} {out_of}"
        );
        assert!(within > into, "{within} should hide more than {into}");
        for (got, want) in colour.iter().zip(foggy.colour) {
            assert!(
                (got - want).abs() < 1e-6,
                "clear air lent the fog its colour: {colour:?}"
            );
        }
        assert!(amount(clear, clear, 0.0, 0.0, 1000.0).0.abs() < f32::EPSILON);
    }

    #[test]
    fn a_fog_with_no_top_is_the_same_at_every_height() {
        let everywhere = Sample {
            colour: [0.5; 3],
            density: density(50),
            top: NO_TOP,
        };
        let (low, _) = amount(everywhere, everywhere, -300.0, -300.0, 20.0);
        let (high, _) = amount(everywhere, everywhere, 3000.0, 3000.0, 20.0);
        assert!((low - high).abs() < 1e-5, "{low} {high}");
    }
}
