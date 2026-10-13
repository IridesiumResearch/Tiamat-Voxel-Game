#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Signs and notarises a staged macOS release in place, between
# `package.sh --stage-only` and `package.sh --archive-from`.
#
# What a Mac checks when a player opens a download: that every executable is
# signed by a Developer ID with the hardened runtime, and that Apple has seen
# the signed bytes (notarisation). Without both, Gatekeeper says the app is
# damaged and the README has to tell the player to clear a quarantine flag.
# The three executables are signed one by one — the launcher inside the
# bundle, then the bundle, and the client and server beside it, which the
# launcher starts and which carry the quarantine flag of the download too —
# and all three go to Apple in one zip. The ticket is stapled to the app,
# which is the thing a player opens; the client and server are checked
# online by their hashes, which notarisation also records.
#
# Needs, in the environment:
#   APPLE_SIGNING_IDENTITY  "Developer ID Application: Name (TEAMID)", already
#                           in a keychain the runner can use
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD
#                           an Apple ID on the team and an app-specific
#                           password, for notarytool
#
# Usage: scripts/sign-macos.sh <staged dir>
set -euo pipefail
stage="${1:?usage: scripts/sign-macos.sh <staged dir>}"
: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is not set}"
: "${APPLE_ID:?APPLE_ID is not set}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is not set}"
: "${APPLE_APP_PASSWORD:?APPLE_APP_PASSWORD is not set}"

app="$stage/Tiamat.app"
[ -d "$app" ] || { echo "error: no Tiamat.app under $stage" >&2; exit 1; }

# The hardened runtime is what notarisation requires; the timestamp is what
# keeps the signature valid after the certificate expires.
sign() {
    codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$1"
    codesign --verify --strict --verbose=2 "$1"
}

echo "==> signing"
sign "$stage/current/client"
sign "$stage/current/server"
sign "$app/Contents/MacOS/tiamat"
sign "$app"
codesign --verify --deep --strict --verbose=2 "$app"

echo "==> notarising"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# One zip of everything executable, with the paths kept, so one submission
# covers the bundle and the two binaries it starts.
(cd "$stage" && ditto -c -k --keepParent Tiamat.app "$work/Tiamat.app.zip")
(cd "$stage/current" && ditto -c -k client "$work/client.zip" && ditto -c -k server "$work/server.zip")
for archive in "$work/Tiamat.app.zip" "$work/client.zip" "$work/server.zip"; do
    xcrun notarytool submit "$archive" \
        --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" \
        --wait
done
xcrun stapler staple "$app"
xcrun stapler validate "$app"
# Gatekeeper's own verdict on the bundle, as a player's Mac would reach it.
spctl --assess --type execute --verbose=2 "$app"

touch "$stage/.signed"
echo "==> signed and notarised: $stage"
