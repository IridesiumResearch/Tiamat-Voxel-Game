// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The cloud pass: one fullscreen triangle, marched.
//!
//! Self-contained — its own uniform, bind group and pipeline pair — like
//! [`super::particle`], and for the same reason: it reads nothing the world's
//! globals carry but the camera and the sky, and `Renderer::new` is at
//! clippy's line ceiling.
//!
//! See `clouds.wgsl` for the field and the march, and
//! [`tiamot_core::atmosphere::CloudLayer`] for why a deck is a field rather
//! than a mesh of cubes.
//!
//! # Where it draws in the frame
//!
//! After opaque terrain and **before** fluid and particles. It writes the
//! depth of the cube it hit, so clouds sort against the world in both
//! directions — high ground can reach into the deck, and a player above it
//! looks down on the tops — and drawing it before the transparent things means
//! those still sort against the clouds.

use tiamot_core::atmosphere::{CloudLayer, Clouds};

use super::{DEPTH_FORMAT, Gpu, graph};

/// How far the deck is drawn at each quality, in blocks, and how far the
/// small-cube rind reaches.
///
/// **The player's own choice, and the server is never told.** A mod declares
/// the deck; how much of it this machine draws is a graphics setting like view
/// distance, and a mod must not assume its clouds are being drawn at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// No clouds at all.
    Off,
    /// Coarse cubes, to the horizon. Quarter resolution.
    Coarse,
    /// The registered cube size.
    #[default]
    Normal,
    /// Finer cubes, nearer. Full resolution.
    Fine,
}

impl Quality {
    /// Multiplies the deck's registered cube size.
    ///
    /// Coarser cubes mean fewer steps for the same distance, which is why the
    /// slider moves cube size and draw distance together rather than offering
    /// two dials that can be set to something unplayable.
    #[must_use]
    pub const fn cell_scale(self) -> f32 {
        match self {
            Self::Off | Self::Coarse => 2.0,
            Self::Normal => 1.0,
            Self::Fine => 0.5,
        }
    }

    /// How far the deck is drawn, in blocks.
    #[must_use]
    pub const fn reach(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Coarse => 6000.0,
            Self::Normal => 4000.0,
            Self::Fine => 3000.0,
        }
    }

    /// How far the small-cube rind reaches, in blocks.
    ///
    /// It FADES to nothing over this rather than stopping at it: a visible
    /// line across the sky where the detail starts is worse than no detail.
    #[must_use]
    pub const fn detail_reach(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Coarse => 200.0,
            Self::Normal => 600.0,
            Self::Fine => 1200.0,
        }
    }

    /// What share of the screen's resolution the pass is drawn at.
    ///
    /// **Crisp cube edges are the whole aesthetic**, so this is a look
    /// decision before it is a cost one: a quarter-resolution deck scaled up
    /// softens and shimmers at exactly the edges the references are made of.
    /// Full resolution wherever the mode can show the difference.
    #[must_use]
    pub const fn resolution_scale(self) -> f32 {
        match self {
            Self::Off | Self::Coarse => 0.5,
            Self::Normal | Self::Fine => 1.0,
        }
    }

    /// Whether anything is drawn at all.
    #[must_use]
    pub const fn draws(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// Everything the renderer holds about clouds between frames.
///
/// **One struct rather than four fields**, because a renderer needs all of
/// them or none: a deck with no weather is a clear sky, weather with no deck
/// is nothing to draw, a player who turned clouds off gets neither however
/// much a mod registered, and the seed decides which sky it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deck {
    /// The deck a mod registered.
    pub layer: Option<CloudLayer>,
    /// What this player is under.
    pub clouds: Option<Clouds>,
    /// The player's own quality setting.
    pub quality: Quality,
    /// The world's seed, so a sky is the same one twice.
    pub seed: u64,
}

/// `Clouds` in `clouds.wgsl`, field for field.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    inverse_view_projection: [[f32; 4]; 4],
    view_projection: [[f32; 4]; 4],
    camera: [f32; 4],
    sun_direction: [f32; 4],
    sun: [f32; 4],
    sky: [f32; 4],
    colour: [f32; 4],
    shade: [f32; 4],
    weather: [f32; 4],
    motion: [f32; 4],
    quality: [f32; 4],
}

/// What the pass needs to know about the frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// The camera's view and projection.
    pub view_projection: glam::Mat4,
    /// The camera in WORLD blocks, so the deck stays where the world is.
    pub camera: [f64; 3],
    /// Which way sunlight travels.
    pub sun_direction: [f32; 3],
    /// The sun's colour.
    pub sun: [f32; 3],
    /// The sky's colour.
    pub sky: [f32; 3],
    /// Where distance fog is total, in blocks.
    pub fog_end: f32,
    /// The lighting mode, as `LightingMode::code` numbers it.
    pub mode: u32,
}

/// The pipelines, buffer and binding.
pub struct Pass {
    direct: wgpu::RenderPipeline,
    hdr: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
    /// Whether this frame has a deck to draw.
    draws: bool,
    /// The deck, the weather over it, the player's quality and the seed.
    ///
    /// **Held here rather than on the renderer**, for the reason this pass has
    /// its own uniform and bind group: `Renderer::new` is at clippy's line
    /// ceiling, and a pass that keeps its own state costs it one line instead
    /// of four. It is also where the state belongs.
    deck: Deck,
    /// Seconds since the client started, for drift and evolution.
    seconds: f32,
}

impl Pass {
    /// Builds the pass for the surface format and for mode 3's float target.
    #[must_use]
    pub fn new(gpu: &Gpu) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::include_wgsl!("clouds.wgsl"));
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("cloud-bind-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let uniforms = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cloud-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clouds"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        Self {
            direct: pipeline(gpu, &shader, &layout, gpu.surface_format()),
            hdr: pipeline(gpu, &shader, &layout, graph::HDR_FORMAT),
            uniforms,
            bind,
            draws: false,
            deck: Deck::default(),
            seconds: 0.0,
        }
    }

    /// Sets the deck, the weather over it, and the player's own quality.
    pub const fn set(&mut self, deck: Deck) {
        self.deck = deck;
    }

    /// Advances the deck's own clock.
    ///
    /// Seconds rather than ticks: drift and evolution are presentation and run
    /// on the frame loop, not the simulation's — charter rule 4 exempts this.
    pub const fn advance(&mut self, seconds: f32) {
        self.seconds += seconds;
    }

    /// Writes this frame's uniform, or marks the pass as drawing nothing.
    ///
    /// Nothing to draw is the ordinary case: most worlds register no deck, and
    /// a player may have turned clouds off.
    pub fn prepare(&mut self, gpu: &Gpu, frame: &Frame) {
        let Some(layer) = self.deck.layer.filter(|_| self.deck.quality.draws()) else {
            self.draws = false;
            return;
        };
        self.draws = true;
        let quality = self.deck.quality;
        let state = self.deck.clouds.unwrap_or(Clouds {
            cover: 0.0,
            darkness: 0.0,
            base: None,
            ease_ticks: 0,
        });
        let cell = (layer.cell * quality.cell_scale()).max(1.0);
        let small = (cell / f32::from(layer.detail.max(1))).max(0.5);
        let base = state.base.unwrap_or(layer.base);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the cloud field's scale is hundreds of blocks; a fraction of a \
                      block of precision at the world's edge is nothing to it"
        )]
        let camera = [
            frame.camera[0] as f32,
            frame.camera[1] as f32,
            frame.camera[2] as f32,
        ];
        #[expect(
            clippy::cast_precision_loss,
            reason = "the seed only has to pick a field, not be read back"
        )]
        let seed = (self.deck.seed & 0xFFFF) as f32;
        let uniforms = Uniforms {
            inverse_view_projection: frame.view_projection.inverse().to_cols_array_2d(),
            view_projection: frame.view_projection.to_cols_array_2d(),
            camera: [camera[0], camera[1], camera[2], self.seconds],
            sun_direction: [
                frame.sun_direction[0],
                frame.sun_direction[1],
                frame.sun_direction[2],
                base,
            ],
            sun: [frame.sun[0], frame.sun[1], frame.sun[2], layer.thickness],
            sky: [frame.sky[0], frame.sky[1], frame.sky[2], frame.fog_end],
            colour: [layer.colour[0], layer.colour[1], layer.colour[2], cell],
            shade: [layer.shade[0], layer.shade[1], layer.shade[2], small],
            weather: [state.cover, state.darkness, layer.frequency, layer.towers],
            motion: [layer.drift[0], layer.drift[1], layer.evolve, seed],
            quality: [
                quality.reach(),
                quality.detail_reach(),
                f32::from(layer.octaves.max(1)),
                frame.mode.clamp(1, 3) as f32,
            ],
        };
        gpu.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Draws the deck into a pass whose target is the float scene texture
    /// (`hdr`) or the surface.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, hdr: bool) {
        if !self.draws {
            return;
        }
        pass.set_pipeline(if hdr { &self.hdr } else { &self.direct });
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// The pipeline for one target format.
///
/// Depth is TESTED and WRITTEN, unlike the particle pass: a cloud is a solid
/// surface at a real distance, and everything drawn after it has to sort
/// against it.
fn pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud-pipeline-layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("clouds"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::COLOR,
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
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cloud_setting_moves_cube_size_and_distance_together() {
        // **One slider, not two dials.** A player who could set a tiny cube
        // size AND the horizon would be choosing something unplayable without
        // being told; coarser cubes are what pays for the extra distance.
        let coarse = Quality::Coarse;
        let fine = Quality::Fine;
        assert!(coarse.cell_scale() > fine.cell_scale());
        assert!(coarse.reach() > fine.reach());

        // Off draws nothing and asks for nothing.
        assert!(!Quality::Off.draws());
        assert!((Quality::Off.reach() - 0.0).abs() < f32::EPSILON);
        assert!(Quality::Normal.draws());
    }

    #[test]
    fn the_modes_that_show_crisp_edges_get_full_resolution() {
        // Crisp cube edges are the aesthetic, so resolution is a look decision
        // before it is a cost one: scaling a quarter-resolution deck up softens
        // and shimmers at exactly the edges the references are made of.
        assert!((Quality::Fine.resolution_scale() - 1.0).abs() < f32::EPSILON);
        assert!((Quality::Normal.resolution_scale() - 1.0).abs() < f32::EPSILON);
        assert!(Quality::Coarse.resolution_scale() < 1.0);
    }

    #[test]
    fn detail_never_reaches_further_than_the_deck_is_drawn() {
        // The rind fades out WITHIN the draw distance. Detail reaching past it
        // would be arithmetic that could never be seen, and a fade that never
        // completed would put a line across the sky.
        for quality in [
            Quality::Off,
            Quality::Coarse,
            Quality::Normal,
            Quality::Fine,
        ] {
            assert!(
                quality.detail_reach() <= quality.reach(),
                "{quality:?} details past where it draws"
            );
        }
    }
}
