// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a horizon summary makes of round things, printed.
//!
//! Run with `cargo run --example lodprobe -p tiamot-core`.
//!
//! Reported from the world mod: past the view distance a woodland's trunks came
//! out as "+" shapes floating at canopy height, and small leaf clumps as lone
//! crosses. This builds a tree the way that mod does — a round trunk and a
//! round clump of leaves, at sub-node resolution — and prints a horizontal slice
//! of the first three summary levels under three rules side by side, beside the
//! engine's own `Summary::chain`, so the shapes can be seen rather than argued
//! about. The rule the engine uses is `lod::SOLID_CELLS_PER_BLOCK` and
//! `lod::SOLID_CHILDREN`.

use tiamot_core::lod::Summary;
use tiamot_core::{Chunk, ChunkPos, MaterialId, SubNodePos};

const WOOD: MaterialId = MaterialId(3);
const LEAVES: MaterialId = MaterialId(4);

/// A tree in the middle of one chunk: a trunk of radius `trunk` blocks up to
/// y = 9, and a ball of leaves of radius `crown` blocks centred at y = 11.
fn tree(trunk: f32, crown: f32, centre: f32) -> Chunk {
    let pos = ChunkPos::new(0, 0, 0);
    let mut chunk = Chunk::new(pos, MaterialId::AIR);
    let (cx, cz) = (centre, centre);
    for x in 0..48 {
        for y in 0..48 {
            for z in 0..48 {
                // The centre of the cell, in blocks.
                let at = |cell: i32| (cell as f32 + 0.5) / 3.0;
                let (bx, by, bz) = (at(x), at(y), at(z));
                let flat = (bx - cx) * (bx - cx) + (bz - cz) * (bz - cz);
                let material = if by < 9.0 && flat <= trunk * trunk {
                    WOOD
                } else if flat + (by - 11.0) * (by - 11.0) <= crown * crown {
                    LEAVES
                } else {
                    continue;
                };
                chunk
                    .set_subnode(SubNodePos::new(x, y, z), material)
                    .expect("in chunk");
            }
        }
    }
    chunk
}

/// A summary built with a different rule: a block is solid when at least
/// `fine` of its 27 cells are, and a coarser cell when at least `coarse` of
/// its eight are — taking the commonest solid material either way.
fn with_rule(chunk: &Chunk, fine: usize, coarse: usize) -> Vec<Vec<MaterialId>> {
    let mut levels = Vec::new();
    let mut cells = Vec::new();
    for z in 0..16 {
        for y in 0..16 {
            for x in 0..16 {
                let view = chunk.get_block_local(tiamot_core::coords::LocalBlock::new(x, y, z));
                let materials: Vec<MaterialId> = (0..27).map(|i| view.subnode(i)).collect();
                cells.push(pick(&materials, fine));
            }
        }
    }
    levels.push(cells);
    let mut width = 16;
    while width > 1 {
        let below = levels.last().expect("a level");
        let above = width / 2;
        let mut next = Vec::new();
        for z in 0..above {
            for y in 0..above {
                for x in 0..above {
                    let mut group = Vec::new();
                    for dz in 0..2 {
                        for dy in 0..2 {
                            for dx in 0..2 {
                                group.push(
                                    below[(x * 2 + dx)
                                        + (y * 2 + dy) * width
                                        + (z * 2 + dz) * width * width],
                                );
                            }
                        }
                    }
                    next.push(pick(&group, coarse));
                }
            }
        }
        levels.push(next);
        width = above;
    }
    levels
}

fn pick(cells: &[MaterialId], at_least: usize) -> MaterialId {
    let solid: Vec<MaterialId> = cells.iter().copied().filter(|m| !m.is_air()).collect();
    if solid.len() < at_least {
        return MaterialId::AIR;
    }
    let mut best = MaterialId::AIR;
    let mut count = 0;
    for m in &solid {
        let c = solid.iter().filter(|o| *o == m).count();
        if c > count || (c == count && m.0 < best.0) {
            best = *m;
            count = c;
        }
    }
    best
}

fn rows(cells: &[MaterialId], width: usize, y_block: usize) -> Vec<String> {
    let y = y_block / (16 / width);
    (0..width)
        .map(|z| {
            (0..width)
                .map(|x| match cells[x + y * width + z * width * width] {
                    m if m == WOOD => '#',
                    m if m == LEAVES => '*',
                    m if m.is_air() => '.',
                    _ => '?',
                })
                .collect()
        })
        .collect()
}

fn main() {
    let rules = [
        ("majority (now)", 14, 5),
        ("a third", 9, 3),
        ("a third / half", 9, 4),
    ];
    for (trunk, crown) in [(0.5, 1.5), (0.8, 2.5), (1.5, 3.5)] {
        let chunk = tree(trunk, crown, 8.5);
        let engine = Summary::chain(&chunk);
        println!("=== trunk radius {trunk}, crown radius {crown}, centred on a block ===");
        for level in 0..3 {
            let width = 16 >> level;
            println!(
                "level {} — per rule: trunk at y=4 | crown at y=11",
                level + 1
            );
            let mut columns: Vec<Vec<String>> = Vec::new();
            for (_, fine, coarse) in rules {
                let levels = with_rule(&chunk, fine, coarse);
                let trunk_rows = rows(&levels[level], width, 4);
                let crown_rows = rows(&levels[level], width, 11);
                columns.push(
                    trunk_rows
                        .iter()
                        .zip(&crown_rows)
                        .map(|(a, b)| format!("{a} {b}"))
                        .collect(),
                );
            }
            // The engine's own, so the probe's "now" can be checked against it.
            let engine_crown = rows(engine[level].cells(), width, 11);
            println!(
                "  {}",
                rules
                    .iter()
                    .map(|r| format!("{:<width$}", r.0, width = 2 * width + 1))
                    .collect::<Vec<_>>()
                    .join("   ")
            );
            for row in 0..width {
                let line: Vec<&str> = columns.iter().map(|c| c[row].as_str()).collect();
                println!("  {}   engine: {}", line.join("   "), engine_crown[row]);
            }
        }
    }
}
