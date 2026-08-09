param(
    [ValidateSet("chromium", "firefox", "webkit")]
    [string] $Browser = "chromium",
    [string] $MediaNode = (Join-Path $env:LOCALAPPDATA "Fluvora\target\debug\fluvora-media-node.exe"),
    [string] $Grep = "",
    [ValidateRange(0, 3600)]
    [int] $SoakSeconds = 0,
    [switch] $SkipLoad
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$runRoot = Join-Path $projectRoot ".tmp-browser-interop-$PID"
$media = $null
$api = $null
$web = $null

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

New-Item -ItemType Directory -Force -Path $runRoot, (Join-Path $runRoot "state") | Out-Null
try {
    $openssl = @(
        "C:\Program Files\Git\usr\bin\openssl.exe",
        "C:\Program Files\Git\mingw64\bin\openssl.exe"
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $openssl) {
        throw "OpenSSL executable was not found"
    }
    foreach ($executable in @(
        $MediaNode,
        (Join-Path $projectRoot "target\debug\fluvora-api-server.exe"),
        (Join-Path $projectRoot "target\debug\fluvora-admin.exe")
    )) {
        if (-not (Test-Path -LiteralPath $executable)) {
            throw "required executable was not built: $executable"
        }
    }

    $certificate = Join-Path $runRoot "cert.pem"
    $privateKey = Join-Path $runRoot "key.pem"
    $fingerprint = Join-Path $runRoot "fingerprint.txt"
    $savedErrorAction = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -sha256 -nodes `
        -subj "/CN=localhost" -days 1 -keyout $privateKey -out $certificate 2>$null
    $opensslExitCode = $LASTEXITCODE
    $ErrorActionPreference = $savedErrorAction
    if ($opensslExitCode -ne 0) {
        throw "failed to create the browser interop certificate"
    }

    Remove-Item Env:FLUVORA_STATUS_URL -ErrorAction SilentlyContinue
    $env:FLUVORA_MEDIA_UDP_BIND = "127.0.0.1:51000"
    $env:FLUVORA_MEDIA_CONTROL_BIND = "127.0.0.1:18092"
    $env:FLUVORA_MEDIA_CONTROL_TOKEN = "browser-e2e-media-control-token"
    $env:FLUVORA_DTLS_CERT_PEM = $certificate
    $env:FLUVORA_DTLS_KEY_PEM = $privateKey
    $env:FLUVORA_DTLS_FINGERPRINT_FILE = $fingerprint
    $media = Start-Process -FilePath $MediaNode -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $runRoot "media.stdout.log") `
        -RedirectStandardError (Join-Path $runRoot "media.stderr.log")
    Wait-ForUrl "http://127.0.0.1:18092/health/live"

    $env:FLUVORA_API_BIND = "127.0.0.1:18080"
    $env:FLUVORA_TOKEN_SECRET = "browser-e2e-access-token-secret-32-bytes-minimum"
    $env:FLUVORA_GATEWAY_TOKEN = "browser-e2e-gateway-token"
    $env:FLUVORA_WORKER_TOKEN = "browser-e2e-worker-token"
    $env:FLUVORA_TURN_REST_SECRET = "browser-e2e-turn-rest-secret-32-bytes-minimum"
    $env:FLUVORA_GIFT_WEBHOOK_SECRET = "browser-e2e-gift-webhook-secret-32-bytes-minimum"
    $env:FLUVORA_ICE_CANDIDATE = "1 1 UDP 2130706431 127.0.0.1 51000 typ host"
    $env:FLUVORA_MEDIA_CONTROL_URL = "http://127.0.0.1:18092"
    $env:FLUVORA_GATEWAY_URL = "http://127.0.0.1:18193"
    $env:FLUVORA_WORKER_URL = "http://127.0.0.1:18191"
    $env:FLUVORA_STATE_DIR = Join-Path $runRoot "state"
    $env:FLUVORA_CORS_ORIGINS = "http://127.0.0.1:18000"
    $api = Start-Process -FilePath (Join-Path $projectRoot "target\debug\fluvora-api-server.exe") `
        -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $runRoot "api.stdout.log") `
        -RedirectStandardError (Join-Path $runRoot "api.stderr.log")
    Wait-ForUrl "http://127.0.0.1:18080/health/live"

    $web = Start-Process -FilePath "python" `
        -ArgumentList @("-m", "http.server", "18000", "--bind", "127.0.0.1", "--directory", $projectRoot) `
        -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $runRoot "web.stdout.log") `
        -RedirectStandardError (Join-Path $runRoot "web.stderr.log")
    Wait-ForUrl "http://127.0.0.1:18000/tests/browser/"

    $admin = Join-Path $projectRoot "target\debug\fluvora-admin.exe"
    $token = (& $admin token --subject 1 --room "*" --ttl 900 --scopes all).Trim()
    $secondToken = (& $admin token --subject 2 --room "*" --ttl 900 --scopes all).Trim()
    if (-not $SkipLoad) {
        $env:FLUVORA_LOAD_TOKEN = $token
        $savedErrorAction = $ErrorActionPreference
        try {
            # Native tools legitimately use stderr for warnings and progress. Their exit code,
            # rather than PowerShell's NativeCommandError wrapper, determines gate success.
            $ErrorActionPreference = "Continue"
            if ($SoakSeconds -gt 0) {
                & node (Join-Path $projectRoot "scripts\load-control-plane.mjs") `
                    --concurrency 16 --iterations 10000000 --duration-seconds $SoakSeconds `
                    --maximum-p95-ms 1000
            }
            else {
                & node (Join-Path $projectRoot "scripts\load-control-plane.mjs") --profile quick
            }
            $loadExitCode = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $savedErrorAction
        }
        if ($loadExitCode -ne 0) {
            throw "control-plane load failed"
        }
    }

    $env:FLUVORA_BROWSER_TOKEN = $token
    $env:FLUVORA_BROWSER_TOKEN_2 = $secondToken
    $env:FLUVORA_BROWSER_BASE_URL = "http://127.0.0.1:18000"
    Push-Location (Join-Path $projectRoot "tests\browser")
    try {
        $testArguments = @("playwright", "test", "--project=$Browser")
        if ($Grep) {
            $testArguments += @("--grep", $Grep)
        }
        $savedErrorAction = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            & npx.cmd @testArguments
            $browserExitCode = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $savedErrorAction
        }
        if ($browserExitCode -ne 0) {
            Get-Content (Join-Path $runRoot "media.stderr.log") -ErrorAction SilentlyContinue
            Get-Content (Join-Path $runRoot "api.stderr.log") -ErrorAction SilentlyContinue
            throw "$Browser browser interop failed"
        }
    }
    finally {
        Pop-Location
    }
}
finally {
    Stop-OwnedProcess $web
    Stop-OwnedProcess $api
    Stop-OwnedProcess $media
    $resolvedRunRoot = [System.IO.Path]::GetFullPath($runRoot)
    $resolvedProjectRoot = [System.IO.Path]::GetFullPath($projectRoot)
    if ($resolvedRunRoot.StartsWith($resolvedProjectRoot, [System.StringComparison]::OrdinalIgnoreCase) `
        -and (Split-Path -Leaf $resolvedRunRoot).StartsWith(".tmp-browser-interop-")) {
        Remove-Item -LiteralPath $resolvedRunRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
