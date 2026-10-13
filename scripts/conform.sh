#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# The conformance run a mod's own CI calls against the engine (milestone
# 0.5.0, A3): one mod, beside the default mods it may depend on, through what
# a release puts it through short of a player.
#
#   1. `server --check-mods game`: the whole registration window in a dry-run
#      VM, which fails on anything that would be disabled.
#   2. A real server on loopback with every mod in game/ enabled, the fixture
#      bot joining it, saying something, reading its inventory and leaving,
#      and the server stopped cleanly — with no mod disabled along the way.
#
# Exits 0 only if every step did. Usage, from the engine checkout:
#
#   scripts/conform.sh <the mod's directory, the one holding mod.toml>
#
# The mod is linked into game/<id> for the run, replacing a copy
# scripts/fetch-mods.sh put there, or a link to somewhere else (said so).
# Every other default mod comes from scripts/fetch-mods.sh at the commit
# bundle.toml pins, so the mod is tested beside what a release would ship.
# docs/mod-conformance.md has the workflow a mod repository runs this from.
#
#   CONFORM_SCRIPT  a bot script to run instead of the smoke join
#   CONFORM_KEEP    set to keep the world and the server log, and say where
set -euo pipefail
cd "$(dirname "$0")/.."
mod="${1:?usage: scripts/conform.sh <mod dir>}"
[ -d "$mod" ] || { echo "error: $mod is not a directory" >&2; exit 1; }
mod="$(cd "$mod" && pwd -P)"
[ -f "$mod/mod.toml" ] || { echo "error: $mod has no mod.toml" >&2; exit 1; }
id="$(sed -n 's/^id = "\([^"]*\)".*/\1/p' "$mod/mod.toml" | head -1)"
[ -n "$id" ] || { echo "error: $mod/mod.toml names no id" >&2; exit 1; }

echo "==> the default mods beside $id"
bash scripts/fetch-mods.sh
if [ -L "game/$id" ]; then
    if [ "$(readlink -f "game/$id")" != "$mod" ]; then
        echo "note: game/$id pointed at $(readlink "game/$id"); it now points at $mod"
    fi
    rm "game/$id"
elif [ -e "game/$id" ]; then
    rm -rf "game/$id"
fi
ln -s "$mod" "game/$id"
echo "game/$id -> $mod"

echo "==> building the server and the bot"
cargo build -q -p server -p bot
target="${CARGO_TARGET_DIR:-target}"
server_bin="$target/debug/server"
bot_bin="$target/debug/bot"

echo "==> check-mods"
"$server_bin" --check-mods game

# The server's log is coloured, with escape codes between a field's name and
# its value; this takes them out so a field can be read.
plain() { sed "s/$(printf '\033')\[[0-9;]*m//g"; }

echo "==> a server with every mod in game/, and the bot joining it"
work="$(mktemp -d)"
server_pid=""
cleanup() {
    if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
        kill -TERM "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    if [ -n "${CONFORM_KEEP:-}" ]; then
        echo "kept: $work"
    else
        rm -rf "$work"
    fi
}
trap cleanup EXIT
cat > "$work/server.toml" <<TOML
bind_addr = "127.0.0.1:0"
world_path = "$work/world"
mods_path = "$(pwd -P)/game"
max_players = 4
TOML
"$server_bin" --config "$work/server.toml" > "$work/server.log" 2>&1 &
server_pid=$!
addr=""
for _ in $(seq 1 120); do
    # `|| true`: under pipefail a grep with no match yet would end the script
    # here, silently, through the assignment.
    addr="$({ grep -a 'server listening' "$work/server.log" || true; } | plain \
        | sed -n 's/.*local_addr=\([0-9.]*:[0-9]*\).*/\1/p' | head -1)"
    [ -n "$addr" ] && break
    if ! kill -0 "$server_pid" 2>/dev/null; then
        echo "error: the server exited during startup:" >&2
        cat "$work/server.log" >&2
        exit 1
    fi
    sleep 1
done
if [ -z "$addr" ]; then
    echo "error: the server never reported listening:" >&2
    cat "$work/server.log" >&2
    exit 1
fi
script="${CONFORM_SCRIPT:-crates/bot/scripts/smoke_join.lua}"
echo "server at $addr; running $script"
if ! "$bot_bin" run "$script" --server "$addr" --name conform; then
    echo "error: the bot's run failed; the server said:" >&2
    tail -40 "$work/server.log" >&2
    exit 1
fi

kill -TERM "$server_pid"
wait "$server_pid" || { echo "error: the server did not exit 0 on SIGTERM" >&2; tail -40 "$work/server.log" >&2; exit 1; }
server_pid=""
grep -q "world flushed and closed" "$work/server.log" \
    || { echo "error: the server did not flush on shutdown:" >&2; tail -40 "$work/server.log" >&2; exit 1; }

# A disabled mod is a failure here, as it is for check-mods: the point of
# the run is to notice. Not a grep for "disabled" alone, which the healthy
# "disabled=0" summary line would match.
if grep -E 'mod failed to load and is disabled|mod disabled after' "$work/server.log"; then
    echo "error: a mod was disabled during the run (above)" >&2
    exit 1
fi
mods="$({ grep -a 'mods loaded and registries frozen' "$work/server.log" || true; } | plain \
    | sed -n 's/.*mods=\([0-9]*\).*/\1/p' | head -1)"
echo "==> conformance: OK — $id loaded, joined and left beside ${mods:-?} mods"
