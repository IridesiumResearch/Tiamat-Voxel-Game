// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Density fields: 3D terrain and caves, described by a mod and evaluated here.
//!
//! Implements `docs/subnode-contract.md` §5: a density field writes at BLOCK
//! resolution. It never expands a buffer to sub-nodes, so a generator that uses
//! one pays nothing for sub-nodes existing — the opt-in the section describes
//! stays a separate, deliberate act.
//!
//! # Why this exists
//!
//! A mod may not compute simulation values itself. Charter rule 4 guarantees
//! that the same seed produces bit-identical worlds on every supported target,
//! and that guarantee rests on restricting which floating-point operations run
//! — which the engine cannot police inside a script VM. So worldgen has always
//! been "ask for a whole buffer, hand it to a whole-buffer operation", and the
//! only buffer on offer was a heightmap.
//!
//! A heightmap cannot describe an overhang, an arch or a cave. Those need a
//! value at a POINT IN SPACE rather than a height per column, and a mod cannot
//! produce one without doing exactly the per-sample arithmetic that is
//! forbidden.
//!
//! This closes that gap without opening the other: a mod describes an
//! expression as a Lua table, the engine compiles it once, and the engine
//! evaluates it. The mod says WHAT to compute and never computes it.
//!
//! # Whole arrays, not a tree walk per sample
//!
//! Evaluation is a stack machine over ARRAYS. Each operation consumes whole
//! buffers and produces one, so a noise node is a single [`super::fill_3d`]
//! call over the region and an add is one pass over two slices. The obvious
//! implementation — walk the tree once per sample — would call the noise
//! kernel from the innermost loop of an interpreter, which is the shape that
//! defeats every optimisation the fills were written to allow.
//!
//! The cost is memory: one buffer per stack slot, `region.len()` floats each.
//! At block resolution over a chunk that is 4,096 floats, 16 KiB a slot, and
//! [`MAX_DEPTH`] bounds how many can be live.
//!
//! # What it may contain, and why the list is short
//!
//! Every operation here is in the deterministic subset: `+ - * /`, `min`,
//! `max`, `abs`, comparison, and the gradient noise that
//! [`super::noise`] already provides. **No transcendentals**, so there is no
//! `pow`, `sin`, `exp` or `sqrt` node and there will not be one — see
//! `docs/float-determinism.md`. A mod wanting a curve builds it from
//! multiplication, or asks for a shape this module does not have and gets a
//! new node with a determinism argument attached.
//!
//! # What it deliberately is NOT
//!
//! Not a biome system, not an ore placer, not erosion. Those are content and
//! belong in a mod (charter rule 1) — this is the mechanism that makes them
//! expressible. A mod composes `noise`, `y` and arithmetic into whatever idea
//! of a landscape it has; the engine holds no opinion about which idea is
//! right.

use super::noise::{
    BufferSizeMismatch, Fractal, FractalParams, Region3d, fill_3d, fill_3d_stretched,
};

/// How many operations one density program may hold.
///
/// A bound rather than a limit anybody should meet: a hand-written field is a
/// dozen nodes and this is orders above that. It exists because the program
/// arrives from a script and a runaway table should be refused with a message
/// rather than allocated. (512 until the coast's code field — eighteen strata
/// and four zones over a coastline read eight times — came to 658; 1,024 until
/// the world mod's shore programs reached 985 and 939 and its reefs, blowholes
/// and tidal gutters had to come out of the world to fit. World ask 30.)
///
/// **The cap is not the budget, and the numbers say why.** Measured over a 16³
/// region (`cargo run --release -p tiamat-core --example density_cap`, on the
/// machine this was written on):
///
/// | program | ms a chunk | of a 50 ms tick |
/// |---|---|---|
/// | 1,000 ops, 1 noise read | 0.85 | 1.7% |
/// | 1,000 ops, 5 noise reads | 2.36 | 4.7% |
/// | 1,000 ops, 16 noise reads | 6.18 | 12.4% |
/// | 4,000 ops, 1 noise read | 2.32 | 4.6% |
/// | 4,000 ops, 17 noise reads | 8.20 | 16.4% |
///
/// Read off those: **one noise read is about 0.34 ms and a thousand arithmetic
/// operations about 0.5 ms**, so a noise node is worth roughly seven hundred
/// arithmetic ones. Quadrupling the cap therefore buys a mod three thousand
/// more operations for about 1.5 ms a chunk in the worst case — and a program
/// that spends the room on noise was already the expensive thing at the old
/// cap. What actually bounds generation is the serve clock the tick keeps
/// (`docs/performance-targets.md`), which meters chunks however costly each one
/// is; this number only decides when a program is refused outright.
///
/// Raising it moves [`Interval::widened`]'s margin, which is derived from it
/// for exactly that reason: a bound too narrow by an ulp is a bound that can
/// put a hole in a world.
pub const MAX_OPS: usize = 4096;

/// How many array buffers may be live at once.
///
/// Each is `region.len()` floats, so this is the memory bound: sixteen buffers
/// over a chunk at block resolution is 256 KiB. **A refusal bound, not an
/// allocation** — `Scratch` grows to what a program actually uses and is reused
/// between chunks, so a program with three live values pays for three.
///
/// Eight until World ask 30, whose real content is that reuse trades operations
/// for buffers: a value read twice has to stay live between its two uses, and
/// the world mod's shore programs were already at eight. Raising both is what
/// makes either of them usable.
pub const MAX_DEPTH: usize = 16;

/// The range a density program can take over a box, as `[low, high]`.
///
/// Always contains every value the program produces there, and usually more:
/// it is an interval extension, not a measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    /// No sample in the box is below this.
    pub low: f32,
    /// No sample in the box is above this.
    pub high: f32,
}

impl Interval {
    /// The interval holding exactly one value.
    #[must_use]
    pub const fn exactly(value: f32) -> Self {
        Self {
            low: value,
            high: value,
        }
    }

    /// Whether every value in the box is solid — the field is positive
    /// throughout, so a fill can write the material without asking.
    #[must_use]
    pub const fn is_all_solid(self) -> bool {
        self.low > 0.0
    }

    /// Whether no value in the box is solid, so a fill can do nothing at all.
    ///
    /// The comparison is `<=` because [`Density`] treats a sample as solid when
    /// it is strictly greater than zero, and the two rules have to agree
    /// exactly or a plane of cells appears or disappears at the boundary.
    #[must_use]
    pub const fn is_all_empty(self) -> bool {
        self.high <= 0.0
    }

    /// Whether the surface might cross this box, so it has to be evaluated.
    #[must_use]
    pub const fn is_undecided(self) -> bool {
        !self.is_all_solid() && !self.is_all_empty()
    }

    /// Widens by the arithmetic's own error, outwards.
    ///
    /// Interval arithmetic in `f32` rounds to nearest, which can move an end
    /// the WRONG way by half an ulp per operation, and a bound that is too
    /// narrow by one ulp is still a bound that can put a hole in a world.
    /// [`MAX_OPS`] caps the program, and half an ulp is `2⁻²⁴` relative in
    /// `f32`, so `MAX_OPS × 2⁻²⁴` covers the whole accumulation; the absolute
    /// term covers an interval that straddles zero, where relative means
    /// nothing.
    ///
    /// **Derived from the cap and not written out**, because it was written out
    /// (`6.2e-5`, which is 1,024 ops' worth) and raising the cap would have
    /// left it silently too narrow — a bound that is not a bound, and the one
    /// mistake in this file that puts a hole in a world rather than an error on
    /// a screen.
    fn widened(self) -> Self {
        /// Half an ulp, relative, per operation, for the whole program.
        const MARGIN: f32 = MAX_OPS as f32 / 16_777_216.0;
        let magnitude = self.low.abs().max(self.high.abs());
        let margin = magnitude * MARGIN + 1e-6;
        Self {
            low: self.low - margin,
            high: self.high + margin,
        }
    }

    /// The interval containing both ends of a product's four corners.
    fn multiply(self, other: Self) -> Self {
        let corners = [
            self.low * other.low,
            self.low * other.high,
            self.high * other.low,
            self.high * other.high,
        ];
        let mut low = corners[0];
        let mut high = corners[0];
        for corner in &corners[1..] {
            low = low.min(*corner);
            high = high.max(*corner);
        }
        Self { low, high }
    }

    /// The same for a quotient, which has one case a product does not.
    fn divide(self, other: Self) -> Self {
        // **A divisor that straddles zero gives up.** The quotient runs to
        // both infinities and no finite interval contains it, so the honest
        // answer is the one that decides nothing and makes the caller
        // evaluate. `Op::Divide`'s own doc explains why an infinity is allowed
        // to exist at all.
        if other.low <= 0.0 && other.high >= 0.0 {
            return Self {
                low: f32::NEG_INFINITY,
                high: f32::INFINITY,
            };
        }
        self.multiply(Self {
            low: 1.0 / other.high,
            high: 1.0 / other.low,
        })
    }

    /// The interval of `|x|` over this one.
    fn absolute(self) -> Self {
        if self.low <= 0.0 && self.high >= 0.0 {
            // Straddles zero, so the smallest magnitude available is zero
            // itself — not `min(|low|, |high|)`, which is the mistake that
            // makes `abs` look like it preserves a gap it has closed.
            Self {
                low: 0.0,
                high: self.low.abs().max(self.high.abs()),
            }
        } else {
            let (a, b) = (self.low.abs(), self.high.abs());
            Self {
                low: a.min(b),
                high: a.max(b),
            }
        }
    }
}

/// One step of a compiled density program, in postfix order.
///
/// Values are pushed and consumed on a stack of whole arrays. A binary
/// operation pops two and pushes one.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Push the same number everywhere.
    Constant(f32),
    /// Push the sample's world x, y or z.
    Coordinate(Axis),
    /// Push fractal noise, sampled over the whole region in one call.
    Noise {
        /// The fractal's shape.
        params: FractalParams,
        /// Scales the result. Applied here rather than by a `mul` node so the
        /// common case is one op rather than two.
        amplitude: f32,
        /// Mixed into the world seed so two noise nodes in one program differ.
        /// **Not the chunk position**: a density field has to be continuous
        /// across a chunk boundary, so nothing about which chunk is being
        /// generated may reach the seed.
        stream: u64,
        /// How far the noise is drawn out along x, y and z: a feature is
        /// `stretch` times as long along that axis as the frequency alone
        /// makes it. [`UNSTRETCHED`] is round.
        ///
        /// **Rock is not the same shape in every direction.** It flutes and
        /// weathers vertically and its strata run level, and a noise round in
        /// every axis reads as lumps; with one frequency, the only way to make
        /// a feature taller was to make it wider by the same amount. Here
        /// rather than in [`super::noise::FractalParams`], which is built in
        /// thirty-odd places that mean a 2D or a round field, and whose
        /// meaning would change under all of them.
        stretch: [f32; 3],
    },
    /// The distance, in blocks, from the zero contour of a 2D noise field:
    /// the noise sampled on the ground plane (y held at 0), so the contour
    /// is a line across the ground and the field a vertical wall along it.
    ///
    /// **A crack needs a width, and a band about a contour has none.** The
    /// set where `|noise| < w` is a line where the noise climbs steeply and a
    /// pond where it lies flat — a fifth of the ground, at any `w` that made
    /// the lines visible. What a crack wants is the distance to the line,
    /// which is `|n| / |grad n|`: the gradient by central differences half a
    /// block out, five samples of the ground plane per column and no more,
    /// which is cheaper than one 3D octave. Capped at [`CONTOUR_FAR`].
    Contour {
        /// The fractal's shape, as a noise node's.
        params: FractalParams,
        /// Mixed into the world seed, as a noise node's.
        stream: u64,
        /// Negative on the noise's negative side, so one field says which
        /// side of the line a point is on and how far: a coastline, with the
        /// land where it is positive and the cliff rising over the first few
        /// blocks of it. Unsigned, it is the distance alone.
        signed: bool,
    },
    /// Pop two, push the sum.
    Add,
    /// Pop two, push `first - second`.
    Subtract,
    /// Pop two, push the product.
    Multiply,
    /// Pop two, push `first / second`.
    ///
    /// **A zero divisor gives an infinity, not a panic**, and an infinity that
    /// reaches a comparison is still a decision. What must never happen is a
    /// NaN in simulation state (charter rule 4), and `x / 0.0` is only NaN when
    /// `x` is zero as well — which [`Density::evaluate`] checks for in debug.
    Divide,
    /// Pop two, push the smaller.
    Minimum,
    /// Pop two, push the larger.
    Maximum,
    /// Pop one, push its absolute value.
    Absolute,
    /// Push a map's value at the sample's world x and z, ignoring its y.
    ///
    /// # Why a density needs this at all
    ///
    /// [`super::map`] exists because erosion and rivers cannot be a function of
    /// one position — where water goes depends on the land everywhere else — so
    /// the engine computes whole fields in passes. Without this node the only
    /// exit from a map is `Map::heightmap`, which feeds
    /// `ChunkBuffer::fill_below_heightmap` and nothing else: a world whose
    /// surface is a DENSITY (a dome, overhangs, caves) could not read an eroded
    /// field at all. The mechanism was half-built, and this is the other half.
    ///
    /// # It holds the map, rather than pointing at one
    ///
    /// A density program is a value, and a value that changed underneath a
    /// world would be the worst kind of bug: chunks generated before a mod
    /// blurred its map once more would disagree for ever with chunks generated
    /// after, along a seam nothing downstream can see. So compiling the node
    /// takes a COPY, shared with an `Arc` so that copy is made once however
    /// many programs read it.
    Map {
        /// The field, as it was when the program was compiled.
        map: std::sync::Arc<super::map::Map>,
        /// The smallest and largest value it holds, found once at compile time
        /// so [`Density::bounds`] can answer without walking it again.
        range: (f32, f32),
    },
    /// Pop one, push it bounded to a range.
    Clamp {
        /// Lower bound.
        low: f32,
        /// Upper bound.
        high: f32,
    },
}

/// Which coordinate an [`Op::Coordinate`] pushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// World x.
    X,
    /// World y — the one a terrain field almost always wants, to make density
    /// fall off with height.
    Y,
    /// World z.
    Z,
}

/// What a density program can be wrong about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DensityError {
    /// More operations than [`MAX_OPS`].
    #[error("a density program may hold at most {MAX_OPS} operations, not {found}")]
    TooManyOps {
        /// How many were given.
        found: usize,
    },
    /// The program needs more live buffers than [`MAX_DEPTH`].
    #[error("a density program may use at most {MAX_DEPTH} buffers at once, not {found}")]
    TooDeep {
        /// How many it would need.
        found: usize,
    },
    /// An operation wanted more values than the stack held.
    #[error("operation {index} wanted {wanted} values and the stack held {found}")]
    StackUnderflow {
        /// Which operation.
        index: usize,
        /// How many it needed.
        wanted: usize,
        /// How many there were.
        found: usize,
    },
    /// The program did not finish with exactly one value.
    #[error("a density program must leave exactly one value, not {found}")]
    NotOneResult {
        /// How many were left.
        found: usize,
    },
    /// A program with nothing in it.
    #[error("a density program must have at least one operation")]
    Empty,
    /// A number that cannot be part of a deterministic field.
    #[error("a density program may not contain {what}, which is not a finite number")]
    NotFinite {
        /// Which value.
        what: &'static str,
    },
    /// A noise stretch that is not a positive number.
    #[error("a noise stretch must be greater than zero, not {}", f32::from_bits(*found))]
    NotAStretch {
        /// The stretch given, as its bits: the error is `Eq`, and a float is not.
        found: u32,
    },
    /// The output buffer was the wrong length.
    #[error(transparent)]
    Size(#[from] BufferSizeMismatch),
}

/// Reusable working buffers for [`Density::evaluate_with`].
///
/// A density program is a stack machine, and every slot it uses needs a buffer
/// the size of the region. Held by the caller so that a generator making a
/// thousand small calls — one per block the surface crosses, at sub-node
/// resolution — allocates once rather than a thousand times.
#[derive(Debug, Default)]
pub struct Scratch {
    slots: Vec<Vec<f32>>,
    /// The values of the sampled ops a program uses more than once, kept from
    /// their first use for every later one. See [`Memo`].
    memo: Vec<Vec<f32>>,
}

impl Scratch {
    /// `depth` buffers of `len` values from `slots`, grown as needed.
    ///
    /// Existing buffers are resized rather than replaced, so the steady state
    /// after the first call is no allocation at all. The contents are not
    /// cleared: every op writes its whole slot before anything reads it, which
    /// is the same guarantee the freshly-allocated version relied on.
    fn grow(slots: &mut Vec<Vec<f32>>, depth: usize, len: usize) -> &mut [Vec<f32>] {
        while slots.len() < depth {
            slots.push(vec![0.0; len]);
        }
        for slot in &mut slots[..depth] {
            // **Exactly `len`, not at least it.** Every op writes and reads a
            // whole slot, and the region's own length is what says how long
            // that is — a longer buffer left over from a bigger region would
            // be a size mismatch at the first `fill_3d`. `resize` down keeps
            // the capacity, so growing back allocates nothing.
            if slot.len() != len {
                slot.resize(len, 0.0);
            }
        }
        &mut slots[..depth]
    }
}

/// What evaluation does with one op's value besides pushing it.
///
/// **A mod's field names the same noise many times, and each name was a whole
/// fresh sampling of it.** The density language has no way to bind a value and
/// use it twice, so a helper called twice is compiled twice: the world mod's
/// terrain sampled 155 octaves of noise per point, of which 52 were distinct —
/// the mesa's plateau noise alone appeared twenty-one times, once per terrace.
/// Sampling is nearly all of what a program costs.
///
/// So `compile` finds the noise and contour ops identical, bit for bit, to an
/// earlier one, and evaluation samples each once: the first use is kept and
/// every later one copies it. A copy of the same computation's result is the
/// value sampling again would give, so this changes what a field costs and
/// nothing about any world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Memo {
    /// Used once: sampled, as ever.
    None,
    /// The first of several: sampled, and the value kept in this slot.
    Keep(usize),
    /// A repeat: the value kept in this slot, copied.
    Reuse(usize),
}

/// Everything that decides a sampled op's value, or `None` for an op that is
/// not sampled. Floats by their bits: `0.0` and `-0.0` compare equal and are
/// not the same amplitude.
fn sample_key(op: &Op) -> Option<[u64; 9]> {
    let shape = |params: &super::noise::FractalParams| {
        let kind = match params.fractal {
            super::noise::Fractal::Fbm => 0_u64,
            super::noise::Fractal::Ridged => 1,
            super::noise::Fractal::Billow => 2,
        };
        [
            kind | (u64::from(params.octaves) << 8),
            u64::from(params.frequency.to_bits()),
            u64::from(params.lacunarity.to_bits()) | (u64::from(params.gain.to_bits()) << 32),
        ]
    };
    match op {
        Op::Noise {
            params,
            amplitude,
            stream,
            stretch,
        } => {
            let [a, b, c] = shape(params);
            Some([
                1,
                a,
                b,
                c,
                u64::from(amplitude.to_bits()),
                *stream,
                u64::from(stretch[0].to_bits()),
                u64::from(stretch[1].to_bits()),
                u64::from(stretch[2].to_bits()),
            ])
        }
        Op::Contour {
            params,
            stream,
            signed,
        } => {
            let [a, b, c] = shape(params);
            Some([2, a, b, c, u64::from(*signed), *stream, 0, 0, 0])
        }
        _ => None,
    }
}

/// The [`Memo`] for each op, and how many values evaluation keeps.
fn plan_memo(ops: &[Op]) -> (Vec<Memo>, usize) {
    let keys: Vec<Option<[u64; 9]>> = ops.iter().map(sample_key).collect();
    let mut memo = vec![Memo::None; ops.len()];
    let mut kept: Vec<[u64; 9]> = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let Some(key) = key else { continue };
        if let Some(slot) = kept.iter().position(|seen| seen == key) {
            memo[index] = Memo::Reuse(slot);
        } else if keys[index + 1..].iter().any(|later| later.as_ref() == Some(key)) {
            // Kept only when something later repeats it, so a program with
            // nothing repeated evaluates exactly as it always did.
            memo[index] = Memo::Keep(kept.len());
            kept.push(*key);
        }
    }
    (memo, kept.len())
}

/// A compiled density program.
///
/// Built once — a mod holds one and hands it to every chunk it generates —
/// because compiling walks a script's table and validating it costs more than
/// evaluating a small program does.
#[derive(Debug, Clone, PartialEq)]
pub struct Density {
    ops: Vec<Op>,
    depth: usize,
    /// One per op: see [`Memo`].
    memo: Vec<Memo>,
    /// How many values evaluation keeps for the ops repeated.
    kept: usize,
}

impl Density {
    /// Whether this program's value depends on the height it is sampled at.
    ///
    /// **A question about the program, answered by reading it**, which is what
    /// lets a caller treat a 2D field as the 2D field it is. A level that never
    /// reads `y` has one value per column and can be evaluated on any slice;
    /// one that does has to be solved for the height where it meets itself,
    /// which is more work and is only worth doing where it is really needed.
    /// See [`super::buffer::ChunkBuffer::fill_fluid_terraced`].
    ///
    /// `Coordinate(Y)` reads it outright and a noise node reads it implicitly —
    /// the fractal is sampled in three dimensions. A contour and a map are both
    /// taken on the ground plane with `y` held at zero, so neither does.
    #[must_use]
    pub fn reads_y(&self) -> bool {
        self.ops
            .iter()
            .any(|op| matches!(op, Op::Coordinate(Axis::Y) | Op::Noise { .. }))
    }

    /// Checks a program and records how many buffers it needs.
    ///
    /// # Errors
    ///
    /// Every variant of [`DensityError`] except [`DensityError::Size`], which
    /// belongs to [`Density::evaluate`].
    pub fn compile(ops: Vec<Op>) -> Result<Self, DensityError> {
        if ops.is_empty() {
            return Err(DensityError::Empty);
        }
        if ops.len() > MAX_OPS {
            return Err(DensityError::TooManyOps { found: ops.len() });
        }

        // **Validated by walking the stack at compile time**, so evaluation
        // needs no bounds checks of its own and a malformed program is refused
        // once rather than per chunk.
        let mut height = 0usize;
        let mut peak = 0usize;
        for (index, op) in ops.iter().enumerate() {
            for (value, what) in op.constants() {
                if !value.is_finite() {
                    return Err(DensityError::NotFinite { what });
                }
            }
            // A stretch divides the coordinate, so zero is a division by zero
            // and a negative one mirrors the field for no reason anyone wants.
            if let Op::Noise { stretch, .. } = op
                && let Some(found) = stretch.iter().copied().find(|value| *value <= 0.0)
            {
                return Err(DensityError::NotAStretch {
                    found: found.to_bits(),
                });
            }
            let wanted = op.arity();
            if height < wanted {
                return Err(DensityError::StackUnderflow {
                    index,
                    wanted,
                    found: height,
                });
            }
            height = height - wanted + 1;
            peak = peak.max(height);
        }
        if height != 1 {
            return Err(DensityError::NotOneResult { found: height });
        }
        if peak > MAX_DEPTH {
            return Err(DensityError::TooDeep { found: peak });
        }
        let (memo, kept) = plan_memo(&ops);
        Ok(Self {
            ops,
            depth: peak,
            memo,
            kept,
        })
    }

    /// How many buffers evaluating this needs at once.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// How many operations it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether it holds no operations. Never true — `compile` refuses those.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Evaluates the field over `region`, writing `region.len()` values.
    ///
    /// # Errors
    ///
    /// [`DensityError::Size`] if `out` is not exactly `region.len()` long.
    ///
    /// # Panics
    ///
    /// In debug builds, if the program produces a NaN. Charter rule 4 forbids
    /// one reaching simulation state, and a field is simulation state the
    /// moment a chunk is filled from it.
    pub fn evaluate(
        &self,
        seed: u64,
        region: &Region3d,
        out: &mut [f32],
    ) -> Result<(), DensityError> {
        self.evaluate_with(seed, region, out, &mut Scratch::default())
    }

    /// A noise node's interval over one box.
    ///
    /// **Over THIS box.** [`super::noise::FractalParams::range`] is the range the
    /// field occupies somewhere, which is the same interval in every chunk in the
    /// world and so decides nothing: a generator asking "can this biome be here?"
    /// would get the same yes everywhere. [`super::noise::fractal_3d_bounds`]
    /// answers over the box, and intersects itself with the constant, so this is
    /// never the worse of the two.
    ///
    /// Measured over 1,600 chunks of a 16-block box, as the share of chunks whose
    /// bound lands entirely on one side of a threshold at zero: **72%** at
    /// frequency 0.001, 60% at 0.002, 4% at 0.004, and none at all by 0.01. That
    /// is the honest shape of it — a box cannot bound features smaller than
    /// itself, so this buys a great deal for the wide fields a biome is chosen
    /// from and nothing for the narrow ones terrain detail is made of.
    fn noise_bounds(
        seed: u64,
        axes: &[Interval; 3],
        params: &super::noise::FractalParams,
        amplitude: f32,
        stretch: [f32; 3],
    ) -> Interval {
        // The box the noise is actually SAMPLED over: each axis divided by its
        // stretch, exactly as the fill divides each coordinate. A positive
        // divisor keeps low below high, and `compile` refuses any other.
        let axes = [0, 1, 2].map(|axis| Interval {
            low: axes[axis].low / stretch[axis],
            high: axes[axis].high / stretch[axis],
        });
        let (low, high) = super::noise::fractal_3d_bounds(
            seed,
            (axes[0].low, axes[0].high),
            (axes[1].low, axes[1].high),
            (axes[2].low, axes[2].high),
            params,
        );
        Interval { low, high }.multiply(Interval::exactly(amplitude))
    }

    /// A map node's interval over one box.
    ///
    /// **The range over THIS box, not the whole field**, for the same reason a
    /// noise node is bounded over one: the stored range is every value the map
    /// holds anywhere, which is the same interval in every chunk in the world. See
    /// [`super::map::Map::range_over`], which is exact here because sampling a map
    /// is bilinear and so every value it returns is a convex combination of four
    /// lattice values.
    fn map_bounds(map: &super::map::Map, range: (f32, f32), axes: &[Interval; 3]) -> Interval {
        let (low, high) = map.range_over(
            (
                super::floor_to_i32(axes[0].low),
                super::floor_to_i32(axes[0].high),
            ),
            (
                super::floor_to_i32(axes[2].low),
                super::floor_to_i32(axes[2].high),
            ),
        );
        debug_assert!(
            low >= range.0 && high <= range.1,
            "a box's range escaped the whole field's"
        );
        Interval { low, high }
    }

    /// This field's value at ONE world position.
    ///
    /// # Why this exists when the whole design is bulk fills
    ///
    /// A generator placing structures has to answer "where is the ground at
    /// this x and z?" a few dozen times a chunk — once per tree, not once per
    /// block — and until this call there was no way to ask. Heightmaps and
    /// density programs are opaque to scripts on purpose, and the purpose is
    /// to stop O(volume) arithmetic in Lua, not to stop a script knowing
    /// anything. A handful of point queries is the same shape as scattering
    /// ore: proportional to what is placed, not to the volume it is placed in.
    ///
    /// It is also the only shape that works ACROSS a chunk boundary. A
    /// structure rooted in a neighbouring chunk has to be placed on that
    /// chunk's ground, and a generator cannot read a buffer it does not have —
    /// see [`super::buffer::ChunkBuffer::set_block_world`] for why the
    /// neighbourhood pass is the order-independent way to do this at all.
    ///
    /// # The thing not to do with it
    ///
    /// Looping this over a chunk is 4,096 crossings into the VM and back for
    /// an answer [`Self::evaluate`] gives in one call, in native code, with the
    /// bounds pruning in front of it. It is not merely slower; it is the exact
    /// cost the opaque handles were shaped to prevent. The engine does not stop
    /// a mod doing it, because the engine cannot tell a loop from a list — so
    /// this is said in the mod guide beside the call.
    ///
    /// # Errors
    ///
    /// [`DensityError`] if the program will not evaluate.
    pub fn sample(&self, seed: u64, x: f32, y: f32, z: f32) -> Result<f32, DensityError> {
        let region = Region3d {
            origin_x: x,
            origin_y: y,
            origin_z: z,
            step: 1.0,
            width: 1,
            height: 1,
            depth: 1,
        };
        let mut out = [0.0_f32];
        self.evaluate(seed, &region, &mut out)?;
        Ok(out[0])
    }

    /// What the program can possibly produce over a box, without evaluating it.
    ///
    /// # Why a bound and not nine samples
    ///
    /// The obvious way to ask "could the surface be in this chunk?" is to
    /// evaluate the corners and the centre and look at the signs. **That is not
    /// a bound and it will put holes in a world**: gradient noise between two
    /// samples is not bounded by those samples, so a chunk whose nine samples
    /// all read solid can still contain surface. The holes appear only where
    /// the field happens to wiggle at the wrong scale, which makes them look
    /// like corruption rather than like a wrong constant.
    ///
    /// So this is an interval extension instead: every operation is replaced by
    /// its interval version, and the result is guaranteed to contain every
    /// value the program takes anywhere in the box. It costs one pass over the
    /// operations — for a 26-node program, twenty-six interval operations
    /// against 26 × 5,832 sample evaluations, so it is free beside the thing it
    /// avoids.
    ///
    /// The answer is conservative in ONE direction, always: an interval that is
    /// too wide costs terrain that is generated when it need not have been, and
    /// one that is too narrow is a hole. Where a bound is not known, the widest
    /// possible one is returned rather than a guess — see [`Interval::divide`].
    ///
    /// # What the caller may do with it
    ///
    /// [`Interval::is_all_solid`] and [`Interval::is_all_empty`] are the two
    /// decisions worth taking. Anything else is [`Interval::is_undecided`], and
    /// the only answer to that is to evaluate.
    ///
    /// The box is the one the region SPANS, corner to corner, so a caller that
    /// has a `Region3d` for a chunk already has the right argument. Values
    /// between the sample points are covered too, which matters: a sub-node
    /// pass samples inside the same box at a finer step and must not be able to
    /// find surface a coarser bound said was not there.
    #[must_use]
    pub fn bounds(&self, seed: u64, over: &Region3d) -> Interval {
        let span = |origin: f32, count: usize| -> Interval {
            let far = origin + (count.saturating_sub(1)) as f32 * over.step;
            Interval {
                low: origin.min(far),
                high: origin.max(far),
            }
        };
        let axes = [
            span(over.origin_x, over.width),
            span(over.origin_y, over.height),
            span(over.origin_z, over.depth),
        ];

        let mut stack: Vec<Interval> = Vec::with_capacity(self.depth);
        for op in &self.ops {
            match op {
                Op::Constant(value) => stack.push(Interval::exactly(*value)),
                Op::Coordinate(which) => stack.push(match which {
                    Axis::X => axes[0],
                    Axis::Y => axes[1],
                    Axis::Z => axes[2],
                }),
                Op::Noise {
                    params,
                    amplitude,
                    stream,
                    stretch,
                } => stack.push(Self::noise_bounds(
                    seed ^ stream,
                    &axes,
                    params,
                    *amplitude,
                    *stretch,
                )),
                // A distance: never negative, never past the cap. Nothing
                // tighter without evaluating it, and a crack is thin enough
                // that a box's bound would rarely exclude it anyway.
                Op::Contour { signed, .. } => stack.push(Interval {
                    low: if *signed { -CONTOUR_FAR } else { 0.0 },
                    high: CONTOUR_FAR,
                }),
                Op::Map { map, range } => stack.push(Self::map_bounds(map, *range, &axes)),
                Op::Absolute => {
                    let Some(value) = stack.pop() else {
                        return Interval {
                            low: f32::NEG_INFINITY,
                            high: f32::INFINITY,
                        };
                    };
                    stack.push(value.absolute());
                }
                Op::Clamp { low, high } => {
                    let Some(value) = stack.pop() else {
                        return Interval {
                            low: f32::NEG_INFINITY,
                            high: f32::INFINITY,
                        };
                    };
                    stack.push(Interval {
                        low: value.low.clamp(*low, *high),
                        high: value.high.clamp(*low, *high),
                    });
                }
                binary => {
                    // `first` is the deeper of the two, matching `apply_binary`
                    // folding `second` into `first`. Getting this backwards is
                    // invisible for `add` and wrong for `sub` and `div`.
                    let (Some(second), Some(first)) = (stack.pop(), stack.pop()) else {
                        return Interval {
                            low: f32::NEG_INFINITY,
                            high: f32::INFINITY,
                        };
                    };
                    stack.push(match binary {
                        Op::Add => Interval {
                            low: first.low + second.low,
                            high: first.high + second.high,
                        },
                        Op::Subtract => Interval {
                            low: first.low - second.high,
                            high: first.high - second.low,
                        },
                        Op::Multiply => first.multiply(second),
                        Op::Divide => first.divide(second),
                        Op::Minimum => Interval {
                            low: first.low.min(second.low),
                            high: first.high.min(second.high),
                        },
                        Op::Maximum => Interval {
                            low: first.low.max(second.low),
                            high: first.high.max(second.high),
                        },
                        _ => unreachable!("the outer match limits this to the binary ops"),
                    });
                }
            }
        }

        stack.pop().map_or(
            Interval {
                low: f32::NEG_INFINITY,
                high: f32::INFINITY,
            },
            Interval::widened,
        )
    }

    /// The same, over a scratch buffer the caller keeps.
    ///
    /// # Why this exists
    ///
    /// [`Self::evaluate`] allocates one buffer per stack slot, which is the
    /// right trade for the one big call a chunk used to need. Sub-node terrain
    /// makes a THOUSAND small calls — one 3×3×3 region per block the surface
    /// crosses — and at that size the allocation is the work. The scratch grows
    /// to the largest region it has been asked for and is reused after that.
    ///
    /// # Errors
    ///
    /// [`DensityError::Size`] if `out` is not exactly `region.len()` long.
    ///
    /// # Panics
    ///
    /// In debug builds, if the program produces a NaN — see [`Self::evaluate`].
    pub fn evaluate_with(
        &self,
        seed: u64,
        region: &Region3d,
        out: &mut [f32],
        scratch: &mut Scratch,
    ) -> Result<(), DensityError> {
        let len = region.len();
        if out.len() != len {
            return Err(DensityError::Size(BufferSizeMismatch {
                expected: len,
                found: out.len(),
            }));
        }

        let Scratch { slots, memo } = scratch;
        let stack = Scratch::grow(slots, self.depth, len);
        let memo = Scratch::grow(memo, self.kept, len);
        let mut height = 0usize;

        for (op, plan) in self.ops.iter().zip(&self.memo) {
            if let Memo::Reuse(kept) = *plan {
                stack[height].copy_from_slice(&memo[kept]);
                height += 1;
                continue;
            }
            match op {
                Op::Constant(value) => {
                    stack[height].fill(*value);
                    height += 1;
                }
                Op::Coordinate(axis) => {
                    fill_coordinate(*axis, region, &mut stack[height]);
                    height += 1;
                }
                Op::Noise {
                    params,
                    amplitude,
                    stream,
                    stretch,
                } => {
                    let slot = &mut stack[height];
                    // The round field keeps its own loop, so no world that
                    // never asked for a stretch changes by a bit.
                    if *stretch == UNSTRETCHED {
                        fill_3d(seed ^ stream, region, params, slot)?;
                    } else {
                        fill_3d_stretched(seed ^ stream, region, params, *stretch, slot)?;
                    }
                    if (*amplitude - 1.0).abs() > f32::EPSILON {
                        for value in slot.iter_mut() {
                            *value *= amplitude;
                        }
                    }
                    if let Memo::Keep(kept) = *plan {
                        memo[kept].copy_from_slice(slot);
                    }
                    height += 1;
                }
                Op::Contour {
                    params,
                    stream,
                    signed,
                } => {
                    let slot = &mut stack[height];
                    contour_distance(seed ^ stream, region, params, *signed, slot)?;
                    if let Memo::Keep(kept) = *plan {
                        memo[kept].copy_from_slice(slot);
                    }
                    height += 1;
                }
                Op::Map { map, .. } => {
                    let slot = &mut stack[height];
                    fill_from_map(map, region, slot);
                    height += 1;
                }
                Op::Absolute | Op::Clamp { .. } => {
                    let slot = &mut stack[height - 1];
                    match op {
                        Op::Absolute => {
                            for value in slot.iter_mut() {
                                *value = value.abs();
                            }
                        }
                        Op::Clamp { low, high } => {
                            for value in slot.iter_mut() {
                                *value = value.clamp(*low, *high);
                            }
                        }
                        _ => unreachable!("outer match limits this to the unary ops"),
                    }
                }
                binary => {
                    // **Split so the borrow checker can see two slots.** The
                    // second operand is folded into the first, which is then
                    // the result — one pass and no third buffer.
                    let (left, right) = stack.split_at_mut(height - 1);
                    let first = &mut left[height - 2];
                    let second = &right[0];
                    apply_binary(binary, first, second);
                    height -= 1;
                }
            }
        }

        out.copy_from_slice(&stack[0]);
        debug_assert!(
            out.iter().all(|value| !value.is_nan()),
            "a density program produced a NaN, which charter rule 4 forbids in \
             simulation state"
        );
        Ok(())
    }
}

impl Op {
    /// How many values this consumes.
    const fn arity(&self) -> usize {
        match self {
            Self::Constant(_)
            | Self::Coordinate(_)
            | Self::Noise { .. }
            | Self::Contour { .. }
            | Self::Map { .. } => 0,
            Self::Absolute | Self::Clamp { .. } => 1,
            Self::Add
            | Self::Subtract
            | Self::Multiply
            | Self::Divide
            | Self::Minimum
            | Self::Maximum => 2,
        }
    }

    /// Every literal in this operation, for the finiteness check.
    fn constants(&self) -> Vec<(f32, &'static str)> {
        match self {
            Self::Constant(value) => vec![(*value, "a constant")],
            Self::Noise {
                params,
                amplitude,
                stretch,
                ..
            } => vec![
                (*amplitude, "a noise amplitude"),
                (params.frequency, "a noise frequency"),
                (params.lacunarity, "a noise lacunarity"),
                (params.gain, "a noise gain"),
                (stretch[0], "a noise stretch"),
                (stretch[1], "a noise stretch"),
                (stretch[2], "a noise stretch"),
            ],
            Self::Clamp { low, high } => vec![(*low, "a clamp bound"), (*high, "a clamp bound")],
            _ => Vec::new(),
        }
    }
}

/// A noise drawn out along no axis: [`Op::Noise`]'s `stretch` for a round field.
pub const UNSTRETCHED: [f32; 3] = [1.0; 3];

/// The default fractal shape, for a mod that names only a frequency.
#[must_use]
pub fn default_params() -> FractalParams {
    FractalParams {
        fractal: Fractal::Fbm,
        octaves: 4,
        frequency: 0.02,
        lacunarity: 2.0,
        gain: 0.5,
    }
}

/// Writes a coordinate value into every sample of the region.
/// Fills a slot with a map's value under each sample.
///
/// A map is a surface, so y does nothing here: every sample in a column gets
/// the same value, and a program that wants a height comparison subtracts `y`
/// itself. Sampled per position rather than per column because the region's
/// layout is x-fastest and re-deriving the column would cost more than the
/// bilinear read does.
fn fill_from_map(map: &super::map::Map, region: &Region3d, out: &mut [f32]) {
    let mut index = 0;
    for layer in 0..region.depth {
        let z = region.origin_z + layer as f32 * region.step;
        for _ in 0..region.height {
            for column in 0..region.width {
                let x = region.origin_x + column as f32 * region.step;
                out[index] = map.sample(super::floor_to_i32(x), super::floor_to_i32(z));
                index += 1;
            }
        }
    }
}

fn fill_coordinate(axis: Axis, region: &Region3d, out: &mut [f32]) {
    let mut index = 0;
    for layer in 0..region.depth {
        let z = region.origin_z + layer as f32 * region.step;
        for row in 0..region.height {
            let y = region.origin_y + row as f32 * region.step;
            for column in 0..region.width {
                let x = region.origin_x + column as f32 * region.step;
                out[index] = match axis {
                    Axis::X => x,
                    Axis::Y => y,
                    Axis::Z => z,
                };
                index += 1;
            }
        }
    }
}

/// Folds `second` into `first` elementwise.
///
/// **A fixed order, always.** Charter rule 4 bans float accumulation over a
/// non-deterministic iteration order, and while a slice walk is already
/// ordered, saying so here is what stops somebody reaching for `rayon` on the
/// grounds that the loop is embarrassingly parallel. It is — and a parallel
/// reduction would still be a different answer.
fn apply_binary(op: &Op, first: &mut [f32], second: &[f32]) {
    match op {
        Op::Add => {
            for (a, b) in first.iter_mut().zip(second) {
                *a += *b;
            }
        }
        Op::Subtract => {
            for (a, b) in first.iter_mut().zip(second) {
                *a -= *b;
            }
        }
        Op::Multiply => {
            for (a, b) in first.iter_mut().zip(second) {
                *a *= *b;
            }
        }
        Op::Divide => {
            for (a, b) in first.iter_mut().zip(second) {
                *a /= *b;
            }
        }
        Op::Minimum => {
            for (a, b) in first.iter_mut().zip(second) {
                *a = a.min(*b);
            }
        }
        Op::Maximum => {
            for (a, b) in first.iter_mut().zip(second) {
                *a = a.max(*b);
            }
        }
        _ => unreachable!("only the binary operations reach here"),
    }
}

/// How far from a contour [`Op::Contour`] will say a point is, in blocks:
/// the cap where the gradient is flat and the true distance is anybody's.
pub const CONTOUR_FAR: f32 = 256.0;

/// Evaluates [`Op::Contour`] over a region: the distance to the zero contour
/// of the 2D noise, per column, broadcast down the region's y.
///
/// Five fills of the ground plane — the centre and half a block out each
/// way along x and z — and a division: `|n| / |grad n|`, with the gradient
/// by central differences. Deterministic for the reasons every fill is; the
/// square root is one of the operations the determinism gate admits.
fn contour_distance(
    seed: u64,
    region: &Region3d,
    params: &FractalParams,
    signed: bool,
    out: &mut [f32],
) -> Result<(), DensityError> {
    const HALF: f32 = 0.5;
    let ground = Region3d {
        origin_x: region.origin_x,
        origin_y: 0.0,
        origin_z: region.origin_z,
        step: region.step,
        width: region.width,
        height: 1,
        depth: region.depth,
    };
    let columns = ground.width * ground.depth;
    let mut centre = vec![0.0_f32; columns];
    let mut east = vec![0.0_f32; columns];
    let mut west = vec![0.0_f32; columns];
    let mut north = vec![0.0_f32; columns];
    let mut south = vec![0.0_f32; columns];
    fill_3d(seed, &ground, params, &mut centre)?;
    fill_3d(
        seed,
        &Region3d {
            origin_x: ground.origin_x + HALF,
            ..ground
        },
        params,
        &mut east,
    )?;
    fill_3d(
        seed,
        &Region3d {
            origin_x: ground.origin_x - HALF,
            ..ground
        },
        params,
        &mut west,
    )?;
    fill_3d(
        seed,
        &Region3d {
            origin_z: ground.origin_z + HALF,
            ..ground
        },
        params,
        &mut north,
    )?;
    fill_3d(
        seed,
        &Region3d {
            origin_z: ground.origin_z - HALF,
            ..ground
        },
        params,
        &mut south,
    )?;
    for z in 0..region.depth {
        for x in 0..region.width {
            let column = x + region.width * z;
            let slope_x = (east[column] - west[column]) / (2.0 * HALF);
            let slope_z = (north[column] - south[column]) / (2.0 * HALF);
            let slope = (slope_x * slope_x + slope_z * slope_z).sqrt();
            let distance = if slope > f32::EPSILON {
                (centre[column].abs() / slope).min(CONTOUR_FAR)
            } else {
                CONTOUR_FAR
            };
            let distance = if signed && centre[column] < 0.0 {
                -distance
            } else {
                distance
            };
            for y in 0..region.height {
                out[x + region.width * (y + region.height * z)] = distance;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_region() -> Region3d {
        Region3d {
            origin_x: 0.0,
            origin_y: 0.0,
            origin_z: 0.0,
            step: 1.0,
            width: 16,
            height: 16,
            depth: 16,
        }
    }

    /// The contour distance is small where the noise crosses zero and grows
    /// away from it: along a row of the ground, every sign change of the raw
    /// noise sits within a block of a point the distance calls near, and the
    /// point where the noise is largest is called far.
    #[test]
    fn contour_distance_is_small_at_the_zero_contour_and_grows_away_from_it() {
        let params = FractalParams {
            fractal: Fractal::Fbm,
            octaves: 1,
            frequency: 1.0 / 40.0,
            lacunarity: 2.0,
            gain: 0.5,
        };
        let raw = Density::compile(vec![Op::Noise {
            params,
            amplitude: 1.0,
            stream: 5,
            stretch: UNSTRETCHED,
        }])
        .expect("compiles");
        let contour = Density::compile(vec![Op::Contour {
            params,
            stream: 5,
            signed: false,
        }])
        .expect("compiles");
        let region = Region3d {
            origin_x: 100.5,
            origin_y: 0.0,
            origin_z: 7.5,
            step: 1.0,
            width: 256,
            height: 1,
            depth: 1,
        };
        let mut values = vec![0.0; 256];
        let mut distances = vec![0.0; 256];
        raw.evaluate(3, &region, &mut values).expect("evaluates");
        contour
            .evaluate(3, &region, &mut distances)
            .expect("evaluates");
        let mut crossings = 0;
        for i in 1..256 {
            if (values[i - 1] > 0.0) != (values[i] > 0.0) {
                crossings += 1;
                let near = distances[i - 1].min(distances[i]);
                assert!(near < 1.0, "a crossing at {i} is called {near} blocks away");
            }
        }
        assert!(
            crossings >= 3,
            "a row of 256 blocks at a 40-block wavelength crosses zero more than {crossings} times"
        );
        let (peak, _) = values
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .expect("a row");
        assert!(
            distances[peak] > 2.0,
            "the peak of the noise is {} blocks from a contour",
            distances[peak]
        );
        assert!(distances.iter().all(|d| *d >= 0.0 && *d <= CONTOUR_FAR));
    }

    /// A field with hills in it, the shape a real generator uses: noise that
    /// falls off with height, so the surface is where the two balance.
    fn terrain(frequency: f32, amplitude: f32) -> Vec<Op> {
        vec![
            Op::Noise {
                params: FractalParams {
                    fractal: Fractal::Fbm,
                    octaves: 4,
                    frequency,
                    lacunarity: 2.0,
                    gain: 0.5,
                },
                amplitude,
                stream: 7,
                stretch: UNSTRETCHED,
            },
            Op::Coordinate(Axis::Y),
            Op::Subtract,
        ]
    }

    #[test]
    fn a_bound_decides_most_of_a_column_of_ordinary_terrain() {
        // **The bound is sound whatever it answers, so soundness tests cannot
        // tell whether it is USEFUL.** One that returned the widest possible
        // interval every time would pass every other test in this file and
        // prune nothing, and the failure would show up as a server that had
        // quietly gone back to generating a kilometre of empty sky.
        //
        // The field is the reference fixture's shape — relief of about eleven
        // blocks, one unit of noise to one block of height — over a column
        // reaching well above and below it. What cannot be decided is the band
        // the surface might be in, and at this relief that is a small part of
        // the column.
        let density = Density::compile(terrain(0.008, 11.0)).expect("compile");
        let mut decided = 0;
        let mut total = 0;
        for cy in -12..12 {
            let region = Region3d {
                origin_x: 128.0,
                origin_y: (cy * 16) as f32,
                origin_z: -96.0,
                step: 1.0,
                width: 16,
                height: 16,
                depth: 16,
            };
            total += 1;
            if !density.bounds(9, &region).is_undecided() {
                decided += 1;
            }
        }
        // Measured at 93% over a wider sweep — 88% before the noise and map
        // nodes were bounded over the box rather than globally. Two thirds is
        // the line at which something has gone wrong, not a number to tune
        // towards.
        assert!(
            decided * 3 >= total * 2,
            "only {decided} of {total} chunks could be decided, so the bound has stopped \
             paying for itself — see the densityprobe example for the full picture"
        );
    }

    #[test]
    fn a_bound_contains_every_sample_inside_it() {
        // The whole contract, over boxes all over the world rather than at the
        // origin: a bound that only holds near zero is a bound that fails
        // wherever anybody actually builds.
        let density = Density::compile(terrain(0.03, 12.0)).expect("compile");
        for corner in [-8192.0, -256.0, 0.0, 1024.0, 60_000.0] {
            for height in [-320.0, -16.0, 0.0, 96.0] {
                let region = Region3d {
                    origin_x: corner,
                    origin_y: height,
                    origin_z: corner * 0.5,
                    step: 1.0,
                    width: 16,
                    height: 16,
                    depth: 16,
                };
                let bounds = density.bounds(9, &region);
                let mut field = vec![0.0; region.len()];
                density.evaluate(9, &region, &mut field).expect("evaluate");
                for value in &field {
                    assert!(
                        *value >= bounds.low && *value <= bounds.high,
                        "sample {value} at ({corner}, {height}) fell outside \
                         ({}, {})",
                        bounds.low,
                        bounds.high
                    );
                }
            }
        }
    }

    /// A biome selector's shape: coherent noise and NOTHING else.
    ///
    /// **The case every other bound test misses.** They all bound a terrain
    /// field, which carries a `y` term whose interval is the whole chunk
    /// height and swamps everything else — so the noise part of the bound can
    /// be badly wrong and no assertion moves. A selector has no height term,
    /// and is exactly what a generator asks about when deciding which biome a
    /// chunk can hold.
    fn selector(frequency: f32, octaves: u32) -> Vec<Op> {
        vec![Op::Noise {
            params: crate::detgen::noise::FractalParams {
                fractal: crate::detgen::noise::Fractal::Fbm,
                octaves,
                frequency,
                lacunarity: 2.0,
                gain: 0.5,
            },
            amplitude: 1.0,
            stream: 0,
            stretch: UNSTRETCHED,
        }]
    }

    #[test]
    fn a_point_sample_is_exactly_what_the_bulk_fill_puts_there() {
        // `sample` exists so a generator can ask where the ground is under a
        // structure — including one rooted in a chunk it does not have. It is
        // only worth anything if it agrees with the field the fill writes: a
        // tree placed on a height that disagreed with the terrain by even one
        // block would float or sink, and it would do it only at the positions
        // where the two paths differed, which is the worst way to find out.
        //
        // Bit-identical, not close. Both go through `evaluate`, and if that
        // ever stops being true this is what says so.
        let density = Density::compile(terrain(0.03, 12.0)).expect("compile");
        let region = Region3d {
            origin_x: -37.0,
            origin_y: 5.0,
            origin_z: 214.0,
            step: 1.0,
            width: 8,
            height: 4,
            depth: 8,
        };
        let mut field = vec![0.0; region.len()];
        density.evaluate(9, &region, &mut field).expect("evaluate");

        let mut index = 0;
        for layer in 0..region.depth {
            for row in 0..region.height {
                for column in 0..region.width {
                    let (x, y, z) = (
                        region.origin_x + column as f32,
                        region.origin_y + row as f32,
                        region.origin_z + layer as f32,
                    );
                    let point = density.sample(9, x, y, z).expect("sample");
                    assert_eq!(
                        point.to_bits(),
                        field[index].to_bits(),
                        "a point sample at ({x}, {y}, {z}) gave {point}, and the fill put \
                         {} there",
                        field[index]
                    );
                    index += 1;
                }
            }
        }
    }

    #[test]
    fn a_bound_over_a_box_holds_for_a_field_that_is_only_noise() {
        // Sampled at a third of a block, because a bound that only covered the
        // block lattice would be a bound on the samples rather than on the
        // field — and the sub-node fill looks between them.
        for (frequency, octaves) in [(0.002, 2), (0.01, 4), (0.05, 4)] {
            let density = Density::compile(selector(frequency, octaves)).expect("compile");
            for corner in [-4096.0, -64.0, 0.0, 512.0, 33_000.0] {
                let region = Region3d {
                    origin_x: corner,
                    origin_y: 0.0,
                    origin_z: corner * 0.25 - 17.0,
                    step: 1.0,
                    width: 16,
                    height: 16,
                    depth: 16,
                };
                let bounds = density.bounds(9, &region);
                let fine = Region3d {
                    step: 1.0 / 3.0,
                    width: 46,
                    height: 46,
                    depth: 46,
                    ..region
                };
                let mut field = vec![0.0; fine.len()];
                density.evaluate(9, &fine, &mut field).expect("evaluate");
                for value in &field {
                    assert!(
                        *value >= bounds.low && *value <= bounds.high,
                        "a sample {value} of noise at frequency {frequency} near {corner} \
                         escaped its box's bound ({}, {})",
                        bounds.low,
                        bounds.high
                    );
                }
            }
        }
    }

    #[test]
    fn a_bound_over_a_box_is_tighter_than_the_one_that_holds_everywhere() {
        // The entire point of bounding over a box. `FractalParams::range` is
        // the interval that holds for every seed at every position, so it is
        // the same answer in every chunk in the world and decides nothing: a
        // generator asking "can this biome be here?" gets the same yes
        // everywhere. This is what makes the answer vary.
        let params = crate::detgen::noise::FractalParams {
            fractal: crate::detgen::noise::Fractal::Fbm,
            octaves: 2,
            frequency: 0.002,
            lacunarity: 2.0,
            gain: 0.5,
        };
        let (low, high) = params.range();
        let everywhere = high - low;
        let density = Density::compile(selector(0.002, 2)).expect("compile");

        let mut widest = 0.0_f32;
        let mut seen_low = f32::INFINITY;
        let mut seen_high = f32::NEG_INFINITY;
        for chunk_x in -6..6 {
            for chunk_z in -6..6 {
                let region = Region3d {
                    origin_x: (chunk_x * 16) as f32,
                    origin_y: 0.0,
                    origin_z: (chunk_z * 16) as f32,
                    step: 1.0,
                    width: 16,
                    height: 16,
                    depth: 16,
                };
                let bounds = density.bounds(9, &region);
                widest = widest.max(bounds.high - bounds.low);
                seen_low = seen_low.min(bounds.low);
                seen_high = seen_high.max(bounds.high);
            }
        }
        // Measured at 9% of the constant's width at this frequency. A quarter
        // is the line at which the mechanism has stopped working, not a number
        // to tune towards.
        assert!(
            widest < everywhere * 0.25,
            "the widest box bound was {widest}, which is no better than the {everywhere} \
             that holds everywhere — bounding over a box has stopped doing anything"
        );
        // And it MOVES: two chunks give different answers, which is the
        // property a generator skips work on.
        assert!(
            seen_high - seen_low > widest,
            "every box gave the same interval, so nothing could ever be skipped"
        );
    }

    #[test]
    fn a_bound_still_holds_at_sub_node_resolution() {
        // The case the sub-node fill depends on. A coarse bound is taken over
        // the box, and then the fine pass samples INSIDE it at a third of the
        // step: if the bound only covered the coarse sample points, the fine
        // pass could find surface where the chunk was skipped.
        let density = Density::compile(terrain(0.08, 6.0)).expect("compile");
        let coarse = Region3d {
            origin_x: 32.0,
            origin_y: -4.0,
            origin_z: -64.0,
            step: 1.0,
            width: 16,
            height: 16,
            depth: 16,
        };
        let bounds = density.bounds(9, &coarse);
        let fine = Region3d {
            step: 1.0 / 3.0,
            width: 46,
            height: 46,
            depth: 46,
            ..coarse
        };
        let mut field = vec![0.0; fine.len()];
        density.evaluate(9, &fine, &mut field).expect("evaluate");
        for value in &field {
            assert!(
                *value >= bounds.low && *value <= bounds.high,
                "a sub-node sample {value} escaped the block-resolution bound ({}, {})",
                bounds.low,
                bounds.high
            );
        }
    }

    /// Noise about a threshold, with no height term: the shape of a cave field
    /// or an ore pocket, and the one where a coarse sample grid is blindest.
    fn blobs(frequency: f32, threshold: f32) -> Vec<Op> {
        vec![
            Op::Noise {
                params: FractalParams {
                    fractal: Fractal::Fbm,
                    octaves: 2,
                    frequency,
                    lacunarity: 2.0,
                    gain: 0.5,
                },
                amplitude: 1.0,
                stream: 3,
                stretch: UNSTRETCHED,
            },
            Op::Constant(threshold),
            Op::Subtract,
        ]
    }

    #[test]
    fn sampling_the_corners_and_the_centre_is_not_a_bound() {
        // **The counter-example this whole mechanism exists for.** The cheap
        // way to ask "could the surface be in this chunk" is to evaluate the
        // eight corners and the centre and look at the signs, and it is wrong:
        // noise between two samples is not bounded by those samples. A chunk
        // whose nine samples agree can still contain surface, and a generator
        // that skipped it would leave a hole.
        //
        // **What makes it fail is feature size against sample spacing.** Nine
        // samples over sixteen blocks see nothing smaller than sixteen blocks,
        // and a mod's caves and ore are deliberately smaller than that. A
        // gentle heightmap would survive the same test, which is exactly why
        // "it worked when I tried it" is not evidence here.
        //
        // Searched rather than hand-picked, so it keeps meaning something if
        // the noise is ever retuned.
        let mut found = None;
        'search: for (frequency, threshold) in [(0.45, 0.5), (0.8, 0.35), (1.3, 0.2)] {
            let density = Density::compile(blobs(frequency, threshold)).expect("compile");
            for x in 0..24 {
                for z in 0..24 {
                    let region = Region3d {
                        origin_x: (x * 16) as f32,
                        origin_y: 0.0,
                        origin_z: (z * 16) as f32,
                        step: 1.0,
                        width: 16,
                        height: 16,
                        depth: 16,
                    };
                    let far = 15.0;
                    let mut corners = vec![(far / 2.0, far / 2.0, far / 2.0)];
                    for dx in [0.0, far] {
                        for dy in [0.0, far] {
                            for dz in [0.0, far] {
                                corners.push((dx, dy, dz));
                            }
                        }
                    }
                    let missed = corners.iter().all(|(dx, dy, dz)| {
                        let point = Region3d {
                            origin_x: region.origin_x + dx,
                            origin_y: region.origin_y + dy,
                            origin_z: region.origin_z + dz,
                            step: 1.0,
                            width: 1,
                            height: 1,
                            depth: 1,
                        };
                        let mut one = [0.0];
                        density.evaluate(5, &point, &mut one).expect("evaluate");
                        one[0] <= 0.0
                    });
                    if !missed {
                        continue;
                    }
                    let mut field = vec![0.0; region.len()];
                    density.evaluate(5, &region, &mut field).expect("evaluate");
                    if let Some(solid) = field.iter().copied().find(|value| *value > 0.0) {
                        found = Some((density, region, solid));
                        break 'search;
                    }
                }
            }
        }

        let (density, region, solid) = found.expect(
            "no counter-example found — nine samples are not sound, so this search failing \
             means the search is wrong rather than that the shortcut is safe",
        );
        // The interval extension is not fooled by the chunk that fooled them.
        assert!(
            !density.bounds(7, &region).is_all_empty(),
            "a chunk holding a sample of {solid} was called empty by its own bound"
        );
    }

    fn stretched(stretch: [f32; 3]) -> Op {
        Op::Noise {
            params: FractalParams {
                fractal: Fractal::Fbm,
                octaves: 3,
                frequency: 0.05,
                lacunarity: 2.0,
                gain: 0.5,
            },
            amplitude: 8.0,
            stream: 21,
            stretch,
        }
    }

    #[test]
    fn a_stretched_noise_is_the_round_one_sampled_at_divided_coordinates() {
        // What `stretch` means, stated as the arithmetic: a feature four times
        // as tall is the round field read at y / 4. Bit-identical, so the bulk
        // fill, the point sample and anything a mod reasons about agree.
        let Op::Noise { params, .. } = stretched(UNSTRETCHED) else {
            unreachable!()
        };
        let stretch = [1.0, 4.0, 0.5];
        let density = Density::compile(vec![stretched(stretch)]).expect("compile");
        let region = Region3d {
            origin_x: -40.0,
            origin_y: 3.0,
            origin_z: 900.0,
            step: 1.0 / 3.0,
            width: 7,
            height: 9,
            depth: 5,
        };
        let mut field = vec![0.0; region.len()];
        density.evaluate(9, &region, &mut field).expect("evaluate");

        let mut index = 0;
        for layer in 0..region.depth {
            let z = region.origin_z + layer as f32 * region.step;
            for row in 0..region.height {
                let y = region.origin_y + row as f32 * region.step;
                for column in 0..region.width {
                    let x = region.origin_x + column as f32 * region.step;
                    let (world_seed, stream): (u64, u64) = (9, 21);
                    let expected = super::super::noise::fractal_3d(
                        world_seed ^ stream,
                        x / stretch[0],
                        y / stretch[1],
                        z / stretch[2],
                        &params,
                    ) * 8.0;
                    assert_eq!(
                        field[index].to_bits(),
                        expected.to_bits(),
                        "at ({x}, {y}, {z})"
                    );
                    let point = density.sample(9, x, y, z).expect("sample");
                    assert_eq!(
                        point.to_bits(),
                        expected.to_bits(),
                        "point at ({x}, {y}, {z})"
                    );
                    index += 1;
                }
            }
        }

        // And it is a different field from the round one, or the equality
        // above could be satisfied by a stretch that is read and ignored.
        let round = Density::compile(vec![stretched(UNSTRETCHED)]).expect("compile");
        let mut plain = vec![0.0; region.len()];
        round.evaluate(9, &region, &mut plain).expect("evaluate");
        assert_ne!(plain, field, "the stretch changed nothing");
    }

    #[test]
    fn a_bound_over_a_box_holds_for_a_stretched_noise() {
        // The bound is taken over the box the noise is SAMPLED over, each axis
        // divided by its stretch. Bounding the undivided box would be a bound
        // on a different region of the round field — sound only by luck. A
        // squash (stretch below one) is the dangerous direction: it samples a
        // wider box than the chunk, and a bound on the chunk's own box is
        // narrower than what the fill reads.
        //
        // The slow, hard-squashed case is the one with teeth: at frequency
        // 0.002 a sixteen-block box bounds to a sliver of the noise's range,
        // and a squash of fifty reads eight hundred blocks of it. Bounding the
        // chunk's own box fails there; the faster cases only hold the rest.
        let slow = |stretch: [f32; 3]| {
            let Op::Noise {
                amplitude, stream, ..
            } = stretched(stretch)
            else {
                unreachable!()
            };
            Op::Noise {
                params: FractalParams {
                    fractal: Fractal::Fbm,
                    octaves: 2,
                    frequency: 0.002,
                    lacunarity: 2.0,
                    gain: 0.5,
                },
                amplitude,
                stream,
                stretch,
            }
        };
        let cases = [
            slow([0.02, 1.0, 0.02]),
            slow([1.0, 0.02, 1.0]),
            stretched([1.0, 6.0, 1.0]),
            stretched([3.0, 0.2, 9.0]),
        ];
        for op in cases {
            let Op::Noise { stretch, .. } = op else {
                unreachable!()
            };
            let density = Density::compile(vec![op]).expect("compile");
            for corner in [-3000.0, 0.0, 77.0, 40_000.0] {
                let region = Region3d {
                    origin_x: corner,
                    origin_y: corner * 0.5 - 8.0,
                    origin_z: 13.0 - corner,
                    step: 1.0,
                    width: 16,
                    height: 16,
                    depth: 16,
                };
                let bounds = density.bounds(4, &region);
                let fine = Region3d {
                    step: 1.0 / 3.0,
                    width: 46,
                    height: 46,
                    depth: 46,
                    ..region
                };
                let mut field = vec![0.0; fine.len()];
                density.evaluate(4, &fine, &mut field).expect("evaluate");
                for value in &field {
                    assert!(
                        *value >= bounds.low && *value <= bounds.high,
                        "a sample {value} of noise stretched {stretch:?} near {corner} escaped \
                         its box's bound ({}, {})",
                        bounds.low,
                        bounds.high
                    );
                }
            }
        }
    }

    #[test]
    fn a_stretch_that_is_not_above_zero_is_refused() {
        for bad in [0.0, -1.0, -0.0] {
            let refused = Density::compile(vec![stretched([1.0, bad, 1.0])]);
            assert!(
                matches!(refused, Err(DensityError::NotAStretch { .. })),
                "a stretch of {bad} was {refused:?}"
            );
        }
        assert!(matches!(
            Density::compile(vec![stretched([1.0, f32::INFINITY, 1.0])]),
            Err(DensityError::NotFinite { .. })
        ));
        Density::compile(vec![stretched([0.001, 1000.0, 1.0])]).expect("any positive stretch");
    }

    fn evaluate(ops: Vec<Op>) -> Vec<f32> {
        let density = Density::compile(ops).expect("compile");
        let region = chunk_region();
        let mut out = vec![0.0; region.len()];
        density.evaluate(7, &region, &mut out).expect("evaluate");
        out
    }

    #[test]
    fn a_constant_is_the_same_everywhere() {
        let out = evaluate(vec![Op::Constant(2.5)]);
        assert!(out.iter().all(|value| (*value - 2.5).abs() < f32::EPSILON));
    }

    #[test]
    fn arithmetic_folds_in_the_order_it_was_written() {
        // 10 - 4 = 6, not 4 - 10. Postfix order is easy to get backwards and
        // the symptom is terrain inverted about a plane, which reads as a bug
        // in the noise rather than in the subtraction.
        let out = evaluate(vec![Op::Constant(10.0), Op::Constant(4.0), Op::Subtract]);
        assert!((out[0] - 6.0).abs() < f32::EPSILON, "got {}", out[0]);

        let out = evaluate(vec![Op::Constant(10.0), Op::Constant(4.0), Op::Divide]);
        assert!((out[0] - 2.5).abs() < f32::EPSILON, "got {}", out[0]);
    }

    #[test]
    fn the_y_coordinate_is_the_world_one_not_the_local_one() {
        // **The field has to be continuous across a chunk boundary.** A `y`
        // node that pushed the local index would restart at zero every 16
        // blocks, and terrain built on it would be sixteen-block terraces all
        // the way up — the shape a height bias exists to avoid.
        let density = Density::compile(vec![Op::Coordinate(Axis::Y)]).expect("compile");
        let region = Region3d {
            origin_y: 32.0,
            ..chunk_region()
        };
        let mut out = vec![0.0; region.len()];
        density.evaluate(7, &region, &mut out).expect("evaluate");
        assert!((out[0] - 32.0).abs() < f32::EPSILON, "got {}", out[0]);
    }

    #[test]
    fn a_field_is_identical_for_the_same_seed_and_different_for_another() {
        // Charter rule 4 in the small: this is the property the whole module
        // is shaped around, and it is worth asserting where a change to the
        // evaluator would break it rather than only in the CI hash gate.
        let ops = vec![
            Op::Noise {
                params: default_params(),
                amplitude: 1.0,
                stream: 11,
                stretch: UNSTRETCHED,
            },
            Op::Coordinate(Axis::Y),
            Op::Constant(0.05),
            Op::Multiply,
            Op::Subtract,
        ];
        let first = evaluate(ops.clone());
        let second = evaluate(ops.clone());
        assert_eq!(
            first, second,
            "the same program and seed must agree exactly"
        );

        let density = Density::compile(ops).expect("compile");
        let region = chunk_region();
        let mut other = vec![0.0; region.len()];
        density.evaluate(8, &region, &mut other).expect("evaluate");
        assert_ne!(
            first, other,
            "a different world seed must give a different field"
        );
    }

    #[test]
    fn two_noise_nodes_with_different_streams_do_not_agree() {
        // Otherwise a mod asking for two independent fields — terrain and
        // caves, say — gets the same one twice and the caves follow the hills
        // exactly. The failure looks like a worldgen idea that did not work.
        let one = evaluate(vec![Op::Noise {
            params: default_params(),
            amplitude: 1.0,
            stream: 1,
            stretch: UNSTRETCHED,
        }]);
        let two = evaluate(vec![Op::Noise {
            params: default_params(),
            amplitude: 1.0,
            stream: 2,
            stretch: UNSTRETCHED,
        }]);
        assert_ne!(one, two);
    }

    #[test]
    fn a_malformed_program_is_refused_rather_than_evaluated() {
        assert_eq!(Density::compile(Vec::new()), Err(DensityError::Empty));
        assert_eq!(
            Density::compile(vec![Op::Add]),
            Err(DensityError::StackUnderflow {
                index: 0,
                wanted: 2,
                found: 0
            })
        );
        assert_eq!(
            Density::compile(vec![Op::Constant(1.0), Op::Constant(2.0)]),
            Err(DensityError::NotOneResult { found: 2 })
        );
        assert_eq!(
            Density::compile(vec![Op::Constant(f32::NAN)]),
            Err(DensityError::NotFinite { what: "a constant" })
        );
        assert_eq!(
            Density::compile(vec![Op::Constant(f32::INFINITY)]),
            Err(DensityError::NotFinite { what: "a constant" })
        );
    }

    #[test]
    fn a_program_too_deep_or_too_long_is_refused() {
        // It arrives from a script, so the bound has to be a message rather
        // than an allocation. Deep first: MAX_DEPTH + 1 constants pushed
        // without ever being consumed.
        let deep: Vec<Op> = (0..=MAX_DEPTH).map(|_| Op::Constant(1.0)).collect();
        let mut program = deep.clone();
        for _ in 0..MAX_DEPTH {
            program.push(Op::Add);
        }
        assert_eq!(
            Density::compile(program),
            Err(DensityError::TooDeep {
                found: MAX_DEPTH + 1
            })
        );

        let mut long = vec![Op::Constant(1.0)];
        for _ in 0..MAX_OPS {
            long.push(Op::Absolute);
        }
        assert_eq!(
            Density::compile(long),
            Err(DensityError::TooManyOps { found: MAX_OPS + 1 })
        );
    }

    #[test]
    fn a_wrong_sized_output_is_refused() {
        let density = Density::compile(vec![Op::Constant(1.0)]).expect("compile");
        let region = chunk_region();
        let mut out = vec![0.0; region.len() - 1];
        assert!(matches!(
            density.evaluate(1, &region, &mut out),
            Err(DensityError::Size(_))
        ));
    }
}
