$ErrorActionPreference = 'Stop'
$temporary = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
$unsigned = Join-Path $temporary 'unsigned'
$signed = Join-Path $temporary 'signed'
New-Item -ItemType Directory $unsigned, $signed | Out-Null
$thumbprint = 'A' * 40
function Get-AuthenticodeSignature {
    param([string]$LiteralPath)
    return $global:HydraSigningTestSignature
}
function Assert-Refused {
    param([string]$Expected, [string]$SigningPolicy = 'release-signing')
    try {
        & "$PSScriptRoot/verify-authenticode.ps1" $unsigned $signed $thumbprint -SigningPolicy $SigningPolicy | Out-Null
    } catch {
        if ($_.Exception.Message -notlike "*$Expected*") { throw }
        return
    }
    throw "Expected verification to reject: $Expected"
}
$originalActions = $env:GITHUB_ACTIONS
$originalRunner = $env:RUNNER_ENVIRONMENT
$global:HydraTestRootExists = $false
$global:HydraTestImports = 0
$global:HydraTestRemovals = 0
function Test-Path {
    param([string]$LiteralPath, [string]$PathType = 'Any')
    if ($LiteralPath -like 'Cert:\CurrentUser\Root\*') { return $global:HydraTestRootExists }
    Microsoft.PowerShell.Management\Test-Path -LiteralPath $LiteralPath -PathType $PathType
}
function Import-Certificate {
    param([string]$FilePath, [string]$CertStoreLocation)
    if ($CertStoreLocation -ne 'Cert:\CurrentUser\Root' -or -not $FilePath.EndsWith('test-certificate.cer')) {
        throw 'Unexpected test certificate import'
    }
    $global:HydraTestImports++
    $global:HydraTestRootExists = $true
}
function Remove-Item {
    param([string]$LiteralPath, [string]$Path, [switch]$Force, [switch]$Recurse)
    if ($LiteralPath -like 'Cert:\CurrentUser\Root\*') {
        $global:HydraTestRootExists = $false
        $global:HydraTestRemovals++
        return
    }
    Microsoft.PowerShell.Management\Remove-Item @PSBoundParameters
}
try {
    Assert-Refused 'exactly the input files'
    Set-Content (Join-Path $unsigned 'hydra.exe') 'unsigned fixture'
    Set-Content (Join-Path $signed 'hydra.exe') 'signed fixture'
    $global:HydraSigningTestSignature = [PSCustomObject]@{
        Status = 'Valid'
        SignerCertificate = [PSCustomObject]@{Thumbprint = $thumbprint}
        TimeStamperCertificate = [PSCustomObject]@{Subject = 'timestamp fixture'}
    }
    & "$PSScriptRoot/verify-authenticode.ps1" $unsigned $signed $thumbprint
    Set-Content (Join-Path $signed 'extra.exe') 'unexpected file'
    Assert-Refused 'exactly the input files'
    Remove-Item (Join-Path $signed 'extra.exe')
    $global:HydraSigningTestSignature.Status = 'HashMismatch'
    Assert-Refused 'Invalid, unexpected, or untimestamped'
    $global:HydraSigningTestSignature.Status = 'Valid'
    $global:HydraSigningTestSignature.SignerCertificate.Thumbprint = 'B' * 40
    Assert-Refused 'Invalid, unexpected, or untimestamped'
    $global:HydraSigningTestSignature.SignerCertificate.Thumbprint = $thumbprint
    $global:HydraSigningTestSignature.TimeStamperCertificate = $null
    Assert-Refused 'Invalid, unexpected, or untimestamped'
    Rename-Item (Join-Path $signed 'hydra.exe') 'other.exe'
    Assert-Refused 'Missing signed output'
    $thumbprint = 'not a certificate thumbprint'
    Assert-Refused 'Configure SIGNPATH_CERTIFICATE_THUMBPRINT'
    $thumbprint = 'A' * 40
    Rename-Item (Join-Path $signed 'other.exe') 'hydra.exe'
    $env:GITHUB_ACTIONS = 'false'
    $env:RUNNER_ENVIRONMENT = 'github-hosted'
    Assert-Refused 'disposable GitHub-hosted runners' 'test-signing'
    $env:GITHUB_ACTIONS = 'true'
    $env:RUNNER_ENVIRONMENT = 'self-hosted'
    Assert-Refused 'disposable GitHub-hosted runners' 'test-signing'
    $env:RUNNER_ENVIRONMENT = 'github-hosted'
    Assert-Refused 'Pinned test certificate' 'test-signing'
    $thumbprint = '4924FFE138E36949CA3EDDF03252903CDB65B2AE'
    Assert-Refused 'Production signing cannot use the test certificate'
    $global:HydraSigningTestSignature.SignerCertificate.Thumbprint = $thumbprint
    $global:HydraSigningTestSignature.TimeStamperCertificate = [PSCustomObject]@{Subject = 'timestamp fixture'}
    & "$PSScriptRoot/verify-authenticode.ps1" $unsigned $signed $thumbprint -SigningPolicy test-signing
    if ($global:HydraTestRootExists -or $global:HydraTestImports -ne 1 -or $global:HydraTestRemovals -ne 1) {
        throw 'Temporary trust was not removed after successful verification'
    }
    $global:HydraSigningTestSignature.Status = 'HashMismatch'
    Assert-Refused 'Invalid, unexpected, or untimestamped' 'test-signing'
    if ($global:HydraTestRootExists -or $global:HydraTestImports -ne 2 -or $global:HydraTestRemovals -ne 2) {
        throw 'Temporary trust was not removed after failed verification'
    }
    $global:HydraSigningTestSignature.Status = 'Valid'
    $global:HydraTestRootExists = $true
    & "$PSScriptRoot/verify-authenticode.ps1" $unsigned $signed $thumbprint -SigningPolicy test-signing
    if (-not $global:HydraTestRootExists -or $global:HydraTestImports -ne 2 -or $global:HydraTestRemovals -ne 2) {
        throw 'Verification changed a certificate that was already installed'
    }
    Write-Output 'Authenticode verification guard tests passed'
} finally {
    $env:GITHUB_ACTIONS = $originalActions
    $env:RUNNER_ENVIRONMENT = $originalRunner
    Remove-Variable HydraTestRootExists, HydraTestImports, HydraTestRemovals -Scope Global -ErrorAction SilentlyContinue
    Remove-Variable HydraSigningTestSignature -Scope Global -ErrorAction SilentlyContinue
    Remove-Item $temporary -Recurse -Force
}
