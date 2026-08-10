function Write-SdkPackage([pscustomobject] $Context) {
    $sourceDirectory = Join-Path $Context.StageDirectory "source"
    New-Item -ItemType Directory -Force -Path $sourceDirectory | Out-Null
    $sdkPath = if ($Context.Sdk -eq "swift") { "sdk/ios" } else { "sdk/$($Context.Sdk)" }
    Copy-TrackedTree $Context.ProjectRoot $sdkPath $sourceDirectory
    $changelogPath = Join-Path $Context.ProjectRoot "$sdkPath/CHANGELOG.md"
    if (-not (Test-Path $changelogPath -PathType Leaf)) {
        throw "missing SDK changelog: $changelogPath"
    }
    Copy-Item $changelogPath (Join-Path $Context.StageDirectory "CHANGELOG.md")
    foreach ($file in @("LICENSE", "docs/SDK_INTEGRATION.md", "docs/en/SDK_INTEGRATION.md")) {
        $source = Join-Path $Context.ProjectRoot $file
        if (Test-Path $source -PathType Leaf) {
            $destination = Join-Path $Context.StageDirectory $file
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
            Copy-Item $source $destination
        }
    }
    [ordered]@{
        schemaVersion = 1; product = "Fluvora $($Context.Sdk) SDK"
        version = $Context.Version; gitCommit = $Context.GitCommit
        platform = $Context.Platform; architecture = $Context.Architecture
        compatibleServer = ">=0.1.0 <0.2.0"; protocol = "sdk-contract-v1"
        generatedAt = [DateTimeOffset]::UtcNow.ToString("O")
    } | ConvertTo-Json | Set-Content (Join-Path $Context.StageDirectory "manifest.json") -Encoding utf8
    $checksums = Get-ChildItem $Context.StageDirectory -Recurse -File | Sort-Object FullName | ForEach-Object {
        $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $($_.FullName.Substring($Context.StageDirectory.Length + 1).Replace('\', '/'))"
    }
    $checksums | Set-Content (Join-Path $Context.StageDirectory "SHA256SUMS") -Encoding ascii
    Compress-Archive -LiteralPath $Context.StageDirectory -DestinationPath $Context.ArchivePath -CompressionLevel Optimal
    $hash = (Get-FileHash $Context.ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $(Split-Path -Leaf $Context.ArchivePath)" | Set-Content "$($Context.ArchivePath).sha256" -Encoding ascii
    [pscustomobject]@{ ArchivePath = $Context.ArchivePath; ArchiveHash = $hash }
}
