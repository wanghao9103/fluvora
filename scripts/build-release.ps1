param(
    [string] $Version = "",
    [string] $OutputDirectory = "",
    [string] $CiRunUrl = "",
    [switch] $SkipVerification,
    [switch] $SkipVendoredOpenSsl,
    [switch] $SkipAndroid
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot "release\common.ps1")
. (Join-Path $PSScriptRoot "release\build-components.ps1")
. (Join-Path $PSScriptRoot "release\package-candidate.ps1")

Push-Location -LiteralPath $projectRoot
try {
    $context = Get-ReleaseContext `
        -ProjectRoot $projectRoot `
        -RequestedVersion $Version `
        -RequestedOutputDirectory $OutputDirectory
    $layout = New-ReleaseLayout -Context $context

    if (-not $SkipVerification) {
        & (Join-Path $PSScriptRoot "run-release-gates.ps1") `
            -Profile quick -EvidenceDirectory $layout.Evidence
    }

    $core = Build-CoreComponents `
        -Context $context `
        -Layout $layout `
        -SkipVendoredOpenSsl:$SkipVendoredOpenSsl
    $webStatus = Build-WebSdk -Context $context -Layout $layout
    $androidStatus = Build-AndroidSdk `
        -Context $context `
        -Layout $layout `
        -Skip:$SkipAndroid
    $swiftStatus = Build-SwiftSdk -Context $context -Layout $layout

    Copy-CandidateContent -Context $context -Layout $layout
    $result = Write-CandidatePackage `
        -Context $context `
        -Layout $layout `
        -Core $core `
        -WebStatus $webStatus `
        -AndroidStatus $androidStatus `
        -SwiftStatus $swiftStatus `
        -CiRunUrl $CiRunUrl `
        -Verified:(-not $SkipVerification)

    Write-Host "Release package: $($result.ArchivePath)"
    Write-Host "SHA-256: $($result.ArchiveHash)"
}
finally {
    Pop-Location
}
