// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Where in the cloud march's cells there is any cloud at all — weather ask
//! W27.
//!
//! # Why
//!
//! A step of the march is one asking of the field — the heap search, the
//! sheet's cells, the storm lattice — and a ray takes a step for every cell
//! it crosses. Most cells of most skies are air, or hold their cloud above
//! or below where the ray passes: from over the heaps, every column with a
//! heap in it under the ray, or an anvil or a mackerel sky over it. Asking
//! every cell ONCE a frame, into a small texture, for the heights its cloud
//! spans, and letting the march read a texel before it asks the field,
//! turns those stretches of a ray into texture reads.
//!
//! # What a texel holds
//!
//! Two spans of height: the low cloud's — heaps, the sheet, a tower — and
//! the altocumulus and an anvil's together, as one span from the lowest of
//! them to the highest. A yes or no was the first version, and measured it
//! did nothing for the view from over the deck, where nearly every column
//! has cloud in it somewhere and the ray passes over it or under it.
//!
//! # The shape
//!
//! A level per grid the march can walk: level k's texel is one base cube
//! times 2^k, the grid `grid_level` in `clouds.wgsl` puts a ray on. Every
//! level covers the same square around the camera, wide enough for the
//! deck's reach, and its corner is a whole number of the coarsest cells, so
//! a march cell at any level finds its texel by a subtraction and a shift —
//! no division, and nothing to round onto a neighbour's texel.
//!
//! **The levels side by side in one texture, drawn in one pass**, rather
//! than as the mips of one. Mips want a pass each, and on the software
//! renderer a pass costs about a millisecond whatever it draws: measured,
//! the six passes cost more than the whole march at `Low` saves. One pass,
//! a viewport and a draw per level, and the march reads one level of one
//! texture rather than choosing a mip per step.
//!
//! **A draw, not a dispatch.** The software renderer shades a pass a 64-pixel
//! tile to a thread, and the finest level at `Low` is four tiles, so a
//! compute dispatch — which hands its groups to every thread — was tried
//! too. Measured at 480 x 270, where the pass is most of what the deck
//! costs, it was half a millisecond slower: what it gains in threads the
//! dispatch spends on its own overhead and on the groups of the coarse
//! levels, which have almost nothing to do.
//!
//! **And one texel more: whether the camera stands in cloud.** Every pixel
//! of the deck asked the field that, at the camera's own column, and every
//! pixel got the same answer — a whole column of the field per pixel,
//! wherever the camera is inside the slab, which is every view from over the
//! heaps. It is asked once, beside the levels, in the same pass.
//!
//! # Exact
//!
//! Each texel asks the field at the point the march would, so skipping a
//! clear cell changes no pixel; `occupancy_main` in `clouds.wgsl` says why a
//! detail-free asking over the whole slab finds everything the march's own
//! could. `screenshot.rs` compares the deck with the skip on and off, byte
//! for byte.

use super::Gpu;

/// Levels: the base cube and five doublings, since the march never walks a
/// cell wider than thirty-two base cubes. Read back out of `clouds.wgsl`
/// by a test, with the thirty-two.
pub const LEVELS: u32 = 6;

/// Base cells to one of the coarsest: the unit the square's corner and side
/// are whole numbers of, so that every level divides them exactly.
const COARSEST: u32 = 1 << (LEVELS - 1);

/// The most base cells a side the square may be.
///
/// Enough for the reach at every rung on Weather's deck — 128 at `Low`, 288
/// at `Medium`, 416 at `High` — and a deck of smaller cubes than that is
/// covered to this and asks the field past it, as every cell did before.
pub const MOST: u32 = 512;

/// Two spans of height, in blocks: the low cloud's bottom and top, and the
/// altocumulus's and anvil's together. Full floats, because a height is
/// hundreds of blocks and the march compares it against the ray's own.
///
/// Sixteen bytes a texel, so the texture is made for the square a frame
/// asks for and grows only when a larger one is asked: the finest level
/// `side` a side, the rest in a column half as wide beside it — under four
/// hundred kilobytes at `Low` on Weather's deck, and six megabytes at the
/// very most.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

/// Where the deck's pipelines find it.
pub const BINDING: u32 = 8;

/// The texture, the pipeline that draws it, and this frame's square.
pub struct Occupancy {
    pipeline: wgpu::RenderPipeline,
    /// Every level, to draw into and for the march to read.
    view: wgpu::TextureView,
    /// The largest side the texture holds, in base cells.
    room: u32,
    /// This frame's side, in base cells: zero when there is nothing to draw.
    side: u32,
    /// Whether the march reads it. Always, but for the test that compares.
    on: bool,
}

impl Occupancy {
    /// Makes the smallest texture and builds the pipeline that draws a
    /// level of it: `occupancy_vertex` and `occupancy_main` in `shader`,
    /// against `layout` — which must NOT hold the occupancy itself, since a
    /// pass cannot read what it draws into.
    pub fn new(gpu: &Gpu, shader: &wgpu::ShaderModule, layout: &wgpu::BindGroupLayout) -> Self {
        Self {
            pipeline: pipeline(gpu, shader, layout),
            view: texture(gpu, COARSEST),
            room: COARSEST,
            side: 0,
            on: true,
        }
    }

    /// The deck's bind group layout entry for it: read by texel, never
    /// sampled, so not filtered.
    pub const fn layout_entry() -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding: BINDING,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }
    }

    /// The deck's bind group entry for it.
    pub fn bind_entry(&self) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry {
            binding: BINDING,
            resource: wgpu::BindingResource::TextureView(&self.view),
        }
    }

    /// Whether the march reads it — **for tests alone**: off, every cell is
    /// asked of the field as it was before weather ask W27, and the test that
    /// proves the skip changes no pixel compares the two.
    pub const fn set_on(&mut self, on: bool) {
        self.on = on;
    }

    /// Places this frame's square around the camera for a deck of base cell
    /// `cell` drawn to `reach` blocks, and returns it as the uniform carries
    /// it — the corner x and z in base cells, the side in base cells, and 1
    /// if the march reads it — and whether the texture had to grow for it,
    /// in which case the deck's bind group must be made again. A `cell` of
    /// zero is no deck, and nothing is drawn.
    pub fn place(
        &mut self,
        gpu: &Gpu,
        camera: [f32; 3],
        cell: f32,
        reach: f32,
    ) -> ([f32; 4], bool) {
        if !self.on || cell <= 0.0 {
            self.side = 0;
            return ([0.0; 4], false);
        }
        let (corner, side) = square([camera[0], camera[2]], cell, reach);
        self.side = side;
        let grew = side > self.room;
        if grew {
            self.room = side;
            self.view = texture(gpu, side);
        }
        ([corner[0] as f32, corner[1] as f32, side as f32, 1.0], grew)
    }

    /// Draws every level, if the march is to read them this frame: one
    /// pass, and in it a viewport and a draw per level.
    ///
    /// Before the deck's own pass, and before the world's, which draws the
    /// deck at the frame's own resolution. `bind` is the deck's shared bind
    /// group, the one without the occupancy in it.
    pub fn render(&self, encoder: &mut wgpu::CommandEncoder, bind: &wgpu::BindGroup) {
        if self.side == 0 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cloud-occupancy"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                depth_slice: None,
                resolve_target: None,
                // **Loaded, not cleared**: every texel the march reads this
                // frame is drawn this frame, and clearing the rest would be
                // the one cost here that grows with the texture rather than
                // with the square.
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        for level in 0..LEVELS {
            let [x, y] = slot(level, self.side);
            let side = self.side >> level;
            pass.set_viewport(x as f32, y as f32, side as f32, side as f32, 0.0, 1.0);
            // The level rides in as the instance.
            pass.draw(0..3, level..level + 1);
        }
        // And the camera's texel, as the instance past the last level.
        let [x, y] = camera_texel(self.side);
        pass.set_viewport(x as f32, y as f32, 1.0, 1.0, 0.0, 1.0);
        pass.draw(0..3, LEVELS..LEVELS + 1);
    }
}

/// A texture for squares up to `room` base cells a side: the finest level,
/// and the column beside it.
fn texture(gpu: &Gpu, room: u32) -> wgpu::TextureView {
    gpu.device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("cloud-occupancy"),
            size: wgpu::Extent3d {
                width: room + room / 2,
                height: room,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// Where the camera's texel stands for a square `side` a side: the foot of
/// the column the coarser levels stand in, which the coarsest leaves free.
/// `occupancy_camera` in `clouds.wgsl` is the same.
const fn camera_texel(side: u32) -> [u32; 2] {
    [side, side - 1]
}

/// Where level `level` of a square `side` base cells wide stands in the
/// texture: the finest at the corner, and each coarser one in a column
/// beside it, under the one before. `occupancy_slot` in `clouds.wgsl` is the
/// same arithmetic.
const fn slot(level: u32, side: u32) -> [u32; 2] {
    if level == 0 {
        [0, 0]
    } else {
        [side, side - (side >> (level - 1))]
    }
}

/// The square around `camera` (world x and z) that a deck of base cell
/// `cell` needs covered to `reach` blocks: its corner, in base cells and a
/// whole number of the coarsest, and its side in base cells, a whole number
/// of the coarsest and at most [`MOST`].
///
/// The side is twice the reach and one coarsest cell, and the corner is the
/// camera's cell less half the side, rounded to the nearest coarsest cell —
/// so the camera stands within half a coarsest cell of the middle, and the
/// reach fits either way with that half to spare.
fn square(camera: [f32; 2], cell: f32, reach: f32) -> ([i32; 2], u32) {
    // One over the floor rather than a ceiling, which a determinism lint
    // bans from the workspace: one base cell more than it needs at most.
    let reach_cells = tiamat_core::detgen::floor_to_i32(reach.max(0.0) / cell).max(0) + 1;
    let side = (2 * reach_cells as u32 + COARSEST)
        .next_multiple_of(COARSEST)
        .min(MOST);
    let coarsest = COARSEST as i32;
    let corner = |along: f32| {
        let low = tiamat_core::detgen::floor_to_i32(along / cell) - (side / 2) as i32;
        (low + coarsest / 2).div_euclid(coarsest) * coarsest
    };
    ([corner(camera[0]), corner(camera[1])], side)
}

/// The pipeline that draws one level: the triangle with the level as its
/// instance, `occupancy_main` into one channel, no depth.
fn pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud-occupancy-pipeline-layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cloud-occupancy"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("occupancy_vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("occupancy_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::clouds::Quality;

    /// Weather's registered cube, as it ships (ask W27).
    const WEATHER_CELL: f32 = 32.0;

    #[test]
    fn the_levels_are_the_grids_the_march_can_walk() {
        // Read from the shader, so the two cannot drift apart: the march
        // clamps a cell to thirty-two base cubes, and there must be a level
        // for every power of two up to it — no more, which would cost a pass
        // for a grid nobody walks, and no fewer, which would send the far
        // cells of every ray to the field.
        let shader = include_str!("clouds.wgsl");
        assert!(
            shader.contains("let steps = clamp(want / pixels, 1.0, 32.0);"),
            "the march's widest cell moved; the occupancy's levels must follow"
        );
        assert_eq!(1 << (LEVELS - 1), 32);
        assert!(
            shader.contains(&format!("const OCCUPANCY_LEVELS: f32 = {LEVELS}.0;")),
            "clouds.wgsl and cloud_occupancy.rs disagree about the levels"
        );
        assert!(
            shader.contains(&format!("@group(0) @binding({BINDING}) var occupancy:")),
            "clouds.wgsl reads the occupancy from another binding"
        );
    }

    #[test]
    fn the_square_covers_the_reach_on_every_rung_and_divides_at_every_level() {
        // A cell of the reach the square does not cover is asked of the
        // field, which is correct and slower; one it covered at the wrong
        // texel would be wrong. So: every rung on Weather's deck is covered
        // to its reach, wherever in its cell the camera stands, and the
        // corner and side are whole numbers of the coarsest cell.
        for quality in [Quality::Low, Quality::Medium, Quality::High] {
            let cell = WEATHER_CELL * quality.cell_scale();
            let reach = quality.reach();
            for camera in [
                [0.0, 0.0],
                [24.0, 20.0],
                [-1234.5, 987.25],
                [59_999.0, -59_999.0],
                [cell * 16.5, -cell * 15.5],
            ] {
                let (corner, side) = square(camera, cell, reach);
                assert!(side <= MOST, "{quality:?}: {side} past the texture");
                assert_eq!(side % COARSEST, 0, "{quality:?}: side {side}");
                for (axis, &at) in camera.iter().enumerate() {
                    assert_eq!(corner[axis].rem_euclid(COARSEST as i32), 0);
                    let low = corner[axis] as f32 * cell;
                    let high = (corner[axis] + side as i32) as f32 * cell;
                    assert!(
                        low <= at - reach && high > at + reach,
                        "{quality:?} at {at}: the square {low}..{high} misses the reach {reach}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_levels_fit_the_texture_side_by_side_and_never_overlap() {
        // Every level of a square inside a texture made for it, the camera's
        // texel too, and no two sharing a texel, or one level's cells would
        // read another's. The places are the shader's, so its arithmetic is
        // read back out of it first.
        let shader = include_str!("clouds.wgsl");
        assert!(
            shader.contains("return vec2<i32>(side, side - (side >> (level - 1u)));")
                && shader.contains("return vec2<i32>(side, side - 1);"),
            "clouds.wgsl places the levels, or the camera's texel, elsewhere"
        );
        for side in [COARSEST, 128, 288, 416, MOST] {
            let squares: Vec<([u32; 2], u32)> = (0..LEVELS)
                .map(|level| (slot(level, side), side >> level))
                .chain([(camera_texel(side), 1)])
                .collect();
            for (index, &([x, y], wide)) in squares.iter().enumerate() {
                assert!(wide >= 1, "{side}: level {index} is empty");
                assert!(x + wide <= side + side / 2 && y + wide <= side);
                for &([ox, oy], other) in &squares[index + 1..] {
                    let apart =
                        x + wide <= ox || ox + other <= x || y + wide <= oy || oy + other <= y;
                    assert!(apart, "{side}: level {index} overlaps another");
                }
            }
        }
    }

    #[test]
    fn the_shipped_deck_on_low_is_the_smallest_square_that_covers_it() {
        // The pass's cost is its side squared, four thirds over: the default
        // rung on Weather's deck is 128 a side, twenty-two thousand columns.
        let (_, side) = square([24.0, 20.0], WEATHER_CELL * 2.0, Quality::Low.reach());
        assert_eq!(side, 128);
    }
}
