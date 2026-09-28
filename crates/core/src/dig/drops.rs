// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a dig yields, when a mod said something other than the block itself.
//!
//! Sub-Node Contract §9's ordinary rule: a block drops itself, 27 units whole
//! and one per occupied cell otherwise. A mod may replace that at registration
//! (`register_block{ drops = ... }`) or for one dig (`on_dig_complete`
//! answering `{ drops = ... }`, Craft ask 3), and both say units **per full
//! block**, so a dig that takes nine cells yields a third of it.
//!
//! # Credited as it is earned
//!
//! A block comes apart over several ticks and the player is paid per bite
//! rather than at the end — a dig abandoned halfway keeps what came off, as it
//! always has. A third of three units is not a whole unit, so each bite adds
//! its share in twenty-sevenths and pays out whatever whole units that makes,
//! carrying the remainder to the next bite. Over a whole block the carry comes
//! out exactly: the total is the rule's total whatever the bite pattern (the
//! property test below). What is left when the dig ends is less than a unit,
//! and goes with the dig.
//!
//! # Not a conservation law
//!
//! `removed_units` is one: what an edit took out of the world is what it gives
//! back. A drop rule deliberately is not — ore that drops gems, cracked rock
//! that drops the rock it was — and that is a mod's decision (charter rule 1).
//! The engine's rule is only that what the mod said is paid exactly.

use std::collections::BTreeMap;

use crate::UNITS_PER_BLOCK;
use crate::inventory::Stack;
use crate::material::MaterialId;

/// A drop rule: what a full block yields, in units per material.
pub type Rule = [(MaterialId, u32)];

/// One dig's yield: the rule a hook chose for it, and the fractions owed
/// between bites.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Yield {
    /// What this dig yields when `on_dig_complete` said, instead of what the
    /// block's registration says.
    rule: Option<Vec<(MaterialId, u32)>>,
    /// Twenty-sevenths owed per material, carried between bites.
    pending: BTreeMap<MaterialId, u32>,
}

impl Yield {
    /// Fresh: no rule of its own, nothing owed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rule: None,
            pending: BTreeMap::new(),
        }
    }

    /// Sets, or clears, the rule for this dig.
    pub fn set_rule(&mut self, rule: Option<Vec<(MaterialId, u32)>>) {
        self.rule = rule;
    }

    /// The rule a hook chose, if one did.
    #[must_use]
    pub fn rule(&self) -> Option<&Rule> {
        self.rule.as_deref()
    }

    /// Pays out for `cells` cells of `material` coming off.
    ///
    /// The dig's own rule if a hook set one, else the block's `registered`
    /// rule, else Contract §9: the material itself, one unit per cell. Stacks
    /// come back in ascending material order, like `removed_units`.
    pub fn earn(
        &mut self,
        material: MaterialId,
        cells: u32,
        registered: Option<&Rule>,
    ) -> Vec<Stack> {
        let Some(rule) = self.rule.as_deref().or(registered) else {
            return Stack::new(material, cells).into_iter().collect();
        };
        let per_block = u64::from(UNITS_PER_BLOCK);
        let mut paid: BTreeMap<MaterialId, u32> = BTreeMap::new();
        for (dropped, units) in rule {
            let owed = self.pending.entry(*dropped).or_insert(0);
            // In `u64` so nothing wraps: the rule is a `u32`, a bite is at
            // most a block, and what is owed is under a block.
            let total = u64::from(*owed) + u64::from(*units) * u64::from(cells);
            let whole = u32::try_from(total / per_block).unwrap_or(u32::MAX);
            *owed = u32::try_from(total % per_block).unwrap_or(0);
            if whole > 0 {
                let so_far = paid.entry(*dropped).or_insert(0);
                *so_far = so_far.saturating_add(whole);
            }
        }
        paid.into_iter()
            .filter_map(|(material, units)| Stack::new(material, units))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STONE: MaterialId = MaterialId(3);
    const GEM: MaterialId = MaterialId(5);
    const DUST: MaterialId = MaterialId(9);

    fn units_of(stacks: &[Stack], material: MaterialId) -> u32 {
        stacks
            .iter()
            .filter(|stack| stack.material == material)
            .map(|stack| stack.units)
            .sum()
    }

    #[test]
    fn no_rule_means_the_block_itself_one_unit_per_cell() {
        let mut dig = Yield::new();
        let paid = dig.earn(STONE, 5, None);
        assert_eq!(paid, vec![Stack::new(STONE, 5).expect("stack")]);
    }

    #[test]
    fn a_registered_rule_is_paid_per_full_block_across_the_bites() {
        // Three gems a block, over bites of ten, ten and seven cells: one at
        // each payout and never a fourth, whatever the rounding did along
        // the way.
        let mut dig = Yield::new();
        let rule = [(GEM, 3)];
        let mut total = 0;
        for bite in [10, 10, 7] {
            total += units_of(&dig.earn(STONE, bite, Some(&rule)), GEM);
        }
        assert_eq!(total, 3);
        assert_eq!(
            dig.pending.get(&GEM).copied(),
            Some(0),
            "nothing owed after a whole block"
        );
    }

    #[test]
    fn a_partial_block_pays_its_share_and_the_fraction_goes_with_the_dig() {
        let rule = [(GEM, 3)];
        let mut nine = Yield::new();
        assert_eq!(
            units_of(&nine.earn(STONE, 9, Some(&rule)), GEM),
            1,
            "a third of three"
        );
        let mut eight = Yield::new();
        assert!(
            eight.earn(STONE, 8, Some(&rule)).is_empty(),
            "less than a unit is nothing yet"
        );
    }

    #[test]
    fn a_hooks_rule_beats_the_blocks_and_an_empty_rule_yields_nothing() {
        let mut dig = Yield::new();
        dig.set_rule(Some(vec![(GEM, 54)]));
        let paid = dig.earn(STONE, 1, Some(&[(DUST, 27)]));
        assert_eq!(paid, vec![Stack::new(GEM, 2).expect("stack")]);

        let mut nothing = Yield::new();
        assert!(
            nothing.earn(STONE, 27, Some(&[])).is_empty(),
            "`drops = {{}}` drops nothing"
        );
    }

    #[test]
    fn a_rule_naming_several_things_pays_each_in_material_order() {
        let mut dig = Yield::new();
        let paid = dig.earn(STONE, 27, Some(&[(DUST, 2), (GEM, 1)]));
        assert_eq!(
            paid,
            vec![
                Stack::new(GEM, 1).expect("stack"),
                Stack::new(DUST, 2).expect("stack")
            ]
        );
    }

    proptest::proptest! {
        #[test]
        fn a_whole_block_pays_exactly_the_rule_however_it_comes_apart(
            units in proptest::collection::vec(0..2000u32, 1..4),
            bites in proptest::collection::vec(1..=27u32, 1..27),
        ) {
            let rule: Vec<(MaterialId, u32)> = units
                .iter()
                .enumerate()
                .map(|(index, units)| (MaterialId(10 + index as u16), *units))
                .collect();
            let mut dig = Yield::new();
            let mut left = UNITS_PER_BLOCK;
            let mut paid: BTreeMap<MaterialId, u32> = BTreeMap::new();
            for bite in bites {
                if left == 0 {
                    break;
                }
                let cells = bite.min(left);
                left -= cells;
                for stack in dig.earn(STONE, cells, Some(&rule)) {
                    *paid.entry(stack.material).or_insert(0) += stack.units;
                }
            }
            for stack in dig.earn(STONE, left, Some(&rule)) {
                *paid.entry(stack.material).or_insert(0) += stack.units;
            }
            for (material, units) in &rule {
                proptest::prop_assert_eq!(paid.get(material).copied().unwrap_or(0), *units);
            }
        }
    }
}
