#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# The corresponding source for a release: the engine at HEAD and every bundled
# mod at the commit bundle.toml pins, as one archive published beside the
# binaries. A moving main branch is not a reference for an older binary; this
# file is. docs/distribution.md §7.
#
# Usage: scripts/source-archive.sh [--out dist]

set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib/bundle.sh
source scripts/lib/bundle.sh

out="dist"
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="${2:?--out needs a directory}"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

version="$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)"
commit="$(git rev-parse HEAD)"
name="tiamat-${version}-source"
stage="${out}/${name}"
rm -rf "$stage"
mkdir -p "$stage"

echo "==> engine at ${commit:0:7}"
git archive --format=tar HEAD | tar -x -C "$stage"

echo "==> bundled mods"
mkdir -p "$stage/mods"
while IFS=$'\t' read -r id repo path modcommit; do
    echo "    $id at ${modcommit:0:7}"
    # The whole repository, not only the mod directory: its tools and tests
    # are part of the source a reader needs to rebuild it.
    bundle_extract "$id" "$repo" "$modcommit" "" "$stage/mods/$id"
done < <(bundle_mods)

cat > "$stage/SOURCE.md" <<MD
# Tiamat ${version} — corresponding source

The engine at commit ${commit}
(https://github.com/IridesiumResearch/Tiamat-Voxel-Game/tree/${commit}), and
under mods/, each bundled mod's repository at the commit bundle.toml pins.
This is the source the binaries of release ${version} were built from.

To build: install the toolchain rust-toolchain.toml names (rustup does this on
first use), then from this directory

    scripts/package.sh --target <triple> --channel <channel>

which builds the client, the server and the launcher and lays out the archive;
the mods are taken from bundle.toml, and with the repositories present under
mods/ no network is needed. docs/distribution.md describes the layout and
docs/hosting.md the release steps.
MD

mkdir -p "$out"
archive="${out}/${name}.tar.gz"
rm -f "$archive"
tar -czf "$archive" -C "$out" "$name"
rm -rf "$stage"
echo "==> $archive ($(du -h "$archive" | cut -f1))"
