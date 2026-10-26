// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The lookalike fold that keeps a reserved name's impersonators out.
//!
//! A canonical name is `a–z`, `0–9` and `_`, and the holder picks the display
//! casing. In most sans-serif fonts that re-opens a few confusables: canonical
//! `lridesium` shown as `lridesium` reads as `Iridesium`, and `c0re_testkit`
//! reads as `core_testkit`. The owner's decision (2026-10-26, A7) is that this
//! matters **only for reserved names** — the administrative set and the default
//! mods' ids, where a lookalike is an impersonation of the project or of a
//! mod — and not between players, who "will have their fun". So the fold is
//! applied to the reserved list and nothing else: `alice` and `a1ice` may both
//! be held, `iridesium` and `lridesium` may not.
//!
//! # The fold, in one place
//!
//! A name's **skeleton** is the name with each character replaced by its row:
//!
//! | characters | skeleton |
//! |---|---|
//! | `i`, `I`, `l`, `L`, `1` | `i` |
//! | `o`, `O`, `0` | `o` |
//! | any other letter | itself, lower-cased |
//! | any other character | itself |
//!
//! Upper-case input is folded too, although a canonical name never has any,
//! because the confusion is between what a display casing *shows*: `I` and `l`
//! are the pair that started this. Two names are lookalikes when their
//! skeletons are equal. The table is deliberately small: it is the set the
//! review found readable as one another in the fonts the game and the site
//! use, not a Unicode confusables list (the character set has no room for one).

/// A name's skeleton, as the table in the module documentation defines it.
#[must_use]
pub fn skeleton(name: &str) -> String {
    name.chars()
        .map(|character| match character.to_ascii_lowercase() {
            'i' | 'l' | '1' => 'i',
            'o' | '0' => 'o',
            other => other,
        })
        .collect()
}

/// Whether two names read as one another under the fold.
#[must_use]
pub fn lookalikes(left: &str, right: &str) -> bool {
    left.len() == right.len() && skeleton(left) == skeleton(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_folds_what_it_says_and_nothing_else() {
        assert_eq!(skeleton("il1IL"), "iiiii");
        assert_eq!(skeleton("o0O"), "ooo");
        assert_eq!(skeleton("abc_xyz9"), "abc_xyz9");
        // Every other letter and digit is its own row.
        for character in "abcdefghjkmnpqrstuvwxyz23456789_".chars() {
            assert_eq!(skeleton(&character.to_string()), character.to_string());
        }
    }

    #[test]
    fn a_reserved_name_and_its_impersonators_share_a_skeleton() {
        for impostor in ["lridesium", "ir1desium", "1rides1um", "iridesium"] {
            assert!(lookalikes(impostor, "iridesium"), "{impostor}");
        }
        for impostor in ["c0re_testkit", "core_testk1t", "c0re_testk1t"] {
            assert!(lookalikes(impostor, "core_testkit"), "{impostor}");
        }
        // Different names stay different.
        assert!(!lookalikes("core_testkat", "core_testkit"));
        assert!(!lookalikes("iridesiums", "iridesium"));
        assert!(!lookalikes("bob", "b0bby"));
    }
}
