#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the macOS installer package (.pkg) — the one-click alternative to the
# drag-and-drop DMG.
#
#   scripts/package-macos-pkg.sh
#
# What the package installs:
#   /Applications/Hydra Download Manager.app   GUI + hydra CLI + hydra-host
#       (the ad-hoc-signed bundle from scripts/macos-app-bundle.sh)
#   /Library/Application Support/Hydra/extensions
#       hydra-chrome-<v>.zip and hydra-firefox-<v>.xpi (packed, plus a
#       signed .crx when the build machine has the key), chrome/,
#       firefox/ and safari/ (unpacked, ready for "Load unpacked" / a
#       temporary add-on install), and INSTALL.txt with the instructions
#   /usr/local/share/man/man1/hydra*.1
#       CLI man pages, on the default man path ("man hydra" just works)
#   postinstall:
#       symlinks /usr/local/bin/{hydra,hya} -> the CLI inside the bundle, and
#       writes the per-user native-messaging manifests for the console user
#       (extension IDs baked in at build time, no python needed on the target)
#
# Output: target/dist/Hydra-<version>-<arch>.pkg

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
[ "$(uname -s)" = Darwin ] || { echo "error: the pkg must be built on macOS" >&2; exit 1; }

scripts/macos-app-bundle.sh

# The workspace product version ([workspace.package]), shared by the
# hydra-gui, hydra-cli and hydra-host bin crates the .pkg carries.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# With CARGO_BUILD_TARGET set (CI cross-arch builds), the bundle script puts
# the .app under target/<triple>/release, and the arch in the package name
# must come from the triple, not from the build host.
BIN="target/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release"
case "${CARGO_BUILD_TARGET:-}" in
  x86_64-*)  ARCH=x86_64 ;;
  aarch64-*) ARCH=arm64 ;;
  *)         ARCH=$(uname -m) ;;
esac
APP="$BIN/Hydra Download Manager.app"
IDENTIFIER="io.github.ja7ad.hydra"

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

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
STAGE="$TMP/root"
SCRIPTS="$TMP/scripts"
mkdir -p "$STAGE/Applications" "$STAGE/Library/Application Support/Hydra/extensions" "$SCRIPTS"

ditto "$APP" "$STAGE/Applications/Hydra Download Manager.app"
# Packed .xpi/.zip, unpacked chrome/ and firefox/, and INSTALL.txt written
# with the installed paths. Same builder the app bundle and every other
# package use, so all of them ship byte-identical extensions.
EXT_INSTALLED="/Library/Application Support/Hydra/extensions"
scripts/build-extensions.sh --out "$STAGE$EXT_INSTALLED" --quiet --prefix "$EXT_INSTALLED"
# Safari is unpacked-only: it is installed by the wrapper app that
# scripts/build-safari-extension.sh builds, not by loading a directory.
cp -R extensions/safari "$STAGE$EXT_INSTALLED/"

# CLI man pages onto the default man path.
mkdir -p "$STAGE/usr/local/share/man/man1"
cp docs/man/*.1 "$STAGE/usr/local/share/man/man1/"
chmod 644 "$STAGE/usr/local/share/man/man1/"*.1

# postinstall runs as root: it can write /usr/local/bin, but the browser
# manifests belong in the console user's home (root has no browser profiles).
cat > "$SCRIPTS/postinstall" <<'EOF'
#!/bin/bash
set -u
APP="/Applications/Hydra Download Manager.app"
HOST_BIN="$APP/Contents/MacOS/hydra-host"

mkdir -p /usr/local/bin
ln -sf "$APP/Contents/MacOS/hydra" /usr/local/bin/hydra
# The short second name for the CLI: `hydra` is also THC-Hydra, which
# Homebrew ships, and three letters types better for a command run as often
# as a download. Relative, so it resolves through whatever `hydra` is —
# including after the app has updated itself.
ln -sf hydra /usr/local/bin/hya

CONSOLE_USER=$(stat -f%Su /dev/console 2>/dev/null || true)
case "$CONSOLE_USER" in ""|root|loginwindow|_mbsetupuser) exit 0 ;; esac
USER_HOME=$(dscl . -read "/Users/$CONSOLE_USER" NFSHomeDirectory 2>/dev/null | awk '{print $2}')
[ -d "${USER_HOME:-}" ] || exit 0
AS="$USER_HOME/Library/Application Support"

write_manifest() { # DIR ALLOWED_LINE
  mkdir -p "$1" || return 0
  cat > "$1/com.hydra.host.json" <<MANIFEST
{
  "name": "com.hydra.host",
  "description": "Hydra Download Manager native host",
  "path": "$HOST_BIN",
  "type": "stdio",
  $2
}
MANIFEST
  chown -R "$CONSOLE_USER" "$1"
}

for root in "Google/Chrome" "Chromium" "Microsoft Edge" \
            "BraveSoftware/Brave-Browser" "Vivaldi" "Arc/User Data"; do
  [ -d "$AS/$root" ] || continue
  write_manifest "$AS/$root/NativeMessagingHosts" \
    "\"allowed_origins\": [\"chrome-extension://@EXT_ID@/\"]"
done
for app in Firefox LibreWolf; do
  [ -d "/Applications/$app.app" ] || continue
  write_manifest "$AS/${app/Firefox/Mozilla}/NativeMessagingHosts" \
    "\"allowed_extensions\": [\"@FF_ID@\"]"
done
exit 0
EOF
sed -i '' -e "s/@EXT_ID@/$EXT_ID/" -e "s/@FF_ID@/$FF_ID/" "$SCRIPTS/postinstall"
chmod +x "$SCRIPTS/postinstall"

# Pin the app in /Applications: without this pkgbuild "relocates" the payload
# onto any copy of the bundle Spotlight finds elsewhere on the disk.
PLIST="$TMP/component.plist"
pkgbuild --analyze --root "$STAGE" "$PLIST" >/dev/null
plutil -replace 0.BundleIsRelocatable -bool NO "$PLIST"

mkdir -p target/dist
PKG="target/dist/Hydra-${VERSION}-${ARCH}.pkg"
rm -f "$PKG"
pkgbuild --root "$STAGE" \
  --component-plist "$PLIST" \
  --scripts "$SCRIPTS" \
  --identifier "$IDENTIFIER" \
  --version "${VERSION%%-*}" \
  --install-location / \
  "$PKG" >/dev/null
echo "Built: $PKG"
echo "(unsigned: end users right-click > Open, or: sudo installer -pkg ... -target /)"
