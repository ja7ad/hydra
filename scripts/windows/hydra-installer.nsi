; Copyright (C) 2026 Javad Rajabzadeh
; SPDX-License-Identifier: GPL-3.0-or-later
;
; NSIS installer for Hydra Download Manager on Windows.
;
; Compile from THIS directory (paths below are relative to it):
;
;   makensis -DARCH=x64   hydra-installer.nsi
;   makensis -DARCH=arm64 hydra-installer.nsi
;   makensis -DVERSION=0.3.0 -DBUILD_DIR=..\..\target\custom\release hydra-installer.nsi
;
; Expects hydra-gui.exe, hydra-host.exe, and hydra.exe already built in
; BUILD_DIR (default: target\<rust-triple>\release for the chosen ARCH), and
; the browser extensions packed into target\extensions by
; scripts/build-extensions.sh.
; Cross-build from macOS with scripts/build-windows-installer.sh, or
; natively with `cargo build --release -p hya-gui -p hya-host -p hya-cli`.
;
; Installs per-user (no elevation), mirroring scripts/install-native-host.ps1:
;   * Hydra Download Manager  - the GUI app + logo + shortcuts
;   * IPC host                - hydra-host.exe, native-messaging manifests,
;                               and the HKCU registry keys the browsers read
;   * CLI (hydra.exe + the   - with $INSTDIR appended to the user PATH so
;     hya.exe short name)      all binaries resolve in cmd/PowerShell
;   * Browser extensions      - packed .zip/.xpi + unpacked chrome/ and
;                               firefox/, with INSTALL.txt instructions
;
; Windows has no NativeMessagingHosts directory: each browser reads a
; registry value pointing at a manifest file. Chromium browsers key it by
; extension origin, Firefox by add-on id, so two manifests are written. The
; host is only the FALLBACK transport (and the only thing that can start
; Hydra when it is not running) - day to day the extension talks to the app
; over ws://127.0.0.1:6799, which needs no registration at all.

Unicode true
SetCompressor /SOLID lzma

;--------------------------------
; Defines

!define APP_NAME     "Hydra Download Manager"
!define PUBLISHER    "Javad Rajabzadeh"
!define HOMEPAGE     "https://github.com/ja7ad/hydra"
!define HOST_NAME    "com.hydra.host"
; Chromium id is derived from the pinned key in extensions/chrome/manifest.json
; (first 16 bytes of SHA-256 of the decoded key, mapped a-p), so it can never
; drift as long as the key stays pinned. Recompute if the key ever changes:
;   python3 -c "import json,hashlib,base64;k=json.load(open('extensions/chrome/manifest.json'))['key'];h=hashlib.sha256(base64.b64decode(k)).digest()[:16];print(''.join(chr(97+(b>>4))+chr(97+(b&15)) for b in h))"
!define CHROME_EXT_ID  "jpnonmbbkjdpeebdhkjoliklfhkdcomj"
!define FIREFOX_EXT_ID "hydra@ja7ad.github.io"

!ifndef VERSION
  ; Falls back to the workspace product version ([workspace.package] in
  ; ../../Cargo.toml) so this never drifts out of sync when no -DVERSION is
  ; passed on the command line. Same grep+cut extraction as
  ; .github/workflows/release.yml; piped through a tempfile (rather than
  ; !searchparse /file) because Cargo.toml's UTF-8 comments (em dashes) trip
  ; NSIS's "Bad text encoding" check when read directly, and NSIS strips
  ; backslashes from !system strings so a sed capture-group regex can't be
  ; used inline.
  !tempfile VERSIONFILE
  !system `grep -m1 "^version = " "../../Cargo.toml" | cut -d'"' -f2 > "${VERSIONFILE}"`
  !define /file VERSION "${VERSIONFILE}"
  !delfile "${VERSIONFILE}"
!endif
; VIProductVersion accepts only x.x.x.x numerics, so a pre-release VERSION
; (0.3.0-rc1) passes its numeric part separately; display strings keep the
; full version.
!ifndef NUM_VERSION
  !define NUM_VERSION "${VERSION}"
!endif
!ifndef ARCH
  !define ARCH "x64"
!endif
; "amd64" (or anything else non-arm64) maps to the x86_64 target below.
!if "${ARCH}" == "arm64"
  !define RUST_TARGET "aarch64-pc-windows-msvc"
!else
  !define RUST_TARGET "x86_64-pc-windows-msvc"
!endif
!ifndef BUILD_DIR
  !define BUILD_DIR "..\..\target\${RUST_TARGET}\release"
!endif

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Hydra"

Name "${APP_NAME}"
OutFile "..\..\target\hydra-${VERSION}-windows-${ARCH}-setup.exe"

; Per-user install: needs no elevation, and matches the HKCU-only
; native-messaging registration (a per-machine install would need HKLM keys
; for every browser AND still only cover one user's extension profile).
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Hydra"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"

VIProductVersion "${NUM_VERSION}.0"
VIAddVersionKey "ProductName"     "${APP_NAME}"
VIAddVersionKey "ProductVersion"  "${VERSION}"
VIAddVersionKey "FileVersion"     "${VERSION}"
VIAddVersionKey "FileDescription" "${APP_NAME} Setup"
VIAddVersionKey "CompanyName"     "${PUBLISHER}"
VIAddVersionKey "LegalCopyright"  "(C) 2026 ${PUBLISHER}. GPL-3.0-or-later."

;--------------------------------
; Includes

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "WinMessages.nsh"
!include "StrFunc.nsh"
${Using:StrFunc} StrRep
${Using:StrFunc} StrStr
${Using:StrFunc} UnStrRep

;--------------------------------
; UI

!define MUI_ICON   "hydra.ico"
!define MUI_UNICON "hydra.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\hydra-gui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APP_NAME}"
!define MUI_FINISHPAGE_SHOWREADME "$INSTDIR\extensions\INSTALL.txt"
!define MUI_FINISHPAGE_SHOWREADME_TEXT "How to load the browser extension"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

;--------------------------------
; Sections

Section "Hydra Download Manager" SEC_GUI
  SectionIn RO  ; the app itself is not optional

  ; Replacing a running exe fails with "file in use"; the GUI holds a
  ; single-instance guard anyway, so stop it (and the host) first.
  nsExec::Exec 'taskkill /F /IM hydra-gui.exe'
  nsExec::Exec 'taskkill /F /IM hydra-host.exe'
  Pop $0

  SetOutPath "$INSTDIR"
  File "${BUILD_DIR}\hydra-gui.exe"
  File "hydra.ico"
  File "..\..\LICENSE"

  ; Start-menu shortcut. hydra-gui.exe embeds hydra.ico + VERSIONINFO
  ; (crates/hydra-gui/build.rs), so shortcuts take the exe's own icon.
  CreateDirectory "$SMPROGRAMS\Hydra"
  CreateShortcut "$SMPROGRAMS\Hydra\${APP_NAME}.lnk" "$INSTDIR\hydra-gui.exe"
  CreateShortcut "$SMPROGRAMS\Hydra\Uninstall ${APP_NAME}.lnk" \
    "$INSTDIR\uninstall.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Add/Remove Programs entry (HKCU to match the per-user install).
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayName"     "${APP_NAME}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayVersion"  "${VERSION}"
  WriteRegStr   HKCU "${UNINST_KEY}" "Publisher"       "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINST_KEY}" "URLInfoAbout"    "${HOMEPAGE}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayIcon"     "$INSTDIR\hydra.ico"
  WriteRegStr   HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr   HKCU "${UNINST_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0
SectionEnd

Section "Browser IPC Host" SEC_HOST
  SetOutPath "$INSTDIR"
  File "${BUILD_DIR}\hydra-host.exe"

  ; JSON needs the path escaped for Windows separators.
  ${StrRep} $1 "$INSTDIR\hydra-host.exe" "\" "\\"

  ; Chromium-family manifest (keyed by extension origin).
  FileOpen $0 "$INSTDIR\${HOST_NAME}.json" w
  FileWrite $0 '{$\r$\n'
  FileWrite $0 '  "name": "${HOST_NAME}",$\r$\n'
  FileWrite $0 '  "description": "Hydra Download Manager native host",$\r$\n'
  FileWrite $0 '  "path": "$1",$\r$\n'
  FileWrite $0 '  "type": "stdio",$\r$\n'
  FileWrite $0 '  "allowed_origins": ["chrome-extension://${CHROME_EXT_ID}/"]$\r$\n'
  FileWrite $0 '}$\r$\n'
  FileClose $0

  ; Firefox manifest (keyed by add-on id).
  FileOpen $0 "$INSTDIR\${HOST_NAME}.firefox.json" w
  FileWrite $0 '{$\r$\n'
  FileWrite $0 '  "name": "${HOST_NAME}",$\r$\n'
  FileWrite $0 '  "description": "Hydra Download Manager native host",$\r$\n'
  FileWrite $0 '  "path": "$1",$\r$\n'
  FileWrite $0 '  "type": "stdio",$\r$\n'
  FileWrite $0 '  "allowed_extensions": ["${FIREFOX_EXT_ID}"]$\r$\n'
  FileWrite $0 '}$\r$\n'
  FileClose $0

  ; Each browser reads the DEFAULT value of its NativeMessagingHosts key.
  WriteRegStr HKCU "Software\Google\Chrome\NativeMessagingHosts\${HOST_NAME}"              "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${HOST_NAME}"             "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\${HOST_NAME}" "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Chromium\NativeMessagingHosts\${HOST_NAME}"                   "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Vivaldi\NativeMessagingHosts\${HOST_NAME}"                    "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Mozilla\NativeMessagingHosts\${HOST_NAME}"                    "" "$INSTDIR\${HOST_NAME}.firefox.json"
SectionEnd

Section "Command-Line Tool (hydra, hya) + PATH" SEC_CLI
  SetOutPath "$INSTDIR"
  File "${BUILD_DIR}\hydra.exe"
  ; The short second name for the CLI: `hydra` is also THC-Hydra, the login
  ; auditor, and three letters types better for a command run as often as a
  ; download. A second copy rather than a link: this installer runs per-user
  ; without elevation, and Windows grants symlinks only to an elevated shell
  ; or with Developer Mode on. hya_updater re-copies it after a self update.
  File /oname=hya.exe "${BUILD_DIR}\hydra.exe"

  ; Append $INSTDIR to the per-user PATH (HKCU\Environment) so `hydra`,
  ; `hydra-gui`, and `hydra-host` resolve in cmd, PowerShell, and any
  ; terminal. REG_EXPAND_SZ so pre-existing %VAR% entries keep expanding.
  ReadRegStr $0 HKCU "Environment" "Path"
  ${StrStr} $1 "$0" "$INSTDIR"
  StrCmp $1 "" 0 path_done          ; already present -> skip
  StrCmp $0 "" 0 path_append
  StrCpy $0 "$INSTDIR"              ; PATH was empty/absent
  Goto path_write
path_append:
  StrCpy $0 "$0;$INSTDIR"
path_write:
  WriteRegExpandStr HKCU "Environment" "Path" $0
  ; Tell running shells/Explorer the environment changed; new terminals
  ; pick it up immediately, already-open ones still need a restart.
  SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
path_done:
SectionEnd

Section "Browser Extensions" SEC_EXT
  ; Built by scripts/build-extensions.sh, which build-windows-installer.sh
  ; runs before makensis: the packed .zip/.xpi plus the unpacked directories
  ; those archives were made from. Both shapes ship because an unsigned
  ; extension cannot be installed from a file in either browser family --
  ; Chromium needs "Load unpacked", Firefox takes the .xpi as a TEMPORARY
  ; add-on. The .zip/.xpi are there for a store upload, for a Firefox
  ; Developer Edition/ESR install with signature enforcement off, and so the
  ; files can be copied to another machine as one item each.
  SetOutPath "$INSTDIR\extensions"
  File "..\..\target\extensions\hydra-chrome-*.zip"
  File "..\..\target\extensions\hydra-firefox-*.xpi"
  ; The .crx exists only when the build machine had the extension signing
  ; key (scripts/build-extensions.sh --crx-key), so it is packed
  ; conditionally rather than being a build-breaking requirement.
!if /FileExists "..\..\target\extensions\hydra-chrome-*.crx"
  !define HAVE_CRX
  File "..\..\target\extensions\hydra-chrome-*.crx"
!endif
  File /r "..\..\target\extensions\chrome"
  File /r "..\..\target\extensions\firefox"

  ; The same instructions build-extensions.sh writes as INSTALL.txt on macOS
  ; and Linux, spelled with the real $INSTDIR (which the user may have
  ; changed on the Directory page, so this is generated here rather than
  ; packed in). Keep the two texts in step.
  FileOpen $0 "$INSTDIR\extensions\INSTALL.txt" w
  FileWrite $0 'Hydra browser extension - installing the unsigned build$\r$\n'
  FileWrite $0 '=======================================================$\r$\n$\r$\n'
  FileWrite $0 'These are UNSIGNED builds: they carry no Chrome Web Store or$\r$\n'
  FileWrite $0 'addons.mozilla.org signature, so each browser needs its developer mode$\r$\n'
  FileWrite $0 '(or a temporary install) to accept them. The extension itself is$\r$\n'
  FileWrite $0 'identical to the store build.$\r$\n$\r$\n'
  FileWrite $0 'In this directory:$\r$\n$\r$\n'
  FileWrite $0 '  hydra-chrome-${VERSION}.zip   packed build for the Chromium family$\r$\n'
!ifdef HAVE_CRX
  FileWrite $0 '  hydra-chrome-${VERSION}.crx   the same build, signed, for policy deployment$\r$\n'
!endif
  FileWrite $0 '  hydra-firefox-${VERSION}.xpi  packed build for Firefox$\r$\n'
  FileWrite $0 '  chrome\           the same Chromium build, already unpacked$\r$\n'
  FileWrite $0 '  firefox\          the same Firefox build, already unpacked$\r$\n'
  FileWrite $0 '  INSTALL.txt       this file$\r$\n$\r$\n'
  FileWrite $0 'Chrome, Edge, Opera, Brave, Vivaldi, Arc, Chromium$\r$\n'
  FileWrite $0 '--------------------------------------------------$\r$\n'
  FileWrite $0 'Chromium only installs a .crx that came from the Web Store, so an$\r$\n'
  FileWrite $0 'unsigned build is loaded from the unpacked directory instead.$\r$\n$\r$\n'
  FileWrite $0 '  1. Open the extensions page:$\r$\n'
  FileWrite $0 '         Chrome   chrome://extensions$\r$\n'
  FileWrite $0 '         Edge     edge://extensions$\r$\n'
  FileWrite $0 '         Opera    opera://extensions$\r$\n'
  FileWrite $0 '         Brave    brave://extensions$\r$\n'
  FileWrite $0 '         Vivaldi  vivaldi://extensions$\r$\n'
  FileWrite $0 '  2. Turn on "Developer mode" - top right in Chrome, Brave and Vivaldi;$\r$\n'
  FileWrite $0 '     bottom left in Edge; the sidebar in Opera.$\r$\n'
  FileWrite $0 '  3. Click "Load unpacked" and select:$\r$\n'
  FileWrite $0 '         $INSTDIR\extensions\chrome$\r$\n'
  FileWrite $0 '  4. Leave that folder where it is. The browser re-reads it from disk at$\r$\n'
  FileWrite $0 '     every start, and moving or deleting it uninstalls the extension.$\r$\n$\r$\n'
!ifdef HAVE_CRX
  FileWrite $0 'The signed hydra-chrome-${VERSION}.crx is for deploying by enterprise$\r$\n'
  FileWrite $0 'policy (ExtensionSettings or ExtensionInstallForcelist, pointing at an$\r$\n'
  FileWrite $0 'update manifest you host). Dragging it onto the extensions page will not$\r$\n'
  FileWrite $0 'work: Chromium refuses any .crx that did not come from the Web Store.$\r$\n$\r$\n'
!endif
  FileWrite $0 'The extension id is ${CHROME_EXT_ID} in every Chromium$\r$\n'
  FileWrite $0 'browser - it is pinned by the manifest key, and it is the id this$\r$\n'
  FileWrite $0 'installer already allow-listed for the native messaging host.$\r$\n$\r$\n'
  FileWrite $0 'Firefox$\r$\n'
  FileWrite $0 '-------$\r$\n'
  FileWrite $0 'Temporary - works in every Firefox, removed at the next restart:$\r$\n$\r$\n'
  FileWrite $0 '  1. Open about:debugging#/runtime/this-firefox$\r$\n'
  FileWrite $0 '  2. Click "Load Temporary Add-on..." and select:$\r$\n'
  FileWrite $0 '         $INSTDIR\extensions\hydra-firefox-${VERSION}.xpi$\r$\n'
  FileWrite $0 '     (or $INSTDIR\extensions\firefox\manifest.json)$\r$\n$\r$\n'
  FileWrite $0 'Permanent - Developer Edition, Nightly and ESR only:$\r$\n$\r$\n'
  FileWrite $0 '  1. Open about:config and set$\r$\n'
  FileWrite $0 '         xpinstall.signatures.required = false$\r$\n'
  FileWrite $0 '     Release and Beta Firefox ignore this setting and keep refusing an$\r$\n'
  FileWrite $0 '     unsigned add-on; use the temporary install above instead.$\r$\n'
  FileWrite $0 '  2. Open about:addons, click the gear icon, choose$\r$\n'
  FileWrite $0 '     "Install Add-on From File..." and select the .xpi above.$\r$\n$\r$\n'
  FileWrite $0 'After installing$\r$\n'
  FileWrite $0 '----------------$\r$\n'
  FileWrite $0 'Restart the browser once so it picks up the native-messaging$\r$\n'
  FileWrite $0 'registration this installer wrote, then open the Hydra toolbar icon:$\r$\n'
  FileWrite $0 'the status dot is green when the extension has reached the app$\r$\n'
  FileWrite $0 '(WebSocket 127.0.0.1:6799).$\r$\n'
  FileClose $0
SectionEnd

Section "Logo & Branding Assets" SEC_LOGO
  SetOutPath "$INSTDIR\assets"
  File "..\..\docs\logo.png"
SectionEnd

Section /o "Desktop Shortcut" SEC_DESKTOP
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\hydra-gui.exe"
SectionEnd

;--------------------------------
; Component descriptions

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_GUI}     "The Hydra Download Manager application (required)."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_HOST}    "Native-messaging host bridging browser extensions to Hydra, plus its registry registration for Chrome, Edge, Brave, Chromium, Vivaldi, and Firefox."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_CLI}     "The hydra command-line downloader, with the install directory added to your PATH for cmd, PowerShell, and other terminals."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_EXT}     "Browser extensions for the Chromium family (Chrome, Edge, Opera, Brave, Vivaldi) and Firefox: packed .zip/.xpi plus the unpacked directories a developer-mode install loads, with instructions."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_LOGO}    "Hydra logo image installed alongside the application."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DESKTOP} "Shortcut to Hydra Download Manager on the desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

;--------------------------------
; Uninstaller

Section "Uninstall"
  nsExec::Exec 'taskkill /F /IM hydra-gui.exe'
  nsExec::Exec 'taskkill /F /IM hydra-host.exe'
  Pop $0

  ; Native-messaging registration.
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Chromium\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Vivaldi\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\${HOST_NAME}"

  ; Strip $INSTDIR out of the per-user PATH (all three separator shapes).
  ReadRegStr $0 HKCU "Environment" "Path"
  ${UnStrRep} $0 "$0" ";$INSTDIR" ""
  ${UnStrRep} $0 "$0" "$INSTDIR;" ""
  ${UnStrRep} $0 "$0" "$INSTDIR"  ""
  WriteRegExpandStr HKCU "Environment" "Path" $0
  SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000

  Delete "$INSTDIR\hydra-gui.exe"
  Delete "$INSTDIR\hydra.exe"
  Delete "$INSTDIR\hya.exe"
  Delete "$INSTDIR\hydra-host.exe"
  Delete "$INSTDIR\${HOST_NAME}.json"
  Delete "$INSTDIR\${HOST_NAME}.firefox.json"
  Delete "$INSTDIR\hydra.ico"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\extensions"
  RMDir /r "$INSTDIR\assets"
  ; Not /r: leaves user data behind if anything else ever lands in $INSTDIR.
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\Hydra\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\Hydra\Uninstall ${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\Hydra"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  ; "Launch on startup" login item (written by the app, autostart.rs), plus
  ; the Startup-folder .cmd that pre-0.2.x versions wrote.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${APP_NAME}"
  Delete "$SMSTARTUP\hydra.cmd"

  DeleteRegKey HKCU "${UNINST_KEY}"
SectionEnd
