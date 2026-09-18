// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

// The cloud deck: a ray marched through a field, not a mesh of cubes.
//
// # Why a march
//
// A deck eight kilometres across at eight blocks a cube is twelve million
// cells before any detail, the cauliflower edge exists precisely to defeat
// face merging, and `evolve` means rebuilding all of it for ever. Marching a
// grid costs steps instead of memory, gives exact cube faces by construction
// (a grid march crosses flat walls and flat tops and nothing else), and makes
// drift and evolution an offset on the sample rather than a rebuild.
//
// # The field
//
// At each column of the grid the field gives up to TWO intervals of solid
// cloud. Two rather than one because the reference images' hero cloud
// mushrooms out over its own waist, and one interval — solid from bottom to
// top and nothing else — cannot represent solid, air, solid. The upper lobe
// is driven by a LOWER-frequency field than the stem, which is what makes it
// spread wider than what holds it up.
//
// # Two scales
//
// The march steps at the SMALL cube (`cell / detail`), and the column's
// heights are quantised to the LARGE cube plus a one-small-cube rind driven by
// a higher-frequency field. Big blocky masses with a bumpy skin — which is the
// cauliflower, and it comes out of one uniform march rather than two passes.
// The rind fades out with distance rather than switching off at a radius,
// because a visible line where the detail starts is worse than no detail.

struct Clouds {
    // Clip back to camera-relative space, for turning a pixel into a ray.
    inverse_view_projection: mat4x4<f32>,
    // And forward again, for the depth a hit writes.
    view_projection: mat4x4<f32>,
    // The camera in world blocks (xyz), and seconds since the world began (w).
    // World rather than camera-relative because the field is anchored to the
    // world: a cloud must not travel with the player. f32 is enough — the
    // field's scale is hundreds of blocks, so a fraction of a block of
    // precision at the world's edge is nothing.
    camera: vec4<f32>,
    // How wide one pixel is in radians (x), and three spare.
    //
    // **The LOD is decided against this, not against a distance.** A cube is
    // worth drawing while it covers more than about a pixel, and how far away
    // that is depends on the resolution — so a threshold in blocks is right at
    // one window size and wrong at every other.
    //
    // **Its place in this struct is load-bearing.** A field added here and in
    // the Rust `Uniforms` at two different offsets does not fail to compile:
    // every field after the shorter one reads its neighbour's bytes, and the
    // sky simply empties. That is what happened, and the only symptom was that
    // clouds stopped existing.
    view: vec4<f32>,
    // Which way sunlight travels (xyz), and the deck's floor (w).
    sun_direction: vec4<f32>,
    // The sun's colour (xyz), and the deck's thickness (w).
    sun: vec4<f32>,
    // The sky's colour (xyz), and where distance fog is total (w).
    sky: vec4<f32>,
    // Lit cloud before the sun's colour (xyz), and the large cube (w).
    colour: vec4<f32>,
    // The unlit side before the sky's colour (xyz), and the small cube (w).
    shade: vec4<f32>,
    // cover, darkness, the field's frequency, how much taller towers grow.
    weather: vec4<f32>,
    // drift x, drift z, evolve, the field's seed.
    motion: vec4<f32>,
    // How far to march, how far detail reaches, octaves, and the lighting
    // mode as `LightingMode::code` numbers it: 0 Simple, 1 Classic, 2
    // Beautiful. **The engine's own numbering**, not 1/2/3 — inventing a
    // second one here made Classic draw as Simple and Beautiful draw as
    // Classic, and the only sign was that two of the three pictures were
    // identical.
    quality: vec4<f32>,
}

@group(0) @binding(0) var<uniform> clouds: Clouds;

struct Varyings {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

// The fullscreen triangle: three vertices, no buffers. A quad has a seam down
// its diagonal and rasterises the shared edge twice; one oversized triangle
// clipped to the viewport has neither problem. Same trick as `post.wgsl`.
@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> Varyings {
    var out: Varyings;
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    out.ndc = uv * 2.0 - 1.0;
    out.clip = vec4<f32>(out.ndc, 1.0, 1.0);
    return out;
}

// An integer hash, not the engine's noise: charter rule 4 exempts rendering
// and nothing here is in any determinism hash. It is still SEEDED from the
// world seed, which costs nothing and buys a reproducible screenshot — without
// which "compare this with the reference" has no fixed subject — and two
// players who agree about the cloud they are both looking at.
//
// **Bit mixing rather than `fract(sin(...))`.** The sine trick is the usual
// one-liner and it is not the same on every driver: `sin` is allowed to differ
// in its last bits, and at these magnitudes that is the whole result. A cloud
// field that changed shape when a player updated their graphics driver would
// be a bug nobody could reproduce.
fn hash2(cell: vec2<f32>, seed: f32) -> f32 {
    var h = bitcast<u32>(i32(cell.x)) * 374761393u
        + bitcast<u32>(i32(cell.y)) * 668265263u
        + bitcast<u32>(i32(seed)) * 2246822519u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h) * (1.0 / 4294967296.0);
}

// Value noise with a smooth fade, which is enough for cloud: what a player
// reads is the THRESHOLD, not the noise's own character.
fn value2(p: vec2<f32>, seed: f32) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2(i, seed);
    let b = hash2(i + vec2<f32>(1.0, 0.0), seed);
    let c = hash2(i + vec2<f32>(0.0, 1.0), seed);
    let d = hash2(i + vec2<f32>(1.0, 1.0), seed);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>, octaves: i32, seed: f32) -> f32 {
    var sum = 0.0;
    var amplitude = 0.5;
    var total = 0.0;
    var at = p;
    for (var i = 0; i < octaves; i = i + 1) {
        sum = sum + value2(at, seed + f32(i) * 17.0) * amplitude;
        total = total + amplitude;
        at = at * 2.02;
        amplitude = amplitude * 0.5;
    }
    return sum / max(total, 0.0001);
}

// One column's two intervals, as heights in world y. An empty interval has
// `top <= bottom`, which every test below reads as "no cloud here".
struct Column {
    lower: vec2<f32>,
    upper: vec2<f32>,
};

fn empty_column() -> Column {
    var column: Column;
    column.lower = vec2<f32>(0.0, -1.0);
    column.upper = vec2<f32>(0.0, -1.0);
    return column;
}

// The field at one column of the grid.
//
// `detail_mix` is how much of the small-cube rind applies, which fades with
// distance rather than switching off — a visible line where the detail starts
// is worse than no detail at all.
fn column_at(cell_xz: vec2<f32>, detail_mix: f32) -> Column {
    let base = clouds.sun_direction.w;
    let thickness = clouds.sun.w;
    let cover = clouds.weather.x;
    let frequency = clouds.weather.z;
    let towers = clouds.weather.w;
    let seed = clouds.motion.w;
    let octaves = i32(clouds.quality.z);

    // Drift is a rigid translation of the whole deck and evolution is a slow
    // change of shape; both are an offset on where the field is sampled, which
    // is the whole reason this costs nothing to animate.
    let seconds = clouds.camera.w;
    let drift = vec2<f32>(clouds.motion.x, clouds.motion.y) * seconds;
    let evolve = clouds.motion.z * seconds;
    let at = (cell_xz - drift) * frequency;

    // **Widened before it is compared.** Averaged octaves pile up around the
    // middle — three of them leave a field of roughly 0.5 give or take 0.19 —
    // so a threshold read straight off 0..1 does almost nothing for most of
    // its range and then everything at once. Half a sky of cloud came out as
    // a solid ceiling, which `a_half_covered_sky_has_cloud_and_sky_in_it`
    // caught. Spreading the field first is what makes `cover` mean what it
    // says.
    let raw = fbm(at + vec2<f32>(evolve, -evolve), octaves, seed);
    let stem = clamp((raw - 0.5) * 2.0 + 0.5, 0.0, 1.0);
    // Cover raises the water line rather than scaling the field, so a clear
    // sky is a few islands and an overcast one is a ceiling with holes. The
    // low end goes BELOW zero: overcast should leave almost nothing, and a
    // threshold of zero still lets the field's floor through.
    let threshold = mix(0.95, -0.10, clamp(cover, 0.0, 1.0));
    if (stem <= threshold) {
        return empty_column();
    }
    let strength = clamp((stem - threshold) / max(1.0 - threshold, 0.0001), 0.0, 1.0);

    var column: Column;
    // The lower lobe is most of the deck. Its underside is not the flat base:
    // it lifts where the field is weak, which is what gives the stepped,
    // blocky undersides the references are full of.
    let lift = thickness * 0.18 * (1.0 - strength);
    let rind = clouds.shade.w * detail_mix
        * (fbm(at * 6.0 + vec2<f32>(11.0, 7.0), 2, seed + 91.0) * 2.0 - 1.0);
    column.lower = vec2<f32>(
        base + lift - rind,
        base + thickness * (0.25 + 0.55 * strength) + rind,
    );

    // The anvil. Driven by a LOWER-frequency field than the stem, so it
    // spreads wider than what holds it up — which is what "mushrooms out over
    // its waist" means, and the one thing a single interval cannot do.
    if (towers > 0.0) {
        let spread = fbm(at * 0.45 + vec2<f32>(3.0, -5.0), max(octaves - 1, 1), seed + 53.0);
        let lobe = clamp((spread - 0.5) * 2.2, 0.0, 1.0) * towers;
        if (lobe > 0.02) {
            let gap = thickness * 0.06 * (1.0 - lobe);
            let floor_y = column.lower.y + gap;
            column.upper = vec2<f32>(
                floor_y - rind,
                floor_y + thickness * lobe * 1.4 + rind,
            );
        }
    }
    return column;
}

// Whether a height is inside either interval.
fn inside(column: Column, y: f32) -> bool {
    return (y >= column.lower.x && y <= column.lower.y)
        || (y >= column.upper.x && y <= column.upper.y);
}

// Where a ray segment first enters solid cloud in one column, as a `t`, or a
// negative number for no hit. `face_y` comes back 1 when the entry was through
// a horizontal face and 0 when it was through the column's side.
fn enter_column(column: Column, origin_y: f32, dir_y: f32, t0: f32, t1: f32) -> vec2<f32> {
    var best = -1.0;
    var face = 0.0;
    for (var which = 0; which < 2; which = which + 1) {
        var span = column.lower;
        if (which == 1) {
            span = column.upper;
        }
        if (span.y <= span.x) {
            continue;
        }
        // The segment's overlap with the slab [span.x, span.y].
        var lo = t0;
        var hi = t1;
        var through_face = false;
        if (abs(dir_y) < 0.00001) {
            if (origin_y < span.x || origin_y > span.y) {
                continue;
            }
        } else {
            let a = (span.x - origin_y) / dir_y;
            let b = (span.y - origin_y) / dir_y;
            let near = min(a, b);
            let far = max(a, b);
            if (near > lo) {
                lo = near;
                through_face = true;
            }
            hi = min(hi, far);
        }
        if (hi < lo) {
            continue;
        }
        if (best < 0.0 || lo < best) {
            best = lo;
            face = select(0.0, 1.0, through_face);
        }
    }
    return vec2<f32>(best, face);
}

// How much cloud lies between a point and the sun, as a shadow term.
//
// A short second march rather than a structure: the field is a function, so
// asking "is there cloud above and sunward of this" is just sampling it again.
// Coarse on purpose — a self-shadow reads as depth, not as detail.
fn sun_shadow(hit: vec3<f32>, step_len: f32) -> f32 {
    let toward_sun = -clouds.sun_direction.xyz;
    var blocked = 0.0;
    for (var i = 1; i <= 6; i = i + 1) {
        let at = hit + toward_sun * step_len * f32(i) * 2.0;
        let cell = clouds.colour.w;
        let column = column_at(floor(at.xz / cell) * cell + cell * 0.5, 0.0);
        if (inside(column, at.y)) {
            blocked = blocked + 1.0;
        }
    }
    return blocked / 6.0;
}

struct Hit {
    hit: bool,
    t: f32,
    position: vec3<f32>,
    face_y: f32,
    /// The grid this ray marched on.
    cell: f32,
    /// The face that was crossed, as a unit normal.
    ///
    /// **Taken from the analyser, not from the hit's position.** Working it
    /// out afterwards means asking which cell the point is in, and the point
    /// is exactly ON a cell wall — `floor` puts it either side depending on
    /// the last bit, so the face flips between the two cells that share the
    /// wall and neighbouring pixels disagree. That is what turned solid cloud
    /// into gold confetti. The analyser knows which boundary it crossed, so
    /// there is nothing to infer.
    normal: vec3<f32>,
};

// Marches the grid and returns the first solid cell.
//
// # A DDA, because a fixed step skips cells
//
// The first version advanced `t` by a fixed length and tested whichever column
// that landed in. Wherever the step grew past the cell size — which is most of
// the frame, since the step grows with distance — it skipped columns entirely,
// testing one of them against a segment crossing several. The picture was
// confetti: cubes hit and missed at random along every ray, with no solid
// surface anywhere.
//
// A digital differential analyser steps to the next cell BOUNDARY instead, so
// every cell along the ray is visited exactly once with the exact `t` it was
// entered and left at. That is what makes a silhouette solid, and it is also
// what makes the faces exact — the boundary crossed IS the face.
fn march(origin: vec3<f32>, direction: vec3<f32>, far: f32) -> Hit {
    var out: Hit;
    out.hit = false;
    out.t = far;
    out.position = origin;
    out.face_y = 0.0;
    out.cell = clouds.colour.w;
    out.normal = vec3<f32>(0.0, 1.0, 0.0);

    let base = clouds.sun_direction.w;
    let thickness = clouds.sun.w;
    let base_cell = clouds.colour.w;
    let detail_reach = clouds.quality.y;

    // The slab the whole deck lives in, so a ray that never reaches it costs
    // nothing. The top allows for the anvil, which rises above `thickness`.
    let deck_low = base - thickness * 0.5;
    let deck_high = base + thickness * 2.6;
    var t_enter = 0.0;
    var t_leave = far;
    if (abs(direction.y) < 0.00001) {
        if (origin.y < deck_low || origin.y > deck_high) {
            return out;
        }
    } else {
        let a = (deck_low - origin.y) / direction.y;
        let b = (deck_high - origin.y) / direction.y;
        t_enter = max(t_enter, min(a, b));
        t_leave = min(t_leave, max(a, b));
    }
    if (t_leave <= t_enter) {
        return out;
    }

    // **The grid coarsens with distance, per ray.** A cube whose angular size
    // is under a pixel cannot be drawn, only aliased: neighbouring columns
    // differ by a whole cell where the rind bites, so at a kilometre every
    // pixel samples a different micro-feature and solid cloud turns to
    // confetti. Fading the rind is not enough — the CELLS have to grow.
    //
    // Chosen once per ray from where it meets the deck, so the analyser keeps
    // one uniform grid and stays simple, and quantised to powers of two so
    // that two neighbouring rays at slightly different distances land on the
    // SAME grid rather than on two that disagree by a fraction of a cell,
    // which would shimmer as the camera moved.
    // Grow the cell until it covers at least `WANTED` pixels. A cube at a
    // kilometre is under a pixel across, and a cube under a pixel cannot be
    // drawn — only aliased, because neighbouring columns differ by a whole
    // cell wherever the rind bites.
    let pixels = max(base_cell / max(t_enter * clouds.view.x, 0.000001), 0.0001);
    let steps = clamp(3.0 / pixels, 1.0, 32.0);
    let cell = base_cell * exp2(ceil(log2(steps)));
    out.cell = cell;

    // Set the analyser up on the cell the ray enters the slab in.
    let entry = origin + direction * t_enter;
    var cell_index = floor(entry.xz / cell);
    let step = sign(direction.xz);
    // How far along the ray one whole cell of travel is, per axis. A ray with
    // no component on an axis never crosses one of its boundaries, which is
    // what the huge number stands for.
    let delta = select(
        vec2<f32>(1e30, 1e30),
        abs(vec2<f32>(cell, cell) / direction.xz),
        abs(direction.xz) > vec2<f32>(0.00001, 0.00001),
    );
    // And how far to the FIRST boundary, which depends on where in the cell
    // the ray came in and which way it is going.
    let boundary = (cell_index + max(step, vec2<f32>(0.0, 0.0))) * cell;
    var next = select(
        vec2<f32>(1e30, 1e30),
        t_enter + (boundary - entry.xz) / direction.xz,
        abs(direction.xz) > vec2<f32>(0.00001, 0.00001),
    );

    // Which wall got us into the cell being tested: 0 for an x wall, 1 for a
    // z wall, and -1 for the first, which was entered through the deck's own
    // floor or ceiling rather than through a wall.
    var entered = -1;
    var t = t_enter;
    var guard = 0;
    loop {
        if (t >= t_leave || guard >= 512) {
            break;
        }
        guard = guard + 1;
        let leave = min(min(next.x, next.y), t_leave);
        let detail_mix = clamp(1.0 - t / max(detail_reach, 1.0), 0.0, 1.0);
        let cell_xz = cell_index * cell + cell * 0.5;
        let column = column_at(cell_xz, detail_mix);
        let found = enter_column(column, origin.y, direction.y, t, leave);
        if (found.x >= 0.0) {
            out.hit = true;
            out.t = found.x;
            out.position = origin + direction * found.x;
            out.face_y = found.y;
            // A horizontal face if the ray met the column's top or bottom
            // inside this cell, and otherwise the wall it came in through. A
            // ray travelling downwards that meets a horizontal face met the
            // TOP of the cloud, so the face points up.
            if (found.y > 0.5 || entered < 0) {
                out.normal = vec3<f32>(0.0, select(-1.0, 1.0, direction.y < 0.0), 0.0);
            } else if (entered == 0) {
                out.normal = vec3<f32>(-step.x, 0.0, 0.0);
            } else {
                out.normal = vec3<f32>(0.0, 0.0, -step.y);
            }
            return out;
        }
        // Over the nearer boundary and into the next cell.
        t = leave;
        if (next.x < next.y) {
            cell_index.x = cell_index.x + step.x;
            next.x = next.x + delta.x;
            entered = 0;
        } else {
            cell_index.y = cell_index.y + step.y;
            next.y = next.y + delta.y;
            entered = 1;
        }
    }
    return out;
}

// The sky along one view ray: a gradient, the sun's glow, and its disc.
//
// # Why the horizon keeps the colour it already had
//
// The frame is cleared to one sky colour and the world's fog fades INTO that
// colour — "fog and background must agree or the horizon has a seam exactly
// where the fog was supposed to hide one". So the gradient's horizon is that
// same colour, exactly, and only the zenith moves. Terrain still dissolves
// into a sky that matches it, and looking up gains the depth the reference
// images have.
//
// # Where the colours come from
//
// From the keyframes a mod already registered (charter rule 1): the zenith is
// the mod's own sky colour deepened, the glow is the mod's own sun colour.
// Nothing here invents a palette — it renders the one that was declared more
// faithfully than a flat fill could.
fn sky_along(direction: vec3<f32>) -> vec3<f32> {
    let horizon = clouds.sky.xyz;
    // Deeper and bluer overhead. Multiplying rather than lerping to a constant
    // keeps a mod's own hue: a green sky stays green, and merely darkens.
    let zenith = horizon * vec3<f32>(0.52, 0.66, 0.96);
    let up = clamp(direction.y, 0.0, 1.0);
    // The band near the horizon is where the interesting colour lives, so the
    // gradient is weighted towards it rather than linear in height.
    var colour = mix(horizon, zenith, pow(up, 0.55));

    let toward_sun = -clouds.sun_direction.xyz;
    let alignment = clamp(dot(direction, toward_sun), 0.0, 1.0);
    // The glow, which is most of what reads as golden hour: a wide warm halo
    // around the sun, and a tight one on it.
    //
    // **Only Beautiful may blow out.** It draws into a float target and its
    // post chain tonemaps, so a sun brighter than white survives as one. The
    // other two write straight to an sRGB surface and CLIP, where the same
    // numbers do not make a brighter sun — they make a wider white hole, and
    // the brightness washes into everything sampled near it.
    let headroom = select(1.0, 2.6, clouds.quality.w >= 2.0);
    colour = colour + clouds.sun.xyz * pow(alignment, 8.0) * 0.17 * headroom;
    colour = colour + clouds.sun.xyz * pow(alignment, 128.0) * 0.42 * headroom;
    // And the disc. Small, and deliberately not a texture: it is the one
    // object in the sky whose SIZE a player judges everything else against.
    let disc = smoothstep(0.9994, 0.9997, alignment);
    colour = mix(colour, clouds.sun.xyz * 1.6, disc);
    return colour;
}

struct Painted {
    @location(0) colour: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fragment_main(in: Varyings) -> Painted {
    var out: Painted;

    // The ray for this pixel, in the same camera-relative space the world is
    // drawn in, then lifted to world coordinates for the field.
    let near = clouds.inverse_view_projection * vec4<f32>(in.ndc, 0.0, 1.0);
    let far = clouds.inverse_view_projection * vec4<f32>(in.ndc, 1.0, 1.0);
    let direction = normalize(far.xyz / far.w - near.xyz / near.w);
    let origin = clouds.camera.xyz;

    let cell = clouds.colour.w;
    let reach = clouds.quality.x;
    let mode = clouds.quality.w;

    // **Inside the deck is fog**, not the inside face of a cube. A player can
    // fly up through this, and the moment the camera enters a filled cell the
    // honest picture is that they cannot see.
    let here = column_at(floor(origin.xz / cell) * cell + cell * 0.5, 1.0);
    if (inside(here, origin.y)) {
        let tint = mix(clouds.colour.xyz * clouds.sun.xyz, clouds.shade.xyz, 0.5);
        out.colour = vec4<f32>(mix(tint, clouds.sky.xyz, 0.4), 0.92);
        // Far, so the haze never occludes the world: it is weather, not a
        // surface. Terrain seen from inside a cloud is not tinted by this and
        // that is a known gap rather than a decision.
        out.depth = 1.0;
        return out;
    }

    // No deck, or the player turned clouds off: the sky is still the sky.
    // `cell` of zero is how the pass says there is nothing to march.
    var found: Hit;
    found.hit = false;
    if (cell > 0.0) {
        found = march(origin, direction, reach);
    }
    if (!found.hit) {
        // **Painted, not discarded.** Half of each reference image is sky, and
        // a flat fill behind perfect cubes would not read like them. At the far
        // plane, so anything in the world is in front of it.
        out.colour = vec4<f32>(sky_along(direction), 1.0);
        out.depth = 1.0;
        return out;
    }

    let normal = found.normal;

    // Mode 1 shades by face direction alone: no sun colour, no sky colour, one
    // ambient. It is the mode that pays for nothing it cannot show.
    let toward_sun = -clouds.sun_direction.xyz;
    let facing = dot(normal, toward_sun);
    var lit = clouds.colour.xyz * (0.72 + 0.28 * max(normal.y, 0.0));

    if (mode >= 1.0) {
        // **Golden hour is BACKLIT.** The sun is at the horizon, so it lights
        // cloud BASES, and the violet-grey belongs to the clouds far from it.
        // A model that shaded "up is lit, down is shaded" would get the one
        // hour this is judged on exactly backwards.
        let sunward = clamp(facing * 0.5 + 0.5, 0.0, 1.0);
        let warm = clouds.colour.xyz * clouds.sun.xyz;
        let cool = clouds.shade.xyz * clouds.sky.xyz;
        lit = mix(cool, warm, sunward * sunward);
    }

    if (mode >= 2.0) {
        // Self-shadow: the inside of a heap is darker than its rim because
        // cloud above and sunward of it is in the way.
        let shadow = sun_shadow(found.position, cell);
        lit = lit * (1.0 - 0.55 * shadow);
        // The low-sun rim. Grazing angles near the sun light up along a
        // silhouette, which is most of what reads as "golden hour".
        let grazing = 1.0 - abs(dot(normal, direction));
        let rim = pow(clamp(facing * 0.5 + 0.5, 0.0, 1.0), 6.0) * grazing;
        lit = lit + clouds.sun.xyz * rim * 0.9;
    }

    // Storm grey, and a dark haze under the deck — which is what makes a storm
    // read from outside it. Rain seen at a distance is a curtain kilometres
    // away, and precipitation spawns around the player's own camera.
    let darkness = clouds.weather.y;
    lit = mix(lit, lit * vec3<f32>(0.30, 0.31, 0.38), darkness);

    // Aerial perspective: distant cloud loses contrast into the horizon. Free,
    // because the march already knows how far away the hit is.
    let haze = clamp(found.t / max(clouds.sky.w, 1.0), 0.0, 1.0);
    let painted = mix(lit, clouds.sky.xyz, haze * haze);

    // The hit's depth, so clouds sort against terrain in BOTH directions. High
    // ground can reach into the deck and a player above it looks down on the
    // tops; a pass that only drew behind the world would be wrong at both.
    let clip = clouds.view_projection * vec4<f32>(found.position - origin, 1.0);
    out.depth = clamp(clip.z / max(clip.w, 0.0001), 0.0, 1.0);
    out.colour = vec4<f32>(painted, 1.0);
    return out;
}
