// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

// Particles: soft round sprites facing the camera. See `crate::particles`.
//
// # Its own uniform, not the world's globals
//
// A particle needs the camera's right AND up to face it squarely, the fog, and
// nothing else the world shader reads. Its own small block says exactly that,
// and keeps this pass from depending on the layout of a 464-byte struct three
// other shaders already copy by hand.
//
// # No geometry buffer
//
// The quad is built here from `vertex_index`, as `blob.wgsl` builds a shadow's:
// a particle costs one instance and no vertices.
//
// # Blended, unsorted
//
// Depth-tested against the world and not written, so a particle behind a wall
// is hidden and particles never hide each other. They are not sorted: they are
// small, soft and short-lived, and the difference sorting makes to a spray is
// not one anybody sees — which is also §8.1's position on panes.

struct View {
    view_projection: mat4x4<f32>,
    // The camera's right in xyz, and the sky fog's far distance in w.
    right: vec4<f32>,
    // The camera's up in xyz, and the fog curve's exponent in w.
    up: vec4<f32>,
    // The sky's colour in xyz; in w, 1 when this pass fogs (modes 1 and 2) and
    // 0 when the post chain does (mode 3).
    sky: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;

struct Instance {
    // Camera-relative centre in xyz, size in blocks in w.
    @location(0) centre: vec4<f32>,
    // Lit colour and opacity now.
    @location(1) colour: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) colour: vec4<f32>,
    @location(2) distance: f32,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32, instance: Instance) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    let corner = corners[index];
    let half = instance.centre.w * 0.5;
    let at = instance.centre.xyz + (view.right.xyz * corner.x + view.up.xyz * corner.y) * half;

    var out: VertexOut;
    out.clip = view.view_projection * vec4<f32>(at, 1.0);
    out.local = corner;
    out.colour = instance.colour;
    out.distance = length(instance.centre.xyz);
    return out;
}

@fragment
fn fragment_main(input: VertexOut) -> @location(0) vec4<f32> {
    // Round and soft, the way the blob shadow's disc is, so a particle has no
    // square corners to give its quad away.
    let falloff = 1.0 - smoothstep(0.4, 1.0, length(input.local));
    let alpha = input.colour.a * falloff;
    if (alpha <= 0.002) {
        discard;
    }
    var colour = input.colour.rgb;
    if (view.sky.w > 0.5) {
        // The sky fog's power curve, as `world.wgsl` has it: a spray at the
        // edge of the view fades with the terrain behind it.
        let haze = pow(clamp(input.distance / max(view.right.w, 0.001), 0.0, 1.0), view.up.w);
        colour = mix(colour, view.sky.rgb, haze);
    }
    return vec4<f32>(colour, alpha);
}
