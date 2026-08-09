param(
    [ValidateSet("quick", "full")]
    [string] $Profile = "quick",
    [string] $EvidenceDirectory = "",
    [switch] $SkipBrowser
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$startedAt = [DateTimeOffset]::UtcNow
$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$evidenceRoot = if ($EvidenceDirectory) {
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($EvidenceDirectory)
}
else {
    Join-Path $projectRoot "artifacts\release-gates-$timestamp-$PID"
}
$logsRoot = Join-Path $evidenceRoot "logs"
$results = [System.Collections.Generic.List[object]]::new()

function Invoke-External([string] $FilePath, [string[]] $Arguments) {
    $savedErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        & $FilePath @Arguments
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $savedErrorAction
    }
    if ($exitCode -ne 0) {
        throw "$FilePath exited with code $exitCode"
    }
}

function Invoke-Gate([string] $Name, [scriptblock] $Action) {
    $gateStarted = [DateTimeOffset]::UtcNow
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $logName = ($Name -replace '[^a-zA-Z0-9_.-]', '-') + ".log"
    $logPath = Join-Path $logsRoot $logName
    $status = "pass"
    $detail = ""
    try {
        $output = & $Action *>&1 | Out-String
        Set-Content -LiteralPath $logPath -Value $output -Encoding utf8
    }
    catch {
        $status = "fail"
        $detail = $_.Exception.Message
        $failureOutput = ($_ | Out-String)
        Set-Content -LiteralPath $logPath -Value $failureOutput -Encoding utf8
    }
    finally {
        $stopwatch.Stop()
    }
    $results.Add([pscustomobject]@{
        name = $Name
        status = $status
        startedAt = $gateStarted.ToString("O")
        durationMillis = $stopwatch.ElapsedMilliseconds
        log = "logs/$logName"
        detail = $detail
    })
    Write-Host ("[{0}] {1} ({2} ms)" -f $status.ToUpperInvariant(), $Name, $stopwatch.ElapsedMilliseconds)
}

function Command-Version([string] $FilePath, [string[]] $Arguments) {
    $savedErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $lines = @(& $FilePath @Arguments 2>$null)
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0 -or $lines.Count -eq 0) {
            return "unavailable"
        }
        return [string] $lines[0]
    }
    catch {
        return "unavailable"
    }
    finally {
        $ErrorActionPreference = $savedErrorAction
    }
}

New-Item -ItemType Directory -Force -Path $logsRoot | Out-Null
Push-Location -LiteralPath $projectRoot
try {
    Invoke-Gate "architecture" {
        & (Join-Path $PSScriptRoot "check-architecture.ps1")
    }
    Invoke-Gate "documentation" {
        & (Join-Path $PSScriptRoot "check-docs.ps1")
    }
    Invoke-Gate "rust-format" {
        Invoke-External "cargo" @("fmt", "--all", "--", "--check")
    }
    Invoke-Gate "rust-clippy" {
        Invoke-External "cargo" @(
            "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"
        )
    }
    Invoke-Gate "rust-tests" {
        Invoke-External "cargo" @("test", "--workspace", "--locked")
    }
    Invoke-Gate "sdk-contract" {
        Invoke-External "node" @("scripts/check-sdk-contract.mjs")
    }
    Invoke-Gate "sdk-demo-contract" {
        Invoke-External "node" @("scripts/check-sdk-demos.mjs")
    }
    Invoke-Gate "web-sdk" {
        Push-Location -LiteralPath (Join-Path $projectRoot "sdk\web")
        try {
            Invoke-External "npm" @("run", "check")
            Invoke-External "npm" @("test")
        }
        finally {
            Pop-Location
        }
    }
    Invoke-Gate "turn-udp-tcp-tls" {
        Invoke-External "cargo" @(
            "build", "-p", "fluvora-turn-server", "--bins", "--locked"
        )
        & (Join-Path $PSScriptRoot "smoke-turn.ps1") `
            -EvidenceDirectory (Join-Path $evidenceRoot "turn")
    }

    if ($Profile -eq "full") {
        Invoke-Gate "openssl-vendored" {
            & (Join-Path $PSScriptRoot "check-openssl-vendored.ps1")
        }
        Invoke-Gate "realtime-transcode" {
            & (Join-Path $PSScriptRoot "smoke-realtime-transcode.ps1")
        }
        Invoke-Gate "vod-live-hls" {
            & (Join-Path $PSScriptRoot "smoke-hls-pipelines.ps1")
        }
        Invoke-Gate "media-capacity" {
            Invoke-External "cargo" @(
                "run", "--release", "-p", "fluvora-perf-lab", "--locked", "--", "--assert"
            )
        }
        if (-not $SkipBrowser) {
            Invoke-Gate "browser-and-control-soak" {
                & (Join-Path $PSScriptRoot "run-browser-interop.ps1") `
                    -Browser chromium -SoakSeconds 30
            }
        }
    }
}
finally {
    Pop-Location
}

$failed = @($results | Where-Object { $_.status -ne "pass" })
$finishedAt = [DateTimeOffset]::UtcNow
$summary = [ordered]@{
    schemaVersion = 1
    project = "Fluvora"
    profile = $Profile
    status = if ($failed.Count -eq 0) { "pass" } else { "fail" }
    startedAt = $startedAt.ToString("O")
    finishedAt = $finishedAt.ToString("O")
    durationMillis = [long] ($finishedAt - $startedAt).TotalMilliseconds
    environment = [ordered]@{
        operatingSystem = [System.Environment]::OSVersion.VersionString
        architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        rustc = Command-Version "rustc" @("--version")
        node = Command-Version "node" @("--version")
        ffmpeg = Command-Version "ffmpeg" @("-version")
    }
    gates = $results
}
$summaryPath = Join-Path $evidenceRoot "release-gates.json"
$summary | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $summaryPath -Encoding utf8
Write-Host "Release evidence: $summaryPath"
if ($failed.Count -ne 0) {
    $names = ($failed | ForEach-Object name) -join ", "
    throw "release gates failed: $names"
}
