#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Builds one target and lays out an archive a tester can download.
#
# `docs/distribution.md` §5 defines the layout and §7 the licence files that
# have to be in it; this is the tool that produces them. It does NOT hash or
# sign anything — that is `relman`'s job, so that the thing which decides what
# is authentic is one program with tests rather than a shell script.
#
# Usage:
#   scripts/package.sh --target x86_64-unknown-linux-gnu [--channel test]
#                      [--out dist] [--allow-missing-mods]

set -euo pipefail
cd "$(dirname "$0")/.."

target=""
channel="dev"
out="dist"
allow_missing_mods=0

while [ $# -gt 0 ]; do
    case "$1" in
        --target) target="${2:?--target needs a triple}"; shift 2 ;;
        --channel) channel="${2:?--channel needs a name}"; shift 2 ;;
        --out) out="${2:?--out needs a directory}"; shift 2 ;;
        --allow-missing-mods) allow_missing_mods=1; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

if [ -z "$target" ]; then
    echo "usage: scripts/package.sh --target <triple> [--channel <name>]" >&2
    exit 2
fi

version="$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)"
commit="$(git rev-parse HEAD 2>/dev/null || echo "")"
name="tiamot-${version}-${target}"
stage="${out}/${name}"

echo "==> building ${name} (channel ${channel})"

# **The stamp is an environment variable read at compile time**, which is what
# `core::build` reads back. A build with no commit says so rather than guessing
# one, so a developer's copy can never claim to be a release.
export TIAMOT_CHANNEL="$channel"
[ -n "$commit" ] && export TIAMOT_COMMIT="$commit"

# The updater is built when it exists, so this script works before and after it
# lands rather than having to be rewritten the day it does.
packages=(-p client -p server)
if [ -d crates/updater ]; then
    packages+=(-p updater)
fi
cargo build --release --target "$target" "${packages[@]}"

suffix=""
case "$target" in *windows*) suffix=".exe" ;; esac
built="target/${target}/release"

rm -rf "$stage"
mkdir -p "$stage/current"

echo "==> assembling"
for binary in client server; do
    cp "${built}/${binary}${suffix}" "$stage/current/"
done
if [ -f "${built}/updater${suffix}" ]; then
    # The launcher sits ABOVE `current/`, because it is the one thing an update
    # does not replace — see `docs/distribution.md` §1.
    cp "${built}/updater${suffix}" "${stage}/tiamot${suffix}"
fi

# --- mods ------------------------------------------------------------------
#
# A mod inside this repository carries the repository's licence. A mod from
# outside it is a symlink, and must carry its own — §6. A symlink that cannot
# be read at all stops the build: an archive quietly missing the mod that makes
# the game a game is worse than no archive.
mkdir -p "$stage/current/game"
cp game/README.md "$stage/current/game/"
missing=()
# `game/*` rather than `game/*/`: a trailing slash makes the glob skip a
# symlink whose target is not there, which is precisely the case this loop
# exists to catch. It silently shipped an archive with no content mods in it
# once, which is how this comment came to be written.
for entry in game/*; do
    mod_name="$(basename "$entry")"
    [ "$mod_name" = "README.md" ] && continue
    if [ -L "$entry" ]; then
        if [ ! -r "$entry/mod.toml" ]; then
            missing+=("$mod_name")
            continue
        fi
        if [ ! -f "$entry/LICENSE" ] && [ ! -f "$entry/LICENSE.md" ]; then
            echo "error: ${mod_name} comes from outside this repository and carries no LICENSE." >&2
            echo "       Add one to that mod, or drop the symlink before packaging." >&2
            exit 1
        fi
    elif [ ! -d "$entry" ]; then
        continue
    fi
    # `-L` follows the symlink and copies what it points at, so the archive
    # holds files rather than links into somebody's home directory.
    cp -RL "$entry" "$stage/current/game/"
    rm -rf "$stage/current/game/${mod_name}/.git"
done

if [ ${#missing[@]} -gt 0 ]; then
    echo "error: these mods are symlinks this machine cannot read: ${missing[*]}" >&2
    echo "       Mount the drive they live on, or pass --allow-missing-mods to" >&2
    echo "       build an archive without them (singleplayer will be bare)." >&2
    if [ "$allow_missing_mods" -eq 1 ]; then
        echo "       --allow-missing-mods given; continuing without them." >&2
    else
        exit 1
    fi
fi

# --- licences (§7) ---------------------------------------------------------
cp LICENSE LICENSE.EXCEPTION "$stage/current/"

echo "==> third-party notices"
{
    echo "# Third-party licences"
    echo
    echo "Tiamot ${version} is GPL-3.0-only; see LICENSE. It is built with the"
    echo "crates below, under the licences named. Where a crate offers a choice,"
    echo "every option it offers is listed and we take one compatible with"
    echo "GPL-3.0-only."
    echo
    echo "Source for any MPL-2.0 crate is available from its own repository, and"
    echo "the corresponding source for Tiamot itself is at"
    echo "https://github.com/IridesiumResearch/Tiamot-Voxel-Game"
    [ -n "$commit" ] && echo "at commit ${commit}."
    echo
    echo "The client embeds the Go Mono font; see"
    echo "crates/client/assets/third-party/go-font for its licence."
    echo
    cargo deny list -f tsv 2>/dev/null | awk -F'\t' '
        NR == 1 { for (i = 2; i <= NF; i++) heading[i] = $i; next }
        {
            licences = ""
            for (i = 2; i <= NF; i++) {
                if ($i == "X") {
                    licences = (licences == "" ? heading[i] : licences " OR " heading[i])
                }
            }
            if (licences != "") printf "- %s: %s\n", $1, licences
        }' | sort
} > "$stage/current/THIRD-PARTY.md"

# --- how to run it ---------------------------------------------------------
cat > "$stage/README.txt" <<EOF
Tiamot ${version} (${channel}${commit:+, ${commit:0:7}})

This is a test build. It is not signed, so your operating system will say so.

macOS
  The first time only, clear the quarantine flag:
      xattr -dr com.apple.quarantine .
  then run ./tiamot${suffix} (or double-click it).
  Without that, macOS refuses to open it and says it is damaged. It is not.

Windows
  SmartScreen will warn about an unrecognised app. More info -> Run anyway.

Linux
  ./tiamot${suffix}

What is in here
  tiamot${suffix}    the launcher: it applies updates and starts the game
  current/           the game itself
  current/game/      the mods it loads in singleplayer

Your worlds, settings and identity key are kept outside this folder, so
deleting it loses nothing but the program.

Source: https://github.com/IridesiumResearch/Tiamot-Voxel-Game
Licence: GPL-3.0-only (LICENSE), with a mod exception (LICENSE.EXCEPTION).
EOF

# --- archive ---------------------------------------------------------------
echo "==> archiving"
case "$target" in
    *windows*)
        archive="${out}/${name}.zip"
        rm -f "$archive"
        # 7z on a Windows runner, bsdtar's zip writer anywhere else that has
        # one. Both produce an archive Explorer opens without a third-party
        # tool, which a .tar.gz does not.
        if command -v 7z >/dev/null 2>&1; then
            (cd "$out" && 7z a -tzip -bso0 "${name}.zip" "${name}") >/dev/null
        elif tar --help 2>&1 | grep -q -- "-a,"; then
            (cd "$out" && tar -a -cf "${name}.zip" "${name}")
        else
            echo "error: no zip writer found (tried 7z and bsdtar)" >&2
            exit 1
        fi
        ;;
    *)
        archive="${out}/${name}.tar.gz"
        rm -f "$archive"
        tar -czf "$archive" -C "$out" "$name"
        ;;
esac

echo "==> ${archive}"
ls -l "$archive" | awk '{print "    " $5 " bytes"}'
