#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Update Formula/hydra.rb and Casks/hydra.rb for a release.
#
#   scripts/update-homebrew.sh [vX.Y.Z] [DIST_DIR]
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
fi
VERSION="${VERSION#v}"
TAG="v${VERSION}"
DIST="${2:-dist}"
TAP_DIR="${3:-${HOMEBREW_TAP_DIR:-../homebrew-tap}}"

echo "Updating Homebrew tap at ${TAP_DIR} for version ${VERSION} (tag ${TAG})..."

mkdir -p "${TAP_DIR}/Formula" "${TAP_DIR}/Casks"

# 1. Formula sha256 (source tarball)
SOURCE_URL="https://github.com/ja7ad/hydra/archive/refs/tags/${TAG}.tar.gz"
echo "Fetching source tarball SHA256..."
FORMULA_SHA=$(curl -fsSL "$SOURCE_URL" | sha256sum | awk '{print $1}') || FORMULA_SHA=""
if [ -z "$FORMULA_SHA" ]; then
  echo "warning: could not fetch remote tarball; computing from git archive" >&2
  TMP_TAR=$(mktemp)
  git archive --format=tar.gz --prefix="hydra-${VERSION}/" "$TAG" -o "$TMP_TAR"
  FORMULA_SHA=$(sha256sum "$TMP_TAR" | awk '{print $1}')
  rm -f "$TMP_TAR"
fi

# 2. Cask sha256s (macOS DMGs & Linux Tarballs)
DMG_ARM_SHA=""
DMG_INTEL_SHA=""
LINUX_ARM_SHA=""
LINUX_AMD_SHA=""

if [ -f "$DIST/SHA256SUMS.txt" ]; then
  DMG_ARM_SHA=$(grep "Hydra-${VERSION}-arm64.dmg" "$DIST/SHA256SUMS.txt" | awk '{print $1}' || true)
  DMG_INTEL_SHA=$(grep "Hydra-${VERSION}-x86_64.dmg" "$DIST/SHA256SUMS.txt" | awk '{print $1}' || true)
  LINUX_ARM_SHA=$(grep "hydra-${VERSION}-linux-arm64.tar.gz" "$DIST/SHA256SUMS.txt" | awk '{print $1}' || true)
  LINUX_AMD_SHA=$(grep "hydra-${VERSION}-linux-amd64.tar.gz" "$DIST/SHA256SUMS.txt" | awk '{print $1}' || true)
fi

if [ -z "$DMG_ARM_SHA" ] && [ -f "$DIST/Hydra-${VERSION}-arm64.dmg" ]; then
  DMG_ARM_SHA=$(sha256sum "$DIST/Hydra-${VERSION}-arm64.dmg" | awk '{print $1}')
fi
if [ -z "$DMG_INTEL_SHA" ] && [ -f "$DIST/Hydra-${VERSION}-x86_64.dmg" ]; then
  DMG_INTEL_SHA=$(sha256sum "$DIST/Hydra-${VERSION}-x86_64.dmg" | awk '{print $1}')
fi
if [ -z "$LINUX_ARM_SHA" ] && [ -f "$DIST/hydra-${VERSION}-linux-arm64.tar.gz" ]; then
  LINUX_ARM_SHA=$(sha256sum "$DIST/hydra-${VERSION}-linux-arm64.tar.gz" | awk '{print $1}')
fi
if [ -z "$LINUX_AMD_SHA" ] && [ -f "$DIST/hydra-${VERSION}-linux-amd64.tar.gz" ]; then
  LINUX_AMD_SHA=$(sha256sum "$DIST/hydra-${VERSION}-linux-amd64.tar.gz" | awk '{print $1}')
fi

# Update ${TAP_DIR}/Formula/hydra.rb
cat > "${TAP_DIR}/Formula/hydra.rb" <<EOF
class Hydra < Formula
  desc "Fast, resilient, multi-source file retriever and download engine"
  homepage "https://github.com/ja7ad/hydra"
  url "https://github.com/ja7ad/hydra/archive/refs/tags/${TAG}.tar.gz"
  sha256 "${FORMULA_SHA}"
  license "GPL-3.0-or-later"
  head "https://github.com/ja7ad/hydra.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/hydra-cli")

    man1.install Dir["docs/man/*.1"] if Dir.exist?("docs/man")

    generate_completions_from_executable(bin/"hydra", "completions")

    # "hydra" is also THC-Hydra, which Homebrew ships as hydra too, and three
    # letters types better for a command run as often as a download. A
    # symlink, so it follows whatever "hydra" this keg has.
    bin.install_symlink bin/"hydra" => "hya"
    # A completion script names the command it completes, so the short name
    # needs its own set.
    generate_completions_from_executable(bin/"hydra", "completions", "--bin-name", "hya",
                                         base_name: "hya")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/hydra --version")
    assert_match version.to_s, shell_output("#{bin}/hya --version")
  end
end
EOF

# Update ${TAP_DIR}/Casks/hydra.rb if checksums are available
if [ -n "$DMG_ARM_SHA" ] && [ -n "$DMG_INTEL_SHA" ]; then
cat > "${TAP_DIR}/Casks/hydra.rb" <<EOF
cask "hydra" do
  version "${VERSION}"

  on_macos do
    arch arm: "arm64", intel: "x86_64"

    sha256 arm:   "${DMG_ARM_SHA}",
           intel: "${DMG_INTEL_SHA}"

    url "https://github.com/ja7ad/hydra/releases/download/v#{version}/Hydra-#{version}-#{arch}.dmg"

    depends_on macos: :big_sur

    app "Hydra Download Manager.app"
    binary "#{appdir}/Hydra Download Manager.app/Contents/MacOS/hydra"
    # The short second name, for the same reason the formula has one.
    binary "#{appdir}/Hydra Download Manager.app/Contents/MacOS/hydra", target: "hya"
    manpage "#{appdir}/Hydra Download Manager.app/Contents/Resources/man/man1/hydra.1"
  end
EOF

if [ -n "$LINUX_ARM_SHA" ] && [ -n "$LINUX_AMD_SHA" ]; then
cat >> "${TAP_DIR}/Casks/hydra.rb" <<EOF
  on_linux do
    arch arm: "arm64", intel: "amd64"

    sha256 arm:   "${LINUX_ARM_SHA}",
           intel: "${LINUX_AMD_SHA}"

    url "https://github.com/ja7ad/hydra/releases/download/v#{version}/hydra-#{version}-linux-#{arch}.tar.gz"

    binary "hydra-#{version}-linux-#{arch}/hydra"
    binary "hydra-#{version}-linux-#{arch}/hydra", target: "hya"
    binary "hydra-#{version}-linux-#{arch}/hydra-gui"
    binary "hydra-#{version}-linux-#{arch}/hydra-host"
    manpage "hydra-#{version}-linux-#{arch}/man/hydra.1"
  end
EOF
fi

cat >> "${TAP_DIR}/Casks/hydra.rb" <<EOF

  name "Hydra"
  desc "Multi-source file retriever and download manager"
  homepage "https://github.com/ja7ad/hydra"

  livecheck do
    url :url
    strategy :github_latest
  end

  auto_updates true

  zap trash: [
    "~/.config/hydra",
    "~/Library/Application Support/Hydra",
    "~/Library/Preferences/io.github.ja7ad.hydra.plist",
    "~/Library/Saved Application State/io.github.ja7ad.hydra.savedState",
  ]
end
EOF
fi

echo "Tap at ${TAP_DIR} updated successfully."
