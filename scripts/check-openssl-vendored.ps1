param(
    [string]$ProjectRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$TargetDirectory = (Join-Path $env:LOCALAPPDATA "Fluvora\target"),
    [string]$VisualStudioEnvironment = "",
    [string]$PerlBin = ""
)

$ErrorActionPreference = "Stop"

function Invoke-Cargo([string[]] $Arguments) {
    $savedErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        & cargo @Arguments
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $savedErrorAction
    }
    if ($exitCode -ne 0) {
        throw "cargo exited with code $exitCode"
    }
}

if (-not $VisualStudioEnvironment) {
    $VisualStudioEnvironment = @("Community", "Professional", "Enterprise", "BuildTools") |
        ForEach-Object {
            Join-Path ${env:ProgramFiles} "Microsoft Visual Studio\2022\$_\Common7\Tools\VsDevCmd.bat"
        } |
        Where-Object { Test-Path -LiteralPath $_ } |
        Select-Object -First 1
}
if (-not $PerlBin) {
    $perlToolsRoot = Join-Path $env:LOCALAPPDATA "vcpkg\downloads\tools\perl"
    $PerlBin = Get-ChildItem -LiteralPath $perlToolsRoot -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName "perl\bin" } |
        Where-Object { Test-Path -LiteralPath $_ } |
        Select-Object -First 1
    if (-not $PerlBin) {
        $perlCommand = Get-Command perl -ErrorAction SilentlyContinue
        if ($perlCommand) {
            $PerlBin = Split-Path -Parent $perlCommand.Source
        }
    }
}

if (-not $VisualStudioEnvironment -or -not (Test-Path -LiteralPath $VisualStudioEnvironment)) {
    throw "Visual Studio developer environment was not found"
}
if (-not $PerlBin -or -not (Test-Path -LiteralPath $PerlBin)) {
    throw "Portable Strawberry Perl was not found"
}

$environmentLines = & cmd.exe /d /s /c (
    'call "' + $VisualStudioEnvironment + '" -arch=x64 -host_arch=x64 >nul && set'
)
foreach ($line in $environmentLines) {
    if ($line -match '^([^=]+)=(.*)$') {
        [Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
    }
}

$env:Path = "$PerlBin;$env:Path"
$env:CARGO_TARGET_DIR = $TargetDirectory
Remove-Item Env:PERL5LIB -ErrorAction SilentlyContinue

Push-Location -LiteralPath $ProjectRoot
try {
    # Clippy compiles every media-node target with the vendored feature, while the adapter test
    # links and executes that backend. A separate cargo build repeats the native OpenSSL build.
    Invoke-Cargo @(
        "clippy", "-p", "fluvora-media-node", "--features", "openssl-vendored",
        "--all-targets", "--locked", "--", "-D", "warnings"
    )
    Invoke-Cargo @(
        "test", "-p", "fluvora-dtls-adapter", "--features", "openssl-vendored", "--locked"
    )
}
finally {
    Pop-Location
}
