#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the Linux AppImage: one executable file carrying the GUI, the CLI,
# the native-messaging host and the update finisher.
#
#   scripts/package-appimage.sh [--no-build] [--no-zsync] [--out DIR]
#
# Output: target/dist/Hydra-<version>-<x86_64|aarch64>.AppImage
#
# Why this exists next to the deb and the rpm: those install into /usr, which
# only their package manager may rewrite, so Hydra can never update itself
# from one (see hya-updater's UpdateMethod::Package). An AppImage is a single
# file the user owns wherever they put it — the update replaces that file, and
# nothing else on the system was ever touched.
#
# What the image does NOT bundle: GTK, glib, the graphics stack, ALSA and
# libc all come from the host. Bundling GTK means bundling its pixbuf loaders,
# gio modules, immodules and theme engines, and a half-bundled GTK is how
# AppImages end up unable to draw a file dialog on the one desktop the author
# did not test. Those libraries are present on every Linux desktop that can
# run a GTK application at all; the portability that matters here — no
# install, no root, no package database — does not depend on shipping them.
# What DOES set the floor is glibc, so release CI builds this on the oldest
# supported Ubuntu (.github/workflows/release.yml).
#
# Layout inside the image:
#   AppRun                                 dispatcher + desktop integration
#   hydra.desktop, hydra.png, .DirIcon     what the runtime and stores read
#   usr/bin/{hydra,hydra-gui,hydra-host,hydra-updater}
#   usr/share/applications/hydra.desktop
#   usr/share/icons/hicolor/<N>x<N>/apps/hydra.png
#   usr/share/metainfo/io.github.ja7ad.hydra.appdata.xml
#   usr/share/hydra/extensions/            packed + unpacked browser extensions
#   usr/share/hydra/scripts/               install-native-host.sh, for manual use
#   usr/share/man/man1/                    the CLI man pages
#   usr/share/doc/hydra/                   licences

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

BUILD=1
ZSYNC=1
OUT="$REPO/target/dist"
while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) BUILD=0 ;;
    --no-zsync) ZSYNC=0 ;;
    --out) OUT="$2"; shift ;;
    -h|--help) sed -n '4,10p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
[ "$(uname -s)" = Linux ] || { echo "error: the AppImage must be built on Linux" >&2; exit 1; }

# The workspace product version ([workspace.package]), shared by every binary
# in the image — and the version the updater compares against.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# The AppImage convention names architectures the way `uname -m` does, which
# is neither the release archives' amd64/arm64 nor dpkg's. hya-updater's
# appimage_arch() spells it the same way, which is how the running image finds
# its own successor in a release.
ARCH=$(uname -m)
APP_ID=io.github.ja7ad.hydra
IMAGE="$OUT/Hydra-${VERSION}-${ARCH}.AppImage"
mkdir -p "$OUT"

if [ "$BUILD" = 1 ]; then
  cargo build --release --locked -p hya-cli -p hya-gui -p hya-host -p hya-updater
fi
# With CARGO_BUILD_TARGET set (CI), cargo emits into target/<triple>/release.
BIN="target/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release"
for b in hydra hydra-gui hydra-host hydra-updater; do
  [ -x "$BIN/$b" ] || { echo "missing $BIN/$b (build failed?)" >&2; exit 1; }
done

# Extension IDs, derived exactly as scripts/install-native-host.sh and
# scripts/package-linux.sh derive them. Baked into AppRun at build time so the
# image can write its own native-messaging manifests without needing python3
# on the user's machine — the user of an AppImage is entitled to assume the
# file is self-contained.
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

APPDIR="$(mktemp -d)/Hydra.AppDir"
trap 'rm -rf "$(dirname "$APPDIR")"' EXIT
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/metainfo" "$APPDIR/usr/share/hydra" \
         "$APPDIR/usr/share/man/man1" "$APPDIR/usr/share/doc/hydra"

for b in hydra hydra-gui hydra-host hydra-updater; do
  install -Dm755 "$BIN/$b" "$APPDIR/usr/bin/$b"
done

# --- icons --------------------------------------------------------------------
# docs/logo.png into the hicolor theme, plus the two copies the AppImage spec
# wants at the AppDir root: <icon-name>.png for the desktop file's Icon= key
# and .DirIcon for file managers showing the image itself.
render_icon() { # <size> <out.png>
  if command -v magick >/dev/null 2>&1; then
    magick docs/logo.png -resize "$1x$1" "$2"
  elif command -v convert >/dev/null 2>&1; then
    convert docs/logo.png -resize "$1x$1" "$2"
  elif python3 -c 'import PIL' 2>/dev/null; then
    python3 - "$1" "$2" <<'EOF'
import sys
from PIL import Image
s = int(sys.argv[1])
Image.open("docs/logo.png").convert("RGBA").resize((s, s), Image.LANCZOS).save(sys.argv[2])
EOF
  else
    return 1
  fi
}
icons_ok=1
for size in 16 32 48 64 128 256 512; do
  mkdir -p "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps"
  if ! render_icon "$size" "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps/hydra.png"; then
    icons_ok=0
    break
  fi
done
if [ "$icons_ok" = 0 ]; then
  echo "warning: no ImageMagick/PIL; shipping docs/logo.png at 512x512 only" >&2
  rm -rf "$APPDIR/usr/share/icons"
  install -Dm644 docs/logo.png "$APPDIR/usr/share/icons/hicolor/512x512/apps/hydra.png"
fi
install -Dm644 "$APPDIR/usr/share/icons/hicolor/256x256/apps/hydra.png" "$APPDIR/hydra.png" \
  2>/dev/null || install -Dm644 docs/logo.png "$APPDIR/hydra.png"
cp "$APPDIR/hydra.png" "$APPDIR/.DirIcon"

# --- desktop entry ------------------------------------------------------------
# The basename must stay "hydra": it is the app id hydra-gui sets on its
# windows (Wayland app_id / X11 WM_CLASS), and how the shell matches a running
# window to this entry. Exec is AppRun because that is what the AppImage
# runtime invokes; the entry AppRun writes into the user's own applications
# directory points at the image file instead (see integrate() below).
cat > "$APPDIR/usr/share/applications/hydra.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Hydra Download Manager
GenericName=Download Manager
Comment=Multi-connection download accelerator
Exec=AppRun
Icon=hydra
Terminal=false
Categories=Network;FileTransfer;
Keywords=download;manager;accelerator;torrent;http;
StartupWMClass=hydra
EOF
cp "$APPDIR/usr/share/applications/hydra.desktop" "$APPDIR/hydra.desktop"

# AppStream metadata: what software centres and appimagehub read.
cat > "$APPDIR/usr/share/metainfo/${APP_ID}.appdata.xml" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>${APP_ID}</id>
  <name>Hydra Download Manager</name>
  <summary>Multi-connection download manager</summary>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>GPL-3.0-or-later</project_license>
  <developer id="io.github.ja7ad"><name>Javad Rajabzadeh</name></developer>
  <description>
    <p>
      Hydra downloads files over many parallel connections with integrity
      verification. The application carries a desktop GUI, a command-line
      client and a native-messaging host that lets the browser extensions
      hand downloads over to Hydra.
    </p>
  </description>
  <launchable type="desktop-id">hydra.desktop</launchable>
  <url type="homepage">https://github.com/ja7ad/hydra</url>
  <url type="bugtracker">https://github.com/ja7ad/hydra/issues</url>
  <provides><binary>hydra</binary></provides>
  <releases><release version="${VERSION}"/></releases>
  <content_rating type="oars-1.1"/>
</component>
EOF

# --- extensions, scripts, docs -------------------------------------------------
# install-native-host.sh resolves the bundle root as the parent of its own
# directory, so it must sit in scripts/ with extensions/ as a sibling — the
# same arrangement the release tarball uses.
mkdir -p "$APPDIR/usr/share/hydra/extensions" "$APPDIR/usr/share/hydra/scripts"
# The prefix is the path INSTALL.txt tells the user to load the unpacked
# extensions from — and it must be a path that still exists tomorrow, because
# Chromium re-reads a developer-mode extension's directory on every start. The
# image's own mount is not one, so AppRun copies the tree to this stable
# location on first run.
# shellcheck disable=SC2088  # a literal for INSTALL.txt, not a path to expand here
scripts/build-extensions.sh --out "$APPDIR/usr/share/hydra/extensions" \
  --quiet --prefix '~/.local/share/hydra/extensions'
install -Dm755 scripts/install-native-host.sh "$APPDIR/usr/share/hydra/scripts/install-native-host.sh"
install -Dm644 LICENSE               "$APPDIR/usr/share/doc/hydra/LICENSE"
install -Dm644 LICENSING.md          "$APPDIR/usr/share/doc/hydra/LICENSING.md"
install -Dm644 THIRD-PARTY-NOTICES.md "$APPDIR/usr/share/doc/hydra/THIRD-PARTY-NOTICES.md"
install -Dm644 README.md             "$APPDIR/usr/share/doc/hydra/README.md"
for page in docs/man/*.1; do
  install -Dm644 "$page" "$APPDIR/usr/share/man/man1/$(basename "$page")"
done

# --- AppRun -------------------------------------------------------------------
# Written with the extension IDs and the version substituted in; everything
# after the header is literal (quoted heredoc), so nothing else is expanded.
{
cat <<EOF
#!/bin/sh
# Hydra Download Manager ${VERSION} — AppImage entry point.
# Generated by scripts/package-appimage.sh; do not edit inside the image.
HYDRA_VERSION='${VERSION}'
HYDRA_EXT_ID='${EXT_ID}'
HYDRA_FF_ID='${FF_ID}'
EOF
cat <<'EOF'
set -eu
HERE=$(dirname "$(readlink -f "$0")")
HOST_NAME=com.hydra.host

export PATH="$HERE/usr/bin:${PATH:-/usr/bin:/bin}"
# The app's own data (icons, the AppStream file) has to be findable, but the
# host's directories must keep priority: icon themes, GTK modules and mime
# data belong to the desktop the user is running, not to this image.
export XDG_DATA_DIRS="${XDG_DATA_DIRS:-/usr/local/share:/usr/share}:$HERE/usr/share"

# Desktop integration: a menu entry, an icon, and native-messaging manifests
# so the browser extensions can reach hydra-host.
#
# All of it points at $APPIMAGE — the file the user launched — because a path
# inside $HERE is a mount that exists only while this process does. Rewritten
# on every start, which is what makes the image survive being moved, renamed
# or replaced by an update: the next launch repairs every reference to it.
#
# Set HYDRA_APPIMAGE_NO_INTEGRATION=1 for a strictly portable run that writes
# nothing outside the download directory.
data_home() { echo "${XDG_DATA_HOME:-$HOME/.local/share}"; }

integrate() {
  [ -n "${APPIMAGE:-}" ] && [ -f "${APPIMAGE:-}" ] || return 0
  [ -z "${HYDRA_APPIMAGE_NO_INTEGRATION:-}" ] || return 0
  [ -n "${HOME:-}" ] && [ -d "$HOME" ] || return 0
  DATA=$(data_home)/hydra
  mkdir -p "$DATA" || return 0

  # The stable path the browsers are told about. A manifest may not name the
  # image directly: Chromium hands the host no arguments, and the host must
  # come up knowing which AppImage to launch the GUI from.
  cat > "$DATA/hydra-host" <<HOSTEOF
#!/bin/sh
# Generated by the Hydra AppImage on every start; edits are overwritten.
IMAGE='$APPIMAGE'
[ -x "\$IMAGE" ] || { echo "hydra: $APPIMAGE is gone" >&2; exit 1; }
HYDRA_GUI_BIN="\$IMAGE" exec "\$IMAGE" --hydra-exec hydra-host "\$@"
HOSTEOF
  chmod 755 "$DATA/hydra-host"

  # Chromium family, then Firefox: same binary, different manifest dialect —
  # the add-on is allow-listed by its gecko id, not by an extension origin.
  # Only browsers with a profile on this machine get a manifest.
  for pair in \
    "$HOME/.config/google-chrome:NativeMessagingHosts" \
    "$HOME/.config/chromium:NativeMessagingHosts" \
    "$HOME/.config/microsoft-edge:NativeMessagingHosts" \
    "$HOME/.config/BraveSoftware/Brave-Browser:NativeMessagingHosts" \
    "$HOME/.config/vivaldi:NativeMessagingHosts"
  do
    root=${pair%:*}; sub=${pair##*:}
    [ -d "$root" ] || continue
    mkdir -p "$root/$sub" || continue
    cat > "$root/$sub/$HOST_NAME.json" <<MANIFESTEOF
{
  "name": "$HOST_NAME",
  "description": "Hydra Download Manager native host",
  "path": "$DATA/hydra-host",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$HYDRA_EXT_ID/"]
}
MANIFESTEOF
  done
  for root in "$HOME/.mozilla" "$HOME/.librewolf"; do
    [ -d "$root" ] || continue
    mkdir -p "$root/native-messaging-hosts" || continue
    cat > "$root/native-messaging-hosts/$HOST_NAME.json" <<MANIFESTEOF
{
  "name": "$HOST_NAME",
  "description": "Hydra Download Manager native host",
  "path": "$DATA/hydra-host",
  "type": "stdio",
  "allowed_extensions": ["$HYDRA_FF_ID"]
}
MANIFESTEOF
  done

  # The unpacked extensions, copied to a path that outlives this mount:
  # Chromium remembers the directory a developer-mode extension was loaded
  # from and re-reads it on every start, so pointing it at $HERE would give
  # the user an extension that disappears when Hydra closes. Refreshed when
  # the stamp does not match this image's version.
  if [ "$(cat "$DATA/extensions/.version" 2>/dev/null || echo none)" != "$HYDRA_VERSION" ]; then
    rm -rf "$DATA/extensions.new"
    if cp -a "$HERE/usr/share/hydra/extensions" "$DATA/extensions.new" 2>/dev/null; then
      chmod -R u+w "$DATA/extensions.new" 2>/dev/null || true
      rm -rf "$DATA/extensions"
      mv "$DATA/extensions.new" "$DATA/extensions"
      echo "$HYDRA_VERSION" > "$DATA/extensions/.version"
    fi
    rm -rf "$DATA/extensions.new"
  fi

  # Menu entry and icon. StartupWMClass must stay "hydra" — that is the app
  # id hydra-gui sets on its windows, and how the shell pairs the running
  # window with this entry.
  APPS=$(data_home)/applications
  mkdir -p "$APPS" "$DATA" || return 0
  cp -f "$HERE/hydra.png" "$DATA/hydra.png" 2>/dev/null || true
  cat > "$APPS/hydra.desktop" <<DESKTOPEOF
[Desktop Entry]
Type=Application
Name=Hydra Download Manager
GenericName=Download Manager
Comment=Multi-connection download accelerator
Exec="$APPIMAGE"
Icon=$DATA/hydra.png
Terminal=false
Categories=Network;FileTransfer;
StartupWMClass=hydra
X-AppImage-Version=$HYDRA_VERSION
DESKTOPEOF
  chmod 644 "$APPS/hydra.desktop"
  command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database -q "$APPS" 2>/dev/null || true
  return 0
}

unintegrate() {
  DATA=$(data_home)/hydra
  rm -f "$(data_home)/applications/hydra.desktop" "$DATA/hydra-host" "$DATA/hydra.png"
  rm -rf "$DATA/extensions"
  for f in \
    "$HOME/.config/google-chrome/NativeMessagingHosts/$HOST_NAME.json" \
    "$HOME/.config/chromium/NativeMessagingHosts/$HOST_NAME.json" \
    "$HOME/.config/microsoft-edge/NativeMessagingHosts/$HOST_NAME.json" \
    "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/$HOST_NAME.json" \
    "$HOME/.config/vivaldi/NativeMessagingHosts/$HOST_NAME.json" \
    "$HOME/.mozilla/native-messaging-hosts/$HOST_NAME.json" \
    "$HOME/.librewolf/native-messaging-hosts/$HOST_NAME.json"
  do
    rm -f "$f"
  done
  echo "removed the menu entry and the browser manifests."
  echo "downloads, settings and the image file itself were left alone."
}

# Which binary does this invocation want?
#   --hydra-exec NAME   explicit (how the generated host wrapper re-enters)
#   $ARGV0              the name the image was called by, for a symlink farm
#                       (`hya` is the CLI's short name, same as everywhere else)
#   otherwise           the GUI
TARGET=hydra-gui
case "${1:-}" in
  --hydra-exec)
    [ $# -ge 2 ] || { echo "usage: --hydra-exec <hydra|hydra-gui|hydra-host>" >&2; exit 2; }
    TARGET=$2; shift 2
    case "$TARGET" in
      hydra|hydra-gui|hydra-host|hydra-updater) ;;
      *) echo "unknown target: $TARGET" >&2; exit 2 ;;
    esac
    ;;
  --hydra-install)
    # Everything integration writes points at $APPIMAGE, so without it there
    # is nothing to write — say so rather than reporting success.
    if [ -z "${APPIMAGE:-}" ]; then
      echo "hydra: not running from an AppImage (\$APPIMAGE is unset)" >&2
      exit 1
    fi
    unset HYDRA_APPIMAGE_NO_INTEGRATION
    integrate
    echo "desktop entry and browser manifests written."
    exit 0 ;;
  --hydra-uninstall) unintegrate; exit 0 ;;
  --hydra-version) echo "hydra $HYDRA_VERSION ($(uname -m) AppImage)"; exit 0 ;;
  --hydra-extensions)
    EXTS=$(data_home)/hydra/extensions
    [ -d "$EXTS" ] || EXTS="$HERE/usr/share/hydra/extensions"
    echo "$EXTS"; exit 0 ;;
  *)
    case "$(basename "${ARGV0:-hydra-gui}")" in
      hydra|hydra-cli|hya) TARGET=hydra ;;
      hydra-host) TARGET=hydra-host ;;
      hydra-updater) TARGET=hydra-updater ;;
    esac
    ;;
esac

# Integration is for the desktop app. hydra-host is started by the browser
# through the wrapper integration already wrote, and the CLI is a terminal
# program that has no business writing desktop files.
if [ "$TARGET" = hydra-gui ]; then
  integrate
fi

exec "$HERE/usr/bin/$TARGET" "$@"
EOF
} > "$APPDIR/AppRun"
chmod 755 "$APPDIR/AppRun"

# --- build the image ----------------------------------------------------------
# appimagetool ships as an AppImage itself, so it needs either FUSE or
# APPIMAGE_EXTRACT_AND_RUN=1 — CI containers have no FUSE, and extracting is
# both allowed and faster there. $APPIMAGETOOL overrides the download for
# offline or pinned builds.
TOOLS="$REPO/target/appimage-tools"
mkdir -p "$TOOLS"
APPIMAGETOOL="${APPIMAGETOOL:-$TOOLS/appimagetool-$ARCH.AppImage}"
if [ ! -x "$APPIMAGETOOL" ]; then
  URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
  echo "fetching appimagetool: $URL"
  curl -fsSL --retry 3 -o "$APPIMAGETOOL" "$URL"
  chmod 755 "$APPIMAGETOOL"
fi

# Update information: AppImageUpdate and appimaged use this to offer a
# delta update straight from the release. Hydra's own updater does not need
# it — it reads the release API and replaces the file itself — but an image
# that claims to be production ready should also work with the tools its own
# ecosystem provides. The .zsync file appimagetool writes alongside is
# published as a release asset.
UPDATE_INFO="gh-releases-zsync|ja7ad|hydra|latest|Hydra-*-${ARCH}.AppImage.zsync"
ARGS="--no-appstream"
if [ "$ZSYNC" = 1 ] && command -v zsyncmake >/dev/null 2>&1; then
  ARGS="$ARGS --updateinformation $UPDATE_INFO"
else
  [ "$ZSYNC" = 1 ] && echo "note: zsyncmake not found; building without update information" >&2
fi

rm -f "$IMAGE" "$IMAGE.zsync"
# shellcheck disable=SC2086
env APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$ARCH" \
  "$APPIMAGETOOL" $ARGS "$APPDIR" "$IMAGE"
chmod 755 "$IMAGE"

echo "built: $IMAGE"
[ -f "$IMAGE.zsync" ] && echo "built: $IMAGE.zsync"
ls -lh "$IMAGE"*
exit 0
