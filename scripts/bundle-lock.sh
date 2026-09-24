#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Writes `bundle.toml`: the default mods a release bundles, pinned by commit.
#
# The mods live in repositories of their own and appear here as symlinks under
# `game/`. A release must record exactly which version of each it carries —
# the corresponding source for a binary is the engine at its commit AND every
# mod at its commit — and CI, which has no symlinks, packages from this file.
# Run it before tagging a release, and commit the result with the version bump.
#
# A commit that only exists on somebody's machine is one nobody else can fetch,
# so an unpushed HEAD is refused unless --allow-unpushed says otherwise.
#
# Usage: scripts/bundle-lock.sh [--allow-unpushed] [--out bundle.toml]

set -euo pipefail
cd "$(dirname "$0")/.."

allow_unpushed=0
out="bundle.toml"
while [ $# -gt 0 ]; do
    case "$1" in
        --allow-unpushed) allow_unpushed=1; shift ;;
        --out) out="${2:?--out needs a path}"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

# The repositories sit on a mounted drive here, which git treats as somebody
# else's until told otherwise.
g() { git -c safe.directory='*' "$@"; }

entries=()
failed=0
for link in game/*; do
    [ -L "$link" ] || continue
    id="$(basename "$link")"
    dir="$(readlink -f "$link" 2>/dev/null || true)"
    if [ -z "$dir" ] || [ ! -f "$dir/mod.toml" ]; then
        echo "error: $link points nowhere readable; mount the drive it lives on" >&2
        failed=1
        continue
    fi
    root="$(g -C "$dir" rev-parse --show-toplevel)"
    path="${dir#"$root"/}"
    commit="$(g -C "$dir" rev-parse HEAD)"
    repo="$(g -C "$dir" remote get-url origin)"
    if [ -n "$(g -C "$root" status --porcelain)" ]; then
        echo "note: $id has uncommitted changes; the lock records its HEAD, $commit" >&2
    fi
    if ! g -C "$root" branch -r --contains "$commit" 2>/dev/null | grep -q .; then
        if [ "$allow_unpushed" -eq 1 ]; then
            echo "warning: $id's HEAD $commit is not on any remote branch; CI cannot fetch it" >&2
        else
            echo "error: $id's HEAD $commit is not on any remote branch. Push it, or pass --allow-unpushed." >&2
            failed=1
            continue
        fi
    fi
    entries+=("$id	$repo	$path	$commit")
done

if [ "$failed" -ne 0 ]; then
    exit 1
fi
if [ "${#entries[@]}" -eq 0 ]; then
    echo "error: no mod symlinks under game/; nothing to pin" >&2
    exit 1
fi

{
    echo "# SPDX-FileCopyrightText: Iridesium"
    echo "# SPDX-License-Identifier: GPL-3.0-only"
    echo "#"
    echo "# The default mods a release bundles, each pinned to the commit it is taken"
    echo "# at. Written by scripts/bundle-lock.sh from the repositories beside this"
    echo "# checkout; read by scripts/package.sh, which packages a mod from this commit"
    echo "# and never from a working tree. docs/distribution.md §6 and §7."
    printf '%s\n' "${entries[@]}" | LC_ALL=C sort | while IFS=$'\t' read -r id repo path commit; do
        echo
        echo "[[mod]]"
        echo "id = \"$id\""
        echo "repo = \"$repo\""
        echo "path = \"$path\""
        echo "commit = \"$commit\""
    done
} > "$out"
echo "wrote $out with ${#entries[@]} mods"
