// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a placement request claims to be spending, and what a cut of several
//! materials landed.
//!
//! The placement loop in `handle` decides; this is the part of it that is
//! arithmetic on a request and on edits, pulled out so it can be tested
//! without a world and so the loop does not grow by the size of Sub-Node
//! Contract §9.1.

use tiamat_core::MaterialId;
use tiamat_core::block::Cells;
use tiamat_core::inventory::{Shape, Stack, StackKey};

use crate::transport::endpoint::{PlacementRequest, Shared};

/// Which stack a placement request names, in this session's ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The material — the lowest of the cells, for a cut of several.
    pub material: MaterialId,
    /// The authored cut, before it is turned to face anybody.
    pub shape: Option<Shape>,
    /// Each cell's material, for a cut of several, as authored.
    pub cells: Option<Cells>,
    /// A mod's own word for which item.
    pub detail: Option<String>,
}

impl Claim {
    /// The key the inventory is asked with: exactly this stack.
    #[must_use]
    pub fn key(&self) -> StackKey<'_> {
        StackKey {
            material: self.material,
            shape: self.shape,
            cells: self.cells.as_ref(),
            detail: self.detail.as_deref(),
        }
    }

    /// Every material this would put in the world: the one, or each cell's.
    pub fn materials(&self) -> impl Iterator<Item = MaterialId> + '_ {
        let cells = self.cells.iter().flatten().copied();
        std::iter::once(self.material)
            .filter(|_| self.cells.is_none())
            .chain(cells.filter(|cell| !cell.is_air()))
    }
}

/// What `request` claims to be spending, or `None` for a claim that names a
/// material this world does not have.
///
/// **A cut of several materials is rebuilt from its cells**, so its material
/// and shape are derived here rather than read off the wire (§9.1 rule 2), and
/// cells that turn out to hold one material are the plain cut they are. A
/// cell naming a world id nobody has is nothing anybody can be carrying.
#[must_use]
pub fn claim(shared: &Shared, request: &PlacementRequest) -> Option<Claim> {
    if request.cells.is_empty() {
        return Some(Claim {
            material: shared.runtime_material(request.material)?,
            shape: Shape::new(request.shape),
            cells: None,
            detail: request.detail.clone(),
        });
    }
    let mut cells = tiamat_core::block::EMPTY_CELLS;
    for (cell, &world) in cells.iter_mut().zip(&request.cells) {
        *cell = if world == 0 {
            MaterialId::AIR
        } else {
            shared.runtime_material(world)?
        };
    }
    // One item's worth, only to be canonical: what is held is asked for below.
    let canonical = Stack::of_cells(&cells, 1)?;
    Some(Claim {
        material: canonical.material,
        shape: canonical.shape,
        cells: canonical.cells.map(|cells| *cells),
        detail: request.detail.clone(),
    })
}

/// How many of `cells` (one material's mask in a cut) one applied edit put in
/// the world.
///
/// `edit` is one `place::writes` produced for that material. A `Partial` or
/// `Block` carries every one of the material's cells — and, as a union,
/// whatever of that material was there already, which is why this counts the
/// cut's cells rather than the edit's bits; a `SubNode` is one.
#[must_use]
pub fn landed(cells: u32, edit: &tiamat_core::proto::Edit) -> u32 {
    match edit {
        tiamat_core::proto::Edit::SubNode { .. } => 1,
        tiamat_core::proto::Edit::Block { .. } | tiamat_core::proto::Edit::Partial { .. } => {
            cells.count_ones()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamat_core::proto::Edit;

    #[test]
    fn what_landed_is_the_cuts_cells_and_not_the_unions_bits() {
        let pos = tiamat_core::BlockPos::new(0, 0, 0);
        let union = Edit::Partial {
            pos,
            material: 2,
            occupancy: 0b1111_1111,
        };
        // The cut is three cells of this material; the union also names five
        // that were already there.
        assert_eq!(landed(0b111, &union), 3);
        let one = Edit::SubNode {
            pos: tiamat_core::SubNodePos::new(0, 0, 0),
            material: 2,
        };
        assert_eq!(landed(0b111, &one), 1);
    }

    #[test]
    fn a_claim_lists_what_it_would_put_in_the_world() {
        let mut cells = tiamat_core::block::EMPTY_CELLS;
        cells[0] = MaterialId(4);
        cells[5] = MaterialId(9);
        let claim = Claim {
            material: MaterialId(4),
            shape: Shape::new(0b10_0001),
            cells: Some(cells),
            detail: None,
        };
        let materials: Vec<_> = claim.materials().collect();
        assert_eq!(materials, vec![MaterialId(4), MaterialId(9)]);
        let plain = Claim {
            cells: None,
            ..claim
        };
        assert_eq!(plain.materials().collect::<Vec<_>>(), vec![MaterialId(4)]);
    }
}
