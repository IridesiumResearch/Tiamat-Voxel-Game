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
    // The sky's colour in xyz; in w, where the fog is total straight up or
    // down. Positive in every lighting mode since weather ask W33: mode 3's
    // composite fogs from the scene's depth and painted out a drop over open
    // sky, so mode 3 draws particles after its fog and they fog themselves here,
    // by their own distance. The composite's sun-scattering halo tint is not
    // reproduced: it tints the far sky and a drop is near. Negative still means
    // "do not fog" (tests).
    sky: vec4<f32>,
    // What the fog fades towards where no sky reaches, in xyz — weather ask
    // W29; w unused.
    cave_fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;

struct Instance {
    // Camera-relative centre in xyz, size in blocks in w.
    @location(0) centre: vec4<f32>,
    // Lit colour and opacity now.
    @location(1) colour: vec4<f32>,
    // How much sky reached it where it was scattered, 0 to 1.
    @location(2) sky: f32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) colour: vec4<f32>,
    // How far along the fog's ellipsoid the centre sits, 0 at the eye and 1
    // where the fog is total — `world.wgsl`'s `fog_reach`.
    @location(2) reach: f32,
    // What its fog fades towards: see `fog_colour`.
    @location(3) @interpolate(flat) fog: vec3<f32>,
};

// Where along the fog's reach a cave's fog gives way to the sky's whatever
// the sky light. Must match `world.wgsl`, whose `CAVE_FOG_EDGE` says why.
const CAVE_FOG_EDGE: f32 = 0.85;

// `world.wgsl`'s `fog_colour`, for a particle: the sky's colour by the share
// of the sky that reached it, the cave's by the rest (weather ask W29). Once
// a particle, in the vertex stage — it is lit once a burst, and its centre's
// reach is the whole quad's.
fn fog_colour(sky_light: f32, reach: f32) -> vec3<f32> {
    let open = max(clamp(sky_light, 0.0, 1.0), smoothstep(CAVE_FOG_EDGE, 1.0, reach));
    return mix(view.cave_fog.rgb, view.sky.rgb, open);
}

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
    let sideways = length(instance.centre.xz) / max(view.right.w, 0.001);
    let vertical = abs(instance.centre.y) / max(abs(view.sky.w), 0.001);
    out.reach = sqrt(sideways * sideways + vertical * vertical);
    out.fog = fog_colour(instance.sky, out.reach);
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
    if (view.sky.w > 0.0) {
        // The sky fog's power curve, as `world.wgsl` has it: a spray at the
        // edge of the view fades with the terrain behind it.
        let haze = pow(clamp(input.reach, 0.0, 1.0), view.up.w);
        colour = mix(colour, input.fog, haze);
    }
    return vec4<f32>(colour, alpha);
}

// A picture a mod registered, drawn on the particle instead of the disc —
// Life ask 15. **Group 1, and only the textured pipeline declares it**, so a
// world of plain rain never binds anything: the hot path is thousands of
// particles a frame and a bind group each would cost more than the pixels.
@group(1) @binding(0) var picture: texture_2d<f32>;
@group(1) @binding(1) var picture_sampler: sampler;

@fragment
fn fragment_textured(input: VertexOut) -> @location(0) vec4<f32> {
    // The quad's own -1..1 corner, as a texture coordinate. `v` is flipped
    // because an image's first row is its top and the quad's +y is up, which
    // is the same correction `sprite_vertex` makes in `world.wgsl`.
    let uv = vec2<f32>(input.local.x * 0.5 + 0.5, 0.5 - input.local.y * 0.5);
    let texel = textureSample(picture, picture_sampler, uv);

    // **The picture's own alpha decides the shape**, not the disc falloff: a
    // heart is a heart because of where its pixels are transparent, and a
    // round fade over it would eat its corners.
    let alpha = input.colour.a * texel.a;
    if (alpha <= 0.002) {
        discard;
    }
    // Tinted by the burst's colour, so one white picture serves a row of red
    // hearts and a row of grey ones, and the light the particle was spawned
    // under still applies.
    var colour = texel.rgb * input.colour.rgb;
    if (view.sky.w > 0.0) {
        let haze = pow(clamp(input.reach, 0.0, 1.0), view.up.w);
        colour = mix(colour, input.fog, haze);
    }
    return vec4<f32>(colour, alpha);
}
