#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the Safari wrapper app for the Hydra web extension.
#
# Safari extensions must ship inside a macOS app, so this generates one from
# extensions/safari/Resources (assembled from the Chrome sources by
# sync-extension-resources.sh safari), installs our SFSafariWebExtensionHandler,
# allows loopback networking, and builds.
#
# The extension itself talks to Hydra over the SAME WebSocket the Chrome one
# uses (ws://127.0.0.1:6799) — the native handler is only the fallback that
# can launch the app when it is not running.
#
# REQUIREMENTS: full Xcode (not just Command Line Tools):
#   sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
#
# After building: open the app once, then Safari > Settings > Extensions >
# enable "Hydra". For unsigned dev builds also tick
# Develop > Allow Unsigned Extensions (re-arm after each Safari restart).

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$REPO/extensions/safari"
GEN="$OUT/generated"
APP_NAME="Hydra Safari Extension"

"$REPO/scripts/sync-extension-resources.sh" safari

if ! xcrun --find safari-web-extension-converter >/dev/null 2>&1; then
  cat >&2 <<EOF

error: safari-web-extension-converter not found — full Xcode is required.

  1. Install Xcode from the App Store
  2. sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
  3. sudo xcodebuild -license accept
  4. re-run this script

The web resources are already assembled in:
  $OUT/Resources
EOF
  exit 1
fi

echo "generating Xcode project..."
rm -rf "$GEN"
xcrun safari-web-extension-converter "$OUT/Resources" \
  --project-location "$GEN" \
  --app-name "$APP_NAME" \
  --bundle-identifier io.github.ja7ad.hydra.safari \
  --macos-only --copy-resources --no-open --no-prompt --force

PROJ_DIR="$GEN/$APP_NAME"

# Ensure parent app bundle identifier matches extension prefix (io.github.ja7ad.hydra.safari)
# so Xcode ValidateEmbeddedBinary passes.
find "$PROJ_DIR" -name "project.pbxproj" -exec sed -i '' \
  -e 's/io\.github\.ja7ad\.hydra\.Hydra-Safari-Extension/io.github.ja7ad.hydra.safari/g' \
  {} + 2>/dev/null || true

# Our handler instead of the generated echo stub.
HANDLER=$(find "$PROJ_DIR" -name "SafariWebExtensionHandler.swift" | head -1)
[ -n "$HANDLER" ] || { echo "handler not found in generated project" >&2; exit 1; }
cp "$OUT/SafariWebExtensionHandler.swift" "$HANDLER"
echo "installed handler: $HANDLER"

# The handler reads ~/.config/hydra/ipc.json and spawns `open`; both are
# denied inside the app sandbox. Local dev build: sandbox off.
find "$PROJ_DIR" -name "*.entitlements" -print0 | while IFS= read -r -d '' ent; do
  /usr/libexec/PlistBuddy -c "Set :com.apple.security.app-sandbox false" "$ent" 2>/dev/null ||
    /usr/libexec/PlistBuddy -c "Add :com.apple.security.app-sandbox bool false" "$ent"
  echo "sandbox disabled: $ent"
done

# ws://127.0.0.1 is cleartext: App Transport Security must allow local
# networking or the extension's socket never opens.
find "$PROJ_DIR" -name "Info.plist" -print0 | while IFS= read -r -d '' plist; do
  /usr/libexec/PlistBuddy -c "Add :NSAppTransportSecurity dict" "$plist" 2>/dev/null || true
  /usr/libexec/PlistBuddy -c "Add :NSAppTransportSecurity:NSAllowsLocalNetworking bool true" "$plist" 2>/dev/null ||
    /usr/libexec/PlistBuddy -c "Set :NSAppTransportSecurity:NSAllowsLocalNetworking true" "$plist"
  echo "local networking allowed: $plist"
done

# Safari WebExtensions require Safari 14+ (macOS 11.0 Big Sur or later).
# Ensure deployment target in generated project is at least 11.0.
find "$PROJ_DIR" -name "project.pbxproj" -exec sed -i '' -e 's/MACOSX_DEPLOYMENT_TARGET = 10\.[0-9]*/MACOSX_DEPLOYMENT_TARGET = 11.0/g' {} + 2>/dev/null || true

echo "building..."
xcodebuild -project "$PROJ_DIR/$APP_NAME.xcodeproj" \
  -scheme "$APP_NAME" \
  -configuration Release \
  MACOSX_DEPLOYMENT_TARGET=11.0 \
  CODE_SIGN_IDENTITY="-" CODE_SIGNING_REQUIRED=NO \
  build

APP=$(find "$HOME/Library/Developer/Xcode/DerivedData" -maxdepth 5 \
  -name "$APP_NAME.app" -path "*Release*" 2>/dev/null | head -1)
echo
echo "Built${APP:+: $APP}"
echo "Open the app once, then enable the extension in Safari > Settings >"
echo "Extensions. Unsigned build: also Develop > Allow Unsigned Extensions."
