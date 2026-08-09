$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$requiredDocuments = @(
    "README.md",
    "docs/README.md",
    "docs/ARCHITECTURE.md",
    "docs/LAYERS.md",
    "docs/CODEBASE.md",
    "docs/API_SERVER_STRUCTURE.md",
    "docs/API.md",
    "docs/SDK_INTEGRATION.md",
    "docs/SDK_DEMOS.md",
    "docs/PRODUCTION_ACCEPTANCE.md",
    "docs/RUNBOOK.md"
)

$violations = [System.Collections.Generic.List[string]]::new()
foreach ($relativePath in $requiredDocuments) {
    $path = Join-Path $projectRoot $relativePath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        $violations.Add("missing required document: $relativePath")
    }
}

$markdownFiles = @(
    Get-ChildItem -LiteralPath $projectRoot -Recurse -File -Filter "*.md" |
        Where-Object {
            $_.FullName -notmatch '\\(?:target|artifacts|node_modules|\.git)\\'
        }
)
$linkCount = 0
foreach ($markdownFile in $markdownFiles) {
    $source = Get-Content -LiteralPath $markdownFile.FullName -Raw -Encoding utf8
    $matches = [regex]::Matches($source, '\[[^\]]+\]\((?<target>[^)]+)\)')
    foreach ($match in $matches) {
        $target = $match.Groups["target"].Value.Trim().Trim('<', '>')
        if (-not $target -or $target.StartsWith('#')) {
            continue
        }
        if ($target -match '^[a-zA-Z][a-zA-Z0-9+.-]*:') {
            continue
        }
        $pathPart = ($target -split '#', 2)[0]
        if (-not $pathPart) {
            continue
        }
        $linkCount += 1
        $decodedPath = [Uri]::UnescapeDataString($pathPart).Replace('/', [IO.Path]::DirectorySeparatorChar)
        $candidate = Join-Path $markdownFile.DirectoryName $decodedPath
        if (-not (Test-Path -LiteralPath $candidate)) {
            $document = $markdownFile.FullName.Substring($projectRoot.Length + 1)
            $violations.Add("broken local link in ${document}: $target")
        }
    }
}

if ($violations.Count -ne 0) {
    $violations | ForEach-Object { Write-Error $_ }
    throw "documentation check failed with $($violations.Count) violation(s)"
}

Write-Host (
    (
        "Documentation check passed: {0} required documents, {1} Markdown files, " +
        "and {2} local links verified."
    ) -f
    $requiredDocuments.Count,
    $markdownFiles.Count,
    $linkCount
)
