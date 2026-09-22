// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Tells the launcher which target it was built for.
//!
//! The manifest names a build per target triple, so the launcher has to know
//! its own — and cargo only tells a build script, never the crate itself.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=TIAMOT_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
    // The release workflow stamps these; a working copy has neither, and the
    // launcher then applies no updates at all.
    println!("cargo:rerun-if-env-changed=TIAMOT_RELEASE_KEY");
}
