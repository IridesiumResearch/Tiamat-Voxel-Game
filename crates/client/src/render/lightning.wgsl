// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

// Lightning: each segment of a bolt as a ribbon of light — weather ask W26.
// See `render::lightning` for the pass and `tiamat_core::lightning` for the
// path.
//
// # A ribbon about its own axis
//
// The quad runs along the segment, and its width lies along the one direction
// square to both the segment and the line of sight to its middle: the cross
// product of the two. That turns the ribbon about the segment to face the
// eye, and only about it, so a bolt seen from any side is a line and not a
// row of discs.
//
// # Two layers, one draw
//
// Twelve vertices an instance: six for a wide, faint glow, then six for the
// narrow core. Light adds, so the order the two land in is no matter and a
// second draw with its own uniform would buy nothing.
//
// # Unlit and unfogged
//
// A bolt is a light, not a surface: nothing here reads the sun or the block
// light, and in modes 1 and 2 nothing fogs it — a bolt seen through the haze
// at the end of the view is the one thing in the haze that is not the haze's
// colour. Mode 3's composite fogs the frame from depth after this pass, as it
// does everything drawn into the scene.

struct View {
    view_projection: mat4x4<f32>,
    // The camera's right in xyz, for a segment pointing straight at the eye;
    // in w, how many blocks one pixel spans one block from the eye.
    right: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;

struct Instance {
    // Camera-relative start in xyz, the core's width in blocks in w.
    @location(0) a: vec4<f32>,
    // Camera-relative end in xyz; w is unused.
    @location(1) b: vec4<f32>,
    // Linear light in rgb, already scaled by the flicker and the segment's
    // strength; a is unused.
    @location(2) colour: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    // -1 at one edge of the ribbon, 1 at the other.
    @location(0) across: f32,
    @location(1) colour: vec3<f32>,
};

// How many times wider than the core the glow is, and how bright.
const GLOW_WIDTH: f32 = 4.0;
const GLOW_LIGHT: f32 = 0.18;
// The fewest pixels across the core and the glow are drawn: under about a
// pixel a line is lit in some rows and missed in the others, and a bolt a few
// hundred blocks off reads as a dotted line or not at all.
const CORE_PIXELS: f32 = 1.5;
const GLOW_PIXELS: f32 = 6.0;

@vertex
fn vertex_main(@builtin(vertex_index) index: u32, instance: Instance) -> VertexOut {
    // Along the segment in x (0 at `a`, 1 at `b`), across it in y.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[index % 6u];
    let core = index >= 6u;
    let a = instance.a.xyz;
    let b = instance.b.xyz;

    // The camera is the origin, so the middle IS the line of sight to it. A
    // segment pointing straight at the eye has no side to turn to, and takes
    // the screen's.
    let square = cross(b - a, (a + b) * 0.5);
    let reach = length(square);
    var side = view.right.xyz;
    if (reach > 1e-6) {
        side = square / reach;
    }

    let at = mix(a, b, corner.x);
    let centre = view.view_projection * vec4<f32>(at, 1.0);
    // Blocks per pixel here: the depth times the span at one block.
    let pixel = max(centre.w, 0.0) * view.right.w;
    let width = instance.a.w;
    let across = select(
        max(width * GLOW_WIDTH, GLOW_PIXELS * pixel),
        max(width, CORE_PIXELS * pixel),
        core,
    );

    var out: VertexOut;
    out.clip = view.view_projection * vec4<f32>(at + side * (corner.y * across * 0.5), 1.0);
    out.across = corner.y;
    out.colour = instance.colour.rgb * select(GLOW_LIGHT, 1.0, core);
    return out;
}

@fragment
fn fragment_main(input: VertexOut) -> @location(0) vec4<f32> {
    // Brightest down the middle and nothing at the edges, so neither layer
    // has a hard side to give its quad away.
    let falloff = 1.0 - input.across * input.across;
    return vec4<f32>(input.colour * falloff, falloff);
}
