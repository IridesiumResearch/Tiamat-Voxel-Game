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
            count: 0,
        }
    }

    /// Sets the particles to draw this frame. Past the buffer's capacity the
    /// rest are dropped, which `crate::particles` already never exceeds.
    pub fn set(&mut self, gpu: &Gpu, sprites: &[Sprite]) {
        let instances: Vec<Instance> = sprites
            .iter()
            .take(MAX_LIVE)
            .map(|sprite| Instance {
                centre: [
                    sprite.centre[0],
                    sprite.centre[1],
                    sprite.centre[2],
                    sprite.size,
                ],
                colour: sprite.colour,
            })
            .collect();
        self.count = u32::try_from(instances.len()).unwrap_or(0);
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
        pass.set_pipeline(if hdr { &self.hdr } else { &self.direct });
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, 0..self.count);
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
