// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A terraced fluid answers the same for a column in every chunk layer.
//!
//! **Reported from the window as walls of water in an ocean, and found here
//! instead.** `fill_fluid_terraced` read its level field on the floor of the
//! chunk it was filling, so the same column asked a different question in each
//! vertical layer — and a level that leans on height, which is what a world's
//! relief is, could answer a block apart between two of them. The lower layer
//! then stopped a block short of the upper one's water and left a sheet of it
//! hanging over an air gap, exactly on the chunk boundary.
//!
//! The property that says it is fixed is not about any one number: **a stacked
//! column of chunks is wet from the bottom up to one top, with no dry gap
//! anywhere in it.** Water over a gap is the bug, whatever the level was.
//!
//! These build the buffers directly rather than going through a server, because
//! what is under test is the generator: the bug is in what is written, not in
//! what is streamed, and one chunk layer disagreeing with the one above it is
//! visible here in milliseconds.

use tiamat_core::coords::LocalBlock;
use tiamat_core::detgen::{Axis, ChunkBuffer, Density, FractalParams, Op, Terraces, UNSTRETCHED};
use tiamat_core::{CHUNK_BLOCKS, ChunkPos, MaterialId};

const SEED: u64 = 7;

/// A level of `12 + slope * y + 10 * noise(x, y, z)`.
///
/// `slope` is what makes it lean on height: at zero it is an ordinary 2D-ish
/// field (the noise still reads `y`), and above zero the height the water
/// stands at depends on the height it is measured from — which is the case the
/// chunk layers used to disagree about.
fn level(slope: f32) -> Density {
    Density::compile(vec![
        Op::Noise {
            params: FractalParams {
                octaves: 3,
                frequency: 0.01,
                ..FractalParams::default()
            },
            amplitude: 10.0,
            stream: 3,
            stretch: UNSTRETCHED,
        },
        Op::Coordinate(Axis::Y),
        Op::Constant(slope),
        Op::Multiply,
        Op::Add,
        Op::Constant(12.0),
        Op::Add,
    ])
    .expect("compiles")
}

/// Fills one chunk of a terraced body and hands back the buffer.
fn layer(field: &Density, pos: ChunkPos) -> ChunkBuffer {
    let mut buffer = ChunkBuffer::new(pos, MaterialId::AIR);
    buffer
        .fill_fluid_terraced(
            SEED,
            &Terraces {
                level: field,
                within: None,
                fluid: tiamat_core::fluid::FluidId(1),
                lip: None,
            },
        )
        .expect("fills");
    buffer
}

/// Every column of a stack of chunks, as "is this block wet", bottom upward.
fn wet_columns(field: &Density, cx: i32, cz: i32, layers: std::ops::Range<i32>) -> Vec<Vec<bool>> {
    let span = CHUNK_BLOCKS;
    let buffers: Vec<ChunkBuffer> = layers
        .map(|cy| layer(field, ChunkPos::new(cx, cy, cz)))
        .collect();
    let mut columns = Vec::new();
    for z in 0..span {
        for x in 0..span {
            columns.push(
                buffers
                    .iter()
                    .flat_map(|buffer| {
                        (0..span)
                            .map(move |y| buffer.fluid().get(LocalBlock::new(x, y, z)).volume() > 0)
                    })
                    .collect(),
            );
        }
    }
    columns
}

/// How many columns hold water somewhere above a dry block.
fn columns_with_a_gap(columns: &[Vec<bool>]) -> usize {
    columns
        .iter()
        .filter(|wet| {
            let top = wet.iter().position(|w| !*w).unwrap_or(wet.len());
            wet[top..].iter().any(|w| *w)
        })
        .count()
}

#[test]
fn a_terraced_body_has_no_water_standing_over_a_gap_at_a_chunk_seam() {
    // Three fields: one that does not lean on height at all, and two that lean
    // hard enough for the layers to have disagreed. Before the level was read
    // on one plane for every layer these gave 0, 8 and 13 columns with a gap,
    // out of 4,096 — and every gap sat exactly on a chunk boundary, which is
    // what named the cause.
    for slope in [0.0_f32, 0.3, 0.5] {
        let field = level(slope);
        let mut gaps = 0;
        let mut columns = 0;
        for cx in -2..2 {
            for cz in -2..2 {
                let stack = wet_columns(&field, cx, cz, -1..4);
                columns += stack.len();
                gaps += columns_with_a_gap(&stack);
            }
        }
        assert_eq!(
            gaps, 0,
            "a level leaning {slope} per block left {gaps} of {columns} columns with water \
             standing over a dry gap, which is a sheet of it hanging on a chunk seam"
        );
    }
}

#[test]
fn a_column_fills_to_the_same_height_whichever_layer_asks() {
    // The same property stated the other way round, and the one a reader can
    // check by eye: the top of the water in a stacked column is ONE height, not
    // a height per chunk.
    //
    // **A guard, and it says so.** Reverting the fix leaves this passing and
    // the test above failing, because this looks at one stack of columns and
    // the disagreement is rare — 8 columns in 4,096. It is here because it
    // states the invariant plainly; the test above is the one with teeth.
    let field = level(0.4);
    let stack = wet_columns(&field, 0, 0, -1..4);
    for (index, wet) in stack.iter().enumerate() {
        let top = wet.iter().position(|w| !*w).unwrap_or(wet.len());
        // Wet below, dry above: one transition, so every layer agreed about
        // where it is.
        assert!(
            wet[..top].iter().all(|w| *w) && wet[top..].iter().all(|w| !*w),
            "column {index} is wet at {wet:?}, which is more than one surface — the layers \
             disagree about where the top of the water is"
        );
    }
}
