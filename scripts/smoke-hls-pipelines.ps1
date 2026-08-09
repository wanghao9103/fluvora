param(
    [int] $WorkerPort = 18091
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$smokeRoot = Join-Path $projectRoot ".tmp-hls-smoke-$PID"
$inputRoot = Join-Path $smokeRoot "input"
$outputRoot = Join-Path $smokeRoot "output"
$liveRoot = Join-Path $smokeRoot "live"
$stateRoot = Join-Path $smokeRoot "state"
$workerToken = "smoke-worker-token-32-bytes-min"
$worker = $null
$producer = $null
$passed = $false
$onWindows = $env:OS -eq "Windows_NT"

function Assert-SmokeRootIsSafe {
    $resolvedProject = [System.IO.Path]::GetFullPath($projectRoot)
    $resolvedSmoke = [System.IO.Path]::GetFullPath($smokeRoot)
    if (
        -not $resolvedSmoke.StartsWith(
            $resolvedProject + [System.IO.Path]::DirectorySeparatorChar,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -or
        [System.IO.Path]::GetFileName($resolvedSmoke) -notmatch '^\.tmp-hls-smoke-\d+$'
    ) {
        throw "refusing to manage unsafe smoke path: $resolvedSmoke"
    }
    return $resolvedSmoke
}

function Wait-ForUrl([string] $Url, [int] $Seconds = 15) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
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

function Wait-ForJob([uint64] $JobId, [int] $Seconds = 30) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $job = Invoke-RestMethod `
            -Method Get `
            -Uri "http://127.0.0.1:$WorkerPort/v1/jobs/$JobId" `
            -Headers @{ Authorization = "Bearer $workerToken" }
        if ($job.state -eq "succeeded") {
            return $job
        }
        if ($job.state -eq "failed" -or $job.state -eq "stopped") {
            throw "media job $JobId ended as $($job | ConvertTo-Json -Compress)"
        }
        Start-Sleep -Milliseconds 100
    }
    throw "media job $JobId did not finish"
}

function Wait-ForLiveAbr([string] $Directory, [int] $Seconds = 20) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $master = Join-Path $Directory "master.m3u8"
        $rendition0 = Join-Path $Directory "rendition_0.m3u8"
        $rendition1 = Join-Path $Directory "rendition_1.m3u8"
        $segments0 = @(
            Get-ChildItem -LiteralPath $Directory `
                -Filter "rendition_0_segment-*.m4s" -ErrorAction SilentlyContinue
        )
        $segments1 = @(
            Get-ChildItem -LiteralPath $Directory `
                -Filter "rendition_1_segment-*.m4s" -ErrorAction SilentlyContinue
        )
        if (
            (Test-Path -LiteralPath $master) -and
            (Test-Path -LiteralPath $rendition0) -and
            (Test-Path -LiteralPath $rendition1) -and
            $segments0.Count -gt 0 -and
            $segments1.Count -gt 0
        ) {
            return
        }
        Start-Sleep -Milliseconds 100
    }
    throw "live ABR master, rendition playlists and segments were not published"
}

function Wait-ForJobState([uint64] $JobId, [string] $Expected, [int] $Seconds = 10) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $job = Invoke-RestMethod `
            -Method Get `
            -Uri "http://127.0.0.1:$WorkerPort/v1/jobs/$JobId" `
            -Headers @{ Authorization = "Bearer $workerToken" }
        if ($job.state -eq $Expected) {
            return
        }
        if ($job.state -eq "failed") {
            throw "media job $JobId failed as $($job | ConvertTo-Json -Compress)"
        }
        Start-Sleep -Milliseconds 100
    }
    throw "media job $JobId did not reach $Expected"
}

try {
    $resolvedSmoke = Assert-SmokeRootIsSafe
    if (Test-Path -LiteralPath $resolvedSmoke) {
        Remove-Item -LiteralPath $resolvedSmoke -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path `
        $inputRoot, $outputRoot, $liveRoot, $stateRoot | Out-Null
    $source = Join-Path $inputRoot "source.mp4"
    & ffmpeg -hide_banner -loglevel error -y `
        -f lavfi -i "testsrc=size=320x180:rate=20" `
        -f lavfi -i "sine=frequency=440:sample_rate=48000" `
        -t 4 -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest $source
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $source)) {
        throw "failed to create the VOD smoke source"
    }

    $workerExecutable = Join-Path $projectRoot "target/debug/fluvora-media-worker"
    if ($onWindows) {
        $workerExecutable = "$workerExecutable.exe"
    }
    $env:FLUVORA_WORKER_BIND = "127.0.0.1:$WorkerPort"
    $env:FLUVORA_WORKER_TOKEN = $workerToken
    $env:FLUVORA_WORKER_INPUT_ROOT = $inputRoot
    $env:FLUVORA_WORKER_OUTPUT_ROOT = $outputRoot
    $env:FLUVORA_WORKER_LIVE_ROOT = $liveRoot
    $env:FLUVORA_WORKER_STATE_ROOT = $stateRoot
    $env:FLUVORA_WORKER_CONCURRENCY = "2"
    $workerStart = @{
        FilePath = $workerExecutable
        PassThru = $true
        RedirectStandardError = Join-Path $smokeRoot "worker.stderr.log"
    }
    if ($onWindows) {
        $workerStart.WindowStyle = "Hidden"
    }
    $worker = Start-Process @workerStart
    Wait-ForUrl "http://127.0.0.1:$WorkerPort/health/ready"

    $vodBody = @{
        asset_id = "smoke-vod"
        input = "source.mp4"
        output_directory = "smoke-vod"
        segment_duration_millis = 1000
        renditions = @(
            @{
                width = 320
                height = 180
                video_bitrate_bps = 300000
                audio_bitrate_bps = 64000
            },
            @{
                width = 160
                height = 90
                video_bitrate_bps = 150000
                audio_bitrate_bps = 32000
            }
        )
    } | ConvertTo-Json -Depth 5
    $vod = Invoke-RestMethod `
        -Method Post `
        -Uri "http://127.0.0.1:$WorkerPort/v1/jobs" `
        -Headers @{ Authorization = "Bearer $workerToken" } `
        -ContentType "application/json" `
        -Body $vodBody
    Wait-ForJob $vod.job_id | Out-Null
    $vodDirectory = Join-Path $outputRoot "smoke-vod"
    if (-not (Test-Path -LiteralPath (Join-Path $vodDirectory "master.m3u8"))) {
        throw "VOD HLS output is missing master.m3u8"
    }
    $vodMaster = Get-Content -Raw -LiteralPath (Join-Path $vodDirectory "master.m3u8")
    foreach ($renditionIndex in 0, 1) {
        foreach ($file in "rendition_$renditionIndex.m3u8", "init_$renditionIndex.mp4") {
            if (-not (Test-Path -LiteralPath (Join-Path $vodDirectory $file))) {
                throw "VOD HLS output is missing $file"
            }
        }
        if (
            @(Get-ChildItem -LiteralPath $vodDirectory -Filter "rendition_$renditionIndex`_*.m4s").Count -eq 0
        ) {
            throw "VOD HLS rendition $renditionIndex contains no media segment"
        }
        $vodPlaylist = Get-Content -Raw -LiteralPath `
            (Join-Path $vodDirectory "rendition_$renditionIndex.m3u8")
        if (
            $vodPlaylist -notmatch "#EXT-X-MAP:URI=`"init_$renditionIndex\.mp4`"" -or
            $vodPlaylist -match '#EXT-X-MAP:URI="(?:[/\\]|[A-Za-z]:)'
        ) {
            throw "VOD HLS rendition $renditionIndex contains a non-portable initialization URI"
        }
        if ($vodMaster -notmatch "rendition_$renditionIndex\.m3u8") {
            throw "VOD HLS master playlist does not reference rendition $renditionIndex"
        }
        & ffprobe -v error -show_entries "format=format_name" -of "default=nw=1" `
            (Join-Path $vodDirectory "rendition_$renditionIndex.m3u8") | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "VOD HLS rendition $renditionIndex is not readable by FFprobe"
        }
    }

    $liveBody = @{
        stream_id = "smoke-live"
        output_directory = "smoke-live"
        segment_duration_millis = 1000
        window_segments = 3
        renditions = @(
            @{
                width = 320
                height = 180
                video_bitrate_bps = 300000
                audio_bitrate_bps = 64000
            },
            @{
                width = 160
                height = 90
                video_bitrate_bps = 150000
                audio_bitrate_bps = 32000
            }
        )
        tracks = @(
            @{
                track_id = 1
                kind = "video"
                codec = "vp8"
                payload_type = 96
                clock_rate = 90000
                channels = $null
                fmtp = $null
            }
        )
    } | ConvertTo-Json -Depth 5
    $live = Invoke-RestMethod `
        -Method Post `
        -Uri "http://127.0.0.1:$WorkerPort/v1/live-jobs" `
        -Headers @{ Authorization = "Bearer $workerToken" } `
        -ContentType "application/json" `
        -Body $liveBody
    $destination = [string] $live.destinations[0].destination
    $producerStart = @{
        FilePath = "ffmpeg"
        ArgumentList = @(
            "-hide_banner", "-loglevel", "error", "-re",
            "-f", "lavfi", "-i", "testsrc=size=320x180:rate=20",
            "-t", "5", "-an", "-c:v", "libvpx", "-deadline", "realtime", "-g", "20",
            "-f", "rtp", "-payload_type", "96", "-ssrc", "111",
            "rtp://$destination`?pkt_size=1200"
        )
        PassThru = $true
        RedirectStandardOutput = Join-Path $smokeRoot "producer.stdout.log"
        RedirectStandardError = Join-Path $smokeRoot "producer.stderr.log"
    }
    if ($onWindows) {
        $producerStart.WindowStyle = "Hidden"
    }
    $producer = Start-Process @producerStart
    $liveDirectory = Join-Path $liveRoot "smoke-live"
    Wait-ForLiveAbr $liveDirectory
    $liveMaster = Get-Content -Raw -LiteralPath (Join-Path $liveDirectory "master.m3u8")
    foreach ($renditionIndex in 0, 1) {
        if ($liveMaster -notmatch "rendition_$renditionIndex\.m3u8") {
            throw "live HLS master playlist does not reference rendition $renditionIndex"
        }
        $livePlaylist = Get-Content -Raw -LiteralPath `
            (Join-Path $liveDirectory "rendition_$renditionIndex.m3u8")
        if (
            $livePlaylist -notmatch "#EXT-X-MAP:URI=`"init_$renditionIndex\.mp4`"" -or
            $livePlaylist -match '#EXT-X-MAP:URI="(?:[/\\]|[A-Za-z]:)'
        ) {
            throw "live HLS rendition $renditionIndex contains a non-portable initialization URI"
        }
    }
    Invoke-RestMethod `
        -Method Delete `
        -Uri "http://127.0.0.1:$WorkerPort/v1/live-jobs/$($live.job_id)" `
        -Headers @{ Authorization = "Bearer $workerToken" } | Out-Null
    Wait-ForJobState $live.job_id "stopped"

    Write-Output "HLS pipeline smoke passed: vod_job=$($vod.job_id) live_job=$($live.job_id)"
    $passed = $true
}
finally {
    if ($producer -and -not $producer.HasExited) {
        Stop-Process -Id $producer.Id -Force
        $producer.WaitForExit()
    }
    if ($worker -and -not $worker.HasExited) {
        Stop-Process -Id $worker.Id -Force
        $worker.WaitForExit()
    }
    if ($passed) {
        $resolvedSmoke = Assert-SmokeRootIsSafe
        Remove-Item -LiteralPath $resolvedSmoke -Recurse -Force
    }
}
