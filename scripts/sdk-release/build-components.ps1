function Build-SdkComponent([pscustomobject] $Context) {
    $artifactDirectory = Join-Path $Context.StageDirectory "artifacts"
    New-Item -ItemType Directory -Force -Path $artifactDirectory | Out-Null
    switch ($Context.Sdk) {
        "web" {
            Push-Location (Join-Path $Context.ProjectRoot "sdk\web")
            try {
                Invoke-ReleaseCommand "npm" @("ci")
                Invoke-ReleaseCommand "npm" @("run", "check")
                Invoke-ReleaseCommand "npm" @("test")
                Invoke-ReleaseCommand "npm" @("pack", "--pack-destination", $artifactDirectory)
            } finally { Pop-Location }
        }
        "rust" {
            Invoke-ReleaseCommand "cargo" @("test", "-p", "fluvora-sdk", "--locked")
            Invoke-ReleaseCommand "cargo" @("build", "--release", "-p", "fluvora-sdk", "--example", "room_client", "--locked")
            Copy-Item (Join-Path $Context.ProjectRoot "target\release\examples\room_client$($Context.ExecutableSuffix)") $artifactDirectory
        }
        "c-abi" {
            Invoke-ReleaseCommand "cargo" @("test", "-p", "fluvora-c-abi", "--locked")
            Invoke-ReleaseCommand "cargo" @("build", "--release", "-p", "fluvora-c-abi", "--locked")
            Copy-Item (Join-Path $Context.ProjectRoot "sdk\c-abi\include\fluvora.h") $artifactDirectory
            Get-ChildItem (Join-Path $Context.ProjectRoot "target\release") -File |
                Where-Object Name -Match '^(lib)?fluvora_c_abi\.(a|so|dylib|dll|lib)$' |
                Copy-Item -Destination $artifactDirectory
        }
        "android" {
            if ($Context.IsWindows -and -not $env:ANDROID_HOME) {
                $defaultAndroidSdk = Join-Path $env:LOCALAPPDATA "Android\Sdk"
                if (Test-Path $defaultAndroidSdk -PathType Container) {
                    $env:ANDROID_HOME = $defaultAndroidSdk
                }
            }
            $androidDirectory = Resolve-AndroidDirectory $Context
            $wrapper = Join-Path $androidDirectory $(if ($Context.IsWindows) { "gradlew.bat" } else { "gradlew" })
            Push-Location $androidDirectory
            try { Invoke-ReleaseCommand $wrapper @("--no-daemon", "--no-watch-fs", ":fluvora:testDebugUnitTest", ":fluvora:assembleRelease", ":demo:assembleDebug") }
            finally { Pop-Location }
            Copy-Item (Join-Path $Context.ProjectRoot "sdk\android\fluvora\build\outputs\aar\fluvora-release.aar") (Join-Path $artifactDirectory "fluvora-$($Context.Version).aar")
            Copy-Item (Join-Path $Context.ProjectRoot "sdk\android\demo\build\outputs\apk\debug\demo-debug.apk") (Join-Path $artifactDirectory "fluvora-demo-$($Context.Version)-debug.apk")
        }
        "swift" {
            if (-not (Get-Command swift -ErrorAction SilentlyContinue)) { throw "Swift SDK builds require a Swift-capable macOS host" }
            Invoke-ReleaseCommand "swift" @("test", "--package-path", "sdk/ios")
            Invoke-ReleaseCommand "swift" @("build", "-c", "release", "--package-path", "sdk/ios", "--product", "fluvora-swift-demo")
            Copy-Item (Join-Path $Context.ProjectRoot "sdk\ios\.build\release\fluvora-swift-demo") $artifactDirectory
        }
    }
}
