#Requires -Version 7.0
[CmdletBinding()]
param(
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
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

$requiredPairs = @(
    @('ARCHITECTURE_OVERVIEW.en.md', 'ARCHITECTURE_OVERVIEW.vi.md'),
    @('PLUGIN_ARCHITECTURE.en.md', 'PLUGIN_ARCHITECTURE.vi.md'),
    @('OPERATOR_GUIDE.en.md', 'OPERATOR_GUIDE.vi.md')
)
foreach ($pair in $requiredPairs) {
    Assert-File (Join-Path 'docs' $pair[0])
    Assert-File (Join-Path 'docs' $pair[1])
}
foreach ($single in @('TUI.md', 'BUILD_AND_RELEASE.md')) {
    Assert-File (Join-Path 'docs' $single)
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
