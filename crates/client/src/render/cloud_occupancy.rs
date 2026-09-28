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
//! times 2^k, the grid `grid_level` in `clouds.wgsl` puts a ray on. Each
//! level is a square centred on the cell the camera stands in at that
//! level, found from the camera's base cell by a shift — so the pass that
//! draws a level and the march that reads it agree on every texel with no
//! division between them.
//!
//! **A level covers only as far out as the march walks its grid.** A ray
//! moves to the next grid once its cells shrink under the pixels it wants
//! (`want` in the march), and that happens the same number of a level's
//! own cells out from the camera at every level: the cell doubles and so
//! does the distance. So every level but the coarsest needs only that many
//! cells either way, however far the deck reaches, and the coarsest — never
//! left for another — covers the reach. The count falls as the frame's
//! pixels grow, so a small frame draws a small occupancy: at `Low` on
//! Weather's deck about nine thousand columns at 1920 x 1080, where a
//! square over the whole reach at every level was twenty-two thousand.
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
//! heaps. It is asked once, in the texture's corner, in the same pass.
//!
//! # Exact
//!
//! Each texel asks the field at the point the march would, so skipping a
//! clear cell changes no pixel; `occupancy_main` in `clouds.wgsl` says why a
//! detail-free asking over the whole slab finds everything the march's own
//! could, and why a column near one of the field's keep-or-drop lines is
//! written as cloud everywhere — the pass and the march are two shaders, and
//! a driver may compile one expression two ways. `screenshot.rs` compares
//! the deck with the skip on and off, byte for byte, on the software
//! renderer; that it holds on a real card's compiler too is a human gate.

use super::Gpu;

/// Levels: the base cube and five doublings, since the march never walks a
/// cell wider than thirty-two base cubes. Read back out of `clouds.wgsl`
/// by a test, with the thirty-two.
pub const LEVELS: u32 = 6;

/// Base cells to one of the coarsest: the unit the side over the reach is a
/// whole number of, so that every level's share of it is whole.
const COARSEST: u32 = 1 << (LEVELS - 1);

/// The most base cells a side the reach's square may be, and the most cells
/// a side any level may be.
///
/// Enough for the reach at every rung on Weather's deck — 192 at `Low`, 320
/// at `Medium`, 448 at `High` — and a deck of smaller cubes than that is
/// covered to this and asks the field past it, as every cell did before.
pub const MOST: u32 = 512;

/// How many pixels the march wants a cell to cover before it moves a ray to
/// the next grid: `want` in `march`, in `clouds.wgsl`, which a test reads
/// back. The walk of a level's grid follows from it.
const WANT: f32 = 9.0;

/// Cells past where a ray could first move to a coarser grid that a level
/// still covers. The ray moves at the end of the cell it is in rather than
/// the moment it crosses the line, and the shader's logarithm may land a
/// last bit either way; a cell the square misses is only asked of the
/// field, but a ray that asks many is the cost this exists to save.
const LAG: u32 = 2;

/// Two spans of height, in blocks: the low cloud's bottom and top, and the
/// altocumulus's and anvil's together. Full floats, because a height is
/// hundreds of blocks and the march compares it against the ray's own.
///
/// Sixteen bytes a texel, so the texture is made for the squares a frame
/// asks for and grows only when a larger one is asked: about 180 kilobytes
/// at `Low` on Weather's deck at 1920 x 1080, and eight megabytes at the
/// very most.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

/// Where the deck's pipelines find it.
pub const BINDING: u32 = 8;

/// Where the texture keeps whether the camera stands in cloud: the corner,
/// before the levels. `OCCUPANCY_CAMERA` in `clouds.wgsl` is the same.
const CAMERA_TEXEL: [u32; 2] = [0, 0];

/// The texture, the pipeline that draws it, and this frame's squares.
pub struct Occupancy {
    pipeline: wgpu::RenderPipeline,
    /// Every level, to draw into and for the march to read.
    view: wgpu::TextureView,
    /// The texture's width and height, in texels.
    room: [u32; 2],
    /// This frame's squares: nothing to draw when their reach is zero.
    placed: Placement,
    /// Whether the march reads it. Always, but for the test that compares.
    on: bool,
}

impl Occupancy {
    /// Makes the smallest texture and builds the pipeline that draws a
    /// level of it: `occupancy_vertex` and `occupancy_main` in `shader`,
    /// against `layout` — which must NOT hold the occupancy itself, since a
    /// pass cannot read what it draws into.
    pub fn new(gpu: &Gpu, shader: &wgpu::ShaderModule, layout: &wgpu::BindGroupLayout) -> Self {
        let room = [COARSEST, COARSEST];
        Self {
            pipeline: pipeline(gpu, shader, layout),
            view: texture(gpu, room),
            room,
            placed: Placement::default(),
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

    /// Places this frame's squares around the camera for a deck of base cell
    /// `cell` drawn to `reach` blocks, by a march whose pixels are
    /// `pixel_angle` radians wide, and returns them as the uniform carries
    /// them — the camera's base cell x and z, the side over the reach in
    /// base cells, and the side the walk of a grid needs — with whether the
    /// texture had to grow for them, in which case the deck's bind group must
    /// be made again. A `cell` of zero is no deck, and nothing is drawn.
    pub fn place(
        &mut self,
        gpu: &Gpu,
        camera: [f32; 3],
        cell: f32,
        reach: f32,
        pixel_angle: f32,
    ) -> ([f32; 4], bool) {
        if !self.on || cell <= 0.0 {
            self.placed = Placement::default();
            return ([0.0; 4], false);
        }
        self.placed = Placement::new([camera[0], camera[2]], cell, reach, pixel_angle);
        let [width, height] = self.placed.size();
        let grew = width > self.room[0] || height > self.room[1];
        if grew {
            self.room = [width.max(self.room[0]), height.max(self.room[1])];
            self.view = texture(gpu, self.room);
        }
        (self.placed.uniform(), grew)
    }

    /// Draws every level, if the march is to read them this frame: one
    /// pass, and in it a viewport and a draw per level.
    ///
    /// Before the deck's own pass, and before the world's, which draws the
    /// deck at the frame's own resolution. `bind` is the deck's shared bind
    /// group, the one without the occupancy in it.
    pub fn render(&self, encoder: &mut wgpu::CommandEncoder, bind: &wgpu::BindGroup) {
        if self.placed.reach == 0 {
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
                // with the squares.
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
            #[expect(
                clippy::cast_precision_loss,
                reason = "texel counts, a thousand at the most"
            )]
            let (x, side) = (self.placed.x(level) as f32, self.placed.side(level) as f32);
            pass.set_viewport(x, 0.0, side, side, 0.0, 1.0);
            // The level rides in as the instance.
            pass.draw(0..3, level..level + 1);
        }
        // And the camera's texel, as the instance past the last level.
        #[expect(clippy::cast_precision_loss, reason = "zero")]
        let [x, y] = CAMERA_TEXEL.map(|at| at as f32);
        pass.set_viewport(x, y, 1.0, 1.0, 0.0, 1.0);
        pass.draw(0..3, LEVELS..LEVELS + 1);
    }
}

/// Where the levels stand this frame, as whole numbers: what the uniform
/// carries, and what `occupancy_side`, `occupancy_x` and `occupancy_level`
/// in `clouds.wgsl` make of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Placement {
    /// The base cell the camera stands in, x and z.
    camera: [i32; 2],
    /// The side that covers the deck's reach, in base cells: a whole number
    /// of the coarsest, or zero for nothing drawn.
    reach: u32,
    /// The most cells a side any level but the coarsest needs: its walk
    /// either way from the camera's cell, and the lag, and that cell.
    walk: u32,
}

impl Placement {
    /// The squares for a camera at world `camera` (x and z), over a deck of
    /// base cell `cell` drawn to `reach` blocks by a march whose pixels are
    /// `pixel_angle` radians wide.
    ///
    /// **The reach.** A level's square is centred on the camera's cell at
    /// that level, so it reaches half its side less one cell past the
    /// camera on its short side. The side over the reach is twice the reach
    /// in coarsest cells and two coarsest cells more, so that at the
    /// coarsest level — the one cell less being a whole coarsest cell there
    /// — the reach is still covered, and every finer level has that and
    /// more.
    ///
    /// **The walk.** A ray walks level k's grid until a cell of it covers
    /// fewer pixels than the march wants: until `cell * 2^k / (t *
    /// pixel_angle)` falls under `WANT`, which is at `t` of `1 / (WANT *
    /// pixel_angle)` of level k's own cells, the same number at every level.
    /// And the ray is never further out sideways than along itself.
    fn new(camera: [f32; 2], cell: f32, reach: f32, pixel_angle: f32) -> Self {
        // One over the floor rather than a ceiling, which a determinism lint
        // bans from the workspace: one cell more than it needs at most.
        let cells = |blocks: f32| {
            u32::try_from(tiamat_core::detgen::floor_to_i32(blocks).max(0))
                .unwrap_or(0)
                .saturating_add(1)
        };
        let reach_cells = cells(reach.max(0.0) / cell).min(MOST);
        let reach = ((2 * reach_cells.div_ceil(COARSEST) + 2) * COARSEST).min(MOST);
        let walked = cells(1.0 / (WANT * pixel_angle.max(1e-6))).min(MOST);
        let walk = (2 * (walked + LAG + 1)).min(MOST);
        let at = |along: f32| tiamat_core::detgen::floor_to_i32(along / cell);
        Self {
            camera: [at(camera[0]), at(camera[1])],
            reach,
            walk,
        }
    }

    /// Level `level`'s side, in its own cells. `occupancy_side` in
    /// `clouds.wgsl` is the same arithmetic.
    fn side(self, level: u32) -> u32 {
        let whole = self.reach >> level;
        if level + 1 >= LEVELS {
            whole
        } else {
            whole.min(self.walk)
        }
    }

    /// Where level `level` starts along the texture's top row: after the
    /// camera's texel and every finer level. `occupancy_x` in `clouds.wgsl`
    /// is the same arithmetic.
    fn x(self, level: u32) -> u32 {
        1 + (0..level).map(|finer| self.side(finer)).sum::<u32>()
    }

    /// The texture this frame's squares need: every level in a row after
    /// the camera's texel, as tall as the tallest.
    fn size(self) -> [u32; 2] {
        let tallest = (0..LEVELS).map(|level| self.side(level)).max().unwrap_or(0);
        [self.x(LEVELS), tallest.max(1)]
    }

    /// The uniform's `occupancy`.
    #[expect(
        clippy::cast_precision_loss,
        reason = "whole numbers of cells, each far under 2^24, so exact"
    )]
    fn uniform(self) -> [f32; 4] {
        [
            self.camera[0] as f32,
            self.camera[1] as f32,
            self.reach as f32,
            self.walk as f32,
        ]
    }
}

/// A texture of `size` texels, width and height, for the camera's texel
/// and every level.
fn texture(gpu: &Gpu, size: [u32; 2]) -> wgpu::TextureView {
    gpu.device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("cloud-occupancy"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
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

/// The pipeline that draws one level: the triangle with the level as its
/// instance, and `occupancy_main` writing the texel's two spans of height
/// into the four channels of an `Rgba32Float` target, with no depth.
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

    /// The camera's vertical field of view, as `Camera::default` has it.
    const FOV_Y: f32 = 1.221_730_5;

    /// The march's pixel at `quality` in a frame `height` pixels tall, as
    /// `clouds.rs` hands it over.
    #[expect(clippy::cast_precision_loss, reason = "frame heights")]
    fn pixel_at(quality: Quality, height: u32) -> f32 {
        FOV_Y / height as f32 * quality.resolution_divisor() as f32
    }

    /// Where level `level`'s square starts, in its own cells, as
    /// `occupancy_level` in `clouds.wgsl` works it out.
    fn corner(placed: Placement, level: u32) -> [i32; 2] {
        let side = i32::try_from(placed.side(level)).expect("a side");
        placed.camera.map(|at| (at >> level) - (side >> 1))
    }

    #[test]
    fn the_levels_are_the_grids_the_march_can_walk() {
        // Read from the shader, so the two cannot drift apart: the march
        // clamps a cell to thirty-two base cubes, and there must be a level
        // for every power of two up to it — no more, which would cost a pass
        // for a grid nobody walks, and no fewer, which would send the far
        // cells of every ray to the field. And the pixels it wants a cell to
        // cover, which is how far each level is walked.
        let shader = include_str!("clouds.wgsl");
        assert!(
            shader.contains("let steps = clamp(want / pixels, 1.0, 32.0);"),
            "the march's widest cell moved; the occupancy's levels must follow"
        );
        assert!(
            shader.contains(&format!("let want = {WANT:.1};")),
            "the march wants another number of pixels; the walk must follow"
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
    fn every_level_covers_where_its_grid_is_walked_on_every_rung() {
        // A cell a level's square does not cover is asked of the field,
        // which is correct and slower; so every level must cover everywhere
        // the march can walk its grid — out to where a ray moves to the next
        // one, and a lag of cells past it, or to the reach if that is
        // nearer — wherever in its cell the camera stands, in small frames
        // and large.
        for quality in [Quality::Low, Quality::Medium, Quality::High] {
            let cell = WEATHER_CELL * quality.cell_scale();
            let reach = quality.reach();
            for height in [180, 540, 1080, 2160] {
                let pixel = pixel_at(quality, height);
                for camera in [
                    [0.0, 0.0],
                    [24.0, 20.0],
                    [-1234.5, 987.25],
                    [59_999.0, -59_999.0],
                    [cell * 16.5, -cell * 15.5],
                    [-cell * 0.01, cell * 31.99],
                ] {
                    let placed = Placement::new(camera, cell, reach, pixel);
                    assert!(placed.reach <= MOST && placed.walk <= MOST);
                    for level in 0..LEVELS {
                        let wide = cell * f32::from(1u16 << level);
                        #[expect(clippy::cast_precision_loss, reason = "a lag of cells")]
                        let walked = wide / (WANT * pixel) + LAG as f32 * wide;
                        let need = if level + 1 == LEVELS {
                            reach
                        } else {
                            reach.min(walked)
                        };
                        let side = placed.side(level);
                        let corner = corner(placed, level);
                        for axis in 0..2 {
                            #[expect(clippy::cast_precision_loss, reason = "cell counts")]
                            let (low, high) = (
                                corner[axis] as f32 * wide,
                                (corner[axis] + i32::try_from(side).expect("side")) as f32 * wide,
                            );
                            let at = camera[axis];
                            assert!(
                                low <= at - need && high >= at + need,
                                "{quality:?} at {height}p, level {level}, camera {at}: \
                                 {low}..{high} misses {need} either way"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_levels_fit_the_texture_side_by_side_and_never_overlap() {
        // Every level inside the texture made for it, the camera's texel
        // too, and no two sharing a texel, or one level's cells would read
        // another's. The places are the shader's, so its arithmetic is read
        // back out of it first.
        let shader = include_str!("clouds.wgsl");
        for line in [
            "let whole = i32(clouds.occupancy.z) >> level;",
            "if (level + 1u >= u32(OCCUPANCY_LEVELS)) {",
            "return min(whole, i32(clouds.occupancy.w));",
            "var x = 1;",
            "x = x + occupancy_side(finer);",
            "const OCCUPANCY_CAMERA: vec2<i32> = vec2<i32>(0, 0);",
            "out.corner = (vec2<i32>(clouds.occupancy.xy) >> vec2<u32>(shift)) \
             - vec2<i32>(out.side >> 1u);",
            "out.slot = vec2<i32>(occupancy_x(shift), 0);",
        ] {
            assert!(
                shader.contains(line),
                "clouds.wgsl places the levels, or the camera's texel, otherwise: \
                 `{line}` is gone"
            );
        }
        assert_eq!(CAMERA_TEXEL, [0, 0]);
        for reach in [4 * COARSEST, 192, 320, 448, MOST] {
            for walk in [8, 16, 56, 200, MOST] {
                let placed = Placement {
                    camera: [3, -7],
                    reach,
                    walk,
                };
                let [width, height] = placed.size();
                let squares: Vec<([u32; 2], u32)> = (0..LEVELS)
                    .map(|level| ([placed.x(level), 0], placed.side(level)))
                    .chain([(CAMERA_TEXEL, 1)])
                    .collect();
                for (index, &([x, y], wide)) in squares.iter().enumerate() {
                    assert!(wide >= 1, "{reach}/{walk}: level {index} is empty");
                    assert!(x + wide <= width && y + wide <= height);
                    for &([ox, oy], other) in &squares[index + 1..] {
                        let apart =
                            x + wide <= ox || ox + other <= x || y + wide <= oy || oy + other <= y;
                        assert!(apart, "{reach}/{walk}: level {index} overlaps another");
                    }
                }
            }
        }
    }

    #[test]
    fn the_shipped_deck_on_low_asks_fewer_columns_in_smaller_frames() {
        // The pass's cost is its columns: at the default rung on Weather's
        // deck, about nine thousand at 1080p — the square over the whole
        // reach at every level was twenty-two thousand — and fewer the
        // smaller the frame, since the march's pixels are wider and it
        // leaves each grid sooner.
        let columns = |height| {
            let cell = WEATHER_CELL * Quality::Low.cell_scale();
            let placed = Placement::new(
                [24.0, 20.0],
                cell,
                Quality::Low.reach(),
                pixel_at(Quality::Low, height),
            );
            (0..LEVELS)
                .map(|level| placed.side(level).pow(2))
                .sum::<u32>()
        };
        assert_eq!(columns(1080), 9332);
        assert!(columns(270) < columns(540) && columns(540) < columns(1080));
        assert!(columns(270) < 2000, "{}", columns(270));
    }
}
