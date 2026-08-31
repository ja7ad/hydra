#!/usr/bin/env bash
# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the distributable browser extensions: a packed .xpi for Firefox, a
# packed .zip for the Chromium family (Chrome, Edge, Opera, Brave, Vivaldi,
# Arc, Chromium), and an unpacked copy of each next to them.
#
#   scripts/build-extensions.sh [all|chrome|firefox] [options]
#
#     --out DIR      write the artifacts here (default: target/extensions).
#                    Package scripts point this at their staging directory so
#                    the SAME files land inside the installed application.
#     --sign         after building, submit the Firefox add-on to AMO with
#                    web-ext and fetch a signed .xpi (needs WEB_EXT_API_KEY /
#                    WEB_EXT_API_SECRET). Everything else here is unsigned.
#     --crx          also pack a CRX3 for the Chromium family, generating a
#                    signing key at target/hydra-chrome-crx.pem if there is
#                    none. Without this flag a .crx is packed only when a key
#                    is already resolvable (--crx-key or $HYDRA_CRX_KEY or
#                    that default path), so a machine without the project key
#                    never ships one signed by a stranger.
#     --crx-key PEM  the RSA private key to sign the .crx with. It MUST be the
#                    key behind the `key` field pinned in the Chromium
#                    manifest, or the extension id changes — see below.
#     --store        also pack a key-less Chromium zip for uploading to Chrome
#                    Web Store / Edge Add-ons — see below for why.
#     --print-help   print the install instructions and exit, without
#                    building. --prefix DIR spells the paths as they will be
#                    on the target machine.
#     --prefix DIR   the directory the artifacts will live in ON THE USER'S
#                    machine, used only for the paths in INSTALL.txt
#                    (default: the value of --out).
#     --quiet        suppress the install help on stdout.
#
# Output layout — flat on purpose, so a packaging script can drop the whole
# directory into the application root as-is:
#
#     <out>/hydra-chrome-<version>.zip           packed, Chromium family
#     <out>/hydra-chrome-<version>.crx           signed CRX3, Chromium family
#     <out>/hydra-chrome-<version>-webstore.zip  packed, key-less (--store only)
#     <out>/hydra-firefox-<version>.xpi          packed, Firefox
#     <out>/chrome/                              the same build, unpacked
#     <out>/firefox/                             the same build, unpacked
#     <out>/INSTALL.txt                          install help for these UNSIGNED builds
#
# Both packed files are plain zip archives with manifest.json at the ROOT —
# a wrapper directory makes Firefox reject the .xpi outright and makes the
# Chromium "Load unpacked" pick the wrong folder. `version` comes from the
# manifest (the extension versions independently of the app).
#
# The three Chromium shapes are for three different jobs. The unpacked
# directory is the only one a person can install by hand, because Chromium
# stopped accepting a dragged-in .crx from outside the Web Store; the .zip is
# what the Web Store itself consumes; the .crx is what an enterprise policy
# deployment (ExtensionSettings / ExtensionInstallForcelist against a
# self-hosted update manifest) serves, and it is the only one that carries a
# signature at all.
#
# The pinned `key` in the Chromium manifest is what fixes the extension id
# (jpnonmbbkjdpeebdhkjoliklfhkdcomj) that the native messaging host
# allow-lists, so it is kept in the .zip, the .crx and the unpacked copy.
# Chrome Web Store and Edge Add-ons both assign their own id on publish, and
# Edge's manifest validator rejects a `key` field outright ("The manifest
# shouldn't contain the key field"), so --store packs a separate
# *-webstore.zip with `key` stripped — that is the one to upload to either
# store; the other artifacts keep the pin for native messaging. Chromium
# requires a .crx to be signed by exactly that key: signing with any other
# one is still packed, but the manifest key is rewritten to
# match the signer (otherwise the browser rejects the file outright) and the
# id changes with it. The WebSocket transport does not care — it accepts any
# extension origin — but the native messaging fallback, which is also the
# only way the browser can LAUNCH Hydra, is registered against the pinned id
# and would need re-registering. The script says so, loudly, when it happens.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CHROME_SRC="$REPO/extensions/chrome"
FIREFOX_SRC="$REPO/extensions/firefox"

TARGETS=all
OUT=""
PREFIX=""
SIGN=0
CRX=auto
CRX_KEY="${HYDRA_CRX_KEY:-}"
STORE=0
PRINT_HELP=0
QUIET=0

while [ $# -gt 0 ]; do
  case "$1" in
    all | chrome | firefox) TARGETS=$1 ;;
    --out) OUT="${2:?--out needs a directory}"; shift ;;
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift ;;
    --sign) SIGN=1 ;;
    --crx) CRX=1 ;;
    --no-crx) CRX=0 ;;
    --crx-key) CRX_KEY="${2:?--crx-key needs a .pem file}"; shift ;;
    --store) STORE=1 ;;
    --print-help) PRINT_HELP=1 ;;
    --quiet) QUIET=1 ;;
    -h | --help)
      # The header block down to the last option, so this cannot drift out of
      # range when an option is added.
      sed -n '5,/--quiet/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

OUT="${OUT:-$REPO/target/extensions}"
PREFIX="${PREFIX:-$OUT}"
# Never inside $OUT: that directory is copied wholesale into installers and
# release archives, and a private key must not travel with them.
CRX_KEY="${CRX_KEY:-$REPO/target/hydra-chrome-crx.pem}"

# Windows (git bash, where the NSIS installer is packed) has `python` but not
# always `python3`; every other host has both.
PY=$(command -v python3 || command -v python || true)
[ -n "$PY" ] || { echo "error: python3 is required to read the manifests" >&2; exit 1; }

# Python on Windows writes CRLF on stdout, and command substitution strips
# only the trailing newline - the \r stays glued to the value, where it
# corrupts file names ("hydra-chrome-0.2.2\r.zip") and silently breaks every
# exact string comparison. Everything read back from python goes through here.
py() { "$PY" "$@" | tr -d '\r'; }

manifest_version() { # <manifest.json>
  py -c "import json,sys;print(json.load(open(sys.argv[1]))['version'])" "$1"
}

# zip via whatever the host has. Info-ZIP is absent from git bash, so python's
# zipfile is the portable fallback — and it is the only one of the three that
# is deterministic about entry order, which keeps rebuilt archives comparable.
make_zip() { # <archive> <source-dir>
  local archive=$1 dir=$2
  rm -f "$archive"
  py - "$archive" "$dir" <<'EOF'
import os, sys, zipfile

archive, root = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
    for base, dirs, files in os.walk(root):
        dirs.sort()
        for name in sorted(files):
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root).replace(os.sep, "/")
            z.write(path, rel)
EOF
}

# The packed archive must be loadable on its own, so anything that only makes
# sense in the repository is left out.
copy_unpacked() { # <src-dir> <dst-dir>
  local src=$1 dst=$2
  rm -rf "$dst"
  mkdir -p "$dst"
  cp -R "$src/." "$dst/"
  rm -f "$dst/README.md"
  # -mindepth 1 so a dot in the destination's own path can never match, and
  # -prune so a dot-directory is removed whole rather than descended into.
  find "$dst" -mindepth 1 \( -name '.*' -o -name '__MACOSX' \) -prune -exec rm -rf {} +
  # Cheap tripwire: every later step (packing, verification, "Load unpacked")
  # depends on this copy having landed, and a platform that quietly copied
  # nothing should say so here rather than three steps downstream.
  [ -f "$dst/manifest.json" ] ||
    { echo "error: copying $src to $dst produced no manifest.json" >&2; exit 1; }
}

# manifest.json has to be a FIRST-LEVEL entry (Firefox rejects the add-on
# otherwise) and the rest of these are what the manifest actually references —
# a sync step that silently did nothing would otherwise ship an empty shell.
verify_archive() { # <archive>
  local archive=$1 entries required
  entries=$(py -c "
import sys, zipfile
print('\n'.join(zipfile.ZipFile(sys.argv[1]).namelist()))" "$archive")
  for required in manifest.json background.js content.js popup.html popup.js \
                  popup.css welcome.html welcome.js icons/icon48.png; do
    case $'\n'"$entries"$'\n' in
      *$'\n'"$required"$'\n'*) ;;
      *)
        echo "error: '$required' missing from the root of $(basename "$archive")" >&2
        exit 1
        ;;
    esac
  done
}

# --- CRX3 --------------------------------------------------------------------
# A .crx is the zip with a signed header glued in front:
#
#     "Cr24" | version=3 | header length | CrxFileHeader | zip
#
# where CrxFileHeader carries the signer's public key, an RSA-SHA256
# signature, and a SignedData block holding the 16-byte crx id. The signature
# covers "CRX3 SignedData\0", the SignedData length, SignedData itself, and
# the whole zip — so neither the payload nor the declared id can be swapped
# afterwards. Signing is openssl's job (python has no RSA in the standard
# library, and every host that can build Hydra has openssl).

# Base64 of the signing key's public key in DER — the exact form the manifest
# `key` field takes, so it can be compared with the pinned one directly.
crx_public_key() { # <key.pem>
  openssl rsa -in "$1" -pubout -outform DER 2>/dev/null | openssl base64 -A | tr -d '\r'
}

pack_crx() { # <key.pem> <zip> <out.crx>
  py - "$1" "$2" "$3" <<'EOF'
import hashlib, struct, subprocess, sys

key, zip_path, out = sys.argv[1], sys.argv[2], sys.argv[3]


def varint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)


def field(number, payload):  # length-delimited protobuf field
    return varint(number << 3 | 2) + varint(len(payload)) + payload


def openssl(args, stdin=None):
    # stderr is captured, not inherited: `openssl rsa` chats ("writing RSA
    # key") on success, and that noise is only worth showing when it failed.
    p = subprocess.run(["openssl", *args], input=stdin,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if p.returncode != 0:
        sys.exit(f"error: openssl {' '.join(args)} failed\n{p.stderr.decode(errors='replace')}")
    return p.stdout


pub = openssl(["rsa", "-in", key, "-pubout", "-outform", "DER"])
payload = open(zip_path, "rb").read()

# SignedData { crx_id = first 16 bytes of SHA-256(public key) }
signed_header = field(1, hashlib.sha256(pub).digest()[:16])

signature = openssl(
    ["dgst", "-sha256", "-sign", key],
    b"CRX3 SignedData\x00" + struct.pack("<I", len(signed_header)) + signed_header + payload,
)

# CrxFileHeader { sha256_with_rsa = [{public_key, signature}], signed_header_data }
header = field(2, field(1, pub) + field(2, signature)) + field(10000, signed_header)

with open(out, "wb") as f:
    f.write(b"Cr24" + struct.pack("<II", 3, len(header)) + header + payload)
EOF
}

# Chromium spells an extension id as the first 16 bytes of the SHA-256 of the
# public key, each nibble mapped 0-f -> a-p.
crx_id() { # <base64 public key>
  py -c "
import base64, hashlib, sys
d = hashlib.sha256(base64.b64decode(sys.argv[1])).hexdigest()[:32]
print(''.join(chr(ord('a') + int(c, 16)) for c in d))" "$1"
}

# --- install help -------------------------------------------------------------
# One text, written both to <out>/INSTALL.txt and to stdout, and reused by the
# packaging scripts (--print-help) so the instructions inside a .deb, a .app
# and a tarball cannot drift apart. $1 is the directory the files will be in
# on the user's machine.
install_help() { # <dir-on-target> [chrome-zip] [firefox-xpi] [chrome-crx] [crx-id-if-not-pinned]
  local dir=$1 zip=${2:-hydra-chrome-<version>.zip} xpi=${3:-hydra-firefox-<version>.xpi}
  local crx=${4:-} crx_id=${5:-}
  cat <<EOF
Hydra browser extension — installing the unsigned build
=======================================================

These are UNSIGNED builds: they carry no Chrome Web Store or
addons.mozilla.org signature, so each browser needs its developer mode (or a
temporary install) to accept them. The extension itself is identical to the
store build.

The paths below are where Hydra installs these files; if you are reading
this from an archive you extracted yourself, use the directory this file is
in instead.

In this directory:

  $zip
      packed build for the Chromium family (Web Store upload format)
${crx:+  $crx
      the same build signed as a CRX3, for policy deployment
}  $xpi
      packed build for Firefox
  chrome/
      the same Chromium build, already unpacked and ready to load
  firefox/
      the same Firefox build, already unpacked
  INSTALL.txt
      this file

Chrome, Edge, Opera, Brave, Vivaldi, Arc, Chromium
--------------------------------------------------
Chromium only installs a .crx that came from the Web Store, so an unsigned
build is loaded from the unpacked directory instead. (The .zip is the same
build for a store upload — to use it here, unzip it somewhere permanent and
point step 3 at that folder.)

  1. Open the extensions page:
         Chrome   chrome://extensions
         Edge     edge://extensions
         Opera    opera://extensions
         Brave    brave://extensions
         Vivaldi  vivaldi://extensions
         Arc      arc://extensions
  2. Turn on "Developer mode" — top right in Chrome, Brave, Vivaldi and Arc;
     bottom left in Edge; the sidebar in Opera.
  3. Click "Load unpacked" and select:
         $dir/chrome
  4. Leave that folder where it is. The browser re-reads it from disk at every
     start, and moving or deleting it uninstalls the extension.
${crx:+
The signed .crx is for deployment by enterprise policy (ExtensionSettings or
ExtensionInstallForcelist, pointing at an update manifest you host).
Dragging it onto the extensions page will not work — Chromium refuses any
.crx that did not come from the Web Store.
}${crx_id:+
Note that this .crx was signed with a key of your own, not the project
signing key, so it installs as $crx_id
instead of the pinned id below, and the Hydra native messaging host will
not recognise it. Downloads still reach Hydra over the WebSocket, but the
browser cannot launch the app for this build. The unpacked directory and
the .zip are unaffected.
}
The extension id is jpnonmbbkjdpeebdhkjoliklfhkdcomj in every Chromium
browser — it is pinned by the manifest key, and it is the id Hydra's native
messaging host already allows, so nothing else needs configuring.

Firefox
-------
Temporary — works in every Firefox, and is removed at the next restart:

  1. Open about:debugging#/runtime/this-firefox
  2. Click "Load Temporary Add-on..." and select:
         $dir/$xpi
     (or $dir/firefox/manifest.json)

Permanent — Developer Edition, Nightly and ESR only:

  1. Open about:config and set
         xpinstall.signatures.required = false
     Release and Beta Firefox ignore this setting and will keep refusing an
     unsigned add-on; use the temporary install above, or a signed build.
  2. Open about:addons, click the gear icon, choose
     "Install Add-on From File..." and select the .xpi above.

After installing
----------------
Restart the browser once so it picks up Hydra's native-messaging manifest,
then open the Hydra toolbar icon: the status dot is green when the extension
has reached the app (WebSocket 127.0.0.1:6799).

If the dot stays red, this browser has no native-messaging manifest for
Hydra. The DMG/PKG, .deb/.rpm and Windows installers register Chrome, Edge,
Brave, Vivaldi, Chromium and Firefox for you — for anything else (or an
install from a plain archive) run the registration script that ships with
Hydra:
    macOS / Linux   scripts/install-native-host.sh
    Windows         scripts\\install-native-host.ps1
EOF
}

if [ "$PRINT_HELP" = 1 ]; then
  install_help "$PREFIX"
  exit 0
fi

# --- build --------------------------------------------------------------------
mkdir -p "$OUT"
CHROME_ZIP=""
CHROME_CRX=""
CRX_ID_MISMATCH=""
FIREFOX_XPI=""

if [ "$TARGETS" = all ] || [ "$TARGETS" = chrome ]; then
  VERSION=$(manifest_version "$CHROME_SRC/manifest.json")
  CHROME_ZIP="hydra-chrome-$VERSION.zip"
  copy_unpacked "$CHROME_SRC" "$OUT/chrome"
  # Packed from the unpacked copy, not from the source tree: whatever the
  # archive holds is then exactly what "Load unpacked" on the sibling
  # directory loads.
  make_zip "$OUT/$CHROME_ZIP" "$OUT/chrome"
  verify_archive "$OUT/$CHROME_ZIP"
  echo "built: ${OUT#"$REPO/"}/$CHROME_ZIP  (unpacked: ${OUT#"$REPO/"}/chrome)"

  if [ "$STORE" = 1 ]; then
    # Chrome Web Store and Edge Add-ons both assign their own extension id on
    # publish; Edge's validator hard-rejects a manifest that still carries
    # the pinned `key`, so this is a separate copy with it removed.
    STORE_STAGE="$OUT/.store-stage"
    copy_unpacked "$OUT/chrome" "$STORE_STAGE"
    py -c "
import json, sys
path = sys.argv[1]
m = json.load(open(path))
m.pop('key', None)
json.dump(m, open(path, 'w'), indent=2)" "$STORE_STAGE/manifest.json"
    CHROME_WEBSTORE_ZIP="hydra-chrome-$VERSION-webstore.zip"
    make_zip "$OUT/$CHROME_WEBSTORE_ZIP" "$STORE_STAGE"
    verify_archive "$OUT/$CHROME_WEBSTORE_ZIP"
    rm -rf "$STORE_STAGE"
    echo "built: ${OUT#"$REPO/"}/$CHROME_WEBSTORE_ZIP  (upload this one to Chrome Web Store / Edge Add-ons)"
  fi

  # A .crx needs a signing key. With --crx one is generated when missing;
  # otherwise it is packed only if a key is already there, so a machine
  # without the project key produces no misleadingly-signed artifact.
  if [ "$CRX" = auto ] && [ -f "$CRX_KEY" ]; then
    CRX=1
  elif [ "$CRX" = auto ]; then
    CRX=0
    echo "note: no signing key at ${CRX_KEY#"$REPO/"} — skipped the .crx (pass --crx to generate one)"
  fi

  if [ "$CRX" = 1 ]; then
    command -v openssl >/dev/null 2>&1 ||
      { echo "error: openssl is required to sign a .crx" >&2; exit 1; }
    if [ ! -f "$CRX_KEY" ]; then
      mkdir -p "$(dirname "$CRX_KEY")"
      # 2048-bit RSA: what Chromium's own packer produces, and the smallest
      # size current Chromium accepts.
      openssl genrsa -out "$CRX_KEY" 2048 >/dev/null 2>&1
      chmod 600 "$CRX_KEY"
      echo "generated signing key: ${CRX_KEY#"$REPO/"}  (keep it — it is the extension's identity)"
    fi
    SIGNER_KEY=$(crx_public_key "$CRX_KEY")
    [ -n "$SIGNER_KEY" ] || { echo "error: $CRX_KEY is not an RSA private key" >&2; exit 1; }
    SIGNER_ID=$(crx_id "$SIGNER_KEY")
    PINNED_KEY=$(py -c "
import json,sys;print(json.load(open(sys.argv[1])).get('key',''))" "$CHROME_SRC/manifest.json")
    [ -n "$PINNED_KEY" ] ||
      { echo "error: extensions/chrome/manifest.json has no pinned 'key'" >&2; exit 1; }

    CRX_STAGE="$OUT/.crx-stage"
    copy_unpacked "$OUT/chrome" "$CRX_STAGE"
    if [ "$SIGNER_KEY" != "$PINNED_KEY" ]; then
      # Chromium rejects a .crx whose manifest key is not the signer's, so
      # the manifest has to follow the key it was actually signed with.
      py -c "
import json,sys
path = sys.argv[1]
m = json.load(open(path))
m['key'] = sys.argv[2]
json.dump(m, open(path, 'w'), indent=2)" "$CRX_STAGE/manifest.json" "$SIGNER_KEY"
    fi
    make_zip "$CRX_STAGE.zip" "$CRX_STAGE"
    CHROME_CRX="hydra-chrome-$VERSION.crx"
    pack_crx "$CRX_KEY" "$CRX_STAGE.zip" "$OUT/$CHROME_CRX"
    rm -rf "$CRX_STAGE" "$CRX_STAGE.zip"
    echo "built: ${OUT#"$REPO/"}/$CHROME_CRX  (id: $SIGNER_ID)"
    if [ "$SIGNER_KEY" != "$PINNED_KEY" ]; then
      CRX_ID_MISMATCH=$SIGNER_ID
      cat >&2 <<WARN

warning: the .crx was signed with a key that is NOT the one pinned in
         extensions/chrome/manifest.json, so it installs as
             $SIGNER_ID
         instead of $(crx_id "$PINNED_KEY").
         Downloads still reach Hydra over the WebSocket, but the native
         messaging host is registered for the pinned id only: it cannot
         launch Hydra for this build, and the .crx cannot be an update of
         the official extension. The unpacked directory and the .zip are
         unaffected — they keep the pinned id.
         Sign with the project key (--crx-key) for a releasable .crx.

WARN
    fi
  fi
fi

if [ "$TARGETS" = all ] || [ "$TARGETS" = firefox ]; then
  # extensions/chrome is the single source of truth for the shared code; only
  # the manifest is Firefox's own.
  "$REPO/scripts/sync-extension-resources.sh" firefox >/dev/null
  VERSION=$(manifest_version "$FIREFOX_SRC/manifest.json")
  FIREFOX_XPI="hydra-firefox-$VERSION.xpi"
  copy_unpacked "$FIREFOX_SRC" "$OUT/firefox"
  make_zip "$OUT/$FIREFOX_XPI" "$OUT/firefox"
  verify_archive "$OUT/$FIREFOX_XPI"
  echo "built: ${OUT#"$REPO/"}/$FIREFOX_XPI  (unpacked: ${OUT#"$REPO/"}/firefox)"
fi

install_help "$PREFIX" "${CHROME_ZIP:-}" "${FIREFOX_XPI:-}" "${CHROME_CRX:-}" \
  "${CRX_ID_MISMATCH:-}" > "$OUT/INSTALL.txt"

if [ "$SIGN" = 1 ]; then
  [ -n "$FIREFOX_XPI" ] ||
    { echo "error: --sign signs the Firefox add-on; build it too (drop the 'chrome' argument)" >&2; exit 2; }
  command -v web-ext >/dev/null 2>&1 ||
    { echo "error: web-ext not found — install it with: npm i -g web-ext" >&2; exit 1; }
  : "${WEB_EXT_API_KEY:?set WEB_EXT_API_KEY (https://addons.mozilla.org/developers/addon/api/key/)}"
  : "${WEB_EXT_API_SECRET:?set WEB_EXT_API_SECRET}"
  # `unlisted` = self-distribution: signed for a permanent install in release
  # Firefox, but not published on addons.mozilla.org.
  web-ext sign --source-dir "$OUT/firefox" --artifacts-dir "$OUT" --channel unlisted
  echo "Signed .xpi written to ${OUT#"$REPO/"} — install that one by dragging it into Firefox."
fi

if [ "$QUIET" = 0 ]; then
  echo
  cat "$OUT/INSTALL.txt"
fi
