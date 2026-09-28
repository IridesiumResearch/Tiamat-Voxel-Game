// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The lightning pass: every live bolt as ribbons of light — weather ask W26.
//!
//! Self-contained like the particle pass — its own uniform, bind group and
//! pipeline pair — and drawn right after it, last of the blended things in
//! the world pass, after the terrain and before mode 3's fog. See
//! `tiamat_core::lightning` for how a path is built and `lightning.wgsl` for
//! how a segment is drawn.
//!
//! # A ribbon, not a sprite
//!
//! A particle faces the camera on both axes, which makes a disc. A bolt's
//! segment faces it only AROUND ITS OWN AXIS: the quad lies along the
//! segment and turns about it towards the eye, so the jagged line reads as
//! a line from any side. No ribbon existed in the client to copy; this is
//! the whole of it.
//!
//! # Additive, unlit, tested and not written
//!
//! Light adds: a bolt over a night sky is its own colour and over a noon
//! one brighter still, never the grey a lit particle came out — the reason
//! the ask exists. It is tested against the depth the terrain wrote, so a
//! hill hides the part behind it, and writes none, so its glow never cuts
//! its core out and a bolt never hides the water behind it.
//!
//! # Never thinner than a pixel
//!
//! A 0.4-block bolt 400 blocks off is a fifth of a pixel at 1080 lines.
//! Rasterised at its true width it would light some rows and not others —
//! a dotted line, or nothing. So the shader widens each ribbon to a floor in
//! PIXELS and keeps its brightness: a real bolt at that range is far too
//! bright for anyone to see how thin it is.

use tiamat_core::lightning::{Lightning, MAX_SEGMENTS, Path};

use super::{DEPTH_FORMAT, Gpu, graph};

/// One segment as the renderer is handed it: already camera-relative, its
/// light already scaled by the flicker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Instance {
    /// Where it starts, relative to the camera, in blocks.
    pub a: [f32; 3],
    /// Where it ends, the same way.
    pub b: [f32; 3],
    /// The bright core's width, in blocks. The glow is wider.
    pub width: f32,
    /// Linear light, added to what is behind it.
    pub colour: [f32; 3],
}

/// The most segments one frame draws: every bolt a client holds at once,
/// at the cap for forks. Past this the rest are dropped, which
/// [`crate::sky::Bolts`] already never exceeds.
pub const MAX_INSTANCES: usize = crate::sky::MAX_BOLTS * MAX_SEGMENTS;

/// How much of a fork's width it keeps at its tip: a fork is narrower than
/// the trunk as well as dimmer, and narrows as it fades.
const TIP_WIDTH: f32 = 0.5;

/// One segment, as the shader reads it.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Raw {
    a: [f32; 4],
    b: [f32; 4],
    colour: [f32; 4],
}

/// `View` in `lightning.wgsl`, field for field.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_projection: [[f32; 4]; 4],
    right: [f32; 4],
}

/// Light adds: `src + dst`, for the colour and for the alpha the pipeline
/// does not write anyway.
///
/// The first additive blend in the client. wgpu has no constant for it, and
/// `ALPHA_BLENDING` would make a bolt over a bright sky a translucent grey
/// smear rather than a brighter line.
const ADDITIVE: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
};

/// Appends one bolt's segments, `age_ticks` after it struck, to `out`.
///
/// `origin` is the camera's offset to the bolt's `from`, taken in `f64` by
/// `Position::offset_to` — the path is relative to `from`, so adding the
/// two is the whole of placing it (charter rule 7). A segment the flicker
/// has put out is left out rather than drawn black, since black added is
/// nothing drawn at a cost.
pub fn strokes(
    bolt: &Lightning,
    path: &Path,
    age_ticks: f32,
    origin: [f32; 3],
    out: &mut Vec<Instance>,
) {
    let light = tiamat_core::lightning::brightness(bolt, age_ticks);
    if light <= 0.0 {
        return;
    }
    let at = |offset: [f32; 3]| {
        [
            origin[0] + offset[0],
            origin[1] + offset[1],
            origin[2] + offset[2],
        ]
    };
    for segment in &path.segments {
        let amount = light * segment.strength;
        if amount <= 0.0 {
            continue;
        }
        out.push(Instance {
            a: at(segment.a),
            b: at(segment.b),
            width: bolt.width * (TIP_WIDTH + (1.0 - TIP_WIDTH) * segment.strength),
            colour: [
                bolt.colour[0] * amount,
                bolt.colour[1] * amount,
                bolt.colour[2] * amount,
            ],
        });
    }
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
            .create_shader_module(wgpu::include_wgsl!("lightning.wgsl"));
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("lightning-bind-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let view = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightning-view"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instances = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightning"),
            size: (MAX_INSTANCES * size_of::<Raw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightning"),
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

    /// Sets the segments to draw this frame.
    pub fn set(&mut self, gpu: &Gpu, strokes: &[Instance]) {
        let raw: Vec<Raw> = strokes
            .iter()
            .take(MAX_INSTANCES)
            .map(|stroke| Raw {
                a: [stroke.a[0], stroke.a[1], stroke.a[2], stroke.width],
                b: [stroke.b[0], stroke.b[1], stroke.b[2], 0.0],
                colour: [stroke.colour[0], stroke.colour[1], stroke.colour[2], 0.0],
            })
            .collect();
        self.count = u32::try_from(raw.len()).unwrap_or(0);
        if !raw.is_empty() {
            gpu.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&raw));
        }
    }

    /// How many segments the next frame draws.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Writes this frame's view. Skipped when there is nothing to draw.
    ///
    /// `height` is the target's, in pixels: with the projection's vertical
    /// scale it says how many blocks one pixel spans one block from the eye,
    /// which is what keeps a far bolt from falling between the pixels.
    pub fn prepare(
        &self,
        gpu: &Gpu,
        camera: &crate::camera::Camera,
        view_projection: glam::Mat4,
        height: u32,
    ) {
        if self.count == 0 {
            return;
        }
        let right = camera.right();
        // The vertical scale does not depend on the aspect, so any will do;
        // `1 / tan(fov / 2)` read off the matrix rather than recomputed.
        let scale = camera.projection(1.0).y_axis.y;
        let pixel = 2.0 / (scale * height.max(1) as f32).max(f32::EPSILON);
        let view = Uniforms {
            view_projection: view_projection.to_cols_array_2d(),
            right: [right.x, right.y, right.z, pixel],
        };
        gpu.queue
            .write_buffer(&self.view, 0, bytemuck::bytes_of(&view));
    }

    /// Draws the bolts into a pass whose target is the float scene texture
    /// (`hdr`) or the surface.
    ///
    /// **Its own group at slot 0**, as the particle pass binds its own; the
    /// overlays drawn after rebind the world's, for the reason
    /// `draw_overlays` gives.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, hdr: bool) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(if hdr { &self.hdr } else { &self.direct });
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        // Twelve vertices a segment: the glow's quad, then the core's. One
        // draw for both layers, since light adds in either order.
        pass.draw(0..12, 0..self.count);
    }
}

/// The pipeline for one target format: additive, depth-tested, not written.
fn pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lightning-pipeline-layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lightning"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<Raw>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4,
                        1 => Float32x4,
                        2 => Float32x4
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(ADDITIVE),
                    // **Colour only.** In mode 3 the scene's alpha is the
                    // cloud pass's mark for the composite, not coverage, and
                    // a bolt over a cloud must leave the cloud fogged as a
                    // cloud; a swapchain's alpha is the window's.
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                // Faces the camera by construction, and which way it winds
                // depends on which way the segment runs.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bolt() -> Lightning {
        Lightning {
            from: [100_000.5, 380.0, -3.0],
            to: [100_040.5, 80.0, 27.0],
            seed: 5,
            colour: tiamat_core::lightning::DEFAULT_COLOUR,
            width: 0.4,
            branches: 3,
            ticks: 8,
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "an offset of zero added is the same number"
    )]
    fn a_bolt_is_placed_at_its_top_and_lit_by_its_flicker() {
        let bolt = bolt();
        let path = tiamat_core::lightning::build_path(&bolt);
        let mut out = Vec::new();
        strokes(&bolt, &path, 0.0, [10.0, 20.0, 30.0], &mut out);
        assert_eq!(
            out.len(),
            path.segments.len(),
            "every segment is lit at once"
        );
        assert_eq!(out[0].a, [10.0, 20.0, 30.0], "the trunk starts at the top");
        // The trunk at full light is the bolt's colour; a fork is dimmer and
        // narrower.
        assert_eq!(out[0].colour, bolt.colour);
        assert_eq!(out[0].width, bolt.width);
        let fork = out[out.len() - 1];
        assert!(fork.colour[2] < bolt.colour[2] / 2.0);
        assert!(fork.width < bolt.width);

        // Gone at its end, and nothing is handed over for it.
        let mut none = Vec::new();
        strokes(&bolt, &path, f32::from(bolt.ticks), [0.0; 3], &mut none);
        assert!(none.is_empty());
    }
}
