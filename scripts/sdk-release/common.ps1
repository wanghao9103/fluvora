function Get-SdkVersion([string] $ProjectRoot, [string] $Sdk) {
    switch ($Sdk) {
        "web" {
            return [string] ((Get-Content (Join-Path $ProjectRoot "sdk\web\package.json") -Raw |
                ConvertFrom-Json).version)
        }
        "rust" { $packageName = "fluvora-sdk" }
        "c-abi" { $packageName = "fluvora-c-abi" }
        "android" {
            $content = Get-Content (Join-Path $ProjectRoot "sdk\android\build.gradle.kts") -Raw
            if ($content -notmatch 'version\s*=\s*"([^"]+)"') { throw "Android version not found" }
            return $Matches[1]
        }
        "swift" {
            return (Get-Content (Join-Path $ProjectRoot "sdk\ios\VERSION") -Raw).Trim()
        }
    }
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
    $package = @($metadata.packages | Where-Object name -eq $packageName)
    if ($package.Count -ne 1) { throw "package $packageName not found" }
    [string] $package[0].version
}

function Get-SdkReleaseContext(
    [string] $ProjectRoot,
    [string] $Sdk,
    [string] $RequestedVersion,
    [string] $RequestedOutputDirectory,
    [bool] $AllowDirty
) {
    $version = Get-SdkVersion $ProjectRoot $Sdk
    if ($RequestedVersion -and $RequestedVersion -ne $version) {
        throw "requested version $RequestedVersion does not match $Sdk version $version"
    }
    $commit = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw "git rev-parse failed" }
    if (-not $AllowDirty -and ((& git status --porcelain | Out-String).Trim())) {
        throw "SDK release candidates must be built from a clean worktree"
    }
    $isWindows = [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [Runtime.InteropServices.OSPlatform]::Windows)
    $isLinux = [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [Runtime.InteropServices.OSPlatform]::Linux)
    $platform = if ($isWindows) { "windows" } elseif ($isLinux) { "linux" } else { "macos" }
    $architecture = switch ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) {
        "X64" { "x86_64" }; "Arm64" { "aarch64" }; default { $_.ToLowerInvariant() }
    }
    $output = if ($RequestedOutputDirectory) {
        $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($RequestedOutputDirectory)
    } else { Join-Path $ProjectRoot "artifacts\sdk-releases\$Sdk" }
    $name = "fluvora-$Sdk-v$version-$platform-$architecture-$($commit.Substring(0, 8))"
    [pscustomobject]@{
        ProjectRoot = $ProjectRoot; Sdk = $Sdk; Version = $version; GitCommit = $commit
        Platform = $platform; Architecture = $architecture; IsWindows = $isWindows
        ExecutableSuffix = if ($isWindows) { ".exe" } else { "" }
        OutputDirectory = $output; PackageName = $name
        StageDirectory = Join-Path $output $name
        ArchivePath = Join-Path $output "$name.zip"
    }
}
