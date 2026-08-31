#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Assemble a browser target's web resources from the Chrome sources.
#
#   scripts/sync-extension-resources.sh firefox|safari
#
# extensions/chrome is the single source of truth for the extension code —
# background.js, content.js and the popup are browser-neutral (they bind
# `browser` or `chrome` and feature-detect every API). Only the manifest
# differs per browser, so this copies the shared files next to the target's
# own manifest.json into extensions/<target>/Resources.
#
# Firefox can then be loaded from that directory (about:debugging, or packed
# with `web-ext`); Safari needs the extra wrapper-app step in
# scripts/build-safari-extension.sh.

set -euo pipefail

TARGET="${1:-}"
case "$TARGET" in
  firefox | safari) ;;
  *)
    echo "usage: $(basename "$0") firefox|safari" >&2
    exit 2
    ;;
esac

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$REPO/extensions/chrome"

# git bash on Windows (where the NSIS installer is packed) ships `python` but
# not always `python3`.
PY=$(command -v python3 || command -v python || true)
[ -n "$PY" ] || { echo "error: python3 is required to validate the manifest" >&2; exit 1; }

# Firefox loads a plain directory (or a zipped .xpi of it), so the shared
# files land NEXT TO its manifest and `extensions/firefox` is loadable as-is
# — the same shape as extensions/chrome. Safari's converter wants a clean
# resources-only input, so it keeps a Resources/ subdirectory.
if [ "$TARGET" = firefox ]; then
  DST="$REPO/extensions/$TARGET"
  rm -rf "$DST/Resources" # legacy layout
else
  DST="$REPO/extensions/$TARGET/Resources"
  rm -rf "$DST"
  mkdir -p "$DST"
  cp "$REPO/extensions/$TARGET/manifest.json" "$DST/manifest.json"
fi

for f in background.js content.js popup.html popup.css popup.js welcome.html welcome.js; do
  cp "$SRC/$f" "$DST/$f"
done
rm -rf "$DST/icons"
cp -R "$SRC/icons" "$DST/icons"

# Chrome's pinned `key` identifies the Chrome Web Store item; shipping it in
# another browser's manifest is at best ignored and at worst a load error.
if grep -q '"key"' "$DST/manifest.json"; then
  echo "error: $TARGET manifest must not carry Chrome's pinned key" >&2
  exit 1
fi
# Safari exposes no downloads API; requesting it would be a lie in the
# permission prompt.
if [ "$TARGET" = safari ] && grep -q '"downloads"' "$DST/manifest.json"; then
  echo "error: Safari has no downloads API; drop the permission" >&2
  exit 1
fi
# Firefox needs an explicit add-on id or native messaging cannot allow-list it.
if [ "$TARGET" = firefox ] && ! grep -q '"gecko"' "$DST/manifest.json"; then
  echo "error: Firefox manifest needs browser_specific_settings.gecko.id" >&2
  exit 1
fi

# The path travels as an ARGUMENT, never interpolated into the -c source:
# git bash rewrites path-shaped arguments to Windows form (C:\...) when it
# invokes native Windows python, but it cannot see a path buried inside a
# code string, and python would be handed an unusable /c/... path.
"$PY" -c "import json,sys; json.load(open(sys.argv[1]))" "$DST/manifest.json" ||
  { echo "error: $TARGET manifest is not valid JSON" >&2; exit 1; }

echo "synced -> ${DST#"$REPO/"}"
