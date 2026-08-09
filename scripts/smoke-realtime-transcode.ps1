param(
    [int] $WorkerPort = 18091
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$smokeRoot = Join-Path $projectRoot ".tmp-realtime-smoke-$PID"
$onWindows = $env:OS -eq "Windows_NT"
$workerExecutable = Join-Path $projectRoot (
    "target/debug/fluvora-media-worker" + $(if ($onWindows) { ".exe" } else { "" })
)
New-Item -ItemType Directory -Force -Path `
    (Join-Path $smokeRoot "input"), `
    (Join-Path $smokeRoot "output"), `
    (Join-Path $smokeRoot "live"), `
    (Join-Path $smokeRoot "state") | Out-Null

$env:FLUVORA_WORKER_BIND = "127.0.0.1:$WorkerPort"
$env:FLUVORA_WORKER_TOKEN = "smoke-secret-0001"
$env:FLUVORA_WORKER_INPUT_ROOT = Join-Path $smokeRoot "input"
$env:FLUVORA_WORKER_OUTPUT_ROOT = Join-Path $smokeRoot "output"
$env:FLUVORA_WORKER_LIVE_ROOT = Join-Path $smokeRoot "live"
$env:FLUVORA_WORKER_STATE_ROOT = Join-Path $smokeRoot "state"

function Stop-RealtimeJob([uint64] $JobId) {
    Invoke-RestMethod `
        -Method Delete `
        -Uri "http://127.0.0.1:$WorkerPort/v1/realtime-jobs/$JobId" `
        -Headers @{ Authorization = "Bearer smoke-secret-0001" } | Out-Null
    foreach ($attempt in 1..100) {
        $job = Invoke-RestMethod `
            -Method Get `
            -Uri "http://127.0.0.1:$WorkerPort/v1/jobs/$JobId" `
            -Headers @{ Authorization = "Bearer smoke-secret-0001" }
        if ($job.state -eq "stopped") {
            return
        }
        if ($job.state -eq "failed") {
            throw "realtime job failed while stopping: $($job | ConvertTo-Json -Compress)"
        }
        Start-Sleep -Milliseconds 100
    }
    throw "realtime job $JobId did not stop"
}

$workerStart = @{
    FilePath = $workerExecutable
    PassThru = $true
    RedirectStandardError = Join-Path $smokeRoot "worker.stderr.log"
}
if ($onWindows) {
    $workerStart.WindowStyle = "Hidden"
}
$worker = Start-Process @workerStart
try {
    Start-Sleep -Milliseconds 700
    $receiver = [System.Net.Sockets.UdpClient]::new(0)
    try {
        $receiver.Client.ReceiveTimeout = 10000
        $outputPort = ([System.Net.IPEndPoint] $receiver.Client.LocalEndPoint).Port
        $body = @{
            job_key = "smoke-vp8-h264"
            source = @{
                track_id = 1
                kind = "video"
                codec = "vp8"
                payload_type = 96
                clock_rate = 90000
                channels = $null
                fmtp = $null
            }
            target = @{
                codec = "h264"
                destination = "127.0.0.1:$outputPort"
                payload_type = 102
                ssrc = 222
                width = 320
                height = 240
                frames_per_second = 15
                bitrate_bps = 300000
            }
        } | ConvertTo-Json -Depth 5
        $created = Invoke-RestMethod `
            -Method Post `
            -Uri "http://127.0.0.1:$WorkerPort/v1/realtime-jobs" `
            -Headers @{ Authorization = "Bearer smoke-secret-0001" } `
            -ContentType "application/json" `
            -Body $body
        $sourcePort = [int] (($created.source_destination -split ":")[-1])
        $producerStart = @{
            FilePath = "ffmpeg"
            ArgumentList = @(
                "-hide_banner", "-loglevel", "error", "-re",
                "-f", "lavfi", "-i", "testsrc=size=320x240:rate=15",
                "-t", "4", "-an", "-c:v", "libvpx", "-deadline", "realtime", "-g", "15",
                "-f", "rtp", "-payload_type", "96", "-ssrc", "111",
                "rtp://127.0.0.1:$sourcePort`?pkt_size=1200"
            )
            PassThru = $true
            RedirectStandardError = Join-Path $smokeRoot "producer.stderr.log"
        }
        if ($onWindows) {
            $producerStart.WindowStyle = "Hidden"
        }
        $producer = Start-Process @producerStart
        try {
            $remote = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)
            try {
                $packet = $receiver.Receive([ref] $remote)
            }
            catch {
                $job = Invoke-RestMethod `
                    -Method Get `
                    -Uri "http://127.0.0.1:$WorkerPort/v1/jobs/$($created.job_id)" `
                    -Headers @{ Authorization = "Bearer smoke-secret-0001" }
                $producerError = Get-Content `
                    -Raw `
                    -ErrorAction SilentlyContinue `
                    (Join-Path $smokeRoot "producer.stderr.log")
                $workerError = Get-Content `
                    -Raw `
                    -ErrorAction SilentlyContinue `
                    (Join-Path $smokeRoot "worker.stderr.log")
                throw "RTP receive timed out; worker=$($job | ConvertTo-Json -Compress) producer_exit=$($producer.ExitCode) producer_error=$producerError worker_error=$workerError"
            }
            $payloadType = $packet[1] -band 127
            $ssrc = ([uint32] $packet[8] -shl 24) `
                -bor ([uint32] $packet[9] -shl 16) `
                -bor ([uint32] $packet[10] -shl 8) `
                -bor [uint32] $packet[11]
            if ($payloadType -ne 102 -or $ssrc -ne 222) {
                throw "unexpected output RTP header: payload_type=$payloadType ssrc=$ssrc"
            }
            Stop-RealtimeJob $created.job_id
            Write-Output "realtime transcode smoke passed: bytes=$($packet.Length) payload_type=$payloadType ssrc=$ssrc job=$($created.job_id)"
        }
        finally {
            if (-not $producer.HasExited) {
                Stop-Process -Id $producer.Id -Force
            }
            $producer.WaitForExit()
            $producer.Dispose()
        }
    }
    finally {
        $receiver.Dispose()
    }
}
finally {
    if (-not $worker.HasExited) {
        Stop-Process -Id $worker.Id -Force
    }
    $worker.WaitForExit()
    $worker.Dispose()
    $resolvedSmokeRoot = [System.IO.Path]::GetFullPath($smokeRoot)
    $resolvedProjectRoot = [System.IO.Path]::GetFullPath($projectRoot)
    if ($resolvedSmokeRoot.StartsWith($resolvedProjectRoot, [System.StringComparison]::OrdinalIgnoreCase) `
        -and (Split-Path -Leaf $resolvedSmokeRoot).StartsWith(".tmp-realtime-smoke-")) {
        Remove-Item -LiteralPath $resolvedSmokeRoot -Recurse -Force
    }
    else {
        throw "refusing to clean unsafe realtime smoke path: $resolvedSmokeRoot"
    }
}
