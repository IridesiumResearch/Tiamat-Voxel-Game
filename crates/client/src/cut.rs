// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A cut of several materials, as the client draws it.
//!
//! Sub-Node Contract §9.1: a stack may carry a material per cell, and on the
//! wire that is `StackDef::cells` — empty, or exactly 27 world ids with `0`
//! for an empty cell. The server derived `material` (the lowest) and `shape`
//! (the occupancy) from those cells, but `shape` is `0` for a cut that fills
//! the block, which is also how loose material is spelled. So everything that
//! DRAWS a stack asks this module first: a stack with cells is its cells,
//! whatever its `shape` says, and never a whole cube in its lowest material.
//!
//! Presentation only (charter rule 4, Scope). Nothing here decides what a
//! stack is; it only reads what the server said it is.

/// Each cell's material, in world ids, `0` for an empty cell, indexed
/// `x + 3*y + 9*z` like every other cell array in the engine.
pub type Cells = [u16; tiamat_core::block::SUBNODES_PER_BLOCK];

/// The cells a wire field carries, if it carries a cut of several materials.
///
/// Anything but exactly 27 entries is one material. The decoder already
/// refuses any other length from a server, so this is never a judgement about
/// hostile input — it is the one place the "empty or 27" rule is read.
#[must_use]
pub fn cells_of(wire: &[u16]) -> Option<Cells> {
    Cells::try_from(wire).ok()
}

/// Which cells are filled: bit `i` set where cell `i` is not `0`.
///
/// Core's [`tiamat_core::inventory::occupancy_of`], so the client and the
/// server cannot disagree about which cells a cut has.
#[must_use]
pub fn occupancy(cells: &Cells) -> u32 {
    tiamat_core::inventory::occupancy_of(&as_materials(cells))
}

/// The same cells as core's type, for core's functions.
///
/// The numbers stay world ids: a turn or an occupancy test only moves or
/// inspects them, and never asks what they mean.
#[must_use]
pub fn as_materials(cells: &Cells) -> tiamat_core::block::Cells {
    cells.map(tiamat_core::MaterialId)
}

/// The occupancy to DRAW a stack with.
///
/// For a cut of several materials that is its cells' occupancy, which is the
/// full mask for one that fills the block — where the wire's `shape` is `0`
/// and would draw it as loose material. For anything else it is `shape`.
#[must_use]
pub fn drawn_shape(stack: &tiamat_core::proto::StackDef) -> u32 {
    cells_of(&stack.cells).map_or(stack.shape, |cells| occupancy(&cells))
}

/// The distinct materials in a cut, ascending by id.
///
/// Ascending because that is the order Sub-Node Contract §9 puts everything
/// about a cut in, so a tooltip naming them reads the same as the order a
/// placement writes them.
#[must_use]
pub fn materials(cells: &Cells) -> Vec<u16> {
    tiamat_core::inventory::by_material(&as_materials(cells))
        .into_iter()
        .map(|(material, _)| material.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stone along the bottom and oak in one cell above it.
    fn stone_and_oak() -> Cells {
        let mut cells = [0; 27];
        for cell in cells.iter_mut().take(3) {
            *cell = 5;
        }
        cells[4] = 2;
        cells
    }

    #[test]
    fn only_twenty_seven_entries_are_a_cut_of_several_materials() {
        assert!(cells_of(&[]).is_none(), "no cells is one material");
        assert!(cells_of(&[5; 26]).is_none(), "twenty-six is not a block");
        assert!(cells_of(&[5; 28]).is_none(), "twenty-eight is not a block");
        assert_eq!(cells_of(&stone_and_oak()), Some(stone_and_oak()));
    }

    #[test]
    fn a_full_cut_of_several_materials_draws_full_whatever_the_wire_shape_says() {
        // **The case that would draw a lie.** A cut that fills the block has
        // no `Shape` (a shape is never full), so the wire says `0` — and `0`
        // is how loose material is drawn: one cube in one tile.
        let mut cells = [5; 27];
        cells[26] = 2;
        let stack = tiamat_core::proto::StackDef {
            material: 2,
            units: 27,
            shape: 0,
            detail: None,
            cells: cells.to_vec(),
        };
        assert_eq!(drawn_shape(&stack), tiamat_core::block::OCCUPANCY_FULL);

        // And a plain stack is drawn by its own shape, untouched.
        let plain = tiamat_core::proto::StackDef {
            cells: Vec::new(),
            shape: 0b111,
            ..stack
        };
        assert_eq!(drawn_shape(&plain), 0b111);
    }

    #[test]
    fn the_occupancy_and_the_materials_are_the_cells() {
        assert_eq!(occupancy(&stone_and_oak()), 0b1_0111);
        assert_eq!(
            materials(&stone_and_oak()),
            vec![2, 5],
            "ascending id, each once"
        );
    }
}
