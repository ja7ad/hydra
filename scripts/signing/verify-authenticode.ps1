param(
    [Parameter(Mandatory)][string]$UnsignedDirectory,
    [Parameter(Mandatory)][string]$SignedDirectory,
    [Parameter(Mandatory)][string]$CertificateThumbprint,
    [ValidateSet('test-signing', 'release-signing')][string]$SigningPolicy = 'release-signing'
)
$ErrorActionPreference = 'Stop'
$expectedThumbprint = $CertificateThumbprint.Replace(' ', '').ToUpperInvariant()
if ($expectedThumbprint -notmatch '^[0-9A-F]{40}$') {
    throw 'Configure SIGNPATH_CERTIFICATE_THUMBPRINT with the issued release certificate SHA-1 thumbprint'
}
if ($SigningPolicy -eq 'release-signing' -and $expectedThumbprint -eq '4924FFE138E36949CA3EDDF03252903CDB65B2AE') {
    throw 'Production signing cannot use the test certificate'
}
$unsigned = (Resolve-Path $UnsignedDirectory).Path
$signed = (Resolve-Path $SignedDirectory).Path
$inputs = @(Get-ChildItem $unsigned -Recurse -File)
$outputs = @(Get-ChildItem $signed -Recurse -File)
if ($inputs.Count -eq 0 -or $inputs.Count -ne $outputs.Count) {
    throw 'Signed output does not contain exactly the input files'
}
$addedRoot = $false
$rootPath = "Cert:\CurrentUser\Root\$expectedThumbprint"
try {
    if ($SigningPolicy -eq 'test-signing') {
        if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
            throw 'Test certificate trust is restricted to disposable GitHub-hosted runners'
        }
        $certificateFile = Join-Path $PSScriptRoot 'test-certificate.cer'
        $certificate = [Security.Cryptography.X509Certificates.X509Certificate2]::new($certificateFile)
        if ($certificate.Thumbprint -ne $expectedThumbprint) {
            throw 'Pinned test certificate does not match the requested signing certificate'
        }
        if (-not (Test-Path -LiteralPath $rootPath)) {
            $addedRoot = $true
            Import-Certificate -FilePath $certificateFile -CertStoreLocation Cert:\CurrentUser\Root | Out-Null
        }
    }
    foreach ($inputFile in $inputs) {
        $relative = [IO.Path]::GetRelativePath($unsigned, $inputFile.FullName)
        $outputFile = Join-Path $signed $relative
        if (-not (Test-Path -LiteralPath $outputFile -PathType Leaf)) {
            throw "Missing signed output: $relative"
        }
        $signature = Get-AuthenticodeSignature -LiteralPath $outputFile
        if ($signature.Status -ne 'Valid' -or
            $signature.SignerCertificate.Thumbprint -ne $expectedThumbprint -or
            $null -eq $signature.TimeStamperCertificate) {
            throw "Invalid, unexpected, or untimestamped signature: $relative ($($signature.Status))"
        }
        Write-Output "Verified $relative"
    }
} finally {
    if ($addedRoot -and (Test-Path -LiteralPath $rootPath)) {
        Remove-Item -LiteralPath $rootPath -Force
    }
}
