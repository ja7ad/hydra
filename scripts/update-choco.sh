#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Bump the Chocolatey package (packaging/choco) to a release: version,
# x64 / ARM64 installer download URLs and their SHA256 checksums, then
# optionally `choco pack` it and push it to the community repository.
#
#   scripts/update-choco.sh [vX.Y.Z] [DIST_DIR] [--pack] [--push]
#
#   VERSION   defaults to the workspace version in Cargo.toml.
#   DIST_DIR  directory holding hydra-X.Y.Z-windows-{x64,arm64}-setup.exe
#             and/or SHA256SUMS.txt (default: dist). When neither is there
#             the installers are downloaded from the GitHub release and
#             hashed locally, so the script also works after the fact.
#   --pack    run `choco pack` into $CHOCO_OUT (default: dist/choco).
#   --push    pack, then `choco push` to https://push.chocolatey.org/ using
#             $CHOCO_API_KEY.
#
# Files rewritten:
#   packaging/choco/hydra-download-manager.nuspec   <version>, <releaseNotes>
#   packaging/choco/tools/chocolateyinstall.ps1     url64*, checksum64*
#   packaging/choco/tools/VERIFICATION.txt          URLs and checksums
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

VERSION=""
DIST=""
PACK=false
PUSH=false
for arg in "$@"; do
  case "$arg" in
    --pack) PACK=true ;;
    --push) PACK=true; PUSH=true ;;
    -h|--help) sed -n '5,25p' "$0"; exit 0 ;;
    -*) echo "error: unknown option $arg" >&2; exit 2 ;;
    *)
      if [ -z "$VERSION" ]; then VERSION="$arg"
      elif [ -z "$DIST" ]; then DIST="$arg"
      else echo "error: unexpected argument $arg" >&2; exit 2
      fi ;;
  esac
done
if [ -z "$VERSION" ]; then
  VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
fi
VERSION="${VERSION#v}"
DIST="${DIST:-dist}"
CHOCO_DIR="$REPO_ROOT/packaging/choco"
CHOCO_OUT="${CHOCO_OUT:-$REPO_ROOT/dist/choco}"
RELEASE_BASE="https://github.com/ja7ad/hydra/releases/download/v${VERSION}"
NUSPEC="$CHOCO_DIR/hydra-download-manager.nuspec"
INSTALL_PS1="$CHOCO_DIR/tools/chocolateyinstall.ps1"
VERIFICATION="$CHOCO_DIR/tools/VERIFICATION.txt"

echo "Updating Chocolatey package for version ${VERSION}..."

# Print the SHA256 of hydra-VERSION-windows-ARCH-setup.exe, taken from the
# local file, from SHA256SUMS.txt, or from the published release asset.
asset_sha256() {
  local asset="hydra-${VERSION}-windows-$1-setup.exe"
  local sha=""
  if [ -f "$DIST/$asset" ]; then
    sha=$(sha256sum "$DIST/$asset" | awk '{print $1}')
  elif [ -f "$DIST/SHA256SUMS.txt" ]; then
    sha=$(grep -E "  ${asset}\$" "$DIST/SHA256SUMS.txt" | awk '{print $1}' || true)
  fi
  if [ -z "$sha" ]; then
    echo "    $asset not in $DIST; downloading from the release" >&2
    local tmp
    tmp=$(mktemp)
    curl -fsSL --retry 3 -o "$tmp" "$RELEASE_BASE/$asset"
    sha=$(sha256sum "$tmp" | awk '{print $1}')
    rm -f "$tmp"
  fi
  [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || { echo "error: no SHA256 for $asset" >&2; exit 1; }
  # Chocolatey conventionally lists checksums in upper case.
  echo "$sha" | tr '[:lower:]' '[:upper:]'
}

X64_URL="$RELEASE_BASE/hydra-${VERSION}-windows-x64-setup.exe"
ARM64_URL="$RELEASE_BASE/hydra-${VERSION}-windows-arm64-setup.exe"
X64_SHA=$(asset_sha256 x64)
ARM64_SHA=$(asset_sha256 arm64)
echo "    x64:   $X64_SHA"
echo "    arm64: $ARM64_SHA"

# nuspec: version and release notes link
perl -pi -e "s{<version>[^<]*</version>}{<version>${VERSION}</version>}" "$NUSPEC"
perl -pi -e "s{<releaseNotes>[^<]*</releaseNotes>}{<releaseNotes>https://github.com/ja7ad/hydra/releases/tag/v${VERSION}</releaseNotes>}" "$NUSPEC"

# chocolateyinstall.ps1: keep the comments, replace only the quoted values
perl -pi -e "s{^(\s*url64\s*=\s*)'[^']*'}{\$1'${X64_URL}'}" "$INSTALL_PS1"
perl -pi -e "s{^(\s*checksum64\s*=\s*)'[^']*'}{\$1'${X64_SHA}'}" "$INSTALL_PS1"
perl -pi -e "s{^(\s*url64arm\s*=\s*)'[^']*'}{\$1'${ARM64_URL}'}" "$INSTALL_PS1"
perl -pi -e "s{^(\s*checksum64arm\s*=\s*)'[^']*'}{\$1'${ARM64_SHA}'}" "$INSTALL_PS1"

# VERIFICATION.txt is regenerated whole; it is prose, not a template
cat > "$VERIFICATION" <<EOF
VERIFICATION
Verification is intended to assist the Chocolatey moderators and community
in verifying that this package's contents are trustworthy.

The package maintainer is the author of Hydra. The binaries are downloaded directly from the official upstream GitHub releases:
https://github.com/ja7ad/hydra/releases/tag/v${VERSION}

1. Download the installers from the official GitHub Release assets:
   - x64:   ${X64_URL}
   - ARM64: ${ARM64_URL}

2. Verify the SHA256 checksums in PowerShell:
   Get-FileHash -Algorithm SHA256 .\\hydra-${VERSION}-windows-x64-setup.exe
   Get-FileHash -Algorithm SHA256 .\\hydra-${VERSION}-windows-arm64-setup.exe

3. Compare the results with the expected values below, which are the same
   ones pinned in 'tools/chocolateyinstall.ps1':
   - x64:   ${X64_SHA}
   - ARM64: ${ARM64_SHA}

The release's SHA256SUMS.txt asset lists the same checksums for every file.
EOF

# Every value must have landed; a silently unmatched perl pattern would
# otherwise ship the previous release's installer under the new version.
for needle in "$X64_URL" "$X64_SHA" "$ARM64_URL" "$ARM64_SHA"; do
  grep -qF "$needle" "$INSTALL_PS1" || { echo "error: $INSTALL_PS1 missing $needle" >&2; exit 1; }
done
grep -qF "<version>${VERSION}</version>" "$NUSPEC" || { echo "error: $NUSPEC version not updated" >&2; exit 1; }
echo "==> packaging/choco bumped to ${VERSION}"

if [ "$PACK" = true ]; then
  command -v choco >/dev/null 2>&1 || {
    echo "error: choco not found; --pack/--push need Chocolatey (Windows)" >&2
    exit 1
  }
  mkdir -p "$CHOCO_OUT"
  NUPKG="$CHOCO_OUT/hydra-download-manager.${VERSION}.nupkg"
  rm -f "$NUPKG"
  choco pack "$NUSPEC" --outputdirectory "$CHOCO_OUT"
  [ -f "$NUPKG" ] || { echo "error: $NUPKG was not produced" >&2; exit 1; }
  echo "==> Packed $NUPKG"
fi

if [ "$PUSH" = true ]; then
  [ -n "${CHOCO_API_KEY:-}" ] || { echo "error: CHOCO_API_KEY is not set" >&2; exit 1; }
  choco push "$NUPKG" --source https://push.chocolatey.org/ --api-key "$CHOCO_API_KEY"
  echo "==> Pushed hydra-download-manager ${VERSION} to the Chocolatey community repository"
fi
