#Requires -Version 7.0
[CmdletBinding()]
param(
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$docsRoot = Join-Path $repoRoot 'docs'
$errors = [System.Collections.Generic.List[string]]::new()

function Add-DocError([string]$Message) {
    [void]$script:errors.Add($Message)
}

function Assert-File([string]$RelativePath) {
    $path = Join-Path $repoRoot $RelativePath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Add-DocError "Missing required file: $RelativePath"
    }
}

$requiredDocuments = @(
    'docs/architecture/ARCHITECTURE_OVERVIEW.en.md',
    'docs/architecture/ARCHITECTURE_OVERVIEW.vi.md',
    'docs/architecture/PLUGIN_ARCHITECTURE.en.md',
    'docs/architecture/PLUGIN_ARCHITECTURE.vi.md',
    'docs/guides/OPERATOR_GUIDE.en.md',
    'docs/guides/OPERATOR_GUIDE.vi.md',
    'docs/guides/TUI.md',
    'docs/guides/BUILD_AND_RELEASE.md'
)
foreach ($document in $requiredDocuments) {
    Assert-File $document
}

$markdownFiles = Get-ChildItem -LiteralPath $docsRoot -Recurse -Filter '*.md' -File
$deletedTokens = @(
    'MEMORY_AND_CONTINUITY',
    'P4_MEMORY',
    'harness-memory',
    'docs/specs/M7.vi.md',
    'docs/implementation-next/M7.vi.md',
    'docs/implementation/',
    'ARCHITECTURE_REVIEW',
    'HARNESS_MASTER_PLAN',
    'RUST_HARNESS_PLAN'
)
foreach ($file in $markdownFiles) {
    $text = Get-Content -LiteralPath $file.FullName -Raw
    foreach ($token in $deletedTokens) {
        if ($text.Contains($token)) {
            Add-DocError "Retired documentation token '$token' remains in $($file.FullName.Substring($repoRoot.Length + 1))"
        }
    }

    $linkMatches = [regex]::Matches($text, '\[[^\]]+\]\(([^)]+)\)')
    foreach ($match in $linkMatches) {
        # Link syntax shown as code is not a link: `- [Title](path)` in a
        # table that describes a file format was reported as broken. A match
        # is inside inline code when an odd number of backticks precede it on
        # its line.
        $lineStart = $text.LastIndexOf("`n", [Math]::Max(0, $match.Index - 1)) + 1
        $before = $text.Substring($lineStart, $match.Index - $lineStart)
        if ((($before.ToCharArray() | Where-Object { $_ -eq '`' }) | Measure-Object).Count % 2 -eq 1) { continue }
        $target = $match.Groups[1].Value.Trim()
        if ($target -match '^(https?:|mailto:|#)') { continue }
        $cleanTarget = ($target -split '#', 2)[0].Trim('<', '>')
        if ([string]::IsNullOrWhiteSpace($cleanTarget) -or $cleanTarget.EndsWith('/')) { continue }
        $resolved = Join-Path $file.DirectoryName $cleanTarget
        if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
            Add-DocError "Broken link in $($file.FullName.Substring($repoRoot.Length + 1)): $target"
        }
    }
}

if ($SelfTest -and $errors.Count -eq 0) {
    Write-Host 'PASS Verify-Docs self-test: current bilingual docs, retired tokens and links are consistent.'
}

if ($errors.Count -gt 0) {
    $errors | ForEach-Object { Write-Error $_ }
    exit 1
}

if (-not $SelfTest) {
    Write-Host 'PASS Verify-Docs: current bilingual docs, retired tokens and links are consistent.'
}
exit 0
