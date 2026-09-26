// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Tells the launcher which target it was built for, and which key it trusts.
//!
//! The manifest names a build per target triple, so the launcher has to know
//! its own — and cargo only tells a build script, never the crate itself.
//!
//! The release key comes from `release-key.pub` at the root of the repository
//! (see that file for why it lives there rather than in CI), and the
//! environment may override it for a test build. **A build that finds neither
//! trusts nothing and applies no updates**, which is what a copy built from a
//! fork with no key of its own should do.

use std::path::Path;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=TIAMAT_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
    embed_icon();
    println!("cargo:rerun-if-env-changed=TIAMAT_RELEASE_KEY");

    if std::env::var_os("TIAMAT_RELEASE_KEY").is_some() {
        // Set in the environment: cargo passes it through to `option_env!`,
        // and this build script must not override it.
        return;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../release-key.pub");
    println!("cargo:rerun-if-changed={}", root.display());
    let Ok(text) = std::fs::read_to_string(&root) else {
        println!("cargo:warning=no release-key.pub; this build applies no updates");
        return;
    };
    let key = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'));
    match key {
        Some(key) if key.len() == 64 && key.chars().all(|c| c.is_ascii_hexdigit()) => {
            println!("cargo:rustc-env=TIAMAT_RELEASE_KEY={key}");
        }
        _ => println!("cargo:warning=release-key.pub holds no 64-character hex key"),
    }
}

/// The icon on the executable, on Windows: what Explorer, the taskbar and
/// the title bar show. `winresource` compiles a resource script with the
/// SDK's `rc.exe`, which the MSVC toolchain the release builds on has; on
/// every other target this does nothing, and the window sets its own icon at
/// start. A missing tool is a warning and a plain executable, never a failed
/// build.
fn embed_icon() {
    let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icon/tiamat.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let Err(err) = winresource::WindowsResource::new()
        .set_icon(&icon.to_string_lossy())
        .compile()
    {
        println!("cargo:warning=no icon on the executable: {err}");
    }
}
