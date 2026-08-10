param(
    [Parameter(Mandatory)]
    [ValidateSet("web", "rust", "c-abi", "android", "swift")]
    [string] $Sdk,
    [string] $Version = "",
    [string] $OutputDirectory = "",
    [switch] $AllowDirty
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot "release\common.ps1")
. (Join-Path $PSScriptRoot "sdk-release\common.ps1")
. (Join-Path $PSScriptRoot "sdk-release\build-components.ps1")
. (Join-Path $PSScriptRoot "sdk-release\package-sdk.ps1")

Push-Location -LiteralPath $projectRoot
try {
    $context = Get-SdkReleaseContext $projectRoot $Sdk $Version $OutputDirectory $AllowDirty
    New-Item -ItemType Directory -Force -Path $context.StageDirectory | Out-Null
    Build-SdkComponent $context
    $result = Write-SdkPackage $context
    Write-Host "SDK package: $($result.ArchivePath)"
    Write-Host "SHA-256: $($result.ArchiveHash)"
}
finally {
    Pop-Location
}
