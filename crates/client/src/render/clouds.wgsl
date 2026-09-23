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
    // The three genera beside cumulus, each a share of the sky — weather ask
    // W13: stratocumulus (x), altocumulus (y), cumulonimbus (z), and a spare.
    // `weather.x` is cumulus, and keeps being what every mod sends.
    genera: vec4<f32>,
    // The most of each genus anywhere in this sky — the player's own share or
    // the map's highest cell — for the slab the march clips to (ask W16).
    genera_reach: vec4<f32>,
    // The shade map's corner x and z in world blocks, and its side — weather
    // ask W11: the deck seen from straight below, one texel a large cube,
    // drawn for the terrain pass to darken its sun by.
    shadow: vec4<f32>,
    // The cover map's corner x and z, its cell in blocks, and how many cells a
    // side — zero for "no map", which is every world until a mod sends one.
    // Weather ask W10.
    map: vec4<f32>,
    // How much of the star catalog shows, the day's turn as (cos, sin), and
    // whether `fragment_main` draws the stars (1) or `resolve_main` does (0).
    stars: vec4<f32>,
    // The five shares per cell as bytes, two cells to a vec4<u32>, row-major
    // by z: a cell's first word holds cover, darkness, stratocumulus and
    // altocumulus a byte each from the low end, its second cumulonimbus
    // (ask W16).
    //
    // **A vec4 array, and that is not a preference**: WGSL gives a uniform
    // array element a stride of at least sixteen bytes, so a flat array is
    // not expressible here at all. The Rust side packs to match, and the
    // warning on `view` above applies double — a disagreement about this
    // layout empties the sky rather than failing to compile.
    cells: array<vec4<u32>, 128>,
}

@group(0) @binding(0) var<uniform> clouds: Clouds;
// The deck drawn at a lower resolution, for `resolve_main` to lift into the
// frame — weather ask W15's last step. Bound by the resolve pipeline alone.
@group(0) @binding(1) var deck_colour: texture_2d<f32>;
@group(0) @binding(2) var deck_depth: texture_depth_2d;
// The star catalog, sorted into bins over an octahedral map of the sky so
// a pixel asks the few stars near its direction rather than all two
// thousand. An entry is a direction in the catalog's own frame — three
// floats carried as their bits — and brightness and warmth packed as two
// unorm16s. A bin is an offset and a count into the list. Built in
// `clouds.rs::bin_stars`, in both bind group layouts.
@group(0) @binding(3) var<storage, read> star_bins: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read> star_list: array<vec4<u32>>;

const STAR_BINS_PER_AXIS: u32 = 32u;

// The octahedral map's (u, v) in 0..1 for a direction: `clouds.rs::oct_encode`.
fn oct_encode(d: vec3<f32>) -> vec2<f32> {
    let p = d.xy / (abs(d.x) + abs(d.y) + abs(d.z));
    let signs = select(vec2<f32>(-1.0), vec2<f32>(1.0), p >= vec2<f32>(0.0));
    let q = select(p, (1.0 - abs(p.yx)) * signs, d.z < 0.0);
    return q * 0.5 + 0.5;
}

// The stars along a direction in the world's frame, scaled by how much of
// them the sky shows. `pixel` is the frame's pixel in radians: a star is a
// point, so its core is about a pixel wide whatever the resolution, and the
// bright ones a little wider with a halo.
fn stars_along(direction: vec3<f32>, pixel: f32) -> vec3<f32> {
    let visibility = clouds.stars.x;
    if (visibility <= 0.0) {
        return vec3<f32>(0.0);
    }
    // The world turns under the catalog: the gaze is taken back into the
    // catalog's frame by the day's turn, as `sky::unwheeled` does it.
    let c = clouds.stars.y;
    let s = clouds.stars.z;
    let d = vec3<f32>(
        direction.x * c - direction.y * s,
        direction.y * c + direction.x * s,
        direction.z,
    );
    let uv = clamp(oct_encode(d), vec2<f32>(0.0), vec2<f32>(0.99999));
    let cell = vec2<u32>(uv * f32(STAR_BINS_PER_AXIS));
    let bin = star_bins[cell.y * STAR_BINS_PER_AXIS + cell.x];
    let core = max(0.0016, pixel * 0.9);
    var light = vec3<f32>(0.0);
    for (var i = 0u; i < bin.y; i = i + 1u) {
        let star = star_list[bin.x + i];
        let at = bitcast<vec3<f32>>(star.xyz);
        if (dot(d, at) <= 0.0) {
            continue;
        }
        let bw = unpack2x16unorm(star.w);
        // The sine of the angle between the gaze and the star, which for
        // the angles that matter is the angle.
        let off = length(cross(d, at));
        let radius = core * (1.0 + 1.5 * bw.x);
        let point = 1.0 - smoothstep(0.0, radius, off);
        let halo = 1.0 - smoothstep(0.0, radius * 4.0, off);
        let colour = mix(vec3<f32>(0.70, 0.80, 1.0), vec3<f32>(1.0, 0.86, 0.66), bw.y);
        light = light + colour * bw.x * (point + 0.12 * halo * halo);
    }
    return light * visibility;
}

// The weather over one place: the five shares — cumulus `cover`, darkness,
// and the three genera beside them — from the map where there is one and from
// the single per-player state everywhere else. Weather asks W10 and W16.
//
// **Nearest cell, not bilinear.** A cell is hundreds of blocks and the field
// it feeds is heaps of cloud with their own edges, so an interpolated boundary
// buys nothing a player could see and costs three more fetches on every step
// of every ray. The mod's grid is the resolution of its own weather.
struct Weather {
    cover: f32,
    darkness: f32,
    stratocumulus: f32,
    altocumulus: f32,
    cumulonimbus: f32,
};

fn weather_at(cell_xz: vec2<f32>) -> Weather {
    var plain: Weather;
    plain.cover = clouds.weather.x;
    plain.darkness = clouds.weather.y;
    plain.stratocumulus = clouds.genera.x;
    plain.altocumulus = clouds.genera.y;
    plain.cumulonimbus = clouds.genera.z;
    let size = i32(clouds.map.w);
    if (size <= 0) {
        return plain;
    }
    let cell = max(clouds.map.z, 1.0);
    let local = (cell_xz - vec2<f32>(clouds.map.x, clouds.map.y)) / cell;
    let x = i32(floor(local.x));
    let z = i32(floor(local.y));
    // Outside the grid the single state answers, which is what lets a mod
    // describe the weather it knows about and leave the rest of the world
    // alone.
    if (x < 0 || z < 0 || x >= size || z >= size) {
        return plain;
    }
    let index = z * size + x;
    let packed = clouds.cells[index / 2];
    var first = packed.x;
    var second = packed.y;
    if ((index & 1) == 1) {
        first = packed.z;
        second = packed.w;
    }
    var here: Weather;
    here.cover = f32(first & 255u) / 255.0;
    here.darkness = f32((first >> 8u) & 255u) / 255.0;
    here.stratocumulus = f32((first >> 16u) & 255u) / 255.0;
    here.altocumulus = f32((first >> 24u) & 255u) / 255.0;
    here.cumulonimbus = f32(second & 255u) / 255.0;
    return here;
}

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

// Four independent numbers from ONE mix.
//
// **The field asks a cell four questions** — does a heap live here, where in
// the cell is it, and is it a tall one — and four separate hashes is four
// times the mixing for the same 32 bits of entropy. Eight bits each is 256
// levels: plenty to place a heap inside its own cell, and plenty for a
// coverage roll that only has to differ per heap.
//
// This is most of the field's cost, so it is most of what is worth making
// cheap: the heap search reads nine cells and the detail search another nine.
fn hash4(cell: vec2<f32>, seed: f32) -> vec4<f32> {
    var h = bitcast<u32>(i32(cell.x)) * 374761393u
        + bitcast<u32>(i32(cell.y)) * 668265263u
        + bitcast<u32>(i32(seed)) * 2246822519u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return vec4<f32>(
        f32(h & 255u),
        f32((h >> 8u) & 255u),
        f32((h >> 16u) & 255u),
        f32((h >> 24u) & 255u),
    ) * (1.0 / 255.0);
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

// One column's two intervals, as heights in world y, and how deep into a heap
// it is.
//
// An empty interval has `top <= bottom`, which every test below reads as "no
// cloud here".
struct Column {
    // The low cloud: heaps and sheets over the deck's floor, and a storm's
    // tower.
    lower: vec2<f32>,
    // A storm's anvil, spread over its tower.
    upper: vec2<f32>,
    // A mid-level layer — altocumulus, weather ask W13 — which can sit under
    // an anvil and over a heap in one column, so it is a third interval and
    // not a case of the other two.
    mid: vec2<f32>,
    // 0 at a heap's rim, 1 at its crown. The lighting reads it: thin cloud
    // passes light and thick cloud does not.
    density: f32,
    // How grey the weather is over THIS column — ask W10. Carried rather than
    // read in the fragment for the reason the density is: the march has it in
    // hand where it hits, and a storm three squares east must not colour the
    // fair cloud overhead.
    darkness: f32,
};

fn empty_column() -> Column {
    var column: Column;
    column.lower = vec2<f32>(0.0, -1.0);
    column.upper = vec2<f32>(0.0, -1.0);
    column.mid = vec2<f32>(0.0, -1.0);
    column.density = 0.0;
    column.darkness = 0.0;
    return column;
}

// Worley cells: the nearest and second-nearest feature point to `p`, and a
// hash of the nearest's cell. `F2 - F1` is zero on a border between cells and
// grows towards a cell's middle, which is rounded cells with grooves between
// — the shape a sheet, a floret and a cloudlet all share (weather ask W13).
//
// Nine hashes, like the heap search, and worth the same care about when it
// is asked: each genus asks only where that genus can be.
fn cells(p: vec2<f32>, seed: f32) -> vec3<f32> {
    let home = floor(p);
    var f1 = 9.0;
    var f2 = 9.0;
    var tag = 0.0;
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            let id = home + vec2<f32>(f32(i), f32(j));
            let r = hash4(id, seed);
            let d = length(p - (id + 0.1 + 0.8 * r.xy));
            if (d < f1) {
                f2 = f1;
                f1 = d;
                tag = r.z;
            } else if (d < f2) {
                f2 = d;
            }
        }
    }
    return vec3<f32>(f1, f2, tag);
}

// The three genera beside cumulus — weather ask W13 — each a share of the sky
// a mod sends per player, and each with its own scale in field units. A field
// unit is `1 / frequency` blocks: 500 on Weather's deck.
//
// Stratocumulus: a low sheet `STRATO_DEPTH` of the deck's thickness, of
// rounded cells drawn out into rolls across the wind. On Weather's deck a
// cell is 230 by 135 blocks, a dozen cubes, which is what lets it be round:
// at a quarter of this they were confetti.
const STRATO_DEPTH: f32 = 0.32;
const STRATO_CELL_X: f32 = 0.46;
const STRATO_CELL_Z: f32 = 0.27;
// Altocumulus: a mid-level layer `ALTO_LEVEL` thicknesses over the floor, of
// cloudlets `ALTO_CELL` field units across — 70 blocks on Weather's deck —
// lined up in wave bands, lens-shaped, and at most `ALTO_DEPTH` thick.
const ALTO_LEVEL: f32 = 2.4;
const ALTO_DEPTH: f32 = 0.2;
const ALTO_CELL: f32 = 0.14;
// Cumulonimbus: towers on a lattice `CB_LATTICE` heaps wide, `CB_HEIGHT`
// thicknesses tall at their tallest, under anvils half a lattice cell wide.
// At a share of 1 they are supercells; at less they are fewer and smaller.
const CB_LATTICE: f32 = 7.0;
const CB_HEIGHT: f32 = 5.5;
// How far from its centre a storm can reach a column, in lattice cells: the
// anvil's half width over its flattening, plus its shear.
const CB_REACH: f32 = 0.9;

// What tells one heap from its neighbours — weather ask W15. A bank of
// identical hemispheres on one plane reads as a crop with a flat bottom; the
// numbers below come from one more hash per candidate heap and vary a bank
// far more than another octave of noise would.
//
// How far a heap's long axis is stretched, as a share either way.
const HEAP_STRETCH: f32 = 0.2;
// The crown's shape, as a blend from the quarter power of `1 - d^2` — a
// fuller, flatter-topped bun — to the three-quarter power, a pointed heap;
// halfway is about a hemisphere. The blend alone makes one cloud a pancake
// and the next a tower.
const CROWN_FLATTEST: f32 = 0.18;
const CROWN_RANGE: f32 = 0.6;
// A heap's own height, as a share of what the deck would give it.
const HEIGHT_LEAST: f32 = 0.78;
const HEIGHT_RANGE: f32 = 0.44;
// Where a heap's floor sits, as a share of the deck's thickness over its
// base: `(hash - SIT_CENTRE) * SIT_RANGE`, a little under and rather more
// over. Per HEAP rather than per column, which is what keeps the terracing
// W12 removed from coming back: nothing here is a function of the field.
const SIT_RANGE: f32 = 0.15;
const SIT_CENTRE: f32 = 0.4;
// How far the underside lifts towards a heap's own rim, as a share of the
// thickness times the square of the distance from the crown, and how much a
// strong heap's floor rises with it.
const UNDER_LIFT: f32 = 0.10;
const UNDER_SWELL: f32 = 0.03;
// How deep the small cubes ruffle the underside where the camera is close
// enough to see them, in small cubes.
const UNDER_RUFFLE: f32 = 0.6;

// The field at one column of the grid.
//
// # Heaps, not a thresholded height field
//
// The first version read a fractal field as a height: cloud where it crossed
// a threshold, the top rising with the field's value. That gives CONTOURS —
// terraced undersides following the field's own level lines, plateaus and
// ramps for tops, and one continent with holes in it rather than clouds.
// Reported from the window as "a noise pattern running through the sky", and
// the mod author's side-by-side made it unarguable.
//
// So a cloud is a HEAP: points scattered on a grid, one heap each, and each
// heap a hemisphere `sqrt(1 - d^2)` over a flat base. A hemisphere rises
// steeply at the rim and rounds over the crown, which is what "bulbous" is.
// The coverage roll is made AT THE POINT rather than per column, so a heap is
// kept or dropped whole and is always round — the thing a per-column
// threshold can never be.
//
// `detail_mix` is how much of the smaller scale applies, which fades with
// distance rather than switching off: a visible line where the detail starts
// is worse than no detail at all.
// `y_lo..y_hi` is the span of heights the caller can see in this column — a
// ray's segment through the cell, or a point. **Each genus is asked only
// where it can be.** A storm's anvil stands five thicknesses over the floor,
// so the slab a ray marches is a kilometre tall wherever a mod has sent
// cumulonimbus, and most of it is empty: without this every column on the
// way up through it would run the heap search, the sheet's cells and the
// lattice for cloud that could not be there.
fn column_at(cell_xz: vec2<f32>, detail_mix: f32, y_lo: f32, y_hi: f32) -> Column {
    let base = clouds.sun_direction.w;
    let thickness = clouds.sun.w;
    // **The weather over THIS column**, not over the player — ask W10. Every
    // caller of this reaches it through the same sample, so the march, the
    // sun's shadow and the fog inside the deck cannot disagree about where the
    // storm is.
    let weather = weather_at(cell_xz);
    let cover = weather.cover;
    let frequency = clouds.weather.z;
    let towers = clouds.weather.w;
    let seed = clouds.motion.w;
    let cell = clouds.colour.w;
    let small = clouds.shade.w;
    // The genera over THIS column too (ask W16): a storm over the next valley
    // has its sheet and its anvil from here.
    let strato = weather.stratocumulus;
    let alto = weather.altocumulus;
    let cb = weather.cumulonimbus;

    // Drift is a rigid translation of the whole deck and evolution is a slow
    // change of shape; both are an offset on where the field is sampled, which
    // is the whole reason this costs nothing to animate.
    let seconds = clouds.camera.w;
    let drift = vec2<f32>(clouds.motion.x, clouds.motion.y) * seconds;
    let evolve = clouds.motion.z * seconds;
    let at = (cell_xz - drift) * frequency + vec2<f32>(evolve, -evolve);

    var column = empty_column();
    column.darkness = weather.darkness;

    // The rind: a cube or so of low-frequency noise on every top, near the
    // camera only — so a crown is not a perfect arc and a sheet is not a
    // plane (ask W13). Seven cycles per field unit; at twenty-two it turned
    // every crown to confetti at sixteen-block cubes.
    var rind = 0.0;
    if (detail_mix > 0.0) {
        rind = small * 1.2 * detail_mix * (value2(at * 7.0, seed + 91.0) - 0.4);
    }

    // The low cloud — heaps, the sheet and a storm's tower — shares the lower
    // interval: over the lowest floor any of them has, up to the tallest top.
    var low_bottom = 1e30;
    var low_top = -1e30;
    var density = 0.0;

    // ------------------------------------------------------------- cumulus
    // The heaps of W12 and W15, where the ray can meet them: from the lowest
    // a floor sits to the tallest heap with its florets on.
    let sits_low = thickness * SIT_RANGE * SIT_CENTRE + small * UNDER_RUFFLE;
    let heaps_high = base + thickness * SIT_RANGE * (1.0 - SIT_CENTRE)
        + thickness * 0.55 * (HEIGHT_LEAST + HEIGHT_RANGE + towers * 1.2) * 1.3
        + small * 1.2 + cell;
    if (y_hi >= base - sits_low && y_lo <= heaps_high) {
        // Cover raises the water line rather than scaling the field, so a clear
        // sky is a few heaps and an overcast one is a ceiling with holes. The low
        // end goes BELOW zero: overcast should leave almost nothing.
        let threshold = mix(0.95, -0.10, clamp(cover, 0.0, 1.0));

        let spacing = 0.42;
        let home = floor(at / spacing);
        let local = at / spacing;
        var crown = 0.0;
        var tall = 0.0;
        var sits = 0.0;
        var swell = 0.0;
        // The widest a heap can be, in cells: its largest radius, stretched, less
        // the margin its point keeps from its cell's edge. A candidate cell whose
        // edge is further from this column than that cannot reach it whatever its
        // hashes say, and finding that out is two subtractions against the four
        // rounds a hash costs — in every column of every ray (ask W15).
        let reach = (0.30 + 0.38) * (1.0 + HEAP_STRETCH) - 0.15;
        // The nine cells a heap could reach this column from. Three by three
        // because a heap's radius may exceed its own cell — which is what lets
        // neighbouring heaps merge into a bank rather than sitting in a grid.
        for (var j = -1; j <= 1; j = j + 1) {
            for (var i = -1; i <= 1; i = i + 1) {
                let id = home + vec2<f32>(f32(i), f32(j));
                let gap = abs(local - (id + 0.5)) - 0.5;
                if (max(gap.x, gap.y) > reach) {
                    continue;
                }
                // **One hash for the roll, not a noise lookup.** A value noise
                // here is four hashes and an interpolation for a number that only
                // has to differ per heap, and the smooth field it would give is
                // not wanted: a heap is kept or dropped whole.
                let roll = hash4(id, seed);
                // **Heaps come in clumps, so the roll is not independent.** An
                // independent roll per heap scatters them evenly and the sky
                // fills with a sheet; correlating neighbours gives clumps with
                // clear sky between, which is what a fair-weather sky is.
                //
                // The correlation is one hash on a COARSER lattice rather than an
                // interpolated field: a field is four hashes for a number whose
                // only job is to make neighbours agree, and it cost more than the
                // whole rest of the column. The lattice's edges fall between
                // heaps rather than through them, because a heap is a disc and
                // the lattice is two and a half heaps wide.
                let clump = hash2(floor(id * 0.4), seed + 7.0);
                let raw = 0.62 * clump + 0.38 * roll.x;
                let stem = clamp((raw - 0.5) * 2.4 + 0.5, 0.0, 1.0);
                if (stem <= threshold) {
                    continue;
                }
                let strength = clamp((stem - threshold) / max(1.0 - threshold, 0.0001), 0.0, 1.0);
                let point = (id + 0.15 + 0.7 * roll.yz) * spacing;
                let radius = spacing * (0.30 + 0.38 * sqrt(strength));
                // The cheap question first: is this column within the widest the
                // heap could be, stretched? Most candidates are not, and the hash
                // below would be spent on a miss.
                let offset = at - point;
                let widest = radius * (1.0 + HEAP_STRETCH);
                if (dot(offset, offset) > widest * widest) {
                    continue;
                }
                // A heap is not a circle and not the same dome as its neighbour.
                // One more hash gives it a long axis, a crown of its own, a height
                // of its own and a floor of its own — the constants above say how
                // much of each.
                let shape = hash4(id, seed + 67.0);
                let stretch = vec2<f32>(
                    1.0 + 2.0 * HEAP_STRETCH * (shape.x - 0.5),
                    1.0 - 2.0 * HEAP_STRETCH * (shape.x - 0.5),
                );
                let d = length(offset / stretch) / radius;
                if (d < 1.0) {
                    // The crown, from flat bun to pointed heap. **Not `pow`**,
                    // which is a log and an exp for every candidate on every step
                    // of every ray, and measured at twice the deck's whole cost:
                    // two square roots give the quarter and three-quarter powers
                    // and the heap's own number blends between them.
                    let dome = 1.0 - d * d;
                    let root = sqrt(dome);
                    let quarter = sqrt(root);
                    let h = mix(quarter, root * quarter, CROWN_FLATTEST + CROWN_RANGE * shape.y);
                    if (h > crown) {
                        crown = h;
                        // Towers are a SHARE of heaps grown taller, not a second
                        // interval floating over a gap: the gap made shelves,
                        // which read as noise for the same reason the terraces
                        // did.
                        tall = (0.45 + 0.55 * strength) * (HEIGHT_LEAST + HEIGHT_RANGE * shape.z)
                            + towers * select(0.0, 1.2, roll.w > 0.7);
                        sits = (shape.w - SIT_CENTRE) * SIT_RANGE;
                        swell = strength;
                    }
                }
            }
        }
        if (crown > 0.0) {
            // Florets: lobes a third of a heap across, riding the crown and
            // fading at the rim (ask W13). Worley cells rather than the small
            // heaps that were here — rounded cells with grooves between them
            // are what "lumpy rather than a smooth arc" is — and they do not
            // fade with distance the way the small heaps did, so at a
            // kilometre a crown still has its lobes.
            var lobe = 0.0;
            if (crown > 0.08) {
                let florets = cells(at / (spacing * 0.3), seed + 41.0);
                lobe = sqrt(clamp((florets.y - florets.x) * 2.2, 0.0, 1.0));
            }
            let shoulders = smoothstep(0.0, 0.5, crown);
            // **A base is flat per HEAP, not per deck** (ask W15). Each heap
            // sits at its own level, its underside lifts towards its own rim,
            // and the rind ruffles it where the camera is close enough to
            // see. None of it is a function of the FIELD, which is what
            // terraced W12's undersides, so the terracing cannot come back.
            let floor_y = base + thickness * sits;
            let top = floor_y + thickness * 0.55 * tall * (crown + 0.3 * lobe * shoulders)
                + rind * shoulders;
            let rim = 1.0 - crown;
            let under = floor_y + thickness * (UNDER_LIFT * rim * rim + UNDER_SWELL * swell)
                - max(rind, 0.0) * UNDER_RUFFLE;
            // Never thinner than one small cube, and never lower than one
            // large cube over its own floor.
            low_bottom = min(low_bottom, min(under, top - small));
            low_top = max(low_top, max(top, floor_y + cell));
            density = max(density, crown);
        }
    }

    // ------------------------------------------------------- stratocumulus
    // A low sheet of rounded cells drawn out into rolls across the wind, in
    // patches, with grooves of sky between the cells that close as the share
    // rises. Thin: a third of the deck's thickness at most.
    let strato_high = base + thickness * STRATO_DEPTH + small * 1.2 + cell;
    if (strato > 0.0 && y_hi >= base && y_lo <= strato_high) {
        let area = value2(at * 0.9, seed + 13.0);
        if (area < strato * 1.15) {
            let v = cells(at / vec2<f32>(STRATO_CELL_X, STRATO_CELL_Z), seed + 17.0);
            let groove = mix(0.22, 0.06, strato);
            let edge = v.y - v.x;
            if (edge > groove && v.z < 0.55 + 0.5 * strato) {
                let t = smoothstep(groove, groove + 0.45, edge);
                let fade = smoothstep(strato * 1.15, strato * 1.15 - 0.15, area);
                let top = base
                    + thickness * STRATO_DEPTH * (0.3 + 0.7 * sqrt(t)) * (0.5 + 0.5 * fade)
                    + rind;
                low_bottom = min(low_bottom, base);
                low_top = max(low_top, max(top, base + cell));
                density = max(density, t * 0.7);
            }
        }
    }

    // -------------------------------------------------------- cumulonimbus
    // Towers under spreading anvils, on a lattice seven heaps wide. More of
    // the lattice holds a storm as the share rises, and each is taller and
    // wider; at 1 they are supercells, with mammatus under the anvil.
    if (cb > 0.0) {
        let scale = mix(0.55, 1.0, cb);
        let tallest = thickness * CB_HEIGHT * scale;
        if (y_hi >= base && y_lo <= base + tallest * 1.04 + thickness * 0.8 + small * 1.2) {
            let big = 0.42 * CB_LATTICE;
            let home_b = floor(at / big);
            // Florets on a tower's flanks and the mammatus under an anvil are
            // the same Worley cells, larger: asked once, and only once a
            // storm turns out to reach this column.
            var lobe = -1.0;
            for (var j = -1; j <= 1; j = j + 1) {
                for (var i = -1; i <= 1; i = i + 1) {
                    let id = home_b + vec2<f32>(f32(i), f32(j));
                    let r = hash4(id, seed + 31.0);
                    if (r.x > 0.15 + 0.3 * cb) {
                        continue;
                    }
                    let centre = (id + 0.3 + 0.4 * r.yz) * big;
                    let from_centre = length(at - centre);
                    if (from_centre > big * CB_REACH * scale) {
                        continue;
                    }
                    if (lobe < 0.0) {
                        let florets = cells(at / (big * 0.05), seed + 43.0);
                        lobe = sqrt(clamp((florets.y - florets.x) * 2.2, 0.0, 1.0));
                    }
                    let height = thickness * CB_HEIGHT * (0.8 + 0.2 * r.w) * scale;
                    let tower_r = big * 0.24 * scale;
                    let d_t = from_centre / tower_r;
                    if (d_t < 1.0) {
                        // A tower is a tall, flat-topped heap: the same two
                        // square roots as a cumulus crown, blended nearer the
                        // flat end.
                        let dome = 1.0 - d_t * d_t;
                        let root = sqrt(dome);
                        let body = mix(sqrt(root), root, 0.28);
                        let top = base + height * 0.88 * body
                            + thickness * 0.8 * lobe * body * (1.0 - body * 0.6) + rind;
                        if (top > low_top) {
                            low_top = top;
                            density = max(density, body);
                        }
                        low_bottom = min(low_bottom, base);
                    }
                    // The anvil: sheared downwind of the tower, flattened
                    // across it, its top thinning to nothing at the rim
                    // rather than ending in a sheer edge, and flaring down
                    // into the tower that holds it up.
                    let shear = vec2<f32>(big * 0.16, big * 0.04) * scale;
                    let d_a = length((at - centre - shear) * vec2<f32>(0.75, 1.1)) / (big * 0.5 * scale);
                    if (d_a < 1.0) {
                        let edge = sqrt(1.0 - d_a * d_a);
                        let ceiling = base + height * 0.9;
                        let crest = height * 0.1 * sqrt(clamp(1.0 - d_t * d_t * 2.2, 0.0, 1.0));
                        let top = ceiling + height * 0.04 * edge + crest - height * 0.03 * (1.0 - edge);
                        let from_tower = clamp(from_centre / (big * 0.5 * scale), 0.0, 1.0);
                        let flare = height * 0.42 * (1.0 - from_tower) * (1.0 - from_tower);
                        let mammatus = thickness * 0.4 * cb * cb * lobe * edge * smoothstep(0.35, 0.8, d_a);
                        let bottom = ceiling - height * 0.08 * edge - flare - mammatus;
                        if (top - bottom > cell * 0.5) {
                            if (column.upper.y <= column.upper.x) {
                                column.upper = vec2<f32>(bottom, top);
                            } else {
                                column.upper = vec2<f32>(
                                    min(column.upper.x, bottom),
                                    max(column.upper.y, top),
                                );
                            }
                            density = max(density, edge * 0.8);
                        }
                    }
                }
            }
        }
    }

    // --------------------------------------------------------- altocumulus
    // A mid-level layer of small cloudlets, in patches, lined up in bands the
    // way a mackerel sky is. Thin, lens-shaped, and its own interval, since
    // it can sit under an anvil and over a heap in one column.
    if (alto > 0.0) {
        let level = base + thickness * ALTO_LEVEL;
        let deepest = thickness * ALTO_DEPTH;
        if (y_hi >= level - deepest * 0.35 - small && y_lo <= level + deepest + small) {
            let area = value2(at * 0.6 + vec2<f32>(5.0, 9.0), seed + 57.0);
            if (area < alto * 1.1) {
                // A sine across the field, bent by a little noise so the
                // bands are waves rather than rulings.
                let band = 0.5 + 0.5 * sin(dot(at, vec2<f32>(0.82, 0.57)) * 16.0
                    + value2(at * 2.0, seed + 3.0) * 4.0);
                if (band > 0.55 - 0.3 * alto) {
                    let v = cells(at / ALTO_CELL, seed + 61.0);
                    let edge = v.y - v.x;
                    if (edge > 0.22) {
                        let t = smoothstep(0.22, 0.5, edge);
                        let fade = smoothstep(alto * 1.1, alto * 1.1 - 0.12, area);
                        // Lens-shaped: it grows up more than down.
                        let depth = max(deepest * t * (0.4 + 0.6 * fade), cell);
                        column.mid = vec2<f32>(level - depth * 0.35, level + depth);
                        density = max(density, t * 0.5);
                    }
                }
            }
        }
    }

    if (low_top > low_bottom) {
        column.lower = vec2<f32>(low_bottom, low_top);
    }
    column.density = density;
    return column;
}

// Whether a height is inside any of the three intervals.
fn inside(column: Column, y: f32) -> bool {
    return (y >= column.lower.x && y <= column.lower.y)
        || (y >= column.upper.x && y <= column.upper.y)
        || (y >= column.mid.x && y <= column.mid.y);
}

// Where a ray segment first enters solid cloud in one column, as a `t`, or a
// negative number for no hit. `face_y` comes back 1 when the entry was through
// a horizontal face and 0 when it was through the column's side.
fn enter_column(column: Column, origin_y: f32, dir_y: f32, t0: f32, t1: f32) -> vec2<f32> {
    var best = -1.0;
    var face = 0.0;
    for (var which = 0; which < 3; which = which + 1) {
        var span = column.lower;
        if (which == 1) {
            span = column.upper;
        } else if (which == 2) {
            span = column.mid;
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
        let column = column_at(floor(at.xz / cell) * cell + cell * 0.5, 0.0, at.y, at.y);
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
    /// How grey the weather over the hit is — ask W10, and carried for the
    /// same reason the density below is.
    darkness: f32,
    /// How deep into a heap the hit was: 0 at the rim, 1 at the crown.
    ///
    /// **Carried out rather than looked up again.** The march has the column
    /// in hand at the moment it hits; re-reading it in the fragment is the
    /// whole field evaluated a second time for a number already known.
    density: f32,
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

// The grid a ray should walk at `distance`, so that a cell covers about
// `want` pixels.
//
// Quantised to powers of two so that two neighbouring rays at slightly
// different distances land on the SAME grid rather than on two that disagree
// by a fraction of a cell, which would shimmer as the camera moved.
fn grid_for(base_cell: f32, distance: f32, want: f32) -> f32 {
    let pixels = max(base_cell / max(distance * clouds.view.x, 0.000001), 0.0001);
    let steps = clamp(want / pixels, 1.0, 32.0);
    return base_cell * exp2(ceil(log2(steps)));
}

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
    out.density = 0.0;
    out.darkness = clouds.weather.y;

    let base = clouds.sun_direction.w;
    let thickness = clouds.sun.w;
    let towers = clouds.weather.w;
    let base_cell = clouds.colour.w;
    let cell0 = base_cell;
    let detail_reach = clouds.quality.y;

    // The slab the whole deck lives in, so a ray that never reaches it costs
    // nothing — see `deck_slab` for how tight it is and why.
    let slab = deck_slab();
    let deck_low = slab.x;
    let deck_high = slab.y;
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
    // Grow the cell until it covers at least `WANTED` pixels. A cube at a
    // kilometre is under a pixel across, and a cube under a pixel cannot be
    // drawn — only aliased, because neighbouring columns differ by a whole
    // cell wherever the detail bites.
    //
    // **Nine, from six, from three.** Three was measured by nobody; six was,
    // at 960 x 540, and nothing visible was lost; nine is the designer's own
    // call in ask W15 — pictured against six and hard to tell apart — and
    // the largest saving of that pass, a third fewer cells walked along
    // every ray.
    let want = 9.0;
    var cell = grid_for(base_cell, t_enter, want);
    out.cell = cell;

    // Set the analyser up on the cell the ray enters the slab in.
    var entry = origin + direction * t_enter;
    var cell_index = floor(entry.xz / cell);
    let step = sign(direction.xz);
    // How far along the ray one whole cell of travel is, per axis. A ray with
    // no component on an axis never crosses one of its boundaries, which is
    // what the huge number stands for.
    var delta = select(
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
        // The heights this segment of the ray passes through, so the column
        // asks only the genera that can be there.
        let y_from = origin.y + direction.y * t;
        let y_to = origin.y + direction.y * leave;
        let column = column_at(cell_xz, detail_mix, min(y_from, y_to), max(y_from, y_to));
        let found = enter_column(column, origin.y, direction.y, t, leave);
        if (found.x >= 0.0) {
            out.hit = true;
            out.t = found.x;
            out.position = origin + direction * found.x;
            out.face_y = found.y;
            out.density = column.density;
            out.darkness = column.darkness;
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

        // **Coarsen as the ray GOES, not only where it came in.** A ray at the
        // horizon enters the deck near and leaves it kilometres away, so a
        // grid chosen once at entry walks hundreds of fine cells through air
        // it can only ever see as a pixel or two — which is most of the cost
        // of the worst view there is. When the cell it is walking has dropped
        // under the pixel target, double it and re-seat.
        let wanted = grid_for(base_cell, t, want);
        if (wanted > cell) {
            cell = wanted;
            out.cell = cell;
            entry = origin + direction * t;
            cell_index = floor(entry.xz / cell);
            delta = select(
                vec2<f32>(1e30, 1e30),
                abs(vec2<f32>(cell, cell) / direction.xz),
                abs(direction.xz) > vec2<f32>(0.00001, 0.00001),
            );
            let edge = (cell_index + max(step, vec2<f32>(0.0, 0.0))) * cell;
            next = select(
                vec2<f32>(1e30, 1e30),
                t + (edge - entry.xz) / direction.xz,
                abs(direction.xz) > vec2<f32>(0.00001, 0.00001),
            );
            // The re-seat crossed no wall, so the next hit's face comes from
            // the slab rather than from a boundary this grid never had.
            entered = -1;
        }
    }
    return out;
}

// The slab the whole deck lives in, as the lowest and highest a column can
// reach, in world y — so a ray that never meets it costs nothing, and the
// shade map asks every genus at once.
//
// **Tight against what the field can actually reach.** The lowest a heap's
// floor can sit, less the deepest a ruffle can bite, is the bottom; the
// highest a floor can sit, plus the tallest heap times the most its florets
// can add, is the top of the heaps. **The slab grows with the genera a
// player is under** (ask W13), and only with those: a sky with no storm in
// it is marched no taller than it was. Where there is one the slab is a
// kilometre tall, and `column_at` asks each genus only at the heights it can
// be, so the empty part of it costs a lattice test per cell and not the
// whole field.
fn deck_slab() -> vec2<f32> {
    let base = clouds.sun_direction.w;
    let thickness = clouds.sun.w;
    let towers = clouds.weather.w;
    let cell0 = clouds.colour.w;
    let sits_low = thickness * SIT_RANGE * SIT_CENTRE + clouds.shade.w * UNDER_RUFFLE;
    let sits_high = thickness * SIT_RANGE * (1.0 - SIT_CENTRE);
    let tallest = HEIGHT_LEAST + HEIGHT_RANGE + towers * 1.2;
    let rind_high = clouds.shade.w * 1.2;
    let heaps_high = sits_high + thickness * 0.55 * tallest * 1.3 + rind_high;
    let strato_high = select(
        0.0,
        thickness * STRATO_DEPTH + rind_high,
        clouds.genera_reach.x > 0.0,
    );
    let alto_high = select(
        0.0,
        thickness * (ALTO_LEVEL + ALTO_DEPTH) + clouds.shade.w,
        clouds.genera_reach.y > 0.0,
    );
    let cb_high = select(
        0.0,
        thickness * CB_HEIGHT * mix(0.55, 1.0, clouds.genera_reach.z) * 1.04
            + thickness * 0.8
            + rind_high,
        clouds.genera_reach.z > 0.0,
    );
    return vec2<f32>(
        base - sits_low,
        base + max(max(heaps_high, strato_high), max(alto_high, cb_high)) + cell0,
    );
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
    // The stars, behind everything: only where this pass draws at the
    // frame's own resolution. At a lower one `resolve_main` adds them, so
    // a star stays a point rather than a block of pixels.
    if (clouds.stars.w > 0.5) {
        colour = colour + stars_along(direction, clouds.view.y) * headroom;
    }
    return colour;
}

// What the colour's alpha means: a MARK, not coverage. The pass is opaque —
// every fragment it keeps replaces what was under it — and the alpha says
// which of its two things a pixel is, for the post chain's sake (ask W15).
// Mode 3 fogs every pixel by its depth against the terrain's view distance,
// and a cloud is hundreds of blocks up and kilometres out, so fogged as
// terrain every cloud past the view distance was flat sky. The cloud carries
// its own aerial perspective below; marked, the post chain fogs it as the
// sky beside it instead. The float target keeps the mark and the surface
// pipeline masks it off, because a swapchain's alpha is the window's.
const CLOUD_MARK: f32 = 0.0;
const SKY_MARK: f32 = 1.0;

// The most of a cloud's contrast its own haze may take. Cloud is seen
// through air, and never loses all of it; only the last stretch before the
// deck's reach fades the rest of the way, so the reach is not a line.
const HAZE_MOST: f32 = 0.7;

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
    let here = column_at(floor(origin.xz / cell) * cell + cell * 0.5, 1.0, origin.y, origin.y);
    if (inside(here, origin.y)) {
        let tint = mix(clouds.colour.xyz * clouds.sun.xyz, clouds.shade.xyz, 0.5);
        out.colour = vec4<f32>(mix(tint, clouds.sky.xyz, 0.4), CLOUD_MARK);
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
        out.colour = vec4<f32>(sky_along(direction), SKY_MARK);
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
        // **Wrapped**, so light reaches round a heap rather than stopping dead
        // at its terminator. Cloud is not opaque and a hard terminator is the
        // main thing that makes it read as rock.
        let sunward = clamp((facing + 0.6) / 1.6, 0.0, 1.0);
        let warm = clouds.colour.xyz * clouds.sun.xyz;
        let cool = clouds.shade.xyz * clouds.sky.xyz;
        lit = mix(cool, warm, sunward * sunward);
    }

    if (mode >= 2.0) {
        // Self-shadow: the inside of a heap is darker than its rim because
        // cloud above and sunward of it is in the way.
        //
        // **Six more field lookups for every cloud pixel**, and it is depth
        // rather than detail: past a kilometre it moves a pixel or two of
        // grey (ask W15). Faded with distance and skipped beyond twice the
        // detail's reach, which is most of the sky in the views that cost
        // most.
        let shadow_reach = max(clouds.quality.y * 2.0, 1.0);
        if (found.t < shadow_reach) {
            let near = 1.0 - found.t / shadow_reach;
            let shadow = sun_shadow(found.position, cell);
            lit = lit * (1.0 - 0.55 * shadow * near);
        }
        // The low-sun rim. Grazing angles near the sun light up along a
        // silhouette, which is most of what reads as "golden hour".
        let grazing = 1.0 - abs(dot(normal, direction));
        let rim = pow(clamp(facing * 0.5 + 0.5, 0.0, 1.0), 6.0) * grazing;
        lit = lit + clouds.sun.xyz * rim * 0.9;
    }

    if (mode >= 1.0) {
        // **Light that went in the sunward side comes out of the thin parts.**
        // Real cloud is lit from within, and a surface model without it reads
        // as carved rather than as vapour. Rims and the edges of a heap glow,
        // most when the sun is behind them, and every side gets a little.
        //
        // Density comes off the hit rather than from another field lookup.
        let thin = 1.0 - found.density;
        let behind = pow(clamp(dot(direction, toward_sun), 0.0, 1.0), 3.0);
        let scatter = thin * (0.25 + 0.75 * behind) + 0.12;
        lit = lit + clouds.colour.xyz * clouds.sun.xyz * scatter * 0.55;
    }

    // Storm grey, and a dark haze under the deck — which is what makes a storm
    // read from outside it. Rain seen at a distance is a curtain kilometres
    // away, and precipitation spawns around the player's own camera.
    // **The weather where the hit is**, not where the player is — ask W10. A
    // storm over the next valley greys the cloud over the valley and leaves
    // the fair sky overhead alone, which is the whole of what a front looks
    // like from outside it.
    let darkness = found.darkness;
    // **Darkest at a cloud's base and least on its tops** (ask W13), so an
    // anvil keeps its sun over the sheet that has lost it, and a storm read
    // from outside is a bright top over a dark floor rather than a grey slab.
    let rel = clamp(
        (found.position.y - clouds.sun_direction.w) / max(clouds.sun.w * 1.6, 1.0),
        0.0,
        1.0,
    );
    lit = mix(lit, lit * vec3<f32>(0.30, 0.31, 0.38), darkness * (1.0 - 0.6 * rel));

    // Aerial perspective: distant cloud loses contrast into the horizon. Free,
    // because the march already knows how far away the hit is.
    //
    // **At the DECK's scale, not the terrain's** (ask W15). `sky.w` is where
    // the world's fog is total — the view distance, a few hundred blocks —
    // and the deck stands hundreds of blocks up and runs to the horizon, so
    // measured against that every cloud past the terrain's fog was painted
    // flat sky: from the ground the whole deck read as an outline with
    // nothing inside it. Cloud is seen through air, not through fog. Its
    // haze runs to the deck's own reach and takes at most `HAZE_MOST` of the
    // contrast, so distant cloud still has faces in it.
    let haze_far = max(reach, max(clouds.sky.w, 1.0) * 4.0);
    let haze = clamp(found.t / haze_far, 0.0, 1.0);
    let hazed = mix(lit, clouds.sky.xyz, HAZE_MOST * haze * haze);
    // The last stretch before the reach fades the rest of the way, whatever
    // the fog is doing, so the deck's edge is not a line across the sky.
    let edge = smoothstep(0.85, 1.0, found.t / max(reach, 1.0));
    let painted = mix(hazed, clouds.sky.xyz, edge);

    // The hit's depth, so clouds sort against terrain in BOTH directions. High
    // ground can reach into the deck and a player above it looks down on the
    // tops; a pass that only drew behind the world would be wrong at both.
    let clip = clouds.view_projection * vec4<f32>(found.position - origin, 1.0);
    out.depth = clamp(clip.z / max(clip.w, 0.0001), 0.0, 1.0);
    out.colour = vec4<f32>(painted, CLOUD_MARK);
    return out;
}

// The deck seen from straight below, for the terrain's sake — weather ask
// W11. One texel per large cube over `shadow.z` blocks around the camera,
// each how much cloud stands over that column, so the world pass can darken
// its sun term by the deck along the sun's direction with ONE sample rather
// than the whole field per fragment. Drawn once a frame into a small
// texture, which is also what keeps the field in this file alone.
@fragment
fn shadow_main(in: Varyings) -> @location(0) vec4<f32> {
    // Y flips because clip space counts up and texture coordinates count
    // down; `world.wgsl` turns a position into the same uv.
    let uv = vec2<f32>(in.ndc.x * 0.5 + 0.5, 0.5 - in.ndc.y * 0.5);
    let xz = clouds.shadow.xy + uv * clouds.shadow.z;
    let cell = clouds.colour.w;
    if (cell <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let slab = deck_slab();
    let column = column_at(floor(xz / cell) * cell + cell * 0.5, 0.0, slab.x, slab.y);
    let some = column.lower.y > column.lower.x
        || column.upper.y > column.upper.x
        || column.mid.y > column.mid.x;
    // Cubes are opaque, so a heap throws its whole shadow; the density is
    // what lightens its rim, where the cloud is one cube thick.
    let shade = select(0.0, 0.45 + 0.55 * column.density, some);
    return vec4<f32>(shade, 0.0, 0.0, 1.0);
}

struct Resolved {
    @location(0) colour: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

// Lifts the deck from its smaller target into the frame, one texel to a
// block of pixels — nearest, not filtered, because a cube's edge blurred
// across two pixels is the smear the aesthetic exists to avoid, and a depth
// averaged across an edge is a distance nothing is at. The depth is written,
// so the terrain already drawn keeps its place where it is nearer, and the
// glass, the fluid and the particles drawn after still sort against the
// deck. The mark in alpha comes along with the colour.
@fragment
fn resolve_main(in: Varyings) -> Resolved {
    let size = vec2<f32>(textureDimensions(deck_colour));
    let uv = vec2<f32>(in.ndc.x * 0.5 + 0.5, 0.5 - in.ndc.y * 0.5);
    let at = vec2<i32>(clamp(uv * size, vec2<f32>(0.0), size - vec2<f32>(1.0)));
    var out: Resolved;
    out.colour = textureLoad(deck_colour, at, 0);
    out.depth = textureLoad(deck_depth, at, 0);
    // The stars, at the frame's own resolution, on the pixels the deck left
    // as sky: the mark says which those are.
    if (clouds.stars.w < 0.5 && out.colour.a >= SKY_MARK) {
        let near = clouds.inverse_view_projection * vec4<f32>(in.ndc, 0.0, 1.0);
        let far = clouds.inverse_view_projection * vec4<f32>(in.ndc, 1.0, 1.0);
        let direction = normalize(far.xyz / far.w - near.xyz / near.w);
        let headroom = select(1.0, 2.6, clouds.quality.w >= 2.0);
        out.colour = vec4<f32>(
            out.colour.xyz + stars_along(direction, clouds.view.y) * headroom,
            out.colour.a,
        );
    }
    return out;
}
