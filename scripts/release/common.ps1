function Invoke-ReleaseCommand([string] $FilePath, [string[]] $Arguments) {
    $savedErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        & $FilePath @Arguments | Out-Host
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $savedErrorAction
    }
    if ($exitCode -ne 0) {
        throw "$FilePath exited with code $exitCode"
    }
}

function Copy-TrackedTree(
    [string] $ProjectRoot,
    [string] $SourcePrefix,
    [string] $DestinationDirectory
) {
    $normalizedPrefix = $SourcePrefix.Replace("\", "/").TrimEnd("/")
    $trackedFiles = @(& git ls-files -- $normalizedPrefix)
    if ($LASTEXITCODE -ne 0) {
        throw "git ls-files failed for $normalizedPrefix"
    }
    foreach ($relativePath in $trackedFiles) {
        $sourcePath = Join-Path $ProjectRoot $relativePath
        if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
            continue
        }
        $childPath = $relativePath.Substring($normalizedPrefix.Length).TrimStart("/")
        if (-not $childPath) {
            $childPath = Split-Path -Leaf $relativePath
        }
        $destinationPath = Join-Path $DestinationDirectory $childPath
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destinationPath) |
            Out-Null
        Copy-Item -LiteralPath $sourcePath -Destination $destinationPath
    }
}

function Get-ReleaseContext(
    [string] $ProjectRoot,
    [string] $RequestedVersion,
    [string] $RequestedOutputDirectory
) {
    $metadataJson = & cargo metadata --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with code $LASTEXITCODE"
    }
    $metadata = $metadataJson | ConvertFrom-Json
    $workspaceVersions = @($metadata.packages.version | Sort-Object -Unique)
    if ($workspaceVersions.Count -ne 1) {
        throw "workspace package versions are not aligned: $($workspaceVersions -join ', ')"
    }
    $workspaceVersion = [string] $workspaceVersions[0]
    $resolvedVersion = if ($RequestedVersion) { $RequestedVersion } else { $workspaceVersion }
    if ($resolvedVersion -ne $workspaceVersion) {
        throw "requested version $resolvedVersion does not match Cargo workspace $workspaceVersion"
    }

    $webPackage = Get-Content -LiteralPath (
        Join-Path $ProjectRoot "sdk\web\package.json"
    ) -Raw | ConvertFrom-Json
    if ($webPackage.version -ne $resolvedVersion) {
        throw "Web SDK version $($webPackage.version) does not match $resolvedVersion"
    }
    $androidBuild = Get-Content -LiteralPath (
        Join-Path $ProjectRoot "sdk\android\build.gradle.kts"
    ) -Raw
    if ($androidBuild -notmatch (
        'version\s*=\s*"' + [regex]::Escape($resolvedVersion) + '"'
    )) {
        throw "Android SDK version does not match $resolvedVersion"
    }

    $gitCommit = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "git rev-parse failed with code $LASTEXITCODE"
    }
    $worktreeChanges = (& git status --porcelain | Out-String).Trim()
    if ($worktreeChanges) {
        throw "release candidates must be built from a clean worktree"
    }

    $isWindows = [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [Runtime.InteropServices.OSPlatform]::Windows
    )
    $isLinux = [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [Runtime.InteropServices.OSPlatform]::Linux
    )
    $platform = if ($isWindows) { "windows" } elseif ($isLinux) { "linux" } else { "macos" }
    $architecture = switch (
        [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    ) {
        "X64" { "x86_64" }
        "Arm64" { "aarch64" }
        default { $_.ToLowerInvariant() }
    }
    $shortCommit = $gitCommit.Substring(0, 8)
    $packageName = "fluvora-v$resolvedVersion-$platform-$architecture-$shortCommit"
    $outputDirectory = if ($RequestedOutputDirectory) {
        $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath(
            $RequestedOutputDirectory
        )
    }
    else {
        Join-Path $ProjectRoot "artifacts\releases"
    }

    [pscustomobject]@{
        ProjectRoot = $ProjectRoot
        Version = $resolvedVersion
        GitCommit = $gitCommit
        Platform = $platform
        Architecture = $architecture
        IsWindows = $isWindows
        IsLinux = $isLinux
        ExecutableSuffix = if ($isWindows) { ".exe" } else { "" }
        PackageName = $packageName
        OutputDirectory = $outputDirectory
        ArchivePath = Join-Path $outputDirectory "$packageName.zip"
    }
}

function New-ReleaseLayout([pscustomobject] $Context) {
    $root = Join-Path $Context.OutputDirectory $Context.PackageName
    if ((Test-Path -LiteralPath $root) -or (Test-Path -LiteralPath $Context.ArchivePath)) {
        throw "release output already exists: $root or $($Context.ArchivePath)"
    }
    $layout = [ordered]@{
        Root = $root
        Server = Join-Path $root "server"
        Tools = Join-Path $root "tools"
        Sdk = Join-Path $root "sdk"
        Demos = Join-Path $root "demos"
        Docs = Join-Path $root "docs"
        Source = Join-Path $root "source"
        Evidence = Join-Path $root "evidence"
    }
    $layout.Values | ForEach-Object {
        New-Item -ItemType Directory -Force -Path $_ | Out-Null
    }
    [pscustomobject] $layout
}

function Resolve-AndroidDirectory([pscustomobject] $Context) {
    $androidDirectory = Join-Path $Context.ProjectRoot "sdk\android"
    if (-not $Context.IsWindows -or $androidDirectory -notmatch '[^\x00-\x7F]') {
        return $androidDirectory
    }

    $androidJunction = Join-Path $env:LOCALAPPDATA "Fluvora\android-source"
    if (Test-Path -LiteralPath $androidJunction) {
        $junctionItem = Get-Item -LiteralPath $androidJunction -Force
        $resolvedTarget = [IO.Path]::GetFullPath([string] $junctionItem.Target)
        if ($resolvedTarget -ne [IO.Path]::GetFullPath($androidDirectory)) {
            throw "Android ASCII junction points to an unexpected target: $androidJunction"
        }
    }
    else {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $androidJunction) |
            Out-Null
        New-Item -ItemType Junction -Path $androidJunction -Target $androidDirectory | Out-Null
    }
    $androidJunction
}
