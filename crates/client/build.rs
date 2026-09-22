// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Tells the client which target it was built for, and which key it trusts.
//!
//! The same two facts the launcher's build script stamps, and for the same
//! reasons: the manifest names a build per target triple, and the release key
//! is a trust root that lives in the repository rather than in CI. See
//! `release-key.pub` and `docs/distribution.md` §2.

use std::path::Path;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=TIAMOT_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TIAMOT_RELEASE_KEY");

    if std::env::var_os("TIAMOT_RELEASE_KEY").is_some() {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../release-key.pub");
    println!("cargo:rerun-if-changed={}", root.display());
    let Ok(text) = std::fs::read_to_string(&root) else {
        return;
    };
    let key = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'));
    if let Some(key) = key
        && key.len() == 64
        && key.chars().all(|c| c.is_ascii_hexdigit())
    {
        println!("cargo:rustc-env=TIAMOT_RELEASE_KEY={key}");
    }
}
