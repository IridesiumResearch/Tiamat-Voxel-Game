#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Reading bundle.toml and materialising a mod at the commit it pins. Sourced by
# scripts/package.sh and scripts/source-archive.sh, so the two cannot disagree
# about what "the mod at that commit" means.

# The repositories may sit on a mounted drive, which git treats as somebody
# else's until told otherwise.
bundle_git() { git -c safe.directory='*' "$@"; }

# Prints one line per mod: id, repo, path, commit — tab-separated.
bundle_mods() {
    local lock="${1:-bundle.toml}"
    awk '
        function flush() {
            if (id != "") printf "%s\t%s\t%s\t%s\n", id, repo, path, commit
            id = ""; repo = ""; path = ""; commit = ""
        }
        /^\[\[mod\]\]/ { flush(); next }
        /^[a-z]+ = "/ {
            key = $1
            value = $0; sub(/^[a-z]+ = "/, "", value); sub(/".*$/, "", value)
            if (key == "id") id = value
            else if (key == "repo") repo = value
            else if (key == "path") path = value
            else if (key == "commit") commit = value
        }
        END { flush() }
    ' "$lock"
}

# A git directory holding `commit` of `repo`: the checkout beside this one when
# it has the commit, otherwise a fetch of exactly that commit into a cache
# under the target directory. Prints the directory.
bundle_repo_dir() {
    local id="$1" repo="$2" commit="$3"
    local link="game/$id" dir root
    if [ -L "$link" ]; then
        dir="$(readlink -f "$link" 2>/dev/null || true)"
        if [ -n "$dir" ] && [ -d "$dir" ]; then
            root="$(bundle_git -C "$dir" rev-parse --show-toplevel 2>/dev/null || true)"
            if [ -n "$root" ] && bundle_git -C "$root" cat-file -e "$commit^{commit}" 2>/dev/null; then
                printf '%s\n' "$root"
                return 0
            fi
        fi
    fi
    local cache="${CARGO_TARGET_DIR:-target}/bundle/$id"
    if ! bundle_git -C "$cache" cat-file -e "$commit^{commit}" 2>/dev/null; then
        rm -rf "$cache"
        mkdir -p "$cache"
        bundle_git -C "$cache" init -q
        bundle_git -C "$cache" remote add origin "$repo"
        # GitHub serves any reachable commit by hash, so one shallow fetch of
        # exactly the pinned commit is enough — no branch, no history.
        if ! bundle_git -C "$cache" fetch -q --depth 1 origin "$commit"; then
            echo "error: could not fetch $id at $commit from $repo" >&2
            return 1
        fi
    fi
    printf '%s\n' "$cache"
}

# Extracts `subpath` (or the whole tree when empty) of the mod at its commit
# into `dest`, files only: no .git, no symlinks into anybody's home directory.
bundle_extract() {
    local id="$1" repo="$2" commit="$3" subpath="$4" dest="$5"
    local gitdir
    gitdir="$(bundle_repo_dir "$id" "$repo" "$commit")" || return 1
    rm -rf "$dest"
    mkdir -p "$dest"
    if [ -n "$subpath" ]; then
        bundle_git -C "$gitdir" archive --format=tar "$commit" -- "$subpath" \
            | tar -x -C "$dest" --strip-components="$(printf '%s' "$subpath" | awk -F/ '{print NF}')"
    else
        bundle_git -C "$gitdir" archive --format=tar "$commit" | tar -x -C "$dest"
    fi
}
