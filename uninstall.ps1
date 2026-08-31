# Copyright (C) 2026 Javad Rajabzadeh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Uninstall Hydra from Windows.
#
#   powershell -ExecutionPolicy Bypass -File uninstall.ps1
#   powershell -ExecutionPolicy Bypass -File uninstall.ps1 -Purge   # also delete config/state
#
# Or straight from the repo:
#   irm https://raw.githubusercontent.com/ja7ad/hydra/main/uninstall.ps1 | iex
#
# Removes everything install.ps1 created: %LOCALAPPDATA%\Programs\Hydra
# (binaries including the hya shorthand, extensions, scripts), the start-menu
# and desktop shortcuts, the
# "Apps & features" entry, the user-PATH entry, the native-messaging registry
# keys and manifests, and the login-item Run entry. Config and state in
# %APPDATA%\hydra are kept unless -Purge is given.
#
# A copy installed by the .exe installer is NOT touched: that one owns its own
# uninstaller, and this script says so rather than half-removing it.

param(
  [switch]$Purge,
  [string]$InstallDir = ""
)

$ErrorActionPreference = "Stop"

if (-not $InstallDir) { $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\Hydra" }
$HostName = "com.hydra.host"
$Removed = $false

function Remove-Path([string]$Path) {
  if (Test-Path $Path) {
    Remove-Item $Path -Recurse -Force
    Write-Host "removed $Path"
    $script:Removed = $true
  }
}

# Stop running instances so files are not locked. hydra-updater is the
# self-update finisher: it can be mid-swap in the very directory being removed.
$Running = @(Get-Process -Name hydra, hya, hydra-gui, hydra-host, hydra-updater -ErrorAction SilentlyContinue)
foreach ($p in $Running) { $null = $p.CloseMainWindow() }
if ($Running.Count -gt 0) { Start-Sleep -Milliseconds 1200 }
$Running | Where-Object { -not $_.HasExited } | Stop-Process -Force -ErrorAction SilentlyContinue

# The .exe installer's own registration. Leaving it alone is deliberate — it
# has an uninstall.exe that knows what it wrote.
if (Test-Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Hydra") {
  Write-Host "note: a copy installed by the Hydra setup installer is also registered; remove it from Settings > Apps." -ForegroundColor Yellow
}

# Login item + legacy Startup-folder entry (written by the in-app toggle).
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
if ((Get-ItemProperty -Path $RunKey -Name "Hydra Download Manager" -ErrorAction SilentlyContinue)) {
  Remove-ItemProperty -Path $RunKey -Name "Hydra Download Manager"
  Write-Host "removed login item ($RunKey)"
  $Removed = $true
}
Remove-Path (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\hydra.cmd")

# Native-messaging registration (mirrors install-native-host.ps1).
$NmhKeys = @(
  "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$HostName",
  "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$HostName",
  "HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\$HostName",
  "HKCU:\Software\Chromium\NativeMessagingHosts\$HostName",
  "HKCU:\Software\Vivaldi\NativeMessagingHosts\$HostName",
  "HKCU:\Software\Mozilla\NativeMessagingHosts\$HostName"
)
foreach ($k in $NmhKeys) {
  if (Test-Path $k) {
    Remove-Item $k -Recurse -Force
    Write-Host "removed $k"
    $Removed = $true
  }
}
Remove-Path (Join-Path $env:LOCALAPPDATA "Hydra")   # manifest files

# Shortcuts, the Apps & features entry, and the install directory itself.
Remove-Path (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Hydra Download Manager.lnk")
Remove-Path (Join-Path ([Environment]::GetFolderPath("Desktop")) "Hydra Download Manager.lnk")
$UninstKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\HydraDownloadManager"
if (Test-Path $UninstKey) {
  Remove-Item $UninstKey -Recurse -Force
  Write-Host "removed $UninstKey"
  $Removed = $true
}
# Apps & features launches this script from inside $InstallDir. A PowerShell
# script is read into memory rather than held open, so removing it along with
# the rest works — but if something else in there is still locked, name the
# directory instead of dying on a raw exception.
try {
  Remove-Path $InstallDir
} catch {
  Write-Warning "could not fully remove $InstallDir ($($_.Exception.Message)); close any running Hydra and delete it by hand."
}

# Drop the install dir from the user PATH.
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath) {
  $Parts = $UserPath -split ";" | Where-Object { $_ -and $_ -ne $InstallDir }
  $NewPath = $Parts -join ";"
  if ($NewPath -ne $UserPath) {
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    Write-Host "removed $InstallDir from your user PATH"
    $Removed = $true
  }
}

$ConfigDir = Join-Path $env:APPDATA "hydra"
if ($Purge) {
  Remove-Path $ConfigDir
} elseif (Test-Path $ConfigDir) {
  Write-Host "kept $ConfigDir (config/state/logs); rerun with -Purge to delete it"
}

if ($Removed) { Write-Host "done." }
else { Write-Host "nothing to remove; hydra does not appear to be installed." }
