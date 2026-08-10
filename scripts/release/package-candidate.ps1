function Copy-CandidateContent(
    [pscustomobject] $Context,
    [pscustomobject] $Layout
) {
    Copy-TrackedTree $Context.ProjectRoot "examples/web" (Join-Path $Layout.Demos "web")
    Copy-TrackedTree $Context.ProjectRoot "sdk/rust/examples" (Join-Path $Layout.Demos "rust")
    Copy-TrackedTree $Context.ProjectRoot "sdk/c-abi/examples" (Join-Path $Layout.Demos "c-abi")
    Copy-TrackedTree $Context.ProjectRoot "sdk/android/demo" (Join-Path $Layout.Demos "android")
    Copy-TrackedTree $Context.ProjectRoot "sdk/ios/Examples" (Join-Path $Layout.Demos "ios")
    Copy-Item -LiteralPath (Join-Path $Context.ProjectRoot "examples\README.md") `
        -Destination $Layout.Demos

    Copy-TrackedTree $Context.ProjectRoot "docs" $Layout.Docs
    Copy-Item -LiteralPath (Join-Path $Context.ProjectRoot "README.md") -Destination (
        Join-Path $Layout.Docs "PROJECT.zh-CN.md"
    )
    Copy-Item -LiteralPath (Join-Path $Context.ProjectRoot "README.en.md") -Destination (
        Join-Path $Layout.Docs "PROJECT.en.md"
    )
    $licensePath = Join-Path $Context.ProjectRoot "LICENSE"
    if (Test-Path -LiteralPath $licensePath -PathType Leaf) {
        Copy-Item -LiteralPath $licensePath -Destination $Layout.Root
    }

    $sourceArchive = Join-Path $Layout.Source "fluvora-v$($Context.Version)-src.zip"
    Invoke-ReleaseCommand "git" @(
        "archive", "--format=zip", "--output=$sourceArchive", "HEAD"
    )
}

function Write-CandidatePackage(
    [pscustomobject] $Context,
    [pscustomobject] $Layout,
    [pscustomobject] $Core,
    [string] $WebStatus,
    [string] $AndroidStatus,
    [string] $SwiftStatus,
    [string] $CiRunUrl,
    [bool] $Verified
) {
    $manifest = [ordered]@{
        schemaVersion = 1
        product = "Fluvora"
        version = $Context.Version
        package = $Context.PackageName
        gitCommit = $Context.GitCommit
        generatedAt = [DateTimeOffset]::UtcNow.ToString("O")
        platform = $Context.Platform
        architecture = $Context.Architecture
        rustc = (& rustc --version | Select-Object -First 1)
        node = (& node --version | Select-Object -First 1)
        components = [ordered]@{
            server = $Core.ServerBinaries
            tools = $Core.ToolBinaries
            sdk = [ordered]@{
                web = $WebStatus
                rust = "compiled room_client demo plus source archive"
                cAbi = $Core.CAbiLibraries
                android = $AndroidStatus
                swift = $SwiftStatus
            }
            demos = @("web", "rust", "c-abi", "android", "ios")
        }
        verification = [ordered]@{
            localQuickReleaseGates = $Verified
            productionDtls = $Core.ProductionDtls
            sdkDemoContract = "five clients, nine common scenarios"
            githubActions = $CiRunUrl
        }
    }
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (
        Join-Path $Layout.Root "manifest.json"
    ) -Encoding utf8

    @"
# Fluvora v$($Context.Version) candidate package

Git commit: $($Context.GitCommit)

- server/: production service binaries (media-node production DTLS: $($Core.ProductionDtls))
- tools/: administration, capacity, and TURN probe tools
- sdk/: built Web, Rust, C ABI, and available native SDK artifacts
- demos/: Web, Rust, C/C++, Android, and iOS/Swift integration demos
- docs/: Chinese documents at the root and English documents under en/
- source/: reproducible source archive for this exact commit
- evidence/: local release-gate evidence when verification was enabled

Read docs/SDK_INTEGRATION.md or docs/en/SDK_INTEGRATION.md before integrating an SDK.
This is a candidate artifact, not a public registry publication or GitHub Release.
"@ | Set-Content -LiteralPath (Join-Path $Layout.Root "PACKAGE_README.md") -Encoding utf8

    $checksumLines = Get-ChildItem -LiteralPath $Layout.Root -Recurse -File |
        Sort-Object FullName |
        ForEach-Object {
            $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            $relativePath = $_.FullName.Substring($Layout.Root.Length + 1).Replace("\", "/")
            "$hash  $relativePath"
        }
    $checksumLines | Set-Content -LiteralPath (
        Join-Path $Layout.Root "SHA256SUMS"
    ) -Encoding ascii

    Compress-Archive -LiteralPath $Layout.Root `
        -DestinationPath $Context.ArchivePath `
        -CompressionLevel Optimal
    $archiveHash = (
        Get-FileHash -LiteralPath $Context.ArchivePath -Algorithm SHA256
    ).Hash.ToLowerInvariant()
    "$archiveHash  $(Split-Path -Leaf $Context.ArchivePath)" | Set-Content -LiteralPath (
        "$($Context.ArchivePath).sha256"
    ) -Encoding ascii

    [pscustomobject]@{
        ArchivePath = $Context.ArchivePath
        ArchiveHash = $archiveHash
    }
}
