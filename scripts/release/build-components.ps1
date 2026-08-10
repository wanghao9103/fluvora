function Build-CoreComponents(
    [pscustomobject] $Context,
    [pscustomobject] $Layout,
    [switch] $SkipVendoredOpenSsl
) {
    Invoke-ReleaseCommand "cargo" @("build", "--release", "--workspace", "--locked")
    Invoke-ReleaseCommand "cargo" @(
        "build", "--release", "-p", "fluvora-sdk", "--example", "room_client", "--locked"
    )

    $releaseDirectory = Join-Path $Context.ProjectRoot "target\release"
    $mediaNodeSource = Join-Path $releaseDirectory (
        "fluvora-media-node" + $Context.ExecutableSuffix
    )
    $productionDtls = $false
    if ($Context.IsWindows -and -not $SkipVendoredOpenSsl) {
        $vendoredTargetDirectory = Join-Path $env:LOCALAPPDATA "Fluvora\release-target"
        $savedCargoTargetDirectory = $env:CARGO_TARGET_DIR
        try {
            & (Join-Path $Context.ProjectRoot "scripts\check-openssl-vendored.ps1") `
                -TargetDirectory $vendoredTargetDirectory -BuildRelease
            $mediaNodeSource = Join-Path $vendoredTargetDirectory (
                "release\fluvora-media-node.exe"
            )
            $productionDtls = $true
        }
        finally {
            if ($null -eq $savedCargoTargetDirectory) {
                Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
            }
            else {
                $env:CARGO_TARGET_DIR = $savedCargoTargetDirectory
            }
        }
    }
    elseif (-not $Context.IsWindows) {
        Invoke-ReleaseCommand "cargo" @(
            "build", "--release", "-p", "fluvora-media-node",
            "--features", "openssl-backend", "--locked"
        )
        $productionDtls = $true
    }

    $serverBinaries = @(
        "fluvora-api-server",
        "fluvora-event-dispatcher",
        "fluvora-media-gateway",
        "fluvora-media-node",
        "fluvora-media-worker",
        "fluvora-status-service",
        "fluvora-turn-server"
    )
    foreach ($binary in $serverBinaries) {
        $sourcePath = if ($binary -eq "fluvora-media-node") {
            $mediaNodeSource
        }
        else {
            Join-Path $releaseDirectory "$binary$($Context.ExecutableSuffix)"
        }
        if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
            throw "missing release binary: $sourcePath"
        }
        Copy-Item -LiteralPath $sourcePath -Destination $Layout.Server
    }

    $toolBinaries = @("fluvora-admin", "fluvora-perf-lab", "fluvora-turn-probe")
    foreach ($binary in $toolBinaries) {
        $sourcePath = Join-Path $releaseDirectory "$binary$($Context.ExecutableSuffix)"
        if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
            throw "missing release tool: $sourcePath"
        }
        Copy-Item -LiteralPath $sourcePath -Destination $Layout.Tools
    }

    $rustSdkDirectory = Join-Path $Layout.Sdk "rust"
    New-Item -ItemType Directory -Force -Path $rustSdkDirectory | Out-Null
    Copy-Item -LiteralPath (Join-Path $releaseDirectory (
        "examples\room_client$($Context.ExecutableSuffix)"
    )) -Destination $rustSdkDirectory

    $cAbiDirectory = Join-Path $Layout.Sdk "c-abi"
    $cAbiLibraryDirectory = Join-Path $cAbiDirectory "lib"
    New-Item -ItemType Directory -Force -Path $cAbiLibraryDirectory | Out-Null
    Copy-Item -LiteralPath (
        Join-Path $Context.ProjectRoot "sdk\c-abi\include\fluvora.h"
    ) -Destination $cAbiDirectory
    $cAbiNames = if ($Context.IsWindows) {
        @("fluvora_c_abi.dll", "fluvora_c_abi.dll.lib", "fluvora_c_abi.lib")
    }
    elseif ($Context.IsLinux) {
        @("libfluvora_c_abi.so", "libfluvora_c_abi.a")
    }
    else {
        @("libfluvora_c_abi.dylib", "libfluvora_c_abi.a")
    }
    $copiedCAbiLibraries = [System.Collections.Generic.List[string]]::new()
    foreach ($libraryName in $cAbiNames) {
        $sourcePath = Join-Path $releaseDirectory $libraryName
        if (Test-Path -LiteralPath $sourcePath -PathType Leaf) {
            Copy-Item -LiteralPath $sourcePath -Destination $cAbiLibraryDirectory
            $copiedCAbiLibraries.Add($libraryName)
        }
    }
    if ($copiedCAbiLibraries.Count -eq 0) {
        throw "no C ABI release libraries were produced"
    }

    [pscustomobject]@{
        ServerBinaries = $serverBinaries
        ToolBinaries = $toolBinaries
        CAbiLibraries = @($copiedCAbiLibraries)
        ProductionDtls = $productionDtls
    }
}

function Build-WebSdk([pscustomobject] $Context, [pscustomobject] $Layout) {
    $destination = Join-Path $Layout.Sdk "web"
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    Push-Location -LiteralPath (Join-Path $Context.ProjectRoot "sdk\web")
    try {
        Invoke-ReleaseCommand "npm" @("ci")
        Invoke-ReleaseCommand "npm" @("run", "check")
        Invoke-ReleaseCommand "npm" @("test")
        Invoke-ReleaseCommand "npm" @("pack", "--pack-destination", $destination)
    }
    finally {
        Pop-Location
    }
    "built-and-tested"
}

function Build-AndroidSdk(
    [pscustomobject] $Context,
    [pscustomobject] $Layout,
    [switch] $Skip
) {
    if ($Skip) {
        return "source-only"
    }
    if ($Context.IsWindows -and -not $env:ANDROID_HOME) {
        $defaultAndroidSdk = Join-Path $env:LOCALAPPDATA "Android\Sdk"
        if (Test-Path -LiteralPath $defaultAndroidSdk -PathType Container) {
            $env:ANDROID_HOME = $defaultAndroidSdk
        }
    }

    $androidDirectory = Resolve-AndroidDirectory -Context $Context
    $wrapperName = if ($Context.IsWindows) { "gradlew.bat" } else { "gradlew" }
    $gradleWrapper = Join-Path $androidDirectory $wrapperName
    if (-not (Test-Path -LiteralPath $gradleWrapper -PathType Leaf)) {
        return "source-only; Gradle wrapper unavailable"
    }

    Push-Location -LiteralPath $androidDirectory
    try {
        Invoke-ReleaseCommand $gradleWrapper @(
            "--no-daemon",
            "--no-watch-fs",
            ":fluvora:testDebugUnitTest",
            ":fluvora:assembleRelease",
            ":demo:assembleDebug"
        )
    }
    finally {
        Pop-Location
    }

    $destination = Join-Path $Layout.Sdk "android"
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    Copy-Item -LiteralPath (Join-Path $Context.ProjectRoot (
        "sdk\android\fluvora\build\outputs\aar\fluvora-release.aar"
    )) -Destination (Join-Path $destination "fluvora-$($Context.Version).aar")
    Copy-Item -LiteralPath (Join-Path $Context.ProjectRoot (
        "sdk\android\demo\build\outputs\apk\debug\demo-debug.apk"
    )) -Destination (Join-Path $destination "fluvora-demo-$($Context.Version)-debug.apk")
    "built-and-tested"
}

function Build-SwiftSdk([pscustomobject] $Context, [pscustomobject] $Layout) {
    if (-not (Get-Command swift -ErrorAction SilentlyContinue)) {
        return "source-only; build is verified by macOS CI"
    }
    Invoke-ReleaseCommand "swift" @("test", "--package-path", "sdk/ios")
    Invoke-ReleaseCommand "swift" @(
        "build", "-c", "release", "--package-path", "sdk/ios",
        "--product", "fluvora-swift-demo"
    )
    $destination = Join-Path $Layout.Sdk "swift"
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    Copy-Item -LiteralPath (Join-Path $Context.ProjectRoot (
        "sdk\ios\.build\release\fluvora-swift-demo$($Context.ExecutableSuffix)"
    )) -Destination $destination
    "built-and-tested"
}
