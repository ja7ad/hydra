#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Uninstall Hydra from Linux or macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/ja7ad/hydra/main/uninstall.sh | bash
#   ... | bash -s -- --purge          # also delete config/state (~/.config/hydra)
#   ... | bash -s -- --prefix ~/.local
#   ... | bash -s -- --app-dir ~/Applications   # macOS, if you installed there
#
# Removes everything install.sh created: hydra, the hya shorthand, hydra-gui,
# hydra-host and hydra-updater from <prefix>/bin (binaries or symlinks into
# the app bundle),
# <prefix>/share/hydra, the desktop launcher and icons, the autostart login
# item, and the native-messaging manifests. Also removes the macOS
# "Hydra Download Manager.app" from /Applications and ~/Applications. Config
# and state in ~/.config/hydra are kept unless --purge is given.

set -euo pipefail

PREFIX=""
APP_DIR=""
PURGE=0
REMOVED=0
APP_NAME="Hydra Download Manager"

# Everything per-user this removes — the desktop entry, the icons, the login
# item, the browser manifests — was written for the human, not for root, so
# `curl ... | sudo bash` has to look in their home rather than in /root.
# install.sh resolves it the same way.
if [ -n "${SUDO_USER:-}" ] && [ "${SUDO_USER}" != root ] && [ "$(id -u)" = 0 ]; then
  AS_USER="$SUDO_USER"
  USER_HOME=$(eval echo "~$SUDO_USER")
else
  AS_USER=""
  USER_HOME="$HOME"
fi

usage() {
  cat >&2 <<EOF
Usage: uninstall.sh [--purge] [--prefix DIR]

  --purge          also delete config/state/logs (~/.config/hydra)
  --prefix DIR     uninstall from DIR only (default: /usr/local and ~/.local)
  --app-dir DIR    macOS only: also look for "$APP_NAME.app" in DIR
                   (/Applications and ~/Applications are always checked)
EOF
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --purge) PURGE=1 ;;
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
  *) echo "error: unsupported OS $(uname -s) (use uninstall.ps1 on Windows)" >&2; exit 1 ;;
esac

# rm_path PATH — remove a file/dir, with sudo when it is not ours to delete.
rm_path() {
  local p="$1" SUDO=""
  [ -e "$p" ] || [ -L "$p" ] || return 0
  if [ ! -w "$(dirname "$p")" ] && command -v sudo >/dev/null 2>&1; then SUDO="sudo"; fi
  $SUDO rm -rf "$p"
  echo "removed $p"
  REMOVED=1
}

# Stop background instances so binaries and manifests are not in use. On macOS
# the GUI runs as "Hydra Download Manager" — its bundle executable — so
# pkill -x hydra-gui alone would leave the app running and its files busy.
# Ask it to quit first: that lets it persist its state instead of losing it.
if [ "$(uname -s)" = Darwin ] && pgrep -f "$APP_NAME.app/Contents/MacOS/" >/dev/null 2>&1; then
  # Only when it is actually running: AppleScript will happily LAUNCH a
  # registered app in order to quit it.
  if [ -n "$AS_USER" ]; then
    sudo -u "$AS_USER" -H osascript -e "quit app \"$APP_NAME\"" 2>/dev/null || true
  else
    osascript -e "quit app \"$APP_NAME\"" 2>/dev/null || true
  fi
  sleep 1
  pkill -f "$APP_NAME.app/Contents/MacOS/" 2>/dev/null || true
fi
for p in hydra-gui hydra-host; do pkill -x "$p" 2>/dev/null || true; done

# deb/rpm installs are owned by the package manager; don't reach into /usr.
if [ "$OS" = linux ]; then
  if command -v dpkg >/dev/null 2>&1 && dpkg -s hydra-download-manager >/dev/null 2>&1; then
    echo "note: hydra was installed as a package; also run: sudo apt remove hydra-download-manager" >&2
  elif command -v rpm >/dev/null 2>&1 && rpm -q hydra-download-manager >/dev/null 2>&1; then
    echo "note: hydra was installed as a package; also run: sudo dnf remove hydra-download-manager" >&2
  fi
fi

# Homebrew installs are managed by brew.
if command -v brew >/dev/null 2>&1; then
  if brew list --formula hydra >/dev/null 2>&1 || brew list --formula ja7ad/tap/hydra >/dev/null 2>&1; then
    echo "note: hydra CLI was installed via Homebrew; to remove run: brew uninstall hydra" >&2
  fi
  if brew list --cask hydra >/dev/null 2>&1 || brew list --cask ja7ad/tap/hydra >/dev/null 2>&1; then
    echo "note: hydra app was installed via Homebrew Cask; to remove run: brew uninstall --cask hydra" >&2
  fi
fi

# Man pages, by exact name: a glob like hydra*.1 could hit the unrelated
# THC hydra(1) if it shares the prefix.
MAN_PAGES=(hydra.1 hya.1 hydra-interactive.1 hydra-checksum.1 hydra-parity.1
           hydra-formats.1 hydra-bench.1 hydra-completions.1 hydra-host.1)

# rm_alias PATH TARGET — remove the hya shorthand, but only while it is still
# the symlink install.sh made. Someone else's `hya` on the same prefix keeps
# the name; that is the whole point of installing it non-destructively.
rm_alias() {
  [ -L "$1" ] || return 0
  case "$(readlink "$1")" in
    hydra|"$2") rm_path "$1" ;;
    *) ;;
  esac
}

# Binaries + share dir + man pages from the tarball install.
if [ -n "$PREFIX" ]; then PREFIXES=("$PREFIX"); else PREFIXES=(/usr/local "$USER_HOME/.local"); fi
for prefix in "${PREFIXES[@]}"; do
  rm_alias "$prefix/bin/hya" "$prefix/bin/hydra"
  rm_path "$prefix/bin/hydra"
  rm_path "$prefix/bin/hydra-gui"
  rm_path "$prefix/bin/hydra-host"
  rm_path "$prefix/bin/hydra-updater"
  rm_path "$prefix/share/hydra"
  # System-wide desktop integration, when install.sh had write access to the
  # prefix (the per-user copies are handled further down).
  if [ "$OS" = linux ]; then
    rm_path "$prefix/share/applications/hydra.desktop"
    for size in 16 24 32 48 64 128 256 512; do
      rm_path "$prefix/share/icons/hicolor/${size}x${size}/apps/hydra.png"
    done
  fi
  for m in "${MAN_PAGES[@]}"; do
    rm_path "$prefix/share/man/man1/$m"
    rm_path "$prefix/share/man/man1/$m.gz"
  done
done

if [ "$OS" = macos ]; then
  # /Applications (dmg, pkg, install.sh default), the per-user fallback
  # install.sh uses when that is not writable, and any --app-dir given here.
  APP_DIRS=(/Applications "$USER_HOME/Applications")
  if [ -n "$APP_DIR" ]; then APP_DIRS+=("$APP_DIR"); fi
  LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
  for dir in "${APP_DIRS[@]}"; do
    app="$dir/$APP_NAME.app"
    [ -d "$app" ] || continue
    # Drop the Launch Services record before the bundle goes: otherwise the
    # app lingers in Spotlight and "Open With" pointing at nothing.
    if [ -x "$LSREGISTER" ]; then
      "$LSREGISTER" -u "$app" >/dev/null 2>&1 || true
    fi
    rm_path "$app"
  done
  # .pkg installs: bundled extensions + the installer receipt.
  rm_path "/Library/Application Support/Hydra"
  if pkgutil --pkg-info io.github.ja7ad.hydra >/dev/null 2>&1; then
    if command -v sudo >/dev/null 2>&1; then
      sudo pkgutil --forget io.github.ja7ad.hydra >/dev/null 2>&1 || true
      echo "removed pkg receipt io.github.ja7ad.hydra"
    fi
  fi
  # Login item (the "Launch on startup" toggle writes this LaunchAgent).
  launchctl bootout "gui/$(id -u "${AS_USER:-$(id -un)}")/io.github.ja7ad.hydra" 2>/dev/null || true
  rm_path "$USER_HOME/Library/LaunchAgents/io.github.ja7ad.hydra.plist"
else
  rm_path "$USER_HOME/.local/share/applications/hydra.desktop"
  rm_path "$USER_HOME/.config/autostart/hydra.desktop"
  # install.sh renders the logo at every hicolor size it can.
  for size in 16 24 32 48 64 128 256 512; do
    rm_path "$USER_HOME/.local/share/icons/hicolor/${size}x${size}/apps/hydra.png"
  done
fi

# Native-messaging manifests (per-user; mirrors install-native-host.sh).
if [ "$OS" = macos ]; then
  MANIFEST_DIRS=(
    "$USER_HOME/Library/Application Support/Google/Chrome/NativeMessagingHosts"
    "$USER_HOME/Library/Application Support/Chromium/NativeMessagingHosts"
    "$USER_HOME/Library/Application Support/Microsoft Edge/NativeMessagingHosts"
    "$USER_HOME/Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    "$USER_HOME/Library/Application Support/Vivaldi/NativeMessagingHosts"
    "$USER_HOME/Library/Application Support/Arc/User Data/NativeMessagingHosts"
    "$USER_HOME/Library/Application Support/Mozilla/NativeMessagingHosts"
    "$USER_HOME/Library/Application Support/LibreWolf/NativeMessagingHosts"
  )
else
  MANIFEST_DIRS=(
    "$USER_HOME/.config/google-chrome/NativeMessagingHosts"
    "$USER_HOME/.config/chromium/NativeMessagingHosts"
    "$USER_HOME/.config/microsoft-edge/NativeMessagingHosts"
    "$USER_HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    "$USER_HOME/.config/vivaldi/NativeMessagingHosts"
    "$USER_HOME/.mozilla/native-messaging-hosts"
    "$USER_HOME/.librewolf/native-messaging-hosts"
  )
fi
for d in "${MANIFEST_DIRS[@]}"; do
  rm_path "$d/com.hydra.host.json"
done

if [ "$PURGE" = 1 ]; then
  rm_path "$USER_HOME/.config/hydra"
else
  [ -d "$USER_HOME/.config/hydra" ] \
    && echo "kept $USER_HOME/.config/hydra (config/state/logs); rerun with --purge to delete it"
fi

if [ "$REMOVED" = 1 ]; then
  echo "done."
else
  echo "nothing to remove; hydra does not appear to be installed."
fi
