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
    // How far to march, how far detail reaches, octaves, lighting mode.
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

    let stem = fbm(at + vec2<f32>(evolve, -evolve), octaves, seed);
    // Cover raises the water line rather than scaling the field, so a clear
    // sky is a few islands and an overcast one is a ceiling with holes.
    let threshold = mix(0.62, 0.02, clamp(cover, 0.0, 1.0));
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
};

// Marches the grid and returns the first solid cell.
fn march(origin: vec3<f32>, direction: vec3<f32>, far: f32) -> Hit {
    var out: Hit;
    out.hit = false;
    out.t = far;
    out.position = origin;
    out.face_y = 0.0;

    let base = clouds.sun_direction.w;
    let thickness = clouds.sun.w;
    let cell = clouds.colour.w;
    let small = max(clouds.shade.w, 0.25);
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

    // The step grows with distance: a cube a kilometre away is well under a
    // pixel, so stepping at its size there is work nobody can see. This is the
    // LOD, and it is one line rather than a tile scheme.
    var t = t_enter;
    var guard = 0;
    loop {
        if (t >= t_leave || guard >= 512) {
            break;
        }
        guard = guard + 1;
        let detail_mix = clamp(1.0 - t / max(detail_reach, 1.0), 0.0, 1.0);
        let step_len = mix(cell, small, detail_mix) * (1.0 + t / max(detail_reach, 1.0));
        let at = origin + direction * t;
        let cell_xz = floor(at.xz / cell) * cell + cell * 0.5;
        let column = column_at(cell_xz, detail_mix);
        let found = enter_column(column, origin.y, direction.y, t, min(t + step_len, t_leave));
        if (found.x >= 0.0) {
            out.hit = true;
            out.t = found.x;
            out.position = origin + direction * found.x;
            out.face_y = found.y;
            return out;
        }
        t = t + step_len;
    }
    return out;
}

// The cube face a hit landed on, as a unit normal.
//
// Taken from where the hit sits in its own cell rather than from which wall
// the march crossed: the march steps along `t` rather than wall to wall, so
// the cell is what it knows. The dominant axis of the offset from the cell's
// centre IS the face, exactly, which is what keeps the silhouette cubic.
fn face_normal(hit: vec3<f32>, cell_xz: vec2<f32>, span: vec2<f32>, face_y: f32) -> vec3<f32> {
    if (face_y > 0.5) {
        return vec3<f32>(0.0, select(-1.0, 1.0, hit.y > (span.x + span.y) * 0.5), 0.0);
    }
    let offset = hit.xz - cell_xz;
    if (abs(offset.x) >= abs(offset.y)) {
        return vec3<f32>(select(-1.0, 1.0, offset.x > 0.0), 0.0, 0.0);
    }
    return vec3<f32>(0.0, 0.0, select(-1.0, 1.0, offset.y > 0.0));
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

    let found = march(origin, direction, reach);
    if (!found.hit) {
        discard;
    }

    let cell_xz = floor(found.position.xz / cell) * cell + cell * 0.5;
    let column = column_at(cell_xz, clamp(1.0 - found.t / max(clouds.quality.y, 1.0), 0.0, 1.0));
    var span = column.lower;
    if (found.position.y >= column.upper.x && found.position.y <= column.upper.y) {
        span = column.upper;
    }
    let normal = face_normal(found.position, cell_xz, span, found.face_y);

    // Mode 1 shades by face direction alone: no sun colour, no sky colour, one
    // ambient. It is the mode that pays for nothing it cannot show.
    let toward_sun = -clouds.sun_direction.xyz;
    let facing = dot(normal, toward_sun);
    var lit = clouds.colour.xyz * (0.72 + 0.28 * max(normal.y, 0.0));

    if (mode >= 2.0) {
        // **Golden hour is BACKLIT.** The sun is at the horizon, so it lights
        // cloud BASES, and the violet-grey belongs to the clouds far from it.
        // A model that shaded "up is lit, down is shaded" would get the one
        // hour this is judged on exactly backwards.
        let sunward = clamp(facing * 0.5 + 0.5, 0.0, 1.0);
        let warm = clouds.colour.xyz * clouds.sun.xyz;
        let cool = clouds.shade.xyz * clouds.sky.xyz;
        lit = mix(cool, warm, sunward * sunward);
    }

    if (mode >= 3.0) {
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
