#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Every `game.*` function the engine registers must appear in the API stubs.
#
# The stubs are what a mod author's editor reads. A function that exists but is
# undocumented is invisible to them; one documented but removed is worse, since
# they will write code against it. Both drift silently, because neither breaks a
# build — which is exactly why this check exists.
#
# Deliberately one-directional in strictness: an engine function missing from
# the stubs is an ERROR, a stub entry with no engine function is a WARNING.
# Method stubs (`ChunkBuffer:fill_all`) and type declarations legitimately have
# no `game.set("...")` call behind them.
#
# Usage: scripts/check-stubs.sh

set -euo pipefail

cd "$(dirname "$0")/.."

STUBS="api/stubs/game.lua"
SOURCE="crates/core/src/script/mlua_vm.rs"

if [ ! -f "$STUBS" ]; then
    echo "missing $STUBS" >&2
    exit 1
fi

# Everything the engine puts on the `game` table: `game.set("name", ...)`.
#
# Classified by what the STUBS say, not by the name's letter case. An earlier
# version guessed "lowercase means function, uppercase means constant" and
# flagged `game.mod_id` — a lowercase field — as an undocumented function. What
# matters is that each name is documented SOMEHOW, either as a function or as a
# field.
#
# Newlines are stripped first and whitespace is allowed after the paren,
# because `rustfmt` breaks the line whenever the arguments are long enough:
#
#     game.set(
#         "register_on_dig_complete",
#         self.hook_registrar(mod_id, HOOK_DIG, DIGGERS)?,
#     )
#
# A line-oriented grep sees no `game.set("` there and reports the function as
# undocumented-in-reverse — a warning that says the stubs describe something the
# engine does not have, when the engine has it and the check cannot see it. That
# is the worst kind of drift check: one that is wrong in the reassuring
# direction.
registered=$(
    tr -d '\n' < "$SOURCE" \
        | grep -oE 'game\.set\([[:space:]]*"[A-Za-z_]+"' \
        | sed 's/.*"\(.*\)"/\1/' \
        | sort -u
)

status=0
missing=""
documented_count=0

for name in $registered; do
    if grep -qE "^function game\.${name}\b" "$STUBS" \
        || grep -qE "^---@field ${name}\b" "$STUBS"; then
        documented_count=$((documented_count + 1))
    else
        missing="$missing $name"
    fi
done

if [ -n "$missing" ]; then
    status=1
    echo "ENGINE API MISSING FROM $STUBS:"
    for name in $missing; do
        echo "  game.$name"
    done
    echo
    echo "  A mod author's editor reads these stubs. Anything undocumented is"
    echo "  invisible to them. Add a ---@param-annotated function declaration,"
    echo "  or a ---@field entry if it is a constant."
    echo
fi

# The other direction is a warning, not an error: method stubs
# (ChunkBuffer:fill_all) and type declarations legitimately have no
# `game.set(...)` behind them.
for documented in $(grep -oE '^function game\.[A-Za-z_]+' "$STUBS" | sed 's/^function game\.//'); do
    if ! echo "$registered" | grep -qx "$documented"; then
        echo "WARNING: $STUBS documents game.$documented, which the engine does not register."
        echo "  (a removed function still in the stubs is worse than an undocumented one —"
        echo "   mod authors will write code against it)"
    fi
done

# The version the stubs describe is the version the engine reports. The two
# are one number in two files, which is the kind of thing that drifts.
stub_version=$(grep -oE '^-- API version: [0-9]+\.[0-9]+' "$STUBS" | head -1 | sed 's/.*: //')
engine_version=$(grep -oE 'pub const API_VERSION: &str = "[0-9]+\.[0-9]+"' crates/core/src/script/mod.rs \
    | sed 's/.*"\(.*\)"/\1/')
if [ -z "$stub_version" ] || [ -z "$engine_version" ]; then
    status=1
    echo "API VERSION NOT FOUND: stubs '$stub_version', engine '$engine_version'."
    echo "  $STUBS carries '-- API version: X.Y' in its header and"
    echo "  crates/core/src/script/mod.rs 'pub const API_VERSION: &str = \"X.Y\";'."
    echo
elif [ "$stub_version" != "$engine_version" ]; then
    status=1
    echo "API VERSION DISAGREES: $STUBS says $stub_version, the engine says $engine_version."
    echo "  A mod compares game.api_version against the header of the stubs it vendored."
    echo
fi

# A deprecation says since when, until when, and what instead — the whole
# rule, on the line the editor shows struck through.
malformed=$(grep -nE '^---@deprecated' "$STUBS" \
    | grep -vE '^[0-9]+:---@deprecated Since API [0-9]+\.[0-9]+, goes in [0-9]+\.[0-9]+: ' || true)
if [ -n "$malformed" ]; then
    status=1
    echo "DEPRECATION WITHOUT ITS TERMS in $STUBS:"
    echo "$malformed" | sed 's/^/  /'
    echo "  The form is: ---@deprecated Since API 1.x, goes in 1.y: use \`game.other\` instead."
    echo
fi

# The reference section of api/AGENTS.md is generated from the stubs; a
# function the stubs gained or lost since it was written fails here by name.
if ! python3 -I scripts/api-reference.py --check; then
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "API stubs: OK ($documented_count entries documented, API version $stub_version)"
fi

exit "$status"
