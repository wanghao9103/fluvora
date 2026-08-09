param(
    [string] $TurnServer = (Join-Path (Split-Path -Parent $PSScriptRoot) (
        "target/debug/fluvora-turn-server" + $(if ($env:OS -eq "Windows_NT") { ".exe" } else { "" })
    )),
    [string] $TurnProbe = (Join-Path (Split-Path -Parent $PSScriptRoot) (
        "target/debug/fluvora-turn-probe" + $(if ($env:OS -eq "Windows_NT") { ".exe" } else { "" })
    )),
    [string] $EvidenceDirectory = ""
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$runRoot = Join-Path $projectRoot ".tmp-turn-smoke-$PID"
$serverProcess = $null
$onWindows = $env:OS -eq "Windows_NT"
$portOffset = $PID % 100
$turnPort = 30000 + $portOffset
$tlsPort = 32000 + $portOffset
$statusPort = 34000 + $portOffset
$relayPortMinimum = 40000 + ($portOffset * 100)
$relayPortMaximum = $relayPortMinimum + 19

function Wait-ForUrl([string] $Url) {
    foreach ($attempt in 1..100) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $Url -TimeoutSec 1 | Out-Null
            return
        }
        catch {
            Start-Sleep -Milliseconds 100
        }
    }
    throw "timed out waiting for $Url"
}

function Stop-OwnedProcess($Process) {
    if ($null -ne $Process) {
        if (-not $Process.HasExited) {
            Stop-Process -Id $Process.Id -Force
        }
        $Process.WaitForExit()
    }
}

New-Item -ItemType Directory -Force -Path $runRoot | Out-Null
try {
    foreach ($executable in @($TurnServer, $TurnProbe)) {
        if (-not (Test-Path -LiteralPath $executable)) {
            throw "required executable was not built: $executable"
        }
    }
    $openssl = @(
        "C:\Program Files\Git\usr\bin\openssl.exe",
        "C:\Program Files\Git\mingw64\bin\openssl.exe"
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $openssl) {
        $opensslCommand = Get-Command openssl -ErrorAction SilentlyContinue
        if ($opensslCommand) {
            $openssl = $opensslCommand.Source
        }
    }
    if (-not $openssl) {
        throw "OpenSSL executable was not found"
    }

    $certificate = Join-Path $runRoot "turn-cert.pem"
    $privateKey = Join-Path $runRoot "turn-key.pem"
    $savedErrorAction = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -sha256 -nodes `
        -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" `
        -addext "basicConstraints=critical,CA:FALSE" `
        -addext "keyUsage=critical,digitalSignature" `
        -addext "extendedKeyUsage=serverAuth" `
        -days 1 -keyout $privateKey -out $certificate 2>$null
    $opensslExitCode = $LASTEXITCODE
    $ErrorActionPreference = $savedErrorAction
    if ($opensslExitCode -ne 0) {
        throw "failed to create the TURN smoke certificate"
    }

    $env:FLUVORA_TURN_BIND = "127.0.0.1:$turnPort"
    $env:FLUVORA_TURN_TLS_BIND = "127.0.0.1:$tlsPort"
    $env:FLUVORA_TURN_STATUS_BIND = "127.0.0.1:$statusPort"
    $env:FLUVORA_TURN_ADVERTISED_IP = "127.0.0.1"
    $env:FLUVORA_TURN_RELAY_BIND_IP = "127.0.0.1"
    $env:FLUVORA_TURN_RELAY_PORT_MIN = "$relayPortMinimum"
    $env:FLUVORA_TURN_RELAY_PORT_MAX = "$relayPortMaximum"
    $env:FLUVORA_TURN_ALLOW_PRIVATE_PEERS = "true"
    $env:FLUVORA_TURN_MAX_RELAY_BYTES_PER_SECOND = "5000000"
    $env:FLUVORA_TURN_MAX_ALLOCATIONS = "16"
    $env:FLUVORA_TURN_MAX_ALLOCATIONS_PER_IP = "16"
    $env:FLUVORA_TURN_USERNAME = "turn-smoke"
    $env:FLUVORA_TURN_PASSWORD = "turn-smoke-password"
    $env:FLUVORA_TURN_REALM = "turn-smoke.local"
    $env:FLUVORA_TURN_NONCE_SECRET = "turn-smoke-nonce-secret-32-bytes-minimum"
    $env:FLUVORA_TURN_REST_SECRET = "turn-smoke-rest-secret-32-bytes-minimum"
    $env:FLUVORA_TURN_TLS_CERT = $certificate
    $env:FLUVORA_TURN_TLS_KEY = $privateKey

    $serverStart = @{
        FilePath = $TurnServer
        PassThru = $true
        RedirectStandardOutput = Join-Path $runRoot "turn.stdout.log"
        RedirectStandardError = Join-Path $runRoot "turn.stderr.log"
    }
    if ($onWindows) {
        $serverStart.WindowStyle = "Hidden"
    }
    $serverProcess = Start-Process @serverStart
    Wait-ForUrl "http://127.0.0.1:$statusPort/health/live"

    $evidenceRoot = if ($EvidenceDirectory) {
        $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($EvidenceDirectory)
    }
    else {
        Join-Path $projectRoot ("artifacts\turn-smoke-{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), $PID)
    }
    New-Item -ItemType Directory -Force -Path $evidenceRoot | Out-Null

    foreach ($transport in @("udp", "tcp", "tls")) {
        $port = if ($transport -eq "tls") { $tlsPort } else { $turnPort }
        $arguments = @(
            "probe",
            "--transport", $transport,
            "--server", "127.0.0.1:$port",
            "--username", "turn-smoke",
            "--password", "turn-smoke-password",
            "--realm", "turn-smoke.local",
            "--timeout-ms", "5000",
            "--evidence", (Join-Path $evidenceRoot "$transport.json")
        )
        if ($transport -eq "tls") {
            $arguments += @("--server-name", "localhost", "--ca-pem", $certificate)
        }
        & $TurnProbe @arguments
        if ($LASTEXITCODE -ne 0) {
            throw "TURN $transport probe failed with exit code $LASTEXITCODE"
        }
    }

    $metricText = (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$statusPort/metrics").Content
    foreach ($metric in @(
        "fluvora_turn_allocations_created_total",
        "fluvora_turn_client_bytes_relayed_total",
        "fluvora_turn_peer_bytes_relayed_total"
    )) {
        if ($metricText -notmatch "$metric [1-9][0-9]*") {
            throw "TURN smoke did not produce a non-zero $metric metric"
        }
    }
    Write-Host "TURN UDP/TCP/TLS smoke passed; evidence: $evidenceRoot"
}
catch {
    foreach ($log in @("turn.stdout.log", "turn.stderr.log")) {
        $path = Join-Path $runRoot $log
        if (Test-Path -LiteralPath $path) {
            Write-Host "===== $log"
            Get-Content -LiteralPath $path -Tail 200
        }
    }
    throw
}
finally {
    Stop-OwnedProcess $serverProcess
    if (Test-Path -LiteralPath $runRoot) {
        Remove-Item -LiteralPath $runRoot -Recurse -Force
    }
}
