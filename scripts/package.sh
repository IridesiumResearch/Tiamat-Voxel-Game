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

# shellcheck source=scripts/lib/bundle.sh
source scripts/lib/bundle.sh

version="$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)"
commit="$(git rev-parse HEAD 2>/dev/null || echo "")"
name="tiamat-${version}-${target}"
stage="${out}/${name}"

echo "==> building ${name} (channel ${channel})"

# **The stamp is an environment variable read at compile time**, which is what
# `core::build` reads back. A build with no commit says so rather than guessing
# one, so a developer's copy can never claim to be a release.
# `TIAMAT_RELEASE_KEY` and `TIAMAT_MANIFEST_URL` are inherited from the
# environment when the release workflow sets them. A build made without them
# trusts no key and asks nobody for updates, which is what a working copy
# should do — see `core::build` and the launcher's `RELEASE_KEY`.
export TIAMAT_CHANNEL="$channel"
[ -n "$commit" ] && export TIAMAT_COMMIT="$commit"

# The updater is built when it exists, so this script works before and after it
# lands rather than having to be rewritten the day it does.
packages=(-p client -p server)
if [ -d crates/updater ]; then
    packages+=(-p updater)
fi
cargo build --release --target "$target" "${packages[@]}"

suffix=""
case "$target" in *windows*) suffix=".exe" ;; esac
# Cargo builds wherever CARGO_TARGET_DIR says, when it is set.
built="${CARGO_TARGET_DIR:-target}/${target}/release"

rm -rf "$stage"
mkdir -p "$stage/current"

echo "==> assembling"
for binary in client server; do
    cp "${built}/${binary}${suffix}" "$stage/current/"
done
# The launcher's binary is called `tiamat` — it is what the player clicks.
if [ -f "${built}/tiamat${suffix}" ]; then
    # The launcher sits ABOVE `current/`, because it is the one thing an update
    # does not replace — see `docs/distribution.md` §1.
    cp "${built}/tiamat${suffix}" "${stage}/tiamat${suffix}"
fi

# --- mods ------------------------------------------------------------------
#
# Two kinds. The reference mods are tracked in this repository and ship as
# they are. The default mods come from repositories of their own and ship at
# the commit `bundle.toml` pins — never from a working tree, so a release
# records exactly what it carries and CI builds the same archive as a
# maintainer's machine (docs/distribution.md §6 and §7). Each bundled mod
# must carry its own LICENSE inside its directory; an archive quietly missing
# the notice for the mod that makes the game a game is worse than no archive.
mkdir -p "$stage/current/game"
cp game/README.md "$stage/current/game/"

echo "==> reference mods"
for manifest in $(git ls-files 'game/*/mod.toml'); do
    mod_dir="$(dirname "$manifest")"
    [ -L "$mod_dir" ] && continue
    cp -R "$mod_dir" "$stage/current/game/"
done

echo "==> bundled mods"
# `bundled=()` and `${#bundled[@]}` together are an "unbound variable" under
# `set -u` on the bash 3.2 a macOS runner may hand us, so the count is kept
# by hand and the expansion guarded.
bundled=()
bundled_count=0
if [ -f bundle.toml ]; then
    while IFS=$'\t' read -r id repo path modcommit; do
        if ! bundle_extract "$id" "$repo" "$modcommit" "$path" "$stage/current/game/$id"; then
            echo "error: could not take ${id} at ${modcommit:0:7} from a checkout beside this one or from ${repo}." >&2
            if [ "$allow_missing_mods" -eq 1 ]; then
                echo "       --allow-missing-mods given; continuing without it (singleplayer will be bare)." >&2
                rm -rf "$stage/current/game/$id"
                continue
            fi
            exit 1
        fi
        if [ ! -f "$stage/current/game/$id/LICENSE" ] && [ ! -f "$stage/current/game/$id/LICENSE.md" ]; then
            echo "error: ${id} at ${modcommit:0:7} carries no LICENSE inside ${path}." >&2
            echo "       Every bundled mod ships its licence in its own directory (docs/licensing/mod-repo-checklist.md)." >&2
            if [ "$allow_missing_mods" -eq 1 ]; then
                # A bare archive is honest; an archive with the mod and no
                # notice is not. So the mod goes, not the rule.
                echo "       --allow-missing-mods given; leaving it out (singleplayer will be bare)." >&2
                rm -rf "$stage/current/game/$id"
                continue
            fi
            exit 1
        fi
        if [ ! -f "$stage/current/game/$id/LICENSE.EXCEPTION" ]; then
            echo "warning: ${id} at ${modcommit:0:7} carries no LICENSE.EXCEPTION; its own permission is missing from the archive." >&2
        fi
        if [ -L "game/$id" ]; then
            head="$(bundle_git -C "$(readlink -f "game/$id")" rev-parse HEAD 2>/dev/null || true)"
            [ "$head" = "$modcommit" ] || echo "note: game/$id's working tree is at ${head:0:7}; the archive carries ${modcommit:0:7} from bundle.toml." >&2
        fi
        bundled+=("$id	$repo	$path	$modcommit")
        bundled_count=$((bundled_count + 1))
        echo "    $id at ${modcommit:0:7}"
    done < <(bundle_mods)
elif [ "$allow_missing_mods" -eq 1 ]; then
    echo "warning: no bundle.toml; packaging the reference mods only." >&2
else
    echo "error: no bundle.toml. Run scripts/bundle-lock.sh, or pass --allow-missing-mods for a bare build." >&2
    exit 1
fi

# --- licences (§7) ---------------------------------------------------------
cp LICENSE LICENSE.EXCEPTION "$stage/current/"

# Not a list of names: every crate's own licence and notice files, copied into
# the archive, and an index that says where each is. MIT wants its notice
# preserved in every copy; a name and an SPDX identifier preserve nothing.
echo "==> third-party notices"
python3 scripts/third-party-notices.py \
    --target "$target" \
    --out "$stage/current/licenses" \
    --index "$stage/current/THIRD-PARTY.md" \
    --version "$version"

# --- the release record (§7) ----------------------------------------------
#
# What this archive is built from, exactly: the engine's commit and every
# bundled mod's, and where the matching source is. A moving branch is not a
# reference for an older binary; this file and the source archive are.
echo "==> release record"
{
    echo "# Tiamat ${version} — release record"
    echo
    echo "Channel \`${channel}\`, target \`${target}\`, packaged $(date -u +%Y-%m-%dT%H:%M:%SZ)."
    echo
    echo "## Engine"
    echo
    if [ -n "$commit" ]; then
        echo "commit ${commit}"
        echo "https://github.com/IridesiumResearch/Tiamat-Voxel-Game/tree/${commit}"
    else
        echo "Built from a working copy with no commit; this is not a release."
    fi
    echo
    echo "## Bundled mods"
    echo
    if [ "$bundled_count" -gt 0 ]; then
        echo "| mod | repository | commit |"
        echo "|---|---|---|"
        for entry in ${bundled[@]+"${bundled[@]}"}; do
            IFS=$'\t' read -r id repo path modcommit <<< "$entry"
            tree="${repo%.git}/tree/${modcommit}/${path}"
            echo "| \`game/${id}\` | ${repo} | [\`${modcommit}\`](${tree}) |"
        done
    else
        echo "None: the reference mods only."
    fi
    echo
    echo "## Corresponding source"
    echo
    echo "The source these binaries were built from is the engine at the commit above"
    echo "and each bundled mod at the commit named beside it. It is published as one"
    echo "archive, \`tiamat-${version}-source.tar.gz\`, on the same release page this"
    echo "archive came from, for as long as the release is published, and it is the"
    echo "same source that the repositories hold at those commits. That archive's"
    echo "SOURCE.md says how to build."
    echo
    echo "## Licences"
    echo
    echo "- \`LICENSE\` — GPL-3.0-only, the engine and the reference mods under \`game/\`."
    echo "- \`LICENSE.EXCEPTION\` — the Additional Permission for mods, version 1.0."
    echo "- \`THIRD-PARTY.md\` and \`licenses/\` — every crate compiled in, with its own notice files."
    echo "- \`game/<mod>/LICENSE\` — each bundled mod's licence, and its own \`LICENSE.EXCEPTION\`."
    echo "- \`MANIFEST.txt\` — every file in this archive with its SHA-256."
} > "$stage/current/RELEASE.md"

# --- how to run it ---------------------------------------------------------
cat > "$stage/README.txt" <<EOF
Tiamat ${version} (${channel}${commit:+, ${commit:0:7}})

This is a test build. It is not signed, so your operating system will say so.

macOS
  The first time only, clear the quarantine flag:
      xattr -dr com.apple.quarantine .
  then run ./tiamat${suffix} (or double-click it).
  Without that, macOS refuses to open it and says it is damaged. It is not.

Windows
  SmartScreen will warn about an unrecognised app. More info -> Run anyway.

Linux
  ./tiamat${suffix}

What is in here
  tiamat${suffix}    the launcher: it applies updates and starts the game
  current/           the game itself
  current/game/      the mods it loads in singleplayer

Your worlds, settings and identity key are kept outside this folder, so
deleting it loses nothing but the program.

Source and licences
  current/RELEASE.md         exactly what this was built from, and where the source is
  current/LICENSE            GPL-3.0-only
  current/LICENSE.EXCEPTION  the permission that lets mods be licensed as their authors like
  current/THIRD-PARTY.md     every crate compiled in, with its notices under current/licenses/
  current/game/*/LICENSE     each bundled mod's own licence
EOF

# --- the file manifest (§7) -------------------------------------------------
# Last, because it hashes everything above it.
echo "==> file manifest"
python3 scripts/file-manifest.py "$stage" "$stage/current/MANIFEST.txt"

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
