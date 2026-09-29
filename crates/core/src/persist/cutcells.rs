// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A cut of several materials' cells, as they go to disk.
//!
//! **Charter rule 8, twenty-seven times.** A stack's material has been saved
//! under the world's id since inventories were first persisted; a cut of
//! several materials (Sub-Node Contract §9.1) carries a material per cell,
//! and every one of them is a runtime number that means a different material
//! the day a mod's load order changes. So each is translated at the door, as
//! the material beside it is, by the one pair of functions here that the
//! player codec, the container codec and the entity path all call.
//!
//! Air is `0` on both sides whatever the map says: it is reserved as id 0 in
//! every world and every session, and an empty cell is not a material that
//! needs naming.

use crate::block::{Cells, SUBNODES_PER_BLOCK};
use crate::inventory::Stack;
use crate::material::MaterialId;
use crate::persist::idmap::MaterialMap;

/// A cut's cells in world ids, or `None` if some cell's material has no world
/// id — a stack that cannot be named on disk, which the caller drops and
/// counts rather than writing a number that means something else.
#[must_use]
pub(crate) fn to_world(cells: &Cells, materials: &MaterialMap) -> Option<Vec<u16>> {
    cells
        .iter()
        .map(|material| {
            if material.is_air() {
                Some(0)
            } else {
                materials.to_world(*material).ok()
            }
        })
        .collect()
}

/// A cut's cells back in this session's ids, or `None` for a row that is not
/// twenty-seven cells long or names a material this world does not hold.
#[must_use]
pub(crate) fn to_runtime(cells: &[u16], materials: &MaterialMap) -> Option<Cells> {
    if cells.len() != SUBNODES_PER_BLOCK {
        return None;
    }
    let mut out = crate::block::EMPTY_CELLS;
    for (slot, &world) in out.iter_mut().zip(cells) {
        *slot = if world == 0 {
            MaterialId::AIR
        } else {
            materials.to_runtime(world).ok()?
        };
    }
    Some(out)
}

/// A stack read back from disk, in this session's ids and canonical form.
///
/// `material` and `shape` are what the row said; with `cells` they are only a
/// claim, and the stack is rebuilt from the cells so its material is the
/// lowest RUNTIME id among them (§9.1 rule 2) — a fact about this session,
/// which the lowest world id need not be. `None` for anything that cannot be
/// named or is not a stack.
#[must_use]
pub(crate) fn thaw(
    material: u16,
    units: u32,
    shape: Option<u32>,
    cells: Option<&[u16]>,
    detail: Option<String>,
    materials: &MaterialMap,
) -> Option<Stack> {
    let built = if let Some(cells) = cells {
        Stack::of_cells(&to_runtime(cells, materials)?, units)?
    } else {
        let material = materials.to_runtime(material).ok()?;
        let stack = Stack::new(material, units)?;
        Stack {
            shape: shape.and_then(crate::inventory::Shape::new),
            ..stack
        }
    };
    Some(Stack { detail, ..built })
}
