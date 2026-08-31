#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build Linux packages (deb + rpm) carrying the GUI, the CLI and the
# native-messaging IPC host.
#
#   scripts/package-linux.sh deb|rpm|all [--no-build]
#
# What the packages install:
#   /usr/bin/hydra, /usr/bin/hydra-gui, /usr/bin/hydra-host
#   /usr/share/applications/hydra.desktop            desktop/menu entry
#       The basename must stay "hydra": that is the app id hydra-gui sets on
#       its windows (Wayland app_id / X11 WM_CLASS, see hydra-gui/src/app.rs),
#       and it is how the shell matches a running window to this entry for the
#       dock, the switcher and the process list. StartupWMClass repeats it for
#       desktops that only consult that key.
#   /usr/share/icons/hicolor/<N>x<N>/apps/hydra.png  logo, from docs/logo.png
#   /etc/xdg/autostart/hydra.desktop                 start minimized at login;
#       the in-app "Launch Hydra on startup" toggle writes the same basename
#       under ~/.config/autostart, which overrides this per user (a Hidden=true
#       entry when the user turns it off — see hydra-gui/src/autostart.rs)
#   native-messaging manifests for Chrome/Chromium/Edge/Firefox, so the
#       browser extension reaches hydra-host with no per-user install step
#   /usr/share/hydra-download-manager/extensions
#       the browser extensions themselves — hydra-chrome-<v>.zip and
#       hydra-firefox-<v>.xpi packed (plus a signed .crx when the build
#       machine has the key), chrome/ and firefox/ unpacked for a
#       developer-mode load, and INSTALL.txt with the instructions
#
# The package is named hydra-download-manager because plain "hydra" is the THC
# login cracker in distro repos, and both ship /usr/bin/hydra — hence the
# Conflicts tag as well.

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

CMD="${1:-all}"
[ $# -gt 0 ] && shift
BUILD=1
while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) BUILD=0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
case "$CMD" in
  deb|rpm|all) ;;
  *) echo "usage: $0 deb|rpm|all [--no-build]" >&2; exit 2 ;;
esac
[ "$(uname -s)" = Linux ] || { echo "error: Linux packages must be built on Linux" >&2; exit 1; }

NAME=hydra-download-manager
HOST_NAME=com.hydra.host
# The workspace product version ([workspace.package]), shared by the
# hydra-gui, hydra-cli and hydra-host bin crates this package bundles.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# A pre-release version (0.3.0-rc1) has to be spelled differently per format:
# dpkg treats `-` as the start of the Debian revision and would sort
# 0.3.0-rc1 *after* the final 0.3.0, so the suffix becomes `~rc1`, which sorts
# before it. rpm forbids `-` in Version entirely, so the suffix moves into
# Release as 0.<suffix> (which sorts before the plain release's `1`).
DEB_VERSION="${VERSION/-/~}"
RPM_VERSION="${VERSION%%-*}"
RPM_RELEASE=1
if [ "$VERSION" != "$RPM_VERSION" ]; then
  RPM_RELEASE="0.${VERSION#*-}"
  RPM_RELEASE="${RPM_RELEASE//-/.}"
fi
SUMMARY="Multi-connection download manager (GUI, CLI, browser integration)"
OUT="$REPO/target/dist"
mkdir -p "$OUT"

if [ "$BUILD" = 1 ]; then
  cargo build --release -p hya-cli -p hya-gui -p hya-host
fi
# With CARGO_BUILD_TARGET set (CI), cargo emits into target/<triple>/release.
BIN="target/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release"
for b in hydra hydra-gui hydra-host; do
  [ -x "$BIN/$b" ] || { echo "missing $BIN/$b (build failed?)" >&2; exit 1; }
done

# Extension IDs, derived the same way scripts/install-native-host.sh does, so
# regenerating the pinned manifest key can never leave the sides disagreeing.
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

# --- icon rendering -----------------------------------------------------------
# docs/logo.png resized into the hicolor theme. Falls back to installing the
# original at 512x512 only, when no resizer is on the build machine.
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

# --- shared staging -----------------------------------------------------------
write_chromium_manifest() { # <out.json>
  cat > "$1" <<EOF
{
  "name": "$HOST_NAME",
  "description": "Hydra Download Manager native host",
  "path": "/usr/bin/hydra-host",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXT_ID/"]
}
EOF
}

stage() { # <root> <deb|rpm>
  local ROOT=$1 FLAVOR=$2

  install -Dm755 "$BIN/hydra"      "$ROOT/usr/bin/hydra"
  install -Dm755 "$BIN/hydra-gui"  "$ROOT/usr/bin/hydra-gui"
  install -Dm755 "$BIN/hydra-host" "$ROOT/usr/bin/hydra-host"
  # Short second name for the CLI. The package already Conflicts with
  # thc-hydra over /usr/bin/hydra; `hya` is the name that does not collide,
  # and three letters types better for a command run as often as a download.
  ln -sf hydra "$ROOT/usr/bin/hya"

  # Desktop/menu entry (the "desktop icon").
  install -d "$ROOT/usr/share/applications"
  cat > "$ROOT/usr/share/applications/hydra.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Hydra Download Manager
GenericName=Download Manager
Comment=Multi-connection download accelerator
Exec=hydra-gui
Icon=hydra
Terminal=false
Categories=Network;FileTransfer;
StartupWMClass=hydra
EOF

  # Start minimized (to tray) at login. Same basename as the per-user entry
  # autostart.rs manages, so the in-app toggle overrides this system default.
  install -d "$ROOT/etc/xdg/autostart"
  cat > "$ROOT/etc/xdg/autostart/hydra.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Hydra Download Manager
Comment=Multi-connection download accelerator
Exec=hydra-gui --minimized
Icon=hydra
Terminal=false
Categories=Network;FileTransfer;
X-GNOME-Autostart-enabled=true
EOF

  # Logo into the hicolor icon theme.
  local rendered=1 size
  for size in 16 32 48 64 128 256 512; do
    install -d "$ROOT/usr/share/icons/hicolor/${size}x${size}/apps"
    if ! render_icon "$size" "$ROOT/usr/share/icons/hicolor/${size}x${size}/apps/hydra.png"; then
      rendered=0
      break
    fi
  done
  if [ "$rendered" = 0 ]; then
    echo "warning: no ImageMagick/PIL; installing docs/logo.png at 512x512 only" >&2
    rm -rf "$ROOT/usr/share/icons"
    install -Dm644 docs/logo.png "$ROOT/usr/share/icons/hicolor/512x512/apps/hydra.png"
  fi

  # System-wide native-messaging manifests (Chromium family in /etc, Firefox
  # under its lib dir). Brave/Vivaldi have no stable system-wide path — users
  # of those run scripts/install-native-host.sh per user.
  local d
  for d in etc/opt/chrome etc/chromium etc/opt/edge; do
    install -d "$ROOT/$d/native-messaging-hosts"
    write_chromium_manifest "$ROOT/$d/native-messaging-hosts/$HOST_NAME.json"
  done
  local ff_dirs="usr/lib/mozilla"
  # Firefox on lib64 distros looks in /usr/lib64/mozilla; Debian never has lib64.
  [ "$FLAVOR" = rpm ] && ff_dirs="usr/lib/mozilla usr/lib64/mozilla"
  for d in $ff_dirs; do
    install -d "$ROOT/$d/native-messaging-hosts"
    cat > "$ROOT/$d/native-messaging-hosts/$HOST_NAME.json" <<EOF
{
  "name": "$HOST_NAME",
  "description": "Hydra Download Manager native host",
  "path": "/usr/bin/hydra-host",
  "type": "stdio",
  "allowed_extensions": ["$FF_ID"]
}
EOF
  done

  # Browser extensions: the packed .xpi/.zip, the unpacked directories a
  # developer-mode install loads, and INSTALL.txt written with the installed
  # paths. Same builder every other package uses.
  install -d "$ROOT/usr/share/$NAME/extensions"
  scripts/build-extensions.sh --out "$ROOT/usr/share/$NAME/extensions" \
    --quiet --prefix "/usr/share/$NAME/extensions"
  find "$ROOT/usr/share/$NAME/extensions" -type f -exec chmod 644 {} +
  find "$ROOT/usr/share/$NAME/extensions" -type d -exec chmod 755 {} +

  install -Dm644 LICENSE       "$ROOT/usr/share/doc/$NAME/LICENSE"
  install -Dm644 LICENSING.md  "$ROOT/usr/share/doc/$NAME/LICENSING.md"

  # CLI man pages, gzipped per distro policy (-n keeps the build reproducible).
  install -d "$ROOT/usr/share/man/man1"
  local page
  for page in docs/man/*.1; do
    gzip -9n -c "$page" > "$ROOT/usr/share/man/man1/$(basename "$page").gz"
    chmod 644 "$ROOT/usr/share/man/man1/$(basename "$page").gz"
  done
}

# --- deb ----------------------------------------------------------------------
build_deb() {
  command -v dpkg-deb >/dev/null 2>&1 || { echo "error: dpkg-deb not found" >&2; exit 1; }
  local DEB_ARCH ROOT
  DEB_ARCH=$(dpkg --print-architecture)
  ROOT=$(mktemp -d)/pkg
  mkdir -p "$ROOT"
  stage "$ROOT" deb

  mkdir -p "$ROOT/DEBIAN"
  # /etc files are conffiles so user edits survive upgrades.
  (cd "$ROOT" && find etc -type f | sed 's|^|/|') > "$ROOT/DEBIAN/conffiles"
  # The tray speaks StatusNotifierItem over D-Bus, so no appindicator
  # *library* is needed — only a shell that hosts the protocol. KDE Plasma,
  # Cinnamon, Budgie, LXQt and xfce4-panel 4.18+ do natively; GNOME needs
  # the extension recommended below (already present on Ubuntu desktops).
  cat > "$ROOT/DEBIAN/control" <<EOF
Package: $NAME
Version: $DEB_VERSION
Section: net
Priority: optional
Architecture: $DEB_ARCH
Maintainer: Javad Rajabzadeh <ja7ad@live.com>
Homepage: https://github.com/ja7ad/hydra
Conflicts: hydra
Depends: libc6, libgtk-3-0, libasound2t64 | libasound2
Recommends: gnome-shell-extension-appindicator
Installed-Size: $(du -ks "$ROOT" | cut -f1)
Description: $SUMMARY
 Hydra downloads files over many parallel connections with integrity
 verification. This package installs the hydra CLI, the hydra-gui desktop
 app (with menu entry and login autostart), the hydra-host
 native-messaging bridge plus browser manifests for extension capture, and
 the browser extensions under /usr/share/hydra-download-manager/extensions.
EOF
  for s in postinst postrm; do
    cat > "$ROOT/DEBIAN/$s" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -qt /usr/share/icons/hicolor || true
fi
exit 0
EOF
    chmod 755 "$ROOT/DEBIAN/$s"
  done

  local DEB="$OUT/${NAME}_${DEB_VERSION}_${DEB_ARCH}.deb"
  dpkg-deb --build --root-owner-group "$ROOT" "$DEB" >/dev/null
  echo "built: $DEB"
}

# --- rpm ----------------------------------------------------------------------
build_rpm() {
  command -v rpmbuild >/dev/null 2>&1 || { echo "error: rpmbuild not found" >&2; exit 1; }
  local RPM_ARCH TOP STAGE
  RPM_ARCH=$(uname -m)
  TOP=$(mktemp -d)
  STAGE="$TOP/stage"
  mkdir -p "$STAGE"
  stage "$STAGE" rpm

  cat > "$TOP/$NAME.spec" <<EOF
Name: $NAME
Version: $RPM_VERSION
Release: $RPM_RELEASE
Summary: $SUMMARY
License: GPL-3.0-or-later
URL: https://github.com/ja7ad/hydra
Conflicts: hydra
Recommends: gnome-shell-extension-appindicator

%description
Hydra downloads files over many parallel connections with integrity
verification. This package installs the hydra CLI, the hydra-gui desktop
app (with menu entry and login autostart), the hydra-host
native-messaging bridge plus browser manifests for extension capture, and
the browser extensions under /usr/share/hydra-download-manager/extensions.

%install
rm -rf %{buildroot}
cp -a $STAGE/. %{buildroot}/

%post
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || :
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -qt /usr/share/icons/hicolor || :
fi

%postun
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || :
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -qt /usr/share/icons/hicolor || :
fi

%files
/usr/bin/hydra
/usr/bin/hya
/usr/bin/hydra-gui
/usr/bin/hydra-host
/usr/share/applications/hydra.desktop
/usr/share/icons/hicolor/*/apps/hydra.png
/usr/share/$NAME/extensions/
/usr/share/doc/$NAME/
/usr/share/man/man1/hydra*.1.gz
/usr/share/man/man1/hya.1.gz
%config(noreplace) /etc/xdg/autostart/hydra.desktop
%config(noreplace) /etc/opt/chrome/native-messaging-hosts/$HOST_NAME.json
%config(noreplace) /etc/chromium/native-messaging-hosts/$HOST_NAME.json
%config(noreplace) /etc/opt/edge/native-messaging-hosts/$HOST_NAME.json
/usr/lib/mozilla/native-messaging-hosts/$HOST_NAME.json
/usr/lib64/mozilla/native-messaging-hosts/$HOST_NAME.json
EOF

  rpmbuild -bb \
    --define "_topdir $TOP" \
    --define "debug_package %{nil}" \
    --define "_build_id_links none" \
    --buildroot "$TOP/buildroot" \
    --target "$RPM_ARCH" \
    "$TOP/$NAME.spec" >/dev/null
  cp "$TOP"/RPMS/*/*.rpm "$OUT/"
  echo "built: $OUT/$(basename "$TOP"/RPMS/*/*.rpm)"
}

case "$CMD" in
  deb) build_deb ;;
  rpm) build_rpm ;;
  all) build_deb; build_rpm ;;
esac
