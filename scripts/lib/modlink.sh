#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# A developer's mod lives in a checkout beside the engine and appears under
# game/ as a LINK: a symlink, or a link file — a plain text file named for the
# mod holding candidate paths to its directory, one per line, relative to game/
# or absolute, `#` comments and blank lines ignored, the first that holds a
# mod.toml being the one. The engine's scanner follows both
# (`modload::manifest::follow_link_file`). The link file exists because one
# working tree is read from two operating systems, and a symlink made on either
# is a reparse point the other cannot follow; the candidates differ because the
# same checkout is a sibling directory on one machine and a mount on the other.
#
# Sourced by fetch-mods.sh, conform.sh and bundle-lock.sh.

# Whether game/<id> is a link of either kind.
modlink_is() {
    [ -L "$1" ] || { [ -f "$1" ] && [ "$(wc -c < "$1")" -le 4096 ]; }
}

# The mod directory a link resolves to, canonical, or nothing.
modlink_target() {
    local link="$1"
    if [ -L "$link" ]; then
        readlink -f "$link" 2>/dev/null || true
        return
    fi
    [ -f "$link" ] || return 0
    local base line candidate
    base="$(cd "$(dirname "$link")" && pwd -P)"
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%%$'\r'}"
        line="$(printf '%s' "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
        [ -z "$line" ] && continue
        case "$line" in \#*) continue ;; esac
        case "$line" in /*) candidate="$line" ;; *) candidate="$base/$line" ;; esac
        if [ -f "$candidate/mod.toml" ]; then
            (cd "$candidate" && pwd -P)
            return
        fi
    done < "$link"
}

# Writes game/<id> as a link file to an absolute mod directory.
modlink_write() {
    printf '# a link the engine follows; see scripts/lib/modlink.sh\n%s\n' "$2" > "$1"
}
