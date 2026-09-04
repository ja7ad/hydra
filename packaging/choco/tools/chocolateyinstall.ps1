$ErrorActionPreference = 'Stop'

$packageArgs = @{
  packageName       = 'hydra-download-manager'
  fileType          = 'exe'
  # NSIS installer (see scripts/windows/hydra-installer.nsi): the silent
  # switch is an uppercase /S. Inno Setup switches like /VERYSILENT are
  # ignored by NSIS, which leaves the wizard on screen and hangs the install.
  # /S selects the installer's defaults: app, IPC host, CLI + PATH, browser
  # extensions, Start-menu and desktop shortcuts. Skip the desktop shortcut
  # with: choco install hydra-download-manager --install-arguments="'/NODESKTOP'"
  silentArgs        = '/S'
  validExitCodes    = @(0)

  # x64 (AMD64 / Intel)
  url64             = 'https://github.com/ja7ad/hydra/releases/download/v0.4.1/hydra-0.4.1-windows-x64-setup.exe'
  checksum64        = 'F595A13FC771F4695368452C9A208FE32A7465B9C57502E602E745F5162BAF2B'
  checksumType64    = 'sha256'

  # ARM64
  url64arm          = 'https://github.com/ja7ad/hydra/releases/download/v0.4.1/hydra-0.4.1-windows-arm64-setup.exe'
  checksum64arm     = '347E2F01774E3AEE16B2DF461F01E9DBF63925FFD51BB2307A2DF8B4A4542E91'
  checksumType64arm = 'sha256'
}

Install-ChocolateyPackage @packageArgs