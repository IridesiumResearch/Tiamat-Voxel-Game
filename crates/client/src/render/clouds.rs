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
//! [`tiamat_core::atmosphere::CloudLayer`] for why a deck is a field rather
//! than a mesh of cubes.
//!
//! # Where it draws in the frame
//!
//! After opaque terrain and **before** fluid and particles. It writes the
//! depth of the cube it hit, so clouds sort against the world in both
//! directions — high ground can reach into the deck, and a player above it
//! looks down on the tops — and drawing it before the transparent things means
//! those still sort against the clouds.
//!
//! # What it writes in alpha
//!
//! Not coverage: the pass is opaque. Its alpha is a MARK for the post chain —
//! `CLOUD_MARK` on cloud, `SKY_MARK` on sky, in `clouds.wgsl` — so that mode
//! 3's fog, which goes by depth against the terrain's view distance, can
//! leave cloud alone: a deck is hundreds of blocks up and kilometres out, and
//! fogged as terrain it was flat sky (weather ask W15). The float target
//! keeps the mark and the surface pipeline masks it off, because a
//! swapchain's alpha is the window's.

use tiamat_core::atmosphere::{CloudLayer, Clouds};

use super::{DEPTH_FORMAT, Gpu, graph};

/// How many texels a side the deck's shade map has — weather ask W11.
///
/// The map covers [`SHADOW_EXTENT`] blocks a side around the camera, so a
/// texel is sixteen blocks: one large cube on Weather's deck, which is the
/// finest thing the deck has to shade by. Sixty-five thousand columns of the
/// field a frame, against the millions the march asks.
pub const SHADOW_TEXELS: u32 = 256;

/// How wide the shade map is, in blocks.
///
/// Four kilometres: a deck four hundred blocks up throws its shadow past a
/// low sun by that much before the sun is too low to shade anything at all,
/// and the map is centred on the camera and snapped to its own texels, so a
/// player walking under it sees the shade stay where the cloud is.
pub const SHADOW_EXTENT: f32 = 4096.0;

/// How much of the sun a deck takes from the ground under it.
///
/// Not all: the sky still lights what the deck shades, and a floor gone
/// black under a cloud is a cave, not a shadow.
const SHADOW_STRENGTH: f32 = 0.85;

/// The shade map's format: one channel, filterable, so the sample a fragment
/// takes is blended across its texel and a heap's edge is soft on the ground.
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

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
    /// Coarse cubes, to the horizon. Half resolution.
    Coarse,
    /// The registered cube size. Half resolution.
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

    /// How many of the frame's pixels one of the deck's covers, per axis.
    ///
    /// **Crisp cube edges are the whole aesthetic**, so this is a look
    /// decision before it is a cost one, and the deck is lifted into the frame
    /// one of its texels to a block of pixels rather than filtered — an edge is
    /// a step and not a smear. Full resolution on `Fine`, where the cubes are
    /// small enough to show the difference; `Normal` and `Coarse` at half,
    /// which is a quarter of the pixels marched. `Normal` at half is weather
    /// ask W15's last step, the designer's own call, pictured against full to
    /// make it. `Off` marches nothing and paints the sky alone, which is not
    /// worth making smaller.
    #[must_use]
    pub const fn resolution_divisor(self) -> u32 {
        match self {
            Self::Off | Self::Fine => 1,
            Self::Coarse | Self::Normal => 2,
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
#[derive(Debug, Clone, Default)]
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
    view: [f32; 4],
    sun_direction: [f32; 4],
    sun: [f32; 4],
    sky: [f32; 4],
    colour: [f32; 4],
    shade: [f32; 4],
    weather: [f32; 4],
    motion: [f32; 4],
    quality: [f32; 4],
    /// The three genera beside cumulus — stratocumulus, altocumulus and
    /// cumulonimbus, each a share of the sky — and a spare. Weather ask W13.
    genera: [f32; 4],
    /// The shade map's corner x and z in world blocks, and its side — ask
    /// W11.
    shadow: [f32; 4],
    /// The cover map's corner x and z, its cell size in blocks, and how many
    /// cells a side — zero for "no map, use `weather` everywhere". Ask W10.
    map: [f32; 4],
    /// Cover and darkness per cell, `[cover, darkness]` packed two cells to a
    /// `vec4`, row-major by z.
    ///
    /// **A `vec4` array rather than a flat one**: WGSL's uniform address space
    /// gives an array element a stride of at least sixteen bytes, so
    /// `array<f32, N>` is not expressible there and a flat Rust array would
    /// silently disagree with the shader's idea of it.
    cells: [[f32; 4]; MAP_VEC4S],
}

/// How many `vec4`s the packed cover map takes: two cells each.
const MAP_VEC4S: usize = (tiamat_core::atmosphere::MAX_MAP_SIZE as usize
    * tiamat_core::atmosphere::MAX_MAP_SIZE as usize)
    .div_ceil(2);

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
    /// The lighting mode, as `LightingMode::code` numbers it: 0 Simple,
    /// 1 Classic, 2 Beautiful.
    pub mode: u32,
    /// How wide one pixel is, in radians. The LOD is decided against this
    /// rather than a distance in blocks — see `clouds.wgsl`.
    pub pixel_angle: f32,
    /// The frame's size in pixels, which the deck's own target is a share of.
    pub size: (u32, u32),
}

/// The deck drawn smaller than the frame, for the resolve to lift — weather
/// ask W15's last step.
///
/// Its own colour and depth: the deck is marched into these against nothing,
/// and `resolve_main` lifts colour and depth into the frame, where the depth
/// test against the terrain already drawn decides who is in front and the
/// glass, the fluid and the particles drawn after still sort against the deck.
struct HalfTarget {
    colour: wgpu::TextureView,
    depth: wgpu::TextureView,
    size: (u32, u32),
    /// The uniform and the two textures, for the resolve pipeline.
    bind: wgpu::BindGroup,
}

/// The pipelines, buffer and binding.
pub struct Pass {
    direct: wgpu::RenderPipeline,
    hdr: wgpu::RenderPipeline,
    /// Draws the shade map — weather ask W11.
    shadow: wgpu::RenderPipeline,
    shadow_view: wgpu::TextureView,
    shadow_sampler: wgpu::Sampler,
    /// Where the shade map is this frame, for the world pass: its corner
    /// relative to the camera in xy, one over its side in z, and the deck's
    /// floor relative to the camera in w.
    shadow_frame: [f32; 4],
    /// How much of the sun the deck takes this frame: zero with no deck, in
    /// Simple, or with clouds turned off, and then the map is not drawn.
    shadow_strength: f32,
    /// Lifts the smaller target into the frame, for the surface and for the
    /// float scene texture.
    resolve_direct: wgpu::RenderPipeline,
    resolve_hdr: wgpu::RenderPipeline,
    resolve_layout: wgpu::BindGroupLayout,
    /// The smaller target, when the quality asks for one.
    half: Option<HalfTarget>,
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
    /// The coarse cover map, if a mod sent one — weather ask W10.
    ///
    /// **Beside the deck rather than in it.** `Deck` is `Copy` and is handed
    /// over every frame; a grid is half a kilobyte, and it changes when the
    /// weather does rather than when the frame does.
    map: Option<std::sync::Arc<tiamat_core::atmosphere::CloudMap>>,
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
        let (shadow_view, shadow_sampler) = shade_target(gpu);
        let resolve_layout = resolve_layout(gpu);
        Self {
            direct: pipeline(gpu, &shader, &layout, gpu.surface_format(), false),
            hdr: pipeline(gpu, &shader, &layout, graph::HDR_FORMAT, true),
            shadow: shadow_pipeline(gpu, &shader, &layout),
            shadow_view,
            shadow_sampler,
            shadow_frame: [0.0, 0.0, 0.0, 0.0],
            shadow_strength: 0.0,
            resolve_direct: resolve_pipeline(
                gpu,
                &shader,
                &resolve_layout,
                gpu.surface_format(),
                false,
            ),
            resolve_hdr: resolve_pipeline(gpu, &shader, &resolve_layout, graph::HDR_FORMAT, true),
            resolve_layout,
            half: None,
            uniforms,
            bind,
            draws: false,
            deck: Deck::default(),
            map: None,
            seconds: 0.0,
        }
    }

    /// Sets the deck, the weather over it, and the player's own quality.
    pub fn set(&mut self, deck: Deck) {
        self.deck = deck;
    }

    /// Lays a coarse cover map over the world, or takes it away — ask W10.
    pub fn set_map(&mut self, map: Option<std::sync::Arc<tiamat_core::atmosphere::CloudMap>>) {
        self.map = map;
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
        // **The pass always draws, because it paints the sky.** A deck is what
        // may be absent — most worlds register none, and a player may have
        // turned clouds off — and `cell` of zero is how the shader is told to
        // march nothing and paint the gradient alone.
        self.draws = true;
        let quality = self.deck.quality;
        let layer = self.layer_to_draw(quality);
        let state = self.deck.clouds.unwrap_or(Clouds {
            cover: 0.0,
            darkness: 0.0,
            base: None,
            ease_ticks: 0,
            stratocumulus: 0.0,
            altocumulus: 0.0,
            cumulonimbus: 0.0,
        });
        // Zero stays zero: it is the shader's "no deck".
        let cell = if layer.cell > 0.0 {
            (layer.cell * quality.cell_scale()).max(1.0)
        } else {
            0.0
        };
        let small = (cell / f32::from(layer.detail.max(1))).max(0.5);
        // The deck's own target, a share of the frame — or none, and the
        // deck is marched straight into the frame. Keyed on the deck rather
        // than the quality: a world with no deck paints the sky alone, which
        // has nothing to save on and every reason to stay sharp.
        let divisor = if cell > 0.0 {
            quality.resolution_divisor()
        } else {
            1
        };
        self.fit_half_target(gpu, frame.size, divisor);
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
        let shadow_origin = self.place_shade(camera, base, cell, frame.mode);
        let (cells, descriptor) = self.map_cells();
        let uniforms = Uniforms {
            inverse_view_projection: frame.view_projection.inverse().to_cols_array_2d(),
            view_projection: frame.view_projection.to_cols_array_2d(),
            camera: [camera[0], camera[1], camera[2], self.seconds],
            view: [
                #[expect(clippy::cast_precision_loss, reason = "one or two")]
                {
                    frame.pixel_angle.max(1e-6) * divisor as f32
                },
                0.0,
                0.0,
                0.0,
            ],
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
            map: descriptor,
            cells,
            genera: [
                state.stratocumulus,
                state.altocumulus,
                state.cumulonimbus,
                0.0,
            ],
            shadow: [shadow_origin[0], shadow_origin[1], SHADOW_EXTENT, 0.0],
            quality: [
                quality.reach(),
                quality.detail_reach(),
                f32::from(layer.octaves.max(1)),
                #[expect(clippy::cast_precision_loss, reason = "a mode number, 0 to 2")]
                {
                    frame.mode.min(2) as f32
                },
            ],
        };
        gpu.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
    }

    /// The deck to draw at this quality: the registered one, or an empty one
    /// with a `cell` of zero — the shader's "march nothing, paint the sky" —
    /// for a world with no deck or a player who turned clouds off.
    fn layer_to_draw(&self, quality: Quality) -> CloudLayer {
        self.deck
            .layer
            .filter(|_| quality.draws())
            .unwrap_or(CloudLayer {
                base: 0.0,
                thickness: 0.0,
                cell: 0.0,
                detail: 1,
                frequency: 1.0,
                octaves: 1,
                towers: 0.0,
                drift: [0.0; 2],
                evolve: 0.0,
                colour: [1.0; 3],
                shade: [0.5; 3],
            })
    }

    /// **The cover map, unpacked for the uniform.** Bytes on the wire, shares
    /// of one here, and zero cells when a mod sent none — which is what tells
    /// the shader to use the single cover for the whole sky. Returns the
    /// cells and the map's descriptor: corner x and z, cell, cells a side.
    fn map_cells(&self) -> ([[f32; 4]; MAP_VEC4S], [f32; 4]) {
        let mut cells = [[0.0_f32; 4]; MAP_VEC4S];
        let descriptor = self.map.as_ref().map_or([0.0; 4], |map| {
            for (index, (cover, darkness)) in map.cover.iter().zip(map.darkness.iter()).enumerate()
            {
                let Some(slot) = cells.get_mut(index / 2) else {
                    break;
                };
                let half = (index % 2) * 2;
                slot[half] = f32::from(*cover) / 255.0;
                slot[half + 1] = f32::from(*darkness) / 255.0;
            }
            [map.origin[0], map.origin[1], map.cell, f32::from(map.size)]
        });
        (cells, descriptor)
    }

    /// Decides where this frame's shade map lies and how much it shades by,
    /// and returns the map's corner in world blocks — ask W11.
    ///
    /// The corner is snapped to the map's own texels so that a walking camera
    /// does not slide the sample points under the field: the map is redrawn
    /// each frame, but a shadow that swam by a fraction of a texel as the
    /// player moved would read as the ground shimmering. Simple skips the
    /// shade as it skips the rest of the lighting, and a sky with nothing in
    /// it shades nothing.
    fn place_shade(&mut self, camera: [f32; 3], base: f32, cell: f32, mode: u32) -> [f32; 2] {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a count of sixteen-block texels across the world, far inside f32"
        )]
        let corner = |along: f32| {
            let texel = SHADOW_EXTENT / SHADOW_TEXELS as f32;
            let steps = tiamat_core::detgen::floor_to_i32((along - SHADOW_EXTENT * 0.5) / texel);
            steps as f32 * texel
        };
        let origin = [corner(camera[0]), corner(camera[2])];
        self.shadow_frame = [
            origin[0] - camera[0],
            origin[1] - camera[2],
            1.0 / SHADOW_EXTENT,
            base - camera[1],
        ];
        self.shadow_strength = if cell > 0.0 && mode >= 1 {
            SHADOW_STRENGTH
        } else {
            0.0
        };
        origin
    }

    /// The shade map and the sampler the world pass reads it with, for its
    /// bind group.
    #[must_use]
    pub const fn shade(&self) -> (&wgpu::TextureView, &wgpu::Sampler) {
        (&self.shadow_view, &self.shadow_sampler)
    }

    /// Draws the deck's shade map, if there is a deck to shade by — ask W11.
    ///
    /// Its own small pass, before the world's, into the texture the world
    /// pass samples. Nothing to draw is the ordinary case and costs nothing.
    pub fn render_shadow(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.shadow_strength <= 0.0 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cloud-shade"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.shadow_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shadow);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Where the shade map is this frame, as the world pass's globals carry
    /// it: the corner relative to the camera in xy, one over the side in z,
    /// the deck's floor relative to the camera in w.
    #[must_use]
    pub const fn shadow_frame(&self) -> [f32; 4] {
        self.shadow_frame
    }

    /// How much of the sun the deck takes this frame, 0 for none.
    #[must_use]
    pub const fn shadow_strength(&self) -> f32 {
        self.shadow_strength
    }

    /// Draws the deck into a pass whose target is the float scene texture
    /// (`hdr`) or the surface.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, hdr: bool) {
        if !self.draws {
            return;
        }
        // Drawn already, smaller, by `render_half`: lift it. The resolve
        // writes the deck's depth, so the terrain already in the frame keeps
        // its place and what is drawn after still sorts against the deck.
        if let Some(half) = &self.half {
            pass.set_pipeline(if hdr {
                &self.resolve_hdr
            } else {
                &self.resolve_direct
            });
            pass.set_bind_group(0, &half.bind, &[]);
            pass.draw(0..3, 0..1);
            return;
        }
        pass.set_pipeline(if hdr { &self.hdr } else { &self.direct });
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Draws the deck into its smaller target, when the quality asks for one
    /// — before the world pass, which lifts it with [`Self::draw`].
    ///
    /// The float pipeline, whatever the frame's own format: the target is a
    /// float texture either way, and lifting it into an sRGB surface encodes
    /// it on the way.
    pub fn render_half(&self, encoder: &mut wgpu::CommandEncoder) {
        let Some(half) = &self.half else {
            return;
        };
        if !self.draws {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clouds-half"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &half.colour,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &half.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.hdr);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Makes the smaller target the frame's size over `divisor`, drops it for
    /// a divisor of one, and keeps it when nothing changed.
    fn fit_half_target(&mut self, gpu: &Gpu, size: (u32, u32), divisor: u32) {
        if divisor <= 1 || size.0 == 0 || size.1 == 0 {
            self.half = None;
            return;
        }
        let want = (
            size.0.div_ceil(divisor).max(1),
            size.1.div_ceil(divisor).max(1),
        );
        if self.half.as_ref().is_some_and(|half| half.size == want) {
            return;
        }
        let texture = |label: &str, format: wgpu::TextureFormat| {
            gpu.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: want.0,
                        height: want.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let colour = texture("clouds-half-colour", graph::HDR_FORMAT);
        let depth = texture("clouds-half-depth", DEPTH_FORMAT);
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clouds-resolve"),
            layout: &self.resolve_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&colour),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&depth),
                },
            ],
        });
        self.half = Some(HalfTarget {
            colour,
            depth,
            size: want,
            bind,
        });
    }
}

/// The pipeline for one target format.
///
/// Depth is TESTED and WRITTEN, unlike the particle pass: a cloud is a solid
/// surface at a real distance, and everything drawn after it has to sort
/// against it.
///
/// `marks` is whether alpha is written: the float target keeps the cloud mark
/// for the post chain, and the surface — whose alpha is the window's — does
/// not get it.
fn pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    marks: bool,
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
                    // **Opaque.** Every fragment the pass keeps replaces what
                    // was under it — sky, cloud, or the fog inside one — so
                    // there is nothing to blend, and its alpha is a mark
                    // rather than coverage (see `clouds.wgsl`). Blending by
                    // that mark would make every cloud invisible.
                    blend: None,
                    write_mask: if marks {
                        wgpu::ColorWrites::ALL
                    } else {
                        wgpu::ColorWrites::COLOR
                    },
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
                // **`LessEqual`, not `Less`.** The depth buffer is cleared to
                // 1.0 and the sky is painted AT 1.0, so `Less` would throw
                // away every sky pixel — the one thing this pass must not do.
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
}

/// The shade map's texture and the sampler the world pass reads it with.
fn shade_target(gpu: &Gpu) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cloud-shade"),
        size: wgpu::Extent3d {
            width: SHADOW_TEXELS,
            height: SHADOW_TEXELS,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SHADOW_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("cloud-shade"),
        // Clamped: past the map's edge the world pass has already answered
        // "lit", and a repeat would lay the far side's clouds over the near.
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    (view, sampler)
}

/// What the resolve reads: the uniform, and the smaller target's colour and
/// depth. Loaded by texel rather than sampled, so no sampler.
fn resolve_layout(gpu: &Gpu) -> wgpu::BindGroupLayout {
    gpu.device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("clouds-resolve-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        })
}

/// The resolve's pipeline for one target format: `resolve_main` lifting the
/// smaller target's colour and depth into the frame, depth tested and
/// written like the direct draw, so the frame sorts it as if it had been
/// marched there.
fn resolve_pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    marks: bool,
) -> wgpu::RenderPipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("clouds-resolve-pipeline-layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("clouds-resolve"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("resolve_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: if marks {
                        wgpu::ColorWrites::ALL
                    } else {
                        wgpu::ColorWrites::COLOR
                    },
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
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
}

/// The shade map's pipeline: the same triangle and uniform, `shadow_main`
/// into one channel, with no depth to test — the map is a picture of the
/// deck from below, not a surface in the world.
fn shadow_pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud-shade-pipeline-layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cloud-shade"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("shadow_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SHADOW_FORMAT,
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
    fn only_the_finest_setting_draws_the_deck_at_the_frames_own_resolution() {
        // Crisp cube edges are the aesthetic, so resolution is a look decision
        // before it is a cost one — and `Fine` is where the cubes are small
        // enough for it to show. `Normal` at half is weather ask W15's last
        // step, the designer's call; `Off` has nothing to make smaller.
        assert_eq!(Quality::Fine.resolution_divisor(), 1);
        assert_eq!(Quality::Normal.resolution_divisor(), 2);
        assert_eq!(Quality::Coarse.resolution_divisor(), 2);
        assert_eq!(Quality::Off.resolution_divisor(), 1);
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
