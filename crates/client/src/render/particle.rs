// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The particle pass: every live particle as a soft disc facing the camera.
//!
//! Self-contained — its own uniform, bind group and pipeline pair — because it
//! reads nothing the world's globals carry but the view and the sky's fog, and
//! `Renderer::new` is at clippy's line ceiling. See `crate::particles` for what
//! a particle is and `particle.wgsl` for how one is drawn.

use crate::camera::Camera;
use crate::particles::MAX_LIVE;

use super::{DEPTH_FORMAT, Gpu, graph};

/// One particle as the renderer is handed it: already camera-relative.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    /// Its centre, relative to the camera, in blocks.
    pub centre: [f32; 3],
    /// Blocks across.
    pub size: f32,
    /// Lit colour, and opacity now.
    pub colour: [f32; 4],
    /// The registered picture drawn on it, or `None` for a plain disc.
    ///
    /// Life ask 15. Sprites are grouped by this before they are uploaded, so
    /// each picture is one bind and one draw — a particle is a hot path and a
    /// bind group per particle would be worse than the pixels it replaces.
    pub texture: Option<tiamat_core::proto::ContentHash>,
}

/// One particle, as the shader reads it.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    centre: [f32; 4],
    colour: [f32; 4],
}

/// `View` in `particle.wgsl`, field for field.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_projection: [[f32; 4]; 4],
    right: [f32; 4],
    up: [f32; 4],
    sky: [f32; 4],
}

/// What the pass needs to know about the frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// The camera's view and projection.
    pub view_projection: glam::Mat4,
    /// The sky's colour, and where its fog is total.
    pub sky: [f32; 3],
    /// See `sky`.
    pub fog_end: f32,
    /// Where the fog is total straight up or down.
    pub fog_up: f32,
    /// The fog curve's exponent.
    pub fog_curve: f32,
    /// Whether this pass fogs particles, which is every mode without a post
    /// chain; mode 3 fogs from depth after the fact.
    pub fogs: bool,
}

/// The pipelines, buffers and bindings.
pub struct Pass {
    /// The pipelines that sample a picture, for the two target formats.
    textured: wgpu::RenderPipeline,
    /// The same, into the float scene texture.
    textured_hdr: wgpu::RenderPipeline,
    /// The layout a picture is bound with: a view and a sampler.
    picture_layout: wgpu::BindGroupLayout,
    /// One bind group per picture the renderer has been given.
    pictures: std::collections::BTreeMap<tiamat_core::proto::ContentHash, wgpu::BindGroup>,
    /// **Nearest, because a picture on a particle is pixel art.** A heart is
    /// sixteen pixels across and drawn a few dozen wide; filtering it turns
    /// the outline into a smear.
    sampler: wgpu::Sampler,
    /// Where each picture's instances start and end in the buffer, in the
    /// order they were uploaded. Untextured ones come first.
    groups: Vec<(
        Option<tiamat_core::proto::ContentHash>,
        std::ops::Range<u32>,
    )>,
    direct: wgpu::RenderPipeline,
    hdr: wgpu::RenderPipeline,
    instances: wgpu::Buffer,
    view: wgpu::Buffer,
    bind: wgpu::BindGroup,
    count: u32,
}

impl Pass {
    /// Builds the pass for a surface of `format`, and for mode 3's float target.
    #[must_use]
    pub fn new(gpu: &Gpu) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::include_wgsl!("particle.wgsl"));
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("particle-bind-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let view = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particle-view"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instances = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particles"),
            size: (MAX_LIVE * size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let picture_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("particle-picture-layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("particle-picture-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // Nearest on the way up: a heart is pixel art and a smeared
            // outline is worse than a hard one.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view.as_entire_binding(),
            }],
        });
        Self {
            direct: pipeline(gpu, &shader, &layout, gpu.surface_format()),
            hdr: pipeline(gpu, &shader, &layout, graph::HDR_FORMAT),
            instances,
            view,
            bind,
            // Life ask 15: a second pair of pipelines that sample a picture.
            // The plain ones stay, because rain is the hot path and must not
            // pay for a bind group it does not use.
            textured: textured_pipeline(
                gpu,
                &shader,
                &layout,
                &picture_layout,
                gpu.surface_format(),
            ),
            textured_hdr: textured_pipeline(
                gpu,
                &shader,
                &layout,
                &picture_layout,
                graph::HDR_FORMAT,
            ),
            picture_layout,
            pictures: std::collections::BTreeMap::new(),
            sampler,
            groups: Vec::new(),
            count: 0,
        }
    }

    /// Gives the pass a picture a burst may name — Life ask 15.
    ///
    /// **Uploaded once and kept**, because a burst names it by hash and the
    /// same heart is drawn every time anything is hit. A picture that arrives
    /// after the burst that named it simply starts being drawn; until then the
    /// particle is the plain disc it always was.
    pub fn set_picture(
        &mut self,
        gpu: &Gpu,
        hash: tiamat_core::proto::ContentHash,
        image: &crate::texture::Image,
    ) {
        let view = upload(gpu, image);
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle-picture"),
            layout: &self.picture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.pictures.insert(hash, bind);
    }

    /// Forgets every picture, when a world is left.
    ///
    /// A picture belongs to the server that pushed it — the rule a font and a
    /// model follow.
    pub fn clear_pictures(&mut self) {
        self.pictures.clear();
    }

    /// Sets the particles to draw this frame. Past the buffer's capacity the
    /// rest are dropped, which `crate::particles` already never exceeds.
    pub fn set(&mut self, gpu: &Gpu, sprites: &[Sprite]) {
        // **Grouped by picture, plain ones first** (Life ask 15). A bind group
        // is per draw, so the buffer is laid out one picture at a time and
        // each becomes a single `draw` over its own range. Rain, which names
        // no picture, stays one draw of thousands as it always was.
        let mut ordered: Vec<&Sprite> = sprites.iter().take(MAX_LIVE).collect();
        // Sorted rather than bucketed into maps: it is a few thousand small
        // items once a frame, and a stable sort keeps a burst's own particles
        // together, which is what makes the ranges contiguous.
        ordered.sort_by_key(|sprite| sprite.texture);

        let mut instances: Vec<Instance> = Vec::with_capacity(ordered.len());
        self.groups.clear();
        let mut run: Option<(Option<tiamat_core::proto::ContentHash>, u32)> = None;
        for sprite in ordered {
            let index = u32::try_from(instances.len()).unwrap_or(0);
            match run {
                Some((texture, start)) if texture != sprite.texture => {
                    self.groups.push((texture, start..index));
                    run = Some((sprite.texture, index));
                }
                None => run = Some((sprite.texture, index)),
                Some(_) => {}
            }
            instances.push(Instance {
                centre: [
                    sprite.centre[0],
                    sprite.centre[1],
                    sprite.centre[2],
                    sprite.size,
                ],
                colour: sprite.colour,
            });
        }
        self.count = u32::try_from(instances.len()).unwrap_or(0);
        if let Some((texture, start)) = run {
            self.groups.push((texture, start..self.count));
        }
        if !instances.is_empty() {
            gpu.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        }
    }

    /// How many particles the next frame draws.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Writes this frame's view. Skipped when there is nothing to draw.
    pub fn prepare(&self, gpu: &Gpu, camera: &Camera, frame: &Frame) {
        if self.count == 0 {
            return;
        }
        let right = camera.right();
        let up = right.cross(camera.forward()).normalize_or_zero();
        let view = Uniforms {
            view_projection: frame.view_projection.to_cols_array_2d(),
            right: [right.x, right.y, right.z, frame.fog_end],
            up: [up.x, up.y, up.z, frame.fog_curve],
            sky: [
                frame.sky[0],
                frame.sky[1],
                frame.sky[2],
                // The vertical reach when this pass fogs, and a negative
                // number when the post chain does: one slot, two facts.
                if frame.fogs { frame.fog_up } else { -1.0 },
            ],
        };
        gpu.queue
            .write_buffer(&self.view, 0, bytemuck::bytes_of(&view));
    }

    /// Draws the particles into a pass whose target is the float scene texture
    /// (`hdr`) or the surface.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, hdr: bool) {
        if self.count == 0 {
            return;
        }
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for (texture, range) in &self.groups {
            if range.is_empty() {
                continue;
            }
            // A picture the renderer has, or the plain disc: a hash nobody
            // pushed, and one whose bytes have not arrived yet, both fall back
            // rather than drawing nothing.
            match texture.and_then(|hash| self.pictures.get(&hash)) {
                Some(bind) => {
                    pass.set_pipeline(if hdr {
                        &self.textured_hdr
                    } else {
                        &self.textured
                    });
                    pass.set_bind_group(1, bind, &[]);
                }
                None => pass.set_pipeline(if hdr { &self.hdr } else { &self.direct }),
            }
            pass.draw(0..6, range.clone());
        }
    }
}

/// The pipeline for one target format: blended, depth-tested, not written.
fn pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particle-pipeline-layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particles"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: size_of::<[f32; 4]>() as u64,
                            shader_location: 1,
                        },
                    ],
                }],
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
                // Faces the camera by construction; which way it winds depends
                // on the axes it was built from, and a spray with half its
                // particles culled is not worth the argument.
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
}

/// The pipeline for a particle that samples a picture.
///
/// The same vertex stage and the same blending as the plain one; only the
/// fragment entry point and the extra bind group differ.
fn textured_pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    picture: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particle-textured-layout"),
            bind_group_layouts: &[Some(layout), Some(picture)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particle-textured"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
                }],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                // Blended like the plain particle: tested against the world,
                // never written, so two of them do not cut each other out.
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fragment_textured"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
}

/// Uploads one decoded picture, level zero only.
///
/// The same shape `skinned::upload_skin` has, and for the same reason: a
/// picture on a particle is drawn at a few dozen pixels and a mip chain would
/// be work nobody sees.
fn upload(gpu: &Gpu, image: &crate::texture::Image) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: image.width.max(1),
        height: image.height.max(1),
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("particle-picture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &image.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width * 4),
            rows_per_image: Some(size.height),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}
