#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build or validate Flatpak package for Hydra.
#
#   scripts/package-flatpak.sh [--bundle] [--install] [--validate-only] [--out DIR]
#
# Options:
#   --bundle         Export a standalone .flatpak bundle
#   --install        Install the built flatpak locally for the current user
#   --validate-only  Validate metainfo XML and desktop file without building
#   --out DIR        Directory to write output bundles to (default: target/dist)
#   -h, --help       Show this help message

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

APP_ID="io.github.ja7ad.hydra"
MANIFEST="packaging/flatpak/${APP_ID}.yml"
METAINFO="packaging/flatpak/${APP_ID}.metainfo.xml"
DESKTOP="packaging/flatpak/${APP_ID}.desktop"

BUNDLE=0
INSTALL=0
VALIDATE_ONLY=0
OUT="$REPO/target/dist"

while [ $# -gt 0 ]; do
  case "$1" in
    --bundle) BUNDLE=1 ;;
    --install) INSTALL=1 ;;
    --validate-only) VALIDATE_ONLY=1 ;;
    --out) OUT="$2"; shift ;;
    -h|--help)
      sed -n '4,15p' "$0"
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      exit 2
      ;;
  esac
  shift
done

echo "==> Validating Flatpak metadata..."

# Validate desktop file
if command -v desktop-file-validate >/dev/null 2>&1; then
  desktop-file-validate "$DESKTOP"
  echo "  ✓ Desktop file ($DESKTOP) is valid"
else
  echo "  ! desktop-file-validate not found, skipping desktop file check"
fi

# Validate AppStream metainfo
if command -v appstreamcli >/dev/null 2>&1; then
  appstreamcli validate --no-net "$METAINFO"
  echo "  ✓ AppStream metainfo ($METAINFO) is valid"
elif command -v appstream-util >/dev/null 2>&1; then
  appstream-util validate-relax "$METAINFO"
  echo "  ✓ AppStream metainfo ($METAINFO) is valid"
else
  echo "  ! appstreamcli / appstream-util not found, skipping AppStream validation"
fi

if [ "$VALIDATE_ONLY" = 1 ]; then
  echo "Validation complete."
  exit 0
fi

[ "$(uname -s)" = Linux ] || { echo "error: Flatpak builds must be run on Linux" >&2; exit 1; }

command -v flatpak-builder >/dev/null 2>&1 || {
  echo "error: flatpak-builder is required to build flatpak packages" >&2
  exit 1
}

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
BUILD_DIR="$(mktemp -d)/flatpak-build"
REPO_DIR="$REPO/target/flatpak-repo"
mkdir -p "$OUT" "$REPO_DIR"
trap 'rm -rf "$BUILD_DIR"' EXIT

echo "==> Building Flatpak for ${APP_ID} v${VERSION} (${ARCH})..."

BUILD_ARGS=("--force-clean" "--repo=$REPO_DIR")
if [ "$INSTALL" = 1 ]; then
  BUILD_ARGS+=("--user" "--install")
fi

flatpak-builder "${BUILD_ARGS[@]}" "$BUILD_DIR" "$MANIFEST"

if [ "$BUNDLE" = 1 ]; then
  BUNDLE_FILE="$OUT/Hydra-${VERSION}-${ARCH}.flatpak"
  echo "==> Creating bundle: $BUNDLE_FILE"
  flatpak build-bundle "$REPO_DIR" "$BUNDLE_FILE" "$APP_ID"
  echo "built: $BUNDLE_FILE"
fi

echo "==> Flatpak build finished successfully."
