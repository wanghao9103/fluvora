$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot

$layerByPackage = @{
    # Stable domain, codecs, wire models, and dependency-free capabilities.
    "fluvora-bytes-codec" = 0
    "fluvora-data-channel" = 0
    "fluvora-domain" = 0
    "fluvora-media-codec" = 0
    "fluvora-observability" = 0
    "fluvora-protocol" = 0
    "fluvora-rtc-datagram" = 0
    "fluvora-rtcp" = 0
    "fluvora-rtp" = 0
    "fluvora-sdp" = 0
    "fluvora-transcode-bridge" = 0
    "fluvora-turn" = 0

    # Protocol engines and infrastructure adapters.
    "fluvora-auth" = 1
    "fluvora-congestion-control" = 1
    "fluvora-control-store" = 1
    "fluvora-dtls-adapter" = 1
    "fluvora-ice-lite" = 1
    "fluvora-media-pipeline" = 1
    "fluvora-media-store" = 1
    "fluvora-rtc-session" = 1
    "fluvora-sfu-core" = 1
    "fluvora-srtp" = 1
    "fluvora-stun" = 1

    # Shared control-plane services.
    "fluvora-event-dispatcher" = 2
    "fluvora-status-client" = 2
    "fluvora-status-service" = 2

    # Executables, SDK boundaries, and tooling.
    "fluvora-admin" = 3
    "fluvora-api-server" = 3
    "fluvora-c-abi" = 3
    "fluvora-media-gateway" = 3
    "fluvora-media-node" = 3
    "fluvora-media-worker" = 3
    "fluvora-perf-lab" = 3
    "fluvora-sdk" = 3
    "fluvora-turn-server" = 3
}

$packagesByDirectory = [ordered]@{
    "foundation" = @(
        "fluvora-bytes-codec",
        "fluvora-domain",
        "fluvora-observability",
        "fluvora-protocol"
    )
    "webrtc" = @(
        "fluvora-congestion-control",
        "fluvora-data-channel",
        "fluvora-dtls-adapter",
        "fluvora-ice-lite",
        "fluvora-media-codec",
        "fluvora-rtc-datagram",
        "fluvora-rtc-session",
        "fluvora-rtcp",
        "fluvora-rtp",
        "fluvora-sdp",
        "fluvora-sfu-core",
        "fluvora-srtp",
        "fluvora-stun",
        "fluvora-turn"
    )
    "media" = @(
        "fluvora-media-pipeline",
        "fluvora-media-store",
        "fluvora-transcode-bridge"
    )
    "control-plane" = @(
        "fluvora-auth",
        "fluvora-control-store",
        "fluvora-event-dispatcher",
        "fluvora-status-client",
        "fluvora-status-service"
    )
    "services" = @(
        "fluvora-api-server",
        "fluvora-media-gateway",
        "fluvora-media-node",
        "fluvora-media-worker",
        "fluvora-turn-server"
    )
    "tools" = @(
        "fluvora-admin",
        "fluvora-perf-lab"
    )
}

# Existing large entrypoints are migration budgets, not preferred sizes. New logic
# should go into focused modules and these limits should only move downward.
$entrypointLineBudgets = @{
    "crates/services/api-server/src/main.rs" = 40
    "crates/control-plane/event-dispatcher/src/main.rs" = 450
    "crates/services/media-gateway/src/main.rs" = 2720
    "crates/services/media-node/src/main.rs" = 1700
    "crates/services/media-worker/src/main.rs" = 1720
}

# Internal API layering is an enforced contract: the composition root stays tiny,
# transport handlers live under routes, shared wire/state models under models, and
# cross-adapter workflows under services.
$apiModuleLineBudgets = @{
    "crates/services/api-server/src/app.rs" = 500
    "crates/services/api-server/src/runtime.rs" = 100
    "crates/services/api-server/src/models/media.rs" = 250
    "crates/services/api-server/src/models/rooms.rs" = 120
    "crates/services/api-server/src/models/signaling.rs" = 100
    "crates/services/api-server/src/models/state.rs" = 150
    "crates/services/api-server/src/models/webrtc.rs" = 100
    "crates/services/api-server/src/routes/media.rs" = 550
    "crates/services/api-server/src/routes/rooms.rs" = 500
    "crates/services/api-server/src/routes/signaling.rs" = 300
    "crates/services/api-server/src/routes/webrtc.rs" = 450
    "crates/services/api-server/src/services/media_orchestration.rs" = 550
    "crates/services/api-server/src/services/media_sessions.rs" = 150
    "crates/services/api-server/src/services/room_commands.rs" = 400
    "crates/services/api-server/src/services/room_state.rs" = 120
}

Push-Location -LiteralPath $projectRoot
try {
    $metadataJson = & cargo metadata --format-version 1 --no-deps
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE"
    }
    $metadata = $metadataJson | ConvertFrom-Json
    $workspacePackages = @{}
    foreach ($package in $metadata.packages) {
        $workspacePackages[$package.name] = $package
    }

    $violations = [System.Collections.Generic.List[string]]::new()
    foreach ($package in $metadata.packages) {
        if (-not $layerByPackage.ContainsKey($package.name)) {
            $violations.Add("unclassified workspace package: $($package.name)")
            continue
        }

        $manifest = Get-Content -LiteralPath $package.manifest_path -Raw
        if ($manifest -notmatch '(?ms)^\[lints\]\s*^workspace\s*=\s*true\s*$') {
            $violations.Add("$($package.name) must inherit [workspace.lints]")
        }

        $packageLayer = $layerByPackage[$package.name]
        foreach ($dependency in $package.dependencies) {
            if (-not $workspacePackages.ContainsKey($dependency.name)) {
                continue
            }
            $dependencyLayer = $layerByPackage[$dependency.name]
            if ($null -eq $dependencyLayer) {
                $violations.Add(
                    "$($package.name) depends on unclassified package $($dependency.name)"
                )
            }
            elseif ($dependencyLayer -gt $packageLayer) {
                $violations.Add(
                    "outward dependency: $($package.name) (L$packageLayer) -> " +
                    "$($dependency.name) (L$dependencyLayer)"
                )
            }
        }
    }

    foreach ($directory in $packagesByDirectory.Keys) {
        foreach ($packageName in $packagesByDirectory[$directory]) {
            if (-not $workspacePackages.ContainsKey($packageName)) {
                $violations.Add("missing package from crates/$directory`: $packageName")
                continue
            }
            $packageDirectory = Split-Path -Parent $workspacePackages[$packageName].manifest_path
            $expectedDirectory = Join-Path $projectRoot (
                "crates/{0}/{1}" -f $directory, $packageName.Replace("fluvora-", "")
            )
            if (
                [IO.Path]::GetFullPath($packageDirectory) -ne
                [IO.Path]::GetFullPath($expectedDirectory)
            ) {
                $violations.Add(
                    "$packageName belongs in crates/$directory, found at $packageDirectory"
                )
            }
        }
    }

    foreach ($entry in $entrypointLineBudgets.GetEnumerator()) {
        $path = Join-Path $projectRoot $entry.Key
        $lineCount = (Get-Content -LiteralPath $path).Count
        if ($lineCount -gt $entry.Value) {
            $violations.Add(
                "$($entry.Key) has $lineCount lines; budget is $($entry.Value). " +
                "Move logic into a focused module."
            )
        }
    }

    foreach ($entry in $apiModuleLineBudgets.GetEnumerator()) {
        $path = Join-Path $projectRoot $entry.Key
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            $violations.Add("missing layered API module: $($entry.Key)")
            continue
        }
        $lineCount = (Get-Content -LiteralPath $path).Count
        if ($lineCount -gt $entry.Value) {
            $violations.Add(
                "$($entry.Key) has $lineCount lines; focused-module budget is $($entry.Value). " +
                "Split the capability instead of growing a mixed module."
            )
        }
    }

    $apiSourceDirectory = Join-Path $projectRoot "crates/services/api-server/src"
    $apiSourceFiles = @(Get-ChildItem -LiteralPath $apiSourceDirectory -Recurse -File -Filter "*.rs")
    foreach ($sourceFile in $apiSourceFiles) {
        $relativePath = $sourceFile.FullName.Substring($projectRoot.Length + 1)
        $source = Get-Content -LiteralPath $sourceFile.FullName -Raw
        if ($source -match '(?m)^use (?:crate|super)::\*;\r?$') {
            $violations.Add(
                "$relativePath has a production wildcard import; import dependencies explicitly."
            )
        }
        if ($source -match '(?m)^#\[allow\(clippy::too_many_arguments\)\]\r?$') {
            $violations.Add(
                "$relativePath suppresses too_many_arguments; introduce a request/context type."
            )
        }
    }

    $apiRootSourceFiles = @(
        Get-ChildItem -LiteralPath $apiSourceDirectory -File -Filter "*.rs"
    )
    foreach ($sourceFile in $apiRootSourceFiles) {
        $relativePath = $sourceFile.FullName.Substring($projectRoot.Length + 1)
        $source = Get-Content -LiteralPath $sourceFile.FullName -Raw
        if ($source -match '(?m)^use super::') {
            $violations.Add(
                "$relativePath is a root module and must use explicit crate:: imports."
            )
        }
    }

    if ($violations.Count -ne 0) {
        $violations | ForEach-Object { Write-Error $_ }
        throw "architecture check failed with $($violations.Count) violation(s)"
    }

    Write-Host (
        (
            "Architecture check passed: {0} packages classified, " +
            "6 directories verified, {1} entrypoint budgets, {2} API module budgets, " +
            "and {3} API source files style-checked."
        ) -f
        $metadata.packages.Count,
        $entrypointLineBudgets.Count,
        $apiModuleLineBudgets.Count,
        $apiSourceFiles.Count
    )
}
finally {
    Pop-Location
}
