#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Opens a built archive and checks that what has to be in it is in it —
# docs/distribution.md §7. Counsel's point exactly: a build configuration that
# recognises compatible licence identifiers proves nothing about the archive a
# tester downloads; only looking inside does. The release workflow runs this on
# every archive it produces, and so should anybody who packages by hand.
#
# Usage: scripts/check-archive.sh <archive.tar.gz|archive.zip> [--bundle bundle.toml]

set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib/bundle.sh
source scripts/lib/bundle.sh

archive=""
lock="bundle.toml"
while [ $# -gt 0 ]; do
    case "$1" in
        --bundle) lock="${2:?--bundle needs a path}"; shift 2 ;;
        *) archive="$1"; shift ;;
    esac
done
[ -n "$archive" ] && [ -f "$archive" ] || { echo "usage: scripts/check-archive.sh <archive>" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
case "$archive" in
    *.zip)
        if command -v unzip >/dev/null 2>&1; then unzip -q "$archive" -d "$work"
        else tar -xf "$archive" -C "$work"; fi ;;
    *) tar -xzf "$archive" -C "$work" ;;
esac
root="$(find "$work" -mindepth 1 -maxdepth 1 -type d | head -1)"
[ -n "$root" ] || { echo "FAIL: the archive has no top-level directory" >&2; exit 1; }

failures=0
fail() { echo "FAIL $1"; failures=$((failures + 1)); }
ok() { echo "ok   $1"; }
need() { if [ -e "$root/$1" ]; then ok "$1"; else fail "$1 is missing"; fi; }

suffix=""
[ -e "$root/tiamat.exe" ] && suffix=".exe"
need "tiamat${suffix}"
need "current/client${suffix}"
need "current/server${suffix}"
need "current/LICENSE"
need "current/LICENSE.EXCEPTION"
need "current/THIRD-PARTY.md"
need "current/RELEASE.md"
need "current/MANIFEST.txt"
need "current/licenses/go-font/LICENSE"
need "README.txt"

# The licence bundle: every crate the index lists has its directory, with
# files in it, and every directory is listed.
if [ -f "$root/current/THIRD-PARTY.md" ]; then
    # Real entries only: the index's own explanation names `licenses/<crate>-<version>/`.
    listed="$(grep -o '`licenses/[^/`<]*/`' "$root/current/THIRD-PARTY.md" | tr -d '`' | sed 's|licenses/||; s|/$||' | sort -u)"
    present="$(ls "$root/current/licenses" 2>/dev/null | grep -v '^go-font$' | sort -u)"
    missing_dirs="$(comm -23 <(printf '%s\n' "$listed") <(printf '%s\n' "$present"))"
    unlisted="$(comm -13 <(printf '%s\n' "$listed") <(printf '%s\n' "$present"))"
    [ -z "$missing_dirs" ] || fail "crates listed without a licences directory: $(echo "$missing_dirs" | tr '\n' ' ')"
    [ -z "$unlisted" ] || fail "licence directories nobody lists: $(echo "$unlisted" | tr '\n' ' ')"
    empty="$(find "$root/current/licenses" -mindepth 1 -maxdepth 1 -type d -empty)"
    [ -z "$empty" ] || fail "empty licence directories: $(echo "$empty" | tr '\n' ' ')"
    count="$(printf '%s\n' "$listed" | grep -c . || true)"
    [ "$count" -gt 100 ] && ok "THIRD-PARTY.md lists $count crates and each has its notice files" || fail "THIRD-PARTY.md lists only $count crates"
fi

# Every bundled mod, at the commit the lock pins, with its own licence.
if [ -f "$lock" ]; then
    while IFS=$'\t' read -r id repo path commit; do
        if [ ! -f "$root/current/game/$id/mod.toml" ]; then
            fail "bundled mod $id is not in the archive"
            continue
        fi
        [ -f "$root/current/game/$id/LICENSE" ] && ok "game/$id carries its LICENSE" || fail "game/$id has no LICENSE"
        [ -f "$root/current/game/$id/LICENSE.EXCEPTION" ] || echo "warn game/$id has no LICENSE.EXCEPTION (the mod's own permission; see docs/licensing/mod-repo-checklist.md)"
        grep -q "$commit" "$root/current/RELEASE.md" 2>/dev/null && ok "RELEASE.md records $id at ${commit:0:7}" || fail "RELEASE.md does not record $id at $commit"
    done < <(bundle_mods "$lock")
fi

# The engine's own commit is in the record.
if [ -f "$root/current/RELEASE.md" ]; then
    grep -qE 'commit [0-9a-f]{40}' "$root/current/RELEASE.md" && ok "RELEASE.md names the engine commit" || fail "RELEASE.md names no engine commit"
fi

# Nothing that should never ship.
gitdirs="$(find "$root" -name .git | head -3)"
[ -z "$gitdirs" ] || fail "a .git directory is inside the archive: $gitdirs"
links="$(find "$root" -type l | head -3)"
[ -z "$links" ] || fail "symlinks inside the archive: $links"

# Every file is what the manifest says it is.
if [ -f "$root/current/MANIFEST.txt" ]; then
    if python3 scripts/file-manifest.py --check "$root" "$root/current/MANIFEST.txt" >/dev/null; then
        ok "MANIFEST.txt matches every file"
    else
        fail "MANIFEST.txt does not match the files (run scripts/file-manifest.py --check for the list)"
    fi
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures problem(s) in $archive"
    exit 1
fi
echo "$archive: everything a release has to carry is in it"
