// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A cut of several materials: a `Mixed` block a player can carry.
//!
//! **Sub-Node Contract §9.1 is authoritative**; this module is its code. The
//! world has held blocks of several materials since Task 02b. An inventory
//! stack could hold one — loose material, or a cut of one material to a
//! 27-bit mask — so a thing made of stone and oak could be built in place and
//! never carried, stacked, or placed again in one action.
//!
//! A stack now may carry [`Stack::cells`]: the material of each of its 27
//! cells. Everything else about it is derived from those cells, here and
//! nowhere else:
//!
//! - **Canonical form** is `BlockValue::canonical`'s rule applied to a stack.
//!   Two or more distinct non-air materials keep their cells; one material and
//!   not full is a plain cut; one material and full is loose material; all air
//!   is no stack. [`Stack::of_cells`] is where that is decided.
//! - **`material` is the lowest id among the cells** and **`shape` is their
//!   occupancy** — never chosen, so a stack rebuilt from the wire, from disk or
//!   from Lua cannot disagree with itself.
//! - **Identity** is material, shape, cells and detail, asked through one
//!   method ([`Stack::same_item`]) and one borrowed key ([`StackKey`]) rather
//!   than a fourth comparison at every site that used to ask three.
//! - **It moves in whole items** ([`Stack::grain`]). A unit of a cut of one
//!   material is still a unit of that material; a unit of a cut of several is
//!   no material at all, so nothing splits one.

use super::{Shape, Stack};
use crate::UNITS_PER_BLOCK;
use crate::block::Cells;
use crate::material::MaterialId;

/// Exactly which stack: everything that makes two stacks the same item.
///
/// The borrowed form of a stack's identity, for the family of calls that name
/// a stack to take from rather than holding one — `Slots::take`, `View::draw`,
/// [`super::Access::take`], [`super::units_of_exactly`]. It used to be three
/// loose arguments (material, shape, detail) repeated at every call, and a
/// cut of several materials would have made it four at fifteen sites.
///
/// Ordered field by field, material first, which is the order
/// [`super::consolidate`] lists stacks in: ascending material, as Sub-Node
/// Contract §9 orders everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StackKey<'a> {
    /// The material — for a cut of several, the lowest of its cells'.
    pub material: MaterialId,
    /// The cut, or `None` for loose material and for a cut of several that
    /// fills the block.
    pub shape: Option<Shape>,
    /// Each cell's material, for a cut of several.
    pub cells: Option<&'a Cells>,
    /// A mod's own word for which item, if any.
    pub detail: Option<&'a str>,
}

impl<'a> StackKey<'a> {
    /// Loose material, or a cut of one material: what every take named before
    /// a stack could hold several.
    ///
    /// **Never matches a cut of several materials** (§9.1 rule 6), including
    /// one that fills the block and so has no shape either.
    #[must_use]
    pub const fn of(material: MaterialId, shape: Option<Shape>, detail: Option<&'a str>) -> Self {
        Self {
            material,
            shape,
            cells: None,
            detail,
        }
    }

    /// Plain loose material of `material`, with no detail.
    #[must_use]
    pub const fn loose(material: MaterialId) -> Self {
        Self::of(material, None, None)
    }

    /// Whether `stack` is the stack this names.
    #[must_use]
    pub fn matches(&self, stack: &Stack) -> bool {
        *self == stack.key()
    }
}

/// The occupancy of a cells array: bit `i` set where cell `i` is not air.
#[must_use]
pub fn occupancy_of(cells: &Cells) -> u32 {
    cells
        .iter()
        .enumerate()
        .filter(|(_, material)| !material.is_air())
        .fold(0, |mask, (index, _)| mask | (1 << index))
}

/// Each distinct non-air material in `cells` with the mask of the cells it
/// fills, in ascending id.
///
/// What one item of a cut costs, material by material, and the order a
/// placement writes it in (Sub-Node Contract §7.2). A cut has at most 27
/// materials, so a sorted insert into a small `Vec` beats a map.
#[must_use]
pub fn by_material(cells: &Cells) -> Vec<(MaterialId, u32)> {
    let mut out: Vec<(MaterialId, u32)> = Vec::new();
    for (index, &material) in cells.iter().enumerate() {
        if material.is_air() {
            continue;
        }
        match out.binary_search_by_key(&material, |(held, _)| *held) {
            Ok(found) => out[found].1 |= 1 << index,
            Err(at) => out.insert(at, (material, 1 << index)),
        }
    }
    out
}

impl Stack {
    /// `count` items of the cut `cells` describes, in canonical form.
    ///
    /// Sub-Node Contract §9.1. `cells[i]` is the material of cell `i`, indexed
    /// by [`crate::block::subnode_index`], with air for an empty cell. One
    /// item costs one unit per occupied cell, so `units` is `count` times
    /// that and there is still one quantity (charter rule 5).
    ///
    /// Two or more distinct non-air materials give a cut of several; one
    /// material gives a plain cut, or loose material when it fills the block;
    /// all air, zero, or an amount that overflows gives nothing.
    #[must_use]
    pub fn mixed(cells: &Cells, count: u32) -> Option<Self> {
        let occupied = occupancy_of(cells).count_ones();
        Self::of_cells(cells, count.checked_mul(occupied)?)
    }

    /// `units` of the cut `cells` describes, in canonical form.
    ///
    /// [`Self::mixed`] counted in units rather than items: the form a stack
    /// read back from the wire or from disk arrives in. Everything but the
    /// cells and the amount is derived (§9.1 rules 1 to 3).
    #[must_use]
    pub fn of_cells(cells: &Cells, units: u32) -> Option<Self> {
        let mut lowest: Option<MaterialId> = None;
        let mut several = false;
        for &material in cells {
            if material.is_air() {
                continue;
            }
            match lowest {
                None => lowest = Some(material),
                Some(seen) => {
                    several |= material != seen;
                    lowest = Some(seen.min(material));
                }
            }
        }
        let material = lowest?;
        let shape = Shape::new(occupancy_of(cells));
        let stack = Self::new(material, units)?;
        Some(Self {
            shape,
            cells: several.then(|| Box::new(*cells)),
            ..stack
        })
    }

    /// This stack with its derived fields derived again.
    ///
    /// For a stack rebuilt from something that is not this module — a row on
    /// disk, a message, a mod's table — where `material` and `shape` beside
    /// the cells are only a claim (§9.1 rule 2: the lowest RUNTIME id is a
    /// fact about this session). A stack without cells comes back as it was.
    /// `None` for cells that are all air, which are no stack.
    #[must_use]
    pub fn canonical(self) -> Option<Self> {
        let Some(cells) = self.cells else {
            return Some(self);
        };
        Self::of_cells(&cells, self.units).map(|stack| Self {
            detail: self.detail,
            ..stack
        })
    }

    /// Whether this is the same item as `other`: the same material, cut,
    /// cells and detail. What decides whether two stacks may merge.
    #[must_use]
    pub fn same_item(&self, other: &Self) -> bool {
        self.key() == other.key()
    }

    /// This stack's identity, borrowed. See [`StackKey`].
    #[must_use]
    pub fn key(&self) -> StackKey<'_> {
        StackKey {
            material: self.material,
            shape: self.shape,
            cells: self.cells.as_deref(),
            detail: self.detail.as_deref(),
        }
    }

    /// How many units one item of this stack is.
    ///
    /// The cut's cells, or a whole block for loose material and for a cut of
    /// several that fills it.
    #[must_use]
    pub fn per_item(&self) -> u32 {
        self.shape.map_or(UNITS_PER_BLOCK, Shape::cells)
    }

    /// The fewest units that may be split off this stack.
    ///
    /// **One, except for a cut of several materials**, which moves in whole
    /// items (§9.1 rule 7). A unit of loose stone is stone, and a unit of a
    /// stone stair is still stone; a unit of a stair of stone and oak is
    /// neither, and a stack holding a fraction of one would hold material no
    /// placement can spend and no break can give back.
    #[must_use]
    pub fn grain(&self) -> u32 {
        if self.cells.is_some() {
            self.per_item()
        } else {
            1
        }
    }

    /// The most of `asked` units that may be taken from this stack: no more
    /// than it holds, and in whole [`Self::grain`]s.
    #[must_use]
    pub fn takeable(&self, asked: u32) -> u32 {
        let most = self.units.min(asked);
        let grain = self.grain();
        most - most % grain
    }

    /// The material of each of one item's 27 cells.
    ///
    /// What a placement writes and what a picture of the item draws, for any
    /// stack: a cut of several is its cells, a cut of one is its material in
    /// its shape's cells, and loose material is its material in all of them.
    #[must_use]
    pub fn item_cells(&self) -> Cells {
        if let Some(cells) = &self.cells {
            return **cells;
        }
        let mask = self.shape.map_or(Shape::ALL, Shape::occupancy);
        let mut cells = crate::block::EMPTY_CELLS;
        for (index, cell) in cells.iter_mut().enumerate() {
            if mask & (1 << index) != 0 {
                *cell = self.material;
            }
        }
        cells
    }

    /// Each material one item of this stack is made of, with how many cells
    /// of it, in ascending id.
    ///
    /// What one item costs to make (§9.1: one unit per cell, of that cell's
    /// material) and what breaking a placed one gives back.
    #[must_use]
    pub fn materials(&self) -> Vec<(MaterialId, u32)> {
        by_material(&self.item_cells())
            .into_iter()
            .map(|(material, mask)| (material, mask.count_ones()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockView, EMPTY_CELLS, subnode_index};
    use crate::inventory::slots::{Grab, Slots, View};
    use crate::inventory::{
        ITEMS_PER_STACK, StackError, break_block, consolidate, tipped, tipped_cells, turned,
        turned_cells, units_of_exactly,
    };

    const STONE: MaterialId = MaterialId(2);
    const OAK: MaterialId = MaterialId(5);
    const BRICK: MaterialId = MaterialId(7);

    /// A stair: the bottom layer of stone, and a step of oak on the back half
    /// of the middle layer. Oak is written first in cell order and is the
    /// HIGHER id, so a stack that took its material from the first cell it
    /// met would get it wrong.
    fn stair() -> Cells {
        let mut cells = EMPTY_CELLS;
        for z in 0..3 {
            for x in 0..3 {
                cells[subnode_index(x, 0, z)] = STONE;
            }
        }
        for x in 0..3 {
            cells[subnode_index(x, 1, 0)] = OAK;
        }
        cells
    }

    fn slots(stacks: Vec<Option<Stack>>) -> Slots {
        Slots {
            views: vec![View {
                name: "player:main".to_owned(),
                slots: stacks,
            }],
            grab: Grab::default(),
            main_fixed: false,
        }
    }

    #[test]
    fn two_materials_keep_their_cells_and_one_collapses_as_a_block_would() {
        // §9.1 rule 1: `BlockValue::canonical`'s rule, applied to a stack.
        let stair = Stack::mixed(&stair(), 2).expect("a cut of two materials");
        assert_eq!(stair.cells.as_deref(), Some(&self::stair()));

        // One material, not full: a plain cut, with no cells to carry.
        let mut slab = EMPTY_CELLS;
        slab[0] = STONE;
        slab[1] = STONE;
        let plain = Stack::mixed(&slab, 3).expect("a plain cut");
        assert_eq!(plain.cells, None, "one material is not a mixture");
        assert_eq!(plain.shape, Shape::new(0b11));
        assert_eq!(
            plain,
            Stack::shaped(STONE, Shape::new(0b11).expect("cut"), 3).expect("cut")
        );

        // One material, full: loose material, the same thing as 27 units.
        let whole = Stack::mixed(&[STONE; 27], 2).expect("loose");
        assert_eq!(whole, Stack::from_blocks(STONE, 2).expect("loose"));

        // All air, or none of it: no stack.
        assert_eq!(Stack::mixed(&EMPTY_CELLS, 5), None);
        assert_eq!(Stack::mixed(&self::stair(), 0), None);
        assert_eq!(Stack::mixed(&self::stair(), u32::MAX), None, "overflow");
    }

    #[test]
    fn the_material_is_the_lowest_id_among_the_cells_wherever_it_sits() {
        // §9.1 rule 2. Oak (5) sits in cells before stone (2) in index order
        // along the back row; the stack is filed under stone anyway.
        let mut cells = EMPTY_CELLS;
        cells[0] = BRICK;
        cells[1] = OAK;
        cells[26] = STONE;
        let stack = Stack::mixed(&cells, 1).expect("three materials");
        assert_eq!(stack.material, STONE);

        // And a claim to anything else is corrected when it is rebuilt.
        let claimed = Stack {
            material: BRICK,
            shape: None,
            ..stack.clone()
        };
        assert_eq!(claimed.canonical(), Some(stack));
    }

    #[test]
    fn the_shape_is_the_occupancy_and_absent_when_the_cut_fills_the_block() {
        // §9.1 rule 3: `Shape::new` refuses a full mask, and still does.
        let stack = Stack::mixed(&stair(), 1).expect("stair");
        assert_eq!(
            stack.shape.map(Shape::occupancy),
            Some(occupancy_of(&stair()))
        );

        let mut full = [STONE; 27];
        full[13] = OAK;
        let full = Stack::mixed(&full, 1).expect("a full block of two");
        assert_eq!(full.shape, None);
        assert!(full.cells.is_some(), "and still a mixture, not loose stone");
    }

    #[test]
    fn units_are_count_times_cells_and_a_slot_holds_ninety_of_them() {
        // §9.1 rule 4: one quantity, no exchange rate.
        let stack = Stack::mixed(&stair(), 4).expect("stair");
        assert_eq!(stack.units, 4 * 12);
        assert_eq!(stack.count(), 4);
        assert_eq!(stack.per_item(), 12);
        assert_eq!(stack.capacity(), ITEMS_PER_STACK * 12);

        let mut full = [STONE; 27];
        full[0] = OAK;
        let full = Stack::mixed(&full, 3).expect("full");
        assert_eq!(full.units, 3 * UNITS_PER_BLOCK);
        assert_eq!(full.count(), 3);
    }

    #[test]
    fn the_same_mixture_stacks_and_a_different_one_does_not() {
        // §9.1 rule 5. Stone-and-oak and stone-and-brick stairs have the same
        // material (stone, the lowest) and the same outline.
        let oak = Stack::mixed(&stair(), 2).expect("stair");
        let brick_cells = stair().map(|m| if m == OAK { BRICK } else { m });
        let brick = Stack::mixed(&brick_cells, 2).expect("stair");
        assert_eq!((oak.material, oak.shape), (brick.material, brick.shape));
        assert!(!oak.same_item(&brick));

        let mut held = oak.clone();
        assert_eq!(held.merge(&brick), Err(StackError::CellsMismatch));
        assert_eq!(held.units, oak.units, "a refused merge changed the stack");
        assert_eq!(held.merge(&oak), Ok(()));
        assert_eq!(held.count(), 4);

        // Consolidating keeps them apart, and the loose stone and the plain
        // stone cut of the same outline apart from both.
        let outline = Shape::new(occupancy_of(&stair())).expect("outline");
        let merged = consolidate([
            oak.clone(),
            Stack::new(STONE, 27).expect("loose"),
            brick.clone(),
            Stack::shaped(STONE, outline, 1).expect("plain"),
            oak.clone(),
        ]);
        assert_eq!(merged.len(), 4, "got {merged:?}");
        assert_eq!(
            merged.iter().map(|s| s.units).sum::<u32>(),
            3 * 24 + 27 + 12
        );

        // An inventory tops up the same mixture and not a different one.
        let mut inv = slots(vec![Some(oak.clone()), None]);
        assert!(inv.insert("player:main", brick.clone()));
        assert!(inv.insert("player:main", oak.clone()));
        let held: Vec<_> = inv.views[0].slots.iter().flatten().collect();
        assert_eq!(held.len(), 2);
        assert_eq!(held[0].count(), 4, "the oak stairs merged");
        assert_eq!(held[1].cells, brick.cells, "the brick stairs took a slot");
    }

    #[test]
    fn a_take_of_loose_material_never_takes_from_a_mixture() {
        // §9.1 rule 6 — including a mixture that fills the block, whose
        // material is stone and whose shape is `None`, exactly as loose
        // stone's are.
        let mut full = [STONE; 27];
        full[4] = OAK;
        let full = Stack::mixed(&full, 2).expect("full");
        let stair = Stack::mixed(&stair(), 2).expect("stair");
        let mut inv = slots(vec![Some(full.clone()), Some(stair.clone())]);
        assert_eq!(inv.take("player:main", StackKey::loose(STONE), 27), 0);
        let outline = stair.shape;
        assert_eq!(
            inv.take("player:main", StackKey::of(STONE, outline, None), 12),
            0
        );
        assert_eq!(inv.total_units(), u64::from(full.units + stair.units));
        assert_eq!(
            units_of_exactly(std::slice::from_ref(&full), StackKey::loose(STONE)),
            0
        );
        assert_eq!(
            crate::inventory::units_of(std::slice::from_ref(&full), STONE),
            0
        );

        // Named exactly, it is taken.
        assert_eq!(inv.take("player:main", full.key(), 27), 27);
        assert_eq!(inv.take("player:main", stair.key(), 24), 24);
        assert_eq!(inv.total_units(), 27);
    }

    #[test]
    fn a_mixture_moves_in_whole_items() {
        // §9.1 rule 7. A unit of stone-and-oak is neither.
        let stair = Stack::mixed(&stair(), 3).expect("stair");
        assert_eq!(stair.grain(), 12);
        assert_eq!(stair.takeable(30), 24);
        assert_eq!(stair.takeable(11), 0);
        assert_eq!(Stack::new(STONE, 5).expect("loose").grain(), 1);

        // Halving three stairs leaves one and takes two — the larger half in
        // the hand, as for units — and never a stair and a half.
        let mut inv = slots(vec![Some(stair.clone()), None]);
        assert!(inv.right_click("player:main", 0));
        assert_eq!(inv.grab.held.as_ref().map(Stack::count), Some(2));
        assert_eq!(inv.views[0].slots[0].as_ref().map(Stack::count), Some(1));
        // Putting one down puts one stair down.
        assert!(inv.right_click("player:main", 1));
        assert_eq!(inv.views[0].slots[1].as_ref().map(|s| s.units), Some(12));
        assert_eq!(inv.total_units(), u64::from(stair.units));

        // A take asking for part of an item takes the whole ones it covers.
        let mut inv = slots(vec![Some(stair.clone())]);
        assert_eq!(inv.take("player:main", stair.key(), 20), 12);
        assert_eq!(inv.total_units(), 24);
    }

    #[test]
    fn a_split_and_a_merge_keep_the_cells() {
        let mut stack = Stack::mixed(&stair(), 4).expect("stair");
        let half = stack.split(24).expect("split");
        assert_eq!(half.cells, stack.cells);
        assert!(half.same_item(&stack));
        assert_eq!(stack.merge(&half), Ok(()));
        assert_eq!(stack.count(), 4);
    }

    #[test]
    fn one_item_costs_each_material_its_own_cells() {
        // §9.1's conservation: nine of stone and three of oak make a stair.
        let stack = Stack::mixed(&stair(), 1).expect("stair");
        assert_eq!(stack.materials(), vec![(STONE, 9), (OAK, 3)]);
        // And a stack of one material is its material in its cells.
        let plain = Stack::shaped(OAK, Shape::new(0b111).expect("cut"), 2).expect("cut");
        assert_eq!(plain.materials(), vec![(OAK, 3)]);
        assert_eq!(
            Stack::new(OAK, 5).expect("loose").materials(),
            vec![(OAK, 27)]
        );
    }

    #[test]
    fn a_turn_moves_each_material_with_its_cell() {
        // Sub-Node Contract §7.1: one permutation for the mask and the cells.
        let stair = stair();
        for quarters in 0..5 {
            let cells = turned_cells(&stair, quarters);
            assert_eq!(occupancy_of(&cells), turned(occupancy_of(&stair), quarters));
            let cells = tipped_cells(&stair, quarters);
            assert_eq!(occupancy_of(&cells), tipped(occupancy_of(&stair), quarters));
            for (material, mask) in by_material(&stair) {
                let turned_mask = by_material(&turned_cells(&stair, quarters))
                    .into_iter()
                    .find(|(held, _)| *held == material)
                    .map(|(_, mask)| mask);
                assert_eq!(turned_mask, Some(turned(mask, quarters)), "{material:?}");
            }
        }
        assert_eq!(turned_cells(&stair, 4), stair, "four turns are no turn");
        assert_eq!(tipped_cells(&stair, 4), stair, "four tips are no tip");
        // The oak step at the back comes round to the side.
        let east = turned_cells(&stair, 1);
        assert_eq!(east[subnode_index(0, 1, 1)], OAK);
    }

    #[test]
    fn what_breaking_a_placed_mixture_gives_is_loose_material_per_material() {
        // Contract §9 is unchanged by §9.1: a placed cut breaks into rubble.
        let stack = Stack::mixed(&stair(), 1).expect("stair");
        let drops = break_block(BlockView::Mixed(&stack.item_cells()));
        assert_eq!(
            drops,
            vec![
                Stack::new(STONE, 9).expect("stone"),
                Stack::new(OAK, 3).expect("oak"),
            ]
        );
    }

    /// §9.1's conservation, as charter rule 15 asks for it: made from loose
    /// units, placed, and broken, every material's units are what they were.
    mod conservation {
        use proptest::prelude::*;

        use super::*;
        use crate::coords::BlockPos;
        use crate::proto::Edit;

        /// A cut of two to four materials drawn from ids 2..6, at least two
        /// of them present.
        fn any_cut() -> impl Strategy<Value = Cells> {
            prop::collection::vec(prop_oneof![2 => Just(0u16), 3 => 2u16..6], 27)
                .prop_map(|ids| {
                    let mut cells = EMPTY_CELLS;
                    for (cell, id) in cells.iter_mut().zip(ids) {
                        *cell = MaterialId(id);
                    }
                    cells
                })
                .prop_filter("two or more materials", |cells| {
                    by_material(cells).len() >= 2
                })
        }

        /// Applies one edit to a block's cells, as a chunk would.
        fn apply(block: &mut Cells, edit: &Edit) {
            match *edit {
                Edit::Block { material, .. } => *block = [MaterialId(material); 27],
                Edit::Partial {
                    material,
                    occupancy,
                    ..
                } => {
                    for (index, cell) in block.iter_mut().enumerate() {
                        *cell = if occupancy & (1 << index) != 0 {
                            MaterialId(material)
                        } else {
                            MaterialId::AIR
                        };
                    }
                }
                Edit::SubNode { pos, material } => {
                    let axis = |v: i32| v.rem_euclid(3) as u32;
                    block[subnode_index(axis(pos.x), axis(pos.y), axis(pos.z))] =
                        MaterialId(material);
                }
            }
        }

        fn loose_of(inv: &Slots, material: MaterialId) -> u32 {
            inv.views[0]
                .slots
                .iter()
                .flatten()
                .filter(|stack| stack.key() == StackKey::loose(material))
                .map(|stack| stack.units)
                .sum()
        }

        proptest! {
            #[test]
            fn made_placed_and_broken_every_material_comes_back(
                cut in any_cut(),
                count in 1u32..6,
                spare in 0u32..40,
                quarters in 0u32..4,
                wall in any::<bool>(),
            ) {
                let recipe = by_material(&cut);
                // Enough loose material of each for `count` items, and some
                // over, so a take that took the wrong amount would show.
                let mut inv = slots(Vec::new());
                for (material, mask) in &recipe {
                    let units = mask.count_ones() * count + spare;
                    prop_assert!(inv.insert("player:main", Stack::new(*material, units).expect("loose")));
                }
                let before: Vec<u32> = recipe.iter().map(|(m, _)| loose_of(&inv, *m)).collect();

                // Make: take each material's cells times count, give the cut.
                for (material, mask) in &recipe {
                    let cost = mask.count_ones() * count;
                    prop_assert_eq!(inv.take("player:main", StackKey::loose(*material), cost), cost);
                }
                let made = Stack::mixed(&cut, count).expect("the cut");
                let key_cells = made.cells.clone();
                prop_assert!(inv.insert("player:main", made.clone()));

                // Place every item, each into an empty block, turned as a
                // player standing somewhere would turn it; break each, and
                // put what comes out back in the inventory.
                let face = if wall { [1i8, 0, 0] } else { [0i8, 1, 0] };
                let toward = [[0.0f32, 1.0], [1.0, 0.0], [0.0, -1.0], [-1.0, 0.0]][quarters as usize];
                for item in 0..count {
                    let key = StackKey { cells: key_cells.as_deref(), ..made.key() };
                    prop_assert_eq!(inv.take("player:main", key, made.per_item()), made.per_item());
                    let placed = crate::place::oriented_cells(&cut, face, toward);
                    let pos = BlockPos::new(item as i32, 0, 0);
                    let mut block = EMPTY_CELLS;
                    for (_, edits) in crate::place::cut_writes(pos, &placed, &EMPTY_CELLS) {
                        for edit in &edits {
                            apply(&mut block, edit);
                        }
                    }
                    prop_assert_eq!(block, placed, "the block is not the cut");
                    for stack in break_block(BlockView::Mixed(&block)) {
                        prop_assert!(inv.insert("player:main", stack));
                    }
                }

                // Nothing of the cut is left, and every material is back.
                prop_assert_eq!(units_of_exactly(
                    &inv.views[0].slots.iter().flatten().cloned().collect::<Vec<_>>(),
                    made.key(),
                ), 0);
                let after: Vec<u32> = recipe.iter().map(|(m, _)| loose_of(&inv, *m)).collect();
                prop_assert_eq!(before, after);
            }
        }
    }
}
