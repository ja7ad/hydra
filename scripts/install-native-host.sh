#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Install the Hydra native-messaging host for Chromium-family browsers.
#
# Builds crates/hydra-host (unless --no-build), derives the extension ID from
# the "key" pinned in extensions/chrome/manifest.json, and writes the
# NativeMessagingHosts manifest for every Chromium-family browser found.
#
# Usage: scripts/install-native-host.sh [--no-build] [--host-bin PATH]

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
HOST_NAME="com.hydra.host"
BUILD=1
HOST_BIN=""

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) BUILD=0 ;;
    --host-bin) HOST_BIN="$2"; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ -z "$HOST_BIN" ]; then
  if [ "$BUILD" = 1 ]; then
    # Both binaries: hydra-host launches its sibling hydra-gui when the app
    # is not running, so a stale GUI build here means captures reach a
    # binary without the extension bridge.
    echo "building hydra-host + hydra-gui (release)..."
    (cd "$REPO" && cargo build --release -p hya-host -p hya-gui)
  fi
  HOST_BIN="$REPO/target/release/hydra-host"
fi
[ -x "$HOST_BIN" ] || { echo "host binary not found: $HOST_BIN" >&2; exit 1; }

# Extension ID = first 16 bytes of SHA-256 of the DER public key, hex mapped
# to a-p. Deriving it here means regenerating the manifest key never leaves
# the two sides disagreeing.
EXT_ID=$(python3 - "$REPO/extensions/chrome/manifest.json" <<'EOF'
import base64, hashlib, json, sys
key = json.load(open(sys.argv[1]))["key"]
digest = hashlib.sha256(base64.b64decode(key)).hexdigest()[:32]
print("".join(chr(ord('a') + int(c, 16)) for c in digest))
EOF
)
echo "extension id: $EXT_ID"

case "$(uname -s)" in
  Darwin)
    DIRS=(
      "$HOME/Library/Application Support/Google/Chrome/NativeMessagingHosts"
      "$HOME/Library/Application Support/Chromium/NativeMessagingHosts"
      "$HOME/Library/Application Support/Microsoft Edge/NativeMessagingHosts"
      "$HOME/Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts"
      "$HOME/Library/Application Support/Vivaldi/NativeMessagingHosts"
      "$HOME/Library/Application Support/Arc/User Data/NativeMessagingHosts"
    )
    BROWSER_ROOTS=(
      "$HOME/Library/Application Support/Google/Chrome"
      "$HOME/Library/Application Support/Chromium"
      "$HOME/Library/Application Support/Microsoft Edge"
      "$HOME/Library/Application Support/BraveSoftware/Brave-Browser"
      "$HOME/Library/Application Support/Vivaldi"
      "$HOME/Library/Application Support/Arc/User Data"
    )
    # Firefox keeps its own registry and uses a different manifest dialect.
    FF_DIRS=(
      "$HOME/Library/Application Support/Mozilla/NativeMessagingHosts"
      "$HOME/Library/Application Support/LibreWolf/NativeMessagingHosts"
    )
    # Detect by the app bundle, not the profile: a freshly installed Firefox
    # has no profile directory until its first launch, and the manifest must
    # already be in place when the add-on first calls the host.
    FF_ROOTS=(
      "/Applications/Firefox.app"
      "/Applications/LibreWolf.app"
    )
    ;;
  Linux)
    # Snap and Flatpak give each browser its own private tree rather than
    # using ~/.config or ~/.mozilla. Ubuntu has shipped Firefox as a SNAP by
    # default since 22.04, so on a stock Ubuntu the classic paths match
    # nothing at all — which is what "no supported browser profile found"
    # used to mean, unhelpfully.
    DIRS=(
      "$HOME/.config/google-chrome/NativeMessagingHosts"
      "$HOME/.config/chromium/NativeMessagingHosts"
      "$HOME/.config/microsoft-edge/NativeMessagingHosts"
      "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
      "$HOME/.config/vivaldi/NativeMessagingHosts"
      "$HOME/snap/chromium/common/chromium/NativeMessagingHosts"
      "$HOME/.var/app/com.google.Chrome/config/google-chrome/NativeMessagingHosts"
      "$HOME/.var/app/org.chromium.Chromium/config/chromium/NativeMessagingHosts"
      "$HOME/.var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
      "$HOME/.var/app/com.microsoft.Edge/config/microsoft-edge/NativeMessagingHosts"
    )
    BROWSER_ROOTS=(
      "$HOME/.config/google-chrome"
      "$HOME/.config/chromium"
      "$HOME/.config/microsoft-edge"
      "$HOME/.config/BraveSoftware/Brave-Browser"
      "$HOME/.config/vivaldi"
      "$HOME/snap/chromium/common/chromium"
      "$HOME/.var/app/com.google.Chrome/config/google-chrome"
      "$HOME/.var/app/org.chromium.Chromium/config/chromium"
      "$HOME/.var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser"
      "$HOME/.var/app/com.microsoft.Edge/config/microsoft-edge"
    )
    FF_DIRS=(
      "$HOME/.mozilla/native-messaging-hosts"
      "$HOME/.librewolf/native-messaging-hosts"
      "$HOME/snap/firefox/common/.mozilla/native-messaging-hosts"
      "$HOME/.var/app/org.mozilla.firefox/.mozilla/native-messaging-hosts"
      "$HOME/.var/app/io.gitlab.librewolf-community/.librewolf/native-messaging-hosts"
    )
    FF_ROOTS=(
      "$HOME/.mozilla"
      "$HOME/.librewolf"
      "$HOME/snap/firefox/common/.mozilla"
      "$HOME/.var/app/org.mozilla.firefox/.mozilla"
      "$HOME/.var/app/io.gitlab.librewolf-community/.librewolf"
    )
    ;;
  *)
    echo "On Windows, run the PowerShell installer instead:" >&2
    echo "  powershell -ExecutionPolicy Bypass -File scripts\\install-native-host.ps1" >&2
    exit 1
    ;;
esac

INSTALLED=0
for i in "${!DIRS[@]}"; do
  # Only browsers that actually exist on this machine get a manifest.
  [ -d "${BROWSER_ROOTS[$i]}" ] || continue
  mkdir -p "${DIRS[$i]}"
  cat > "${DIRS[$i]}/$HOST_NAME.json" <<EOF
{
  "name": "$HOST_NAME",
  "description": "Hydra Download Manager native host",
  "path": "$HOST_BIN",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXT_ID/"]
}
EOF
  echo "installed: ${DIRS[$i]}/$HOST_NAME.json"
  INSTALLED=1
done

# Firefox: same binary, different manifest dialect — the add-on is
# allow-listed by its gecko id (`allowed_extensions`), not by an
# extension-origin URL, and the file lives in Mozilla's own directory.
FF_ID=$(python3 - "$REPO/extensions/firefox/manifest.json" <<'EOF'
import json, sys
print(json.load(open(sys.argv[1]))["browser_specific_settings"]["gecko"]["id"])
EOF
)
for i in "${!FF_DIRS[@]}"; do
  [ -d "${FF_ROOTS[$i]}" ] || continue
  mkdir -p "${FF_DIRS[$i]}"
  cat > "${FF_DIRS[$i]}/$HOST_NAME.json" <<EOF
{
  "name": "$HOST_NAME",
  "description": "Hydra Download Manager native host",
  "path": "$HOST_BIN",
  "type": "stdio",
  "allowed_extensions": ["$FF_ID"]
}
EOF
  echo "installed: ${FF_DIRS[$i]}/$HOST_NAME.json (firefox, $FF_ID)"
  INSTALLED=1
done

if [ "$INSTALLED" = 0 ]; then
  # Say WHERE it looked. "Nothing installed" on its own sends people to
  # reinstall a browser that is already there, when the real answer is
  # usually that it has not been run once yet and so has no profile.
  echo "no supported browser profile found; nothing installed" >&2
  echo >&2
  echo "Looked for a profile in:" >&2
  for d in "${BROWSER_ROOTS[@]}" "${FF_ROOTS[@]}"; do
    echo "  $d" >&2
  done
  echo >&2
  echo "A browser that has never been launched has no profile yet: start it" >&2
  echo "once, then run this again. Hydra also registers itself on every" >&2
  echo "launch, so running the app is usually enough on its own." >&2
  exit 1
fi

echo
echo "Load the extension:"
echo "  Chromium family: chrome://extensions -> Developer mode ->"
echo "    Load unpacked -> $REPO/extensions/chrome"
echo "  Firefox: about:debugging#/runtime/this-firefox -> Load Temporary"
echo "    Add-on -> $REPO/extensions/firefox/Resources/manifest.json"
echo "    (run scripts/sync-extension-resources.sh firefox first)"
echo "(Restart the browser after installing the manifest.)"
