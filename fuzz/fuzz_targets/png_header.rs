// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `content::png_dimensions` — the twenty-four bytes a server reads off a
//! player's own skin before forwarding it (Life ask 23). Charter rule 14: a
//! parser on hostile input gets a fuzz target the task it lands in.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some((width, height)) = tiamat_core::content::png_dimensions(data) {
        assert!(width > 0 && height > 0);
        assert_eq!(
            tiamat_core::content::png_dimensions(&tiamat_core::content::png_header(width, height)),
            Some((width, height))
        );
    }
});
