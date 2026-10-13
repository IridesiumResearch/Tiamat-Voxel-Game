#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Puts every mod bundle.toml pins into game/, each at its pinned commit, for a
# checkout with no mod repositories beside it: the nightly soak, or a fresh
# clone that wants to run the default mods. A game/<id> that is a symlink is
# left alone — that is a developer's working tree, and the point of the link
# is that it moves. A game/<id> that is a directory is replaced: it can only
# be an earlier run of this script, since the mods do not live in this repo.
#
# Fetching takes one shallow fetch of exactly the pinned commit per mod, into
# target/bundle/, by the same helper the release packaging uses, so what the
# soak flies over is what a release would ship.
#
# Usage: scripts/fetch-mods.sh
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib/bundle.sh
source scripts/lib/bundle.sh

[ -f bundle.toml ] || { echo "error: no bundle.toml; run scripts/bundle-lock.sh" >&2; exit 1; }
while IFS=$'\t' read -r id repo path commit; do
    if [ -L "game/$id" ]; then
        echo "game/$id is a link to a checkout; left alone"
        continue
    fi
    if ! bundle_extract "$id" "$repo" "$commit" "$path" "game/$id"; then
        echo "error: could not take $id at ${commit:0:7} from $repo" >&2
        exit 1
    fi
    echo "game/$id at ${commit:0:7}"
done < <(bundle_mods)
