#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Install the latest Hydra release on Linux or macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/ja7ad/hydra/main/install.sh | bash
#   ... | bash -s -- --cli            # CLI only (default installs the GUI bundle)
#   ... | bash -s -- --version vx.x.x # pin a release instead of latest
#   ... | bash -s -- --beta           # newest -rc pre-release when ahead of latest
#   ... | bash -s -- --prefix ~/.local
#   ... | bash -s -- --app-dir ~/Applications # macOS: install the app just
#                                             # for me (default /Applications)
#
# Default install is the GUI bundle, and it lands the way each desktop
# expects an application to:
#
#   macOS   "Hydra Download Manager.app" in /Applications (the default;
#           --app-dir moves it), so the app has its icon and its name in
#           Launchpad, Spotlight, the Dock and the app switcher.
#           <prefix>/bin/{hydra,hydra-gui,hydra-host} become symlinks into
#           the bundle, so the CLI stays on PATH and one self update
#           refreshes the app and the CLI together.
#   Linux   binaries in <prefix>/bin plus a hicolor icon and a .desktop entry,
#           so the app has its icon and name in the launcher and the dock.
#
# Both also get the browser extensions + native-host installer in
# <prefix>/share/hydra. On Linux this uses the release tarball (no deb/rpm
# needed). --cli installs only the hydra binary.
#
# Every mode also links <prefix>/bin/hya to the CLI — see link_cli_alias.
#
# Homebrew users on macOS/Linux can also run:
#   brew install ja7ad/tap/hydra         # CLI
#   brew install --cask ja7ad/tap/hydra  # macOS Desktop GUI bundle

set -euo pipefail

REPO="ja7ad/hydra"
MODE="gui"
VERSION=""
PREFIX=""
APP_DIR=""
BETA=0

# The macOS bundle, and the executable inside it. The product name is what
# every macOS surface shows — Launchpad, the Dock, the app switcher, Login
# Items — which is the whole reason the GUI is not installed as a bare
# `hydra-gui` binary there.
APP_NAME="Hydra Download Manager"
BUNDLE_ID="io.github.ja7ad.hydra"

usage() {
  cat >&2 <<EOF
Usage: install.sh [--cli] [--version vX.Y.Z] [--beta] [--prefix DIR]

  --cli            install only the hydra CLI binary
  --version TAG    install a specific release tag (default: latest)
  --beta           install the newest -rc pre-release when it is ahead of the
                   latest stable release (otherwise the stable release)
  --prefix DIR     install root (default: /usr/local, falling back to ~/.local)
  --app-dir DIR    macOS only: where "$APP_NAME.app" is installed
                   (default: /Applications, falling back to ~/Applications)
EOF
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --cli) MODE="cli" ;;
    --gui) MODE="gui" ;;
    --version) VERSION="$2"; shift ;;
    --beta) BETA=1 ;;
    --prefix) PREFIX="$2"; shift ;;
    --app-dir) APP_DIR="$2"; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
  shift
done

case "$(uname -s)" in
  Linux)  OS="linux" ;;
  Darwin) OS="macos" ;;
  *) echo "error: unsupported OS $(uname -s) (use install.ps1 on Windows)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64|amd64)  ARCH="amd64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  *) echo "error: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

fetch() { # fetch URL [OUTFILE] — curl with a wget fallback
  if command -v curl >/dev/null 2>&1; then
    if [ $# -eq 2 ]; then curl -fSL --proto '=https' -o "$2" "$1"; else curl -fsSL --proto '=https' "$1"; fi
  elif command -v wget >/dev/null 2>&1; then
    if [ $# -eq 2 ]; then wget -qO "$2" "$1"; else wget -qO- "$1"; fi
  else
    echo "error: need curl or wget" >&2; exit 1
  fi
}

ver_gt() { # ver_gt A B — true when A's numeric core is ahead of B's
  awk -v a="${1#v}" -v b="${2#v}" 'BEGIN{
    sub(/[-+].*/, "", a); sub(/[-+].*/, "", b)
    n = split(a, x, "."); m = split(b, y, ".")
    for (i = 1; i <= 3; i++) {
      ai = (i <= n ? x[i] : 0) + 0; bi = (i <= m ? y[i] : 0) + 0
      if (ai > bi) exit 0
      if (ai < bi) exit 1
    }
    exit 1
  }'
}

# Desktop files, icons and browser manifests belong to the human running the
# install, not to root: `curl ... | sudo bash` would otherwise scatter them
# through /root, where no session ever looks.
if [ -n "${SUDO_USER:-}" ] && [ "${SUDO_USER}" != root ] && [ "$(id -u)" = 0 ]; then
  AS_USER="$SUDO_USER"
  USER_HOME=$(eval echo "~$SUDO_USER")
else
  AS_USER=""
  USER_HOME="$HOME"
fi

own_user() { # own_user PATH... — hand back what root created in a user's home
  [ -n "$AS_USER" ] || return 0
  chown -R "$AS_USER" "$@" 2>/dev/null || true
}

run_as_user() { # run_as_user CMD... — as the human, when running under sudo
  if [ -n "$AS_USER" ]; then
    sudo -u "$AS_USER" -H "$@"
  else
    "$@"
  fi
}

build_macos_app() { # build_macos_app SRC APP — assemble and install the .app
  local src="$1" app="$2" staged="$TMP/bundle/$APP_NAME.app"
  mkdir -p "$staged/Contents/MacOS" "$staged/Contents/Resources"

  # The GUI takes the product name: that file name is what Activity Monitor,
  # the force-quit panel and Login Items show, and it is the name Hydra's own
  # updater maps the archive's `hydra-gui` onto.
  install -m 755 "$src/hydra-gui" "$staged/Contents/MacOS/$APP_NAME"
  # The CLI, the native-messaging host and the update finisher travel inside
  # the bundle, so the app is one self-contained directory and a self update
  # refreshes every part of it at once.
  install -m 755 "$src/hydra" "$staged/Contents/MacOS/hydra"
  install -m 755 "$src/hydra-host" "$staged/Contents/MacOS/hydra-host"
  if [ -f "$src/hydra-updater" ]; then
    install -m 755 "$src/hydra-updater" "$staged/Contents/MacOS/hydra-updater"
  fi

  # Icon. .icns is the only format the Dock, Launchpad, Finder and the app
  # switcher read; sips and iconutil are part of macOS, so no toolchain is
  # needed to build one from the logo the archive ships.
  if [ -f "$src/logo.png" ] && command -v iconutil >/dev/null 2>&1; then
    local iconset="$TMP/hydra.iconset" size
    mkdir -p "$iconset"
    for size in 16 32 64 128 256 512; do
      sips -z "$size" "$size" "$src/logo.png" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null 2>&1 || true
      sips -z "$((size * 2))" "$((size * 2))" "$src/logo.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null 2>&1 || true
    done
    iconutil -c icns "$iconset" -o "$staged/Contents/Resources/hydra.icns" 2>/dev/null ||
      echo "warning: could not build the app icon from logo.png" >&2
  fi

  # CLI man pages ride inside the bundle too, so the app stays complete if it
  # is later moved or copied to another Mac:
  #   man "$app/Contents/Resources/man/man1/hydra.1"
  if [ -d "$src/man" ]; then
    mkdir -p "$staged/Contents/Resources/man/man1"
    install -m 644 "$src"/man/*.1 "$staged/Contents/Resources/man/man1/"
  fi

  # CFBundleVersion and CFBundleShortVersionString take period-separated
  # integers only, so a pre-release suffix (0.4.0-rc1) is dropped here; the
  # release tag itself keeps it.
  cat > "$staged/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key><string>${APP_NAME}</string>
    <key>CFBundleExecutable</key><string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key><string>${VER%%-*}</string>
    <key>CFBundleShortVersionString</key><string>${VER%%-*}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>hydra</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
PLIST

  # Ad-hoc signature: unsigned binaries get silently denied by TCC instead of
  # prompting for Downloads-folder access.
  codesign --force --deep -s - "$staged" >/dev/null 2>&1 ||
    echo "warning: could not sign the app; macOS may deny it Downloads access" >&2

  # A running copy keeps the old binaries mapped, and hydra-host would hand
  # the next browser capture to it; quit it before the replacement lands.
  if pgrep -f "$app/Contents/MacOS/" >/dev/null 2>&1; then
    echo "quitting the running $APP_NAME..."
    run_as_user osascript -e "quit app \"$APP_NAME\"" >/dev/null 2>&1 || true
    sleep 2
    pkill -f "$app/Contents/MacOS/" 2>/dev/null || true
  fi

  $APP_SUDO mkdir -p "$APP_DIR"
  $APP_SUDO rm -rf "$app"
  # ditto preserves the bundle layout and the signature a plain cp -R can break.
  $APP_SUDO ditto "$staged" "$app"
  # An app dragged out of a .dmg belongs to whoever installed it, and that
  # ownership is exactly what lets Hydra update itself later without asking
  # for a password. Installing with sudo would otherwise leave it to root.
  if [ -n "$APP_SUDO" ]; then
    $APP_SUDO chown -R "${AS_USER:-$(id -un)}" "$app" 2>/dev/null || true
  fi

  # Tell Launch Services about the app now, instead of whenever macOS next
  # rescans /Applications: this is what puts it in Launchpad, Spotlight and
  # the "Open With" menu immediately.
  local lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
  if [ -x "$lsregister" ]; then
    "$lsregister" -f "$app" >/dev/null 2>&1 || true
  fi
  echo "installed $app"
}

link_bin() { # link_bin NAME TARGET — <prefix>/bin/NAME -> TARGET
  $SUDO ln -sfn "$2" "$BIN_DIR/$1"
  echo "linked $BIN_DIR/$1 -> $2"
}

link_cli_alias() { # <prefix>/bin/hya -> hydra
  # `hydra` is also THC-Hydra, the login auditor Debian, Ubuntu, Kali, Arch
  # and Homebrew all ship, so a machine can easily carry two of them on PATH
  # — and three letters is a friendlier thing to type for a command run as
  # often as a download. Hence the short name, as a RELATIVE symlink: it
  # resolves through whatever `hydra` is at the time, so a self update (which
  # replaces the CLI in place) moves both names at once, and it keeps
  # pointing at the right file if the prefix is ever relocated.
  local alias_path="$BIN_DIR/hya"
  if [ -L "$alias_path" ]; then
    # Only a link that is already ours may be rewritten.
    case "$(readlink "$alias_path")" in
      hydra|"$BIN_DIR/hydra") ;;
      *) echo "note: $alias_path points somewhere else; left alone" >&2; return 0 ;;
    esac
  elif [ -e "$alias_path" ]; then
    echo "note: $alias_path already exists; left alone (use hydra instead)" >&2
    return 0
  fi
  $SUDO ln -sfn hydra "$alias_path"
  echo "linked $alias_path -> hydra"
}

if [ -z "$VERSION" ]; then
  # Capture the JSON before parsing: grep -m1 on a live curl pipe closes it
  # early, which makes curl fail with exit 56 under pipefail.
  RELEASE_JSON=$(fetch "https://api.github.com/repos/${REPO}/releases/latest") || RELEASE_JSON=""
  VERSION=$(printf '%s\n' "$RELEASE_JSON" | grep -m1 '"tag_name"' | cut -d'"' -f4 || true)
  [ -n "$VERSION" ] || { echo "error: could not resolve the latest release tag" >&2; exit 1; }
  if [ "$BETA" = 1 ]; then
    # The newest -rc pre-release wins only while its version is ahead of the
    # stable release; once stable catches up, --beta installs stable.
    LIST_JSON=$(fetch "https://api.github.com/repos/${REPO}/releases?per_page=30") || LIST_JSON=""
    RC_TAG=$(printf '%s\n' "$LIST_JSON" | grep -o '"tag_name": *"[^"]*"' | cut -d'"' -f4 | grep -m1 -- '-rc' || true)
    if [ -n "$RC_TAG" ] && ver_gt "$RC_TAG" "$VERSION"; then
      VERSION="$RC_TAG"
    fi
  fi
fi
VER="${VERSION#v}"
# Assets are named from the workspace manifest version, which may stay on the
# plain release version while the tag carries a pre-release suffix: the
# v0.3.2-rc release ships hydra-0.3.2-* files. Try the tag spelling first, then
# the tag without its suffix.
CORE="${VER%%-*}"
if [ "$MODE" = cli ]; then
  ASSET_PREFIX="hydra-cli"
else
  ASSET_PREFIX="hydra"
fi
if [ "$CORE" != "$VER" ]; then
  CANDIDATES="$VER $CORE"
else
  CANDIDATES="$VER"
fi
DL_BASE="https://github.com/${REPO}/releases/download/${VERSION}"

# Prefix: /usr/local when writable (or sudo is available), else ~/.local.
SUDO=""
if [ -z "$PREFIX" ]; then
  if [ -w /usr/local/bin ] 2>/dev/null || [ -w /usr/local ]; then
    PREFIX="/usr/local"
  elif command -v sudo >/dev/null 2>&1; then
    PREFIX="/usr/local"; SUDO="sudo"
  else
    PREFIX="$HOME/.local"
  fi
elif [ ! -w "$PREFIX" ] && [ -e "$PREFIX" ] && command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
fi
BIN_DIR="$PREFIX/bin"
SHARE_DIR="$PREFIX/share/hydra"

# macOS: where the .app goes. /Applications is group-writable by admins on a
# normal Mac, so this usually needs no sudo at all; ~/Applications is a
# first-class fallback that Launchpad and Spotlight index just the same.
APP=""
APP_SUDO=""
if [ "$OS" = macos ] && [ "$MODE" = gui ]; then
  if [ -z "$APP_DIR" ]; then
    if [ -w /Applications ]; then
      APP_DIR="/Applications"
    elif command -v sudo >/dev/null 2>&1; then
      APP_DIR="/Applications"; APP_SUDO="sudo"
    else
      APP_DIR="$USER_HOME/Applications"
    fi
  elif [ -e "$APP_DIR" ] && [ ! -w "$APP_DIR" ] && command -v sudo >/dev/null 2>&1; then
    APP_SUDO="sudo"
  fi
  APP="$APP_DIR/$APP_NAME.app"
fi

echo "hydra ${VERSION} (${MODE}) -> ${PREFIX}  [${OS}/${ARCH}]"
if [ -n "$APP" ]; then echo "app bundle -> ${APP}"; fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

NAME=""
for CAND in $CANDIDATES; do
  TRY="${ASSET_PREFIX}-${CAND}-${OS}-${ARCH}"
  echo "downloading ${DL_BASE}/${TRY}.tar.gz"
  # Missing the first spelling is expected on a pre-release tag, so only the
  # last candidate's failure is worth reporting.
  if [ "$CAND" = "$CORE" ]; then
    if fetch "${DL_BASE}/${TRY}.tar.gz" "$TMP/${TRY}.tar.gz"; then NAME="$TRY"; break; fi
  elif fetch "${DL_BASE}/${TRY}.tar.gz" "$TMP/${TRY}.tar.gz" 2>/dev/null; then
    NAME="$TRY"; break
  fi
  rm -f "$TMP/${TRY}.tar.gz"
done
[ -n "$NAME" ] || { echo "error: no ${MODE} archive for ${OS}/${ARCH} in release ${VERSION}" >&2; exit 1; }
tar -xzf "$TMP/${NAME}.tar.gz" -C "$TMP"
SRC="$TMP/$NAME"

$SUDO mkdir -p "$BIN_DIR"
if [ -n "$APP" ]; then
  # macOS GUI install: every binary lives in the bundle and <prefix>/bin gets
  # symlinks to them (below), so a self update refreshes the CLI on PATH too.
  :
else
  $SUDO install -m 755 "$SRC/hydra" "$BIN_DIR/hydra"
  echo "installed $BIN_DIR/hydra"
  link_cli_alias
fi

# Man pages (releases >= 0.2.2 ship them in the tarball as man/*.1).
if [ -d "$SRC/man" ]; then
  MAN_DIR="$PREFIX/share/man/man1"
  $SUDO mkdir -p "$MAN_DIR"
  for m in "$SRC"/man/*.1; do
    $SUDO install -m 644 "$m" "$MAN_DIR/$(basename "$m")"
  done
  echo "installed man pages into $MAN_DIR (try: man hydra)"
fi

if [ "$MODE" = gui ]; then
  # The native-messaging host the browser manifests point at. On macOS that
  # is the copy inside the bundle: it carries the app's identity, which is
  # what TCC granted Downloads access to, and it survives a self update.
  HOST_BIN="$BIN_DIR/hydra-host"
  if [ -n "$APP" ]; then
    build_macos_app "$SRC" "$APP"
    HOST_BIN="$APP/Contents/MacOS/hydra-host"
    link_bin hydra "$APP/Contents/MacOS/hydra"
    link_cli_alias
    link_bin hydra-gui "$APP/Contents/MacOS/$APP_NAME"
    link_bin hydra-host "$APP/Contents/MacOS/hydra-host"
  else
    $SUDO install -m 755 "$SRC/hydra-gui" "$BIN_DIR/hydra-gui"
    $SUDO install -m 755 "$SRC/hydra-host" "$BIN_DIR/hydra-host"
    echo "installed $BIN_DIR/hydra-gui"
    echo "installed $BIN_DIR/hydra-host"
    # The self-update finisher: the GUI runs it from a temp copy, but keeping
    # one next to the app means `apply` refreshes it like any other binary.
    if [ -f "$SRC/hydra-updater" ]; then
      $SUDO install -m 755 "$SRC/hydra-updater" "$BIN_DIR/hydra-updater"
      echo "installed $BIN_DIR/hydra-updater"
    fi
  fi

  # Extensions + native-host installer keep the bundle layout (the script
  # resolves the bundle root as the parent of its own directory).
  $SUDO rm -rf "$SHARE_DIR"
  $SUDO mkdir -p "$SHARE_DIR"
  $SUDO cp -R "$SRC/extensions" "$SRC/scripts" "$SHARE_DIR/"
  echo "installed $SHARE_DIR (browser extensions + native-host installer)"

  if [ "$OS" = linux ]; then
    # Desktop integration goes into the user's data dir, and into the
    # prefix's when that is a system one: XDG resolves both and deduplicates
    # by file name, so the app appears once whether it was installed for one
    # user or for everyone.
    DATA_DIRS=("$USER_HOME/.local/share")
    case "$PREFIX" in
      "$USER_HOME"*) ;;
      *) if [ -d "$PREFIX" ] && { [ -w "$PREFIX" ] || [ -n "$SUDO" ]; }; then
           DATA_DIRS+=("$PREFIX/share")
         fi ;;
    esac

    # write_data MODE SRC DEST — install into one of those data dirs: with
    # sudo for a system one, handed back to the user for their own home.
    write_data() {
      local mode="$1" src="$2" dest="$3" sudo=""
      case "$dest" in "$USER_HOME"*) ;; *) sudo="$SUDO" ;; esac
      $sudo mkdir -p "$(dirname "$dest")"
      $sudo install -m "$mode" "$src" "$dest"
      case "$dest" in "$USER_HOME"*) own_user "$(dirname "$dest")" ;; esac
      echo "installed $dest"
    }

    # scale_icon SIZE OUT — the logo at SIZE, if this machine can resize it.
    scale_icon() {
      if command -v magick >/dev/null 2>&1; then
        magick "$SRC/logo.png" -resize "${1}x${1}" "$2" 2>/dev/null
      elif command -v convert >/dev/null 2>&1; then
        convert "$SRC/logo.png" -resize "${1}x${1}" "$2" 2>/dev/null
      elif python3 -c 'import PIL' 2>/dev/null; then
        python3 -c 'import sys
from PIL import Image
Image.open(sys.argv[1]).convert("RGBA").resize(
    (int(sys.argv[2]),) * 2, Image.LANCZOS).save(sys.argv[3])' \
          "$SRC/logo.png" "$1" "$2" 2>/dev/null
      else
        return 1
      fi
    }

    # The logo into the hicolor theme, so Icon=hydra resolves. Without it the
    # launcher, the dock and the switcher all draw the generic fallback icon.
    # The archive ships one 256px logo; the smaller sizes are rendered when a
    # scaler is around, because a 16px panel scaling 256px down itself looks
    # like it.
    if [ -f "$SRC/logo.png" ]; then
      for data in "${DATA_DIRS[@]}"; do
        write_data 644 "$SRC/logo.png" "$data/icons/hicolor/256x256/apps/hydra.png"
        for size in 16 24 32 48 64 128; do
          if scale_icon "$size" "$TMP/icon-${size}.png"; then
            write_data 644 "$TMP/icon-${size}.png" \
              "$data/icons/hicolor/${size}x${size}/apps/hydra.png"
          fi
        done
      done
    fi

    # Desktop launcher (the tarball ships no packaging metadata, so this is
    # written here rather than shipped). The basename must stay "hydra":
    # hydra-gui sets that as its window app id (Wayland app_id / X11
    # WM_CLASS), which is how the shell matches a running window back to this
    # entry to pick up Icon=. StartupWMClass repeats it for desktops that
    # only consult that key.
    cat > "$TMP/hydra.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Hydra Download Manager
GenericName=Download Manager
Comment=Multi-connection download accelerator
Exec=$BIN_DIR/hydra-gui
TryExec=$BIN_DIR/hydra-gui
Icon=hydra
Terminal=false
Categories=Network;FileTransfer;
Keywords=download;downloader;accelerator;http;
StartupNotify=true
StartupWMClass=hydra
EOF
    for data in "${DATA_DIRS[@]}"; do
      write_data 644 "$TMP/hydra.desktop" "$data/applications/hydra.desktop"
      command -v update-desktop-database >/dev/null 2>&1 &&
        update-desktop-database -q "$data/applications" 2>/dev/null || true
      command -v gtk-update-icon-cache >/dev/null 2>&1 &&
        gtk-update-icon-cache -qt "$data/icons/hicolor" 2>/dev/null || true
    done
  fi

  # Register the native-messaging host for the current user. Manifests land
  # in \$HOME, so this deliberately runs without sudo.
  if command -v python3 >/dev/null 2>&1; then
    run_as_user bash "$SHARE_DIR/scripts/install-native-host.sh" --no-build --host-bin "$HOST_BIN" \
      || echo "warning: native-messaging host registration failed; rerun: $SHARE_DIR/scripts/install-native-host.sh --no-build --host-bin $HOST_BIN" >&2
  else
    echo "note: python3 not found; to enable browser integration run:" >&2
    echo "  $SHARE_DIR/scripts/install-native-host.sh --no-build --host-bin $HOST_BIN" >&2
  fi

  # How to load the extension. The native-messaging side was just registered
  # above, but loading an unsigned build into the browser needs its developer
  # mode and cannot be automated — so spell out the steps here, with the real
  # installed paths, instead of leaving the user to find INSTALL.txt.
  EXT_DIR="$SHARE_DIR/extensions"
  XPI=""
  for f in "$EXT_DIR"/hydra-firefox-*.xpi; do
    if [ -f "$f" ]; then XPI="$f"; break; fi
  done
  # Releases before 0.3.8 shipped only the unpacked directories; Firefox's
  # temporary install accepts the manifest just the same.
  [ -n "$XPI" ] || XPI="$EXT_DIR/firefox/manifest.json"
  cat <<EOF

Browser extension — the builds in $EXT_DIR are unsigned,
so each browser loads them through its developer mode:

  Chrome / Edge / Brave / Opera / Vivaldi / Arc / Chromium
    1. open chrome://extensions (edge://extensions, brave://extensions, ...)
    2. turn on "Developer mode" (top right in Chrome; bottom left in Edge)
    3. click "Load unpacked" and select:
           $EXT_DIR/chrome

  Firefox — temporary (every edition; removed at the next restart)
    1. open about:debugging#/runtime/this-firefox
    2. click "Load Temporary Add-on..." and select:
           $XPI

  Firefox — permanent (Developer Edition, Nightly and ESR only)
    1. in about:config set xpinstall.signatures.required = false
    2. in about:addons use the gear icon > "Install Add-on From File..."
       and pick the .xpi in $EXT_DIR

Restart the browser once so it picks up Hydra's native-messaging manifest,
then check the Hydra toolbar icon: the status dot turns green when the
extension has reached the app.
EOF
  if [ -f "$EXT_DIR/INSTALL.txt" ]; then
    echo "Full instructions: $EXT_DIR/INSTALL.txt"
  fi
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: $BIN_DIR is not on your PATH" >&2 ;;
esac

echo "done."
