#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the macOS disk image.
#
#   scripts/package-macos-dmg.sh
#
# Contents of the image:
#   Hydra Download Manager.app        the ad-hoc-signed bundle from
#       scripts/macos-app-bundle.sh — GUI, plus the hydra CLI and the
#       hydra-host native-messaging bridge inside Contents/MacOS
#   Applications                      drop-target symlink
#   Install Browser Integration.command
#       writes the per-user native-messaging manifests pointing at the
#       /Applications copy of hydra-host (macOS has no system-wide manifest
#       dir the way Linux does). Extension IDs are baked in at build time so
#       the end user needs no python or repo checkout.
#
# Output: target/dist/Hydra-<version>-<arch>.dmg

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
[ "$(uname -s)" = Darwin ] || { echo "error: the DMG must be built on macOS" >&2; exit 1; }

scripts/macos-app-bundle.sh

# The workspace product version ([workspace.package]), shared by the
# hydra-gui, hydra-cli and hydra-host bin crates the .app bundle carries.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# With CARGO_BUILD_TARGET set (CI cross-arch builds), the bundle script puts
# the .app under target/<triple>/release, and the arch in the image name must
# come from the triple, not from the build host.
BIN="target/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release"
case "${CARGO_BUILD_TARGET:-}" in
  x86_64-*)  ARCH=x86_64 ;;
  aarch64-*) ARCH=arm64 ;;
  *)         ARCH=$(uname -m) ;;
esac
APP="$BIN/Hydra Download Manager.app"

EXT_ID=$(python3 - extensions/chrome/manifest.json <<'EOF'
import base64, hashlib, json, sys
key = json.load(open(sys.argv[1]))["key"]
digest = hashlib.sha256(base64.b64decode(key)).hexdigest()[:32]
print("".join(chr(ord('a') + int(c, 16)) for c in digest))
EOF
)
FF_ID=$(python3 - extensions/firefox/manifest.json <<'EOF'
import json, sys
print(json.load(open(sys.argv[1]))["browser_specific_settings"]["gecko"]["id"])
EOF
)

STAGE=$(mktemp -d)/dmg
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/Hydra Download Manager.app"
ln -s /Applications "$STAGE/Applications"

# Standalone per-user manifest installer (IDs expanded now, at build time).
cat > "$STAGE/Install Browser Integration.command" <<EOF
#!/bin/bash
# Writes the native-messaging manifests so the Hydra browser extension can
# talk to the app. Run after dragging the app into /Applications.
set -euo pipefail
HOST_BIN="/Applications/Hydra Download Manager.app/Contents/MacOS/hydra-host"
[ -x "\$HOST_BIN" ] || { echo "Install the app into /Applications first."; exit 1; }
AS="\$HOME/Library/Application Support"
installed=0
for root in "Google/Chrome" "Chromium" "Microsoft Edge" \\
            "BraveSoftware/Brave-Browser" "Vivaldi" "Arc/User Data"; do
  [ -d "\$AS/\$root" ] || continue
  mkdir -p "\$AS/\$root/NativeMessagingHosts"
  cat > "\$AS/\$root/NativeMessagingHosts/com.hydra.host.json" <<MANIFEST
{
  "name": "com.hydra.host",
  "description": "Hydra Download Manager native host",
  "path": "\$HOST_BIN",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXT_ID/"]
}
MANIFEST
  echo "installed: \$root"
  installed=1
done
for app in Firefox LibreWolf; do
  [ -d "/Applications/\$app.app" ] || continue
  dir="\$AS/\${app/Firefox/Mozilla}/NativeMessagingHosts"
  mkdir -p "\$dir"
  cat > "\$dir/com.hydra.host.json" <<MANIFEST
{
  "name": "com.hydra.host",
  "description": "Hydra Download Manager native host",
  "path": "\$HOST_BIN",
  "type": "stdio",
  "allowed_extensions": ["$FF_ID"]
}
MANIFEST
  echo "installed: \$app"
  installed=1
done
[ "\$installed" = 1 ] || echo "no supported browser found; nothing installed"
echo "Done. Restart the browser, then load the Hydra extension."
EOF
chmod +x "$STAGE/Install Browser Integration.command"

mkdir -p target/dist
DMG="target/dist/Hydra-${VERSION}-${ARCH}.dmg"
rm -f "$DMG"
hdiutil create -volname "Hydra Download Manager" -srcfolder "$STAGE" \
  -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
echo "Built: $DMG"
echo "CLI ships inside the bundle; to put it on PATH after installing:"
echo '  ln -s "/Applications/Hydra Download Manager.app/Contents/MacOS/hydra" /usr/local/bin/hydra'
echo '  ln -s hydra /usr/local/bin/hya   # the short name, if you want it'
