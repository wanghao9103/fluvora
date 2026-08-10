$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$coreDocuments = @(
    "README.md",
    "docs/ARCHITECTURE.md",
    "docs/LAYERS.md",
    "docs/CODEBASE.md",
    "docs/API_SERVER_STRUCTURE.md",
    "docs/API.md",
    "docs/SDK_INTEGRATION.md",
    "docs/SDK_DEMOS.md",
    "docs/PRODUCTION_ACCEPTANCE.md",
    "docs/RUNBOOK.md",
    "docs/DEVELOPMENT_PLAN.md"
)
$englishDocuments = @(
    "README.en.md",
    "docs/en/README.md",
    "docs/en/ARCHITECTURE.md",
    "docs/en/LAYERS.md",
    "docs/en/CODEBASE.md",
    "docs/en/API_SERVER_STRUCTURE.md",
    "docs/en/API.md",
    "docs/en/SDK_INTEGRATION.md",
    "docs/en/SDK_DEMOS.md",
    "docs/en/PRODUCTION_ACCEPTANCE.md",
    "docs/en/RUNBOOK.md",
    "docs/en/DEVELOPMENT_PLAN.md"
)
$requiredDocuments = @($coreDocuments + "docs/README.md" + $englishDocuments)

$violations = [System.Collections.Generic.List[string]]::new()
foreach ($relativePath in $requiredDocuments) {
    $path = Join-Path $projectRoot $relativePath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        $violations.Add("missing required document: $relativePath")
    }
}

$languagePairs = @(
    [pscustomobject]@{
        Chinese = "README.md"
        English = "README.en.md"
        ChineseSwitch = "[English](README.en.md)"
        EnglishSwitch = "](README.md) | [English](README.en.md)"
    }
)
$documentationNames = @(
    "README.md",
    "ARCHITECTURE.md",
    "LAYERS.md",
    "CODEBASE.md",
    "API_SERVER_STRUCTURE.md",
    "API.md",
    "SDK_INTEGRATION.md",
    "SDK_DEMOS.md",
    "PRODUCTION_ACCEPTANCE.md",
    "RUNBOOK.md",
    "DEVELOPMENT_PLAN.md"
)
foreach ($name in $documentationNames) {
    $languagePairs += [pscustomobject]@{
        Chinese = "docs/$name"
        English = "docs/en/$name"
        ChineseSwitch = "[English](en/$name)"
        EnglishSwitch = "](../$name) | [English]($name)"
    }
}
foreach ($pair in $languagePairs) {
    $chinesePath = Join-Path $projectRoot $pair.Chinese
    $englishPath = Join-Path $projectRoot $pair.English
    if (-not (Test-Path -LiteralPath $chinesePath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $englishPath -PathType Leaf)) {
        continue
    }
    $chinese = Get-Content -LiteralPath $chinesePath -Raw -Encoding utf8
    $english = Get-Content -LiteralPath $englishPath -Raw -Encoding utf8
    if (-not $chinese.Contains($pair.ChineseSwitch)) {
        $violations.Add("missing language switch in $($pair.Chinese)")
    }
    if (-not $english.Contains($pair.EnglishSwitch)) {
        $violations.Add("missing language switch in $($pair.English)")
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
        "Documentation check passed: {0} required documents, {1} bilingual pairs, " +
        "{2} Markdown files, and {3} local links verified."
    ) -f
    $requiredDocuments.Count,
    $languagePairs.Count,
    $markdownFiles.Count,
    $linkCount
)
