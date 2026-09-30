#!/usr/bin/env bash
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Lays out `Tiamat.app` beside `current/` in a macOS archive
# (docs/distribution.md §5). Sourced by scripts/package.sh.
#
# The bundle exists so that the running game IS the app the Dock shows: the
# launcher inside it `exec`s the client, and a Dock tile pinned from it
# reopens the launcher, which applies updates and starts the game from the
# right directory. It sits BESIDE `current/` and never around it, because an
# older install's client stages updates by finding `current/` at most one
# directory down.

# make_app_bundle <stage dir> <launcher binary> <icon.icns> <version>
make_app_bundle() {
    local stage="$1" launcher="$2" icon="$3" version="$4"
    local app="$stage/Tiamat.app/Contents"
    mkdir -p "$app/MacOS" "$app/Resources"
    # The only copy of the launcher on macOS: no top-level `tiamat`.
    cp "$launcher" "$app/MacOS/tiamat"
    chmod 755 "$app/MacOS/tiamat"
    cp "$icon" "$app/Resources/tiamat.icns"
    cat > "$app/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Tiamat</string>
    <key>CFBundleDisplayName</key>
    <string>Tiamat</string>
    <key>CFBundleIdentifier</key>
    <string>com.tiamatengine.tiamat</string>
    <key>CFBundleExecutable</key>
    <string>tiamat</string>
    <key>CFBundleIconFile</key>
    <string>tiamat</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST
}
