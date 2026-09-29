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
    @('ARCHITECTURE_REVIEW.en.md', 'ARCHITECTURE_REVIEW.vi.md'),
    @('HARNESS_MASTER_PLAN.en.md', 'HARNESS_MASTER_PLAN.vi.md'),
    @('RUST_HARNESS_PLAN.en.md', 'RUST_HARNESS_PLAN.vi.md'),
    @('PLUGIN_ARCHITECTURE.en.md', 'PLUGIN_ARCHITECTURE.vi.md'),
    @('OPERATOR_GUIDE.en.md', 'OPERATOR_GUIDE.vi.md'),
    @('implementation/README.en.md', 'implementation/README.vi.md'),
    @('implementation/ACCEPTANCE_MAP.en.md', 'implementation/ACCEPTANCE_MAP.vi.md')
)

$activePhases = @('P0', 'P1', 'P2', 'P3', 'P5', 'P6', 'P7', 'P8')
foreach ($pair in $requiredPairs) {
    Assert-File (Join-Path 'docs' $pair[0])
    Assert-File (Join-Path 'docs' $pair[1])
}
foreach ($phase in $activePhases) {
    $en = Join-Path 'docs/implementation' "$phase`_"
    $enFile = Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs/implementation') -Filter "$phase`_*.en.md" -File | Select-Object -First 1
    $viFile = Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs/implementation') -Filter "$phase`_*.vi.md" -File | Select-Object -First 1
    if ($null -eq $enFile) { Add-DocError "Missing English runbook for $phase" }
    if ($null -eq $viFile) { Add-DocError "Missing Vietnamese runbook for $phase" }
}

$manifestPath = Join-Path $repoRoot 'docs/implementation/manifest.json'
Assert-File 'docs/implementation/manifest.json'
$manifest = $null
if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
    try {
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    } catch {
        Add-DocError "Invalid JSON: docs/implementation/manifest.json ($($_.Exception.Message))"
    }
}
if ($null -ne $manifest) {
    $manifestIds = @($manifest.phases | ForEach-Object { $_.id })
    if (($manifestIds -join ',') -ne ($activePhases -join ',')) {
        Add-DocError "Manifest phase order must be: $($activePhases -join ', ')"
    }
    foreach ($phase in $manifest.phases) {
        if ($phase.id -notin $activePhases) {
            Add-DocError "Manifest contains retired or unknown phase: $($phase.id)"
        }
        foreach ($relative in @("docs/implementation/$($phase.en)", "docs/implementation/$($phase.vi)")) {
            Assert-File $relative
        }
    }
    for ($i = 0; $i -lt $manifest.phases.Count; $i++) {
        $phase = $manifest.phases[$i]
        $expectedDependency = if ($i -eq 0) { @() } else { @($activePhases[$i - 1]) }
        $actualDependency = @($phase.depends_on)
        if (($actualDependency -join ',') -ne ($expectedDependency -join ',')) {
            Add-DocError "Manifest dependency for $($phase.id) must be $($expectedDependency -join ', ')"
        }
    }
}

$markdownFiles = Get-ChildItem -LiteralPath $docsRoot -Recurse -Filter '*.md' -File
$deletedTokens = @(
    'MEMORY_AND_CONTINUITY',
    'P4_MEMORY',
    'harness-memory',
    'docs/specs/M7.vi.md',
    'docs/implementation-next/M7.vi.md'
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
    Write-Host 'PASS Verify-Docs self-test: current bilingual docs, manifest, retired tokens and links are consistent.'
}

if ($errors.Count -gt 0) {
    $errors | ForEach-Object { Write-Error $_ }
    exit 1
}

if (-not $SelfTest) {
    Write-Host 'PASS Verify-Docs: current bilingual docs, manifest, retired tokens and links are consistent.'
}
exit 0
