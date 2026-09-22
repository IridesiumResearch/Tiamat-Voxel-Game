#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Makes a fresh dev container able to build, test and run Tiamat.
#
# The base image has none of it, and a rebuilt container loses whatever was
# installed by hand, so everything the workspace needs is here rather than in
# somebody's memory. It is safe to run again.
set -euo pipefail
cd "$(dirname "$0")/.."

# What the workspace links, and no more: a C toolchain and pkg-config for the
# `-sys` crates, ALSA's headers for the audio backend (as CI installs), and
# Mesa's Vulkan drivers so the GPU tests find a software adapter rather than
# skipping. SQLite is bundled; winit loads X11 and Wayland at run time.
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential pkg-config libasound2-dev mesa-vulkan-drivers libvulkan1

# rustup, and then the toolchain `rust-toolchain.toml` pins — `cargo --version`
# inside the repository is what installs it.
if ! command -v rustup >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/rustup" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain none
fi
# shellcheck source=/dev/null
. "$HOME/.cargo/env"
cargo --version

# The build directory is a named volume (see devcontainer.json), which Docker
# creates owned by root — and its parent too, when the image had no `~/.cache`.
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    sudo mkdir -p "$CARGO_TARGET_DIR"
    sudo chown "$(id -u):$(id -g)" "$(dirname "$CARGO_TARGET_DIR")" "$CARGO_TARGET_DIR"
fi
