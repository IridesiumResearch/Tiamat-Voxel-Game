// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A schematic cut from shapes: paths with a thickness, ellipsoids and single
//! blocks, rasterised to the cell natively.
//!
//! # Why this exists
//!
//! A mod describes a tree as a trunk that tapers, limbs that leave it at an
//! angle and clumps of leaves on their ends, and something has to turn that
//! into 27-bit cell masks. Done in Lua it is a distance test per cell of every
//! block a shape's box touches: a rainforest megatree — a trunk five blocks
//! wide and fifty tall, buttress walls, a dozen clumps of leaves seven blocks
//! across — is tens of millions of instructions, past the per-call budget
//! before the second tree is cut. Here it is a few hundred thousand float
//! tests and no crossings.
//!
//! # The rules
//!
//! Every shape names a material and a PRIORITY. A cell takes the material of
//! the highest-priority shape that covers it, the later shape winning a tie,
//! so wood can be given priority over the leaves pushed across a limb's end
//! and a hollow of air priority over the log it is carved from. The result is
//! one [`StampBlock`] per block per material, blocks in coordinate order.
//!
//! `rough` moves each cell's edge in or out by up to that share of the shape's
//! size, from an integer hash of the CELL alone — so two shapes of the same
//! roughness agree about a cell they share, and the same shapes cut the same
//! schematic on every platform. Nothing here is a library call: plain `+ - *
//! /` and comparisons on doubles, which are exact under IEEE 754 (charter
//! rule 4).

use std::collections::HashMap;

use crate::material::MaterialId;

use super::buffer::{Schematic, StampBlock};

/// One shape to cut.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Every cell within the radius of the line through the points, the
    /// radius carried linearly from one point to the next. `{x, y, z, r}`
    /// each, in blocks from the root.
    Path {
        /// The points.
        points: Vec<[f64; 4]>,
        /// What the cells become.
        material: MaterialId,
        /// Edge noise, a share of the radius.
        rough: f64,
        /// Higher wins a cell; the later shape wins a tie.
        priority: i32,
    },
    /// Every cell inside the ellipsoid.
    Ellipsoid {
        /// Centre, in blocks from the root.
        centre: [f64; 3],
        /// Half-widths along x, y and z.
        radii: [f64; 3],
        /// What the cells become.
        material: MaterialId,
        /// Edge noise, a share of the squared normalised distance.
        rough: f64,
        /// Higher wins a cell; the later shape wins a tie.
        priority: i32,
    },
    /// The named cells of one block.
    Cells {
        /// The block, from the root.
        at: [i32; 3],
        /// The cells, one bit each, `x + 3*y + 9*z`.
        mask: u32,
        /// What the cells become.
        material: MaterialId,
        /// Higher wins a cell; the later shape wins a tie.
        priority: i32,
    },
}

/// Cells per block along an axis.
const SIDE: i32 = 3;

/// An integer hash of one cell, in the world of cells.
fn cell_hash(x: i64, y: i64, z: i64) -> u64 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^ (h >> 32)
}

/// The edge factor for a cell: 1 moved by up to `rough` either way, in nine
/// steps.
fn edge(rough: f64, x: i64, y: i64, z: i64) -> f64 {
    if rough == 0.0 {
        return 1.0;
    }
    let step = (cell_hash(x, y, z) % 9) as f64 - 4.0;
    1.0 + rough * step / 4.0
}

/// The floor of a double as an integer, without a library call.
fn floor_i64(v: f64) -> i64 {
    let t = v as i64;
    if (t as f64) > v { t - 1 } else { t }
}

/// What each cell holds so far: material, priority. `None` for untouched.
type Cells = [Option<(MaterialId, i32)>; 27];

struct Raster {
    blocks: HashMap<(i32, i32, i32), Cells>,
}

impl Raster {
    fn put(&mut self, cell: (i64, i64, i64), material: MaterialId, priority: i32) {
        let (bx, by, bz) = (
            cell.0.div_euclid(i64::from(SIDE)) as i32,
            cell.1.div_euclid(i64::from(SIDE)) as i32,
            cell.2.div_euclid(i64::from(SIDE)) as i32,
        );
        let (ix, iy, iz) = (
            cell.0.rem_euclid(i64::from(SIDE)) as usize,
            cell.1.rem_euclid(i64::from(SIDE)) as usize,
            cell.2.rem_euclid(i64::from(SIDE)) as usize,
        );
        let slot = &mut self.blocks.entry((bx, by, bz)).or_insert([None; 27])[ix + 3 * iy + 9 * iz];
        match slot {
            Some((_, held)) if *held > priority => {}
            _ => *slot = Some((material, priority)),
        }
    }

    /// The cells whose centres lie in the box, as cell coordinates.
    fn cells_in(lo: [f64; 3], hi: [f64; 3]) -> [(i64, i64); 3] {
        let axis = |a: f64, b: f64| {
            // A cell (c) has its centre at (c + 0.5) / 3.
            (floor_i64(a * 3.0 - 0.5), floor_i64(b * 3.0 - 0.5) + 1)
        };
        [axis(lo[0], hi[0]), axis(lo[1], hi[1]), axis(lo[2], hi[2])]
    }
}

fn centre(c: i64) -> f64 {
    (c as f64 + 0.5) / 3.0
}

impl Raster {
    fn path(&mut self, points: &[[f64; 4]], material: MaterialId, rough: f64, priority: i32) {
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy, dz) = (b[0] - a[0], b[1] - a[1], b[2] - a[2]);
            let len2 = dx * dx + dy * dy + dz * dz;
            let dr = b[3] - a[3];
            let reach = a[3].max(b[3]) * (1.0 + rough.abs()) + 0.01;
            let lo = [
                a[0].min(b[0]) - reach,
                a[1].min(b[1]) - reach,
                a[2].min(b[2]) - reach,
            ];
            let hi = [
                a[0].max(b[0]) + reach,
                a[1].max(b[1]) + reach,
                a[2].max(b[2]) + reach,
            ];
            let [(x0, x1), (y0, y1), (z0, z1)] = Self::cells_in(lo, hi);
            for cz in z0..z1 {
                let pz = centre(cz) - a[2];
                for cy in y0..y1 {
                    let py = centre(cy) - a[1];
                    for cx in x0..x1 {
                        let px = centre(cx) - a[0];
                        let t = if len2 > 0.0 {
                            ((px * dx + py * dy + pz * dz) / len2).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let (ex, ey, ez) = (px - dx * t, py - dy * t, pz - dz * t);
                        let r = (a[3] + dr * t) * edge(rough, cx, cy, cz);
                        if ex * ex + ey * ey + ez * ez <= r * r {
                            self.put((cx, cy, cz), material, priority);
                        }
                    }
                }
            }
        }
    }

    fn ellipsoid(
        &mut self,
        c: [f64; 3],
        radii: [f64; 3],
        material: MaterialId,
        rough: f64,
        priority: i32,
    ) {
        let grow = 1.0 + rough.abs();
        let lo = [
            c[0] - radii[0] * grow,
            c[1] - radii[1] * grow,
            c[2] - radii[2] * grow,
        ];
        let hi = [
            c[0] + radii[0] * grow,
            c[1] + radii[1] * grow,
            c[2] + radii[2] * grow,
        ];
        let [(x0, x1), (y0, y1), (z0, z1)] = Self::cells_in(lo, hi);
        for cz in z0..z1 {
            let nz = (centre(cz) - c[2]) / radii[2];
            for cy in y0..y1 {
                let ny = (centre(cy) - c[1]) / radii[1];
                for cx in x0..x1 {
                    let nx = (centre(cx) - c[0]) / radii[0];
                    if nx * nx + ny * ny + nz * nz <= edge(rough, cx, cy, cz) {
                        self.put((cx, cy, cz), material, priority);
                    }
                }
            }
        }
    }

    fn cells(&mut self, at: [i32; 3], mask: u32, material: MaterialId, priority: i32) {
        for index in 0..27_i64 {
            if mask & (1 << index) != 0 {
                let (ix, iy, iz) = (index % 3, (index / 3) % 3, index / 9);
                let cell = (
                    i64::from(at[0]) * 3 + ix,
                    i64::from(at[1]) * 3 + iy,
                    i64::from(at[2]) * 3 + iz,
                );
                self.put(cell, material, priority);
            }
        }
    }

    /// One stamp per material per block, in the order the materials first
    /// appear among the block's cells: deterministic, and every cell once.
    fn finish(self) -> Schematic {
        let mut blocks = Vec::new();
        // Sorted by position before anything is written: a hash map's order
        // varies between runs, and the schematic must not (charter rule 4).
        let mut positions: Vec<_> = self.blocks.into_iter().collect();
        positions.sort_unstable_by_key(|(position, _)| *position);
        for ((dx, dy, dz), cells) in positions {
            let mut masks: Vec<(MaterialId, u32)> = Vec::new();
            for (index, cell) in cells.iter().enumerate() {
                if let Some((material, _)) = cell {
                    match masks.iter_mut().find(|(m, _)| m == material) {
                        Some((_, mask)) => *mask |= 1 << index,
                        None => masks.push((*material, 1 << index)),
                    }
                }
            }
            for (material, mask) in masks {
                blocks.push(StampBlock {
                    dx,
                    dy,
                    dz,
                    material,
                    mask,
                });
            }
        }
        Schematic::new(blocks)
    }
}

/// Cuts a schematic from shapes. See the module docs for the rules.
#[must_use]
pub fn rasterise(shapes: &[Shape]) -> Schematic {
    let mut raster = Raster {
        blocks: HashMap::new(),
    };
    for shape in shapes {
        match shape {
            Shape::Path {
                points,
                material,
                rough,
                priority,
            } => {
                raster.path(points, *material, *rough, *priority);
            }
            Shape::Ellipsoid {
                centre,
                radii,
                material,
                rough,
                priority,
            } => {
                raster.ellipsoid(*centre, *radii, *material, *rough, *priority);
            }
            Shape::Cells {
                at,
                mask,
                material,
                priority,
            } => {
                raster.cells(*at, *mask, *material, *priority);
            }
        }
    }
    raster.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WOOD: MaterialId = MaterialId(3);
    const LEAVES: MaterialId = MaterialId(4);

    fn cells_of(schematic: &Schematic, material: MaterialId) -> u32 {
        schematic
            .blocks()
            .iter()
            .filter(|b| b.material == material)
            .map(|b| b.mask.count_ones())
            .sum()
    }

    #[test]
    fn a_path_is_a_rod_of_the_right_volume() {
        // A rod of radius 1 and length 6 along y: pi * 1 * 6 blocks is 509
        // cells, plus two half-sphere caps of 4/3 pi / 27 each... about 622.
        let schematic = rasterise(&[Shape::Path {
            points: vec![[0.5, 0.0, 0.5, 1.0], [0.5, 6.0, 0.5, 1.0]],
            material: WOOD,
            rough: 0.0,
            priority: 0,
        }]);
        let cells = cells_of(&schematic, WOOD);
        assert!((560..=700).contains(&cells), "{cells} cells");
        // Whole in the middle of its run: the block the axis passes through.
        let middle = schematic
            .blocks()
            .iter()
            .find(|b| (b.dx, b.dy, b.dz) == (0, 3, 0))
            .expect("the axis block");
        assert_eq!(middle.mask, (1 << 27) - 1);
    }

    #[test]
    fn a_higher_priority_keeps_its_cells_and_a_tie_goes_to_the_later_shape() {
        let rod = Shape::Path {
            points: vec![[0.5, 0.0, 0.5, 0.6], [0.5, 4.0, 0.5, 0.6]],
            material: WOOD,
            rough: 0.0,
            priority: 1,
        };
        let clump = Shape::Ellipsoid {
            centre: [0.5, 2.0, 0.5],
            radii: [4.0, 4.0, 4.0],
            material: LEAVES,
            rough: 0.0,
            priority: 0,
        };
        let wood_alone = cells_of(&rasterise(std::slice::from_ref(&rod)), WOOD);
        let both = rasterise(&[rod.clone(), clump.clone()]);
        assert_eq!(
            cells_of(&both, WOOD),
            wood_alone,
            "the leaves took wood cells"
        );
        // Reversed, at equal priority, the later shape wins.
        let Shape::Path { points, .. } = rod else {
            unreachable!()
        };
        let tied = rasterise(&[
            Shape::Path {
                points,
                material: WOOD,
                rough: 0.0,
                priority: 0,
            },
            clump,
        ]);
        assert_eq!(cells_of(&tied, WOOD), 0);
    }

    #[test]
    fn air_carves_a_hollow_out_of_a_log_when_it_outranks_it() {
        let log = Shape::Path {
            points: vec![[0.0, 1.5, 0.5, 2.0], [8.0, 1.5, 0.5, 2.0]],
            material: WOOD,
            rough: 0.0,
            priority: 1,
        };
        let hollow = Shape::Path {
            points: vec![[-2.0, 1.5, 0.5, 1.2], [10.0, 1.5, 0.5, 1.2]],
            material: MaterialId::AIR,
            rough: 0.0,
            priority: 2,
        };
        let schematic = rasterise(&[log, hollow]);
        // The block on the axis in the middle is all air, and air is written.
        let middle: Vec<_> = schematic
            .blocks()
            .iter()
            .filter(|b| (b.dx, b.dy, b.dz) == (4, 1, 0))
            .collect();
        assert_eq!(middle.len(), 1);
        assert_eq!(middle[0].material, MaterialId::AIR);
        assert_eq!(middle[0].mask, (1 << 27) - 1);
        assert!(cells_of(&schematic, WOOD) > 0);
    }

    #[test]
    fn rough_shapes_cut_the_same_every_time_and_differ_from_smooth_ones() {
        let shape = |rough| Shape::Ellipsoid {
            centre: [0.5, 0.5, 0.5],
            radii: [4.0, 2.0, 4.0],
            material: LEAVES,
            rough,
            priority: 0,
        };
        let a = rasterise(&[shape(0.3)]);
        let b = rasterise(&[shape(0.3)]);
        assert_eq!(a.blocks(), b.blocks());
        assert_ne!(a.blocks(), rasterise(&[shape(0.0)]).blocks());
    }

    #[test]
    fn named_cells_land_where_they_are_named() {
        let schematic = rasterise(&[Shape::Cells {
            at: [-2, 1, 3],
            mask: 0b101,
            material: LEAVES,
            priority: 0,
        }]);
        assert_eq!(schematic.blocks().len(), 1);
        let block = &schematic.blocks()[0];
        assert_eq!(
            (block.dx, block.dy, block.dz, block.mask),
            (-2, 1, 3, 0b101)
        );
    }
}
