#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidatePattern('^P[0-9]+$')] [string] $Phase,
    [switch] $SelfTest
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$activePhases = @('P0', 'P1', 'P2', 'P3', 'P5', 'P6', 'P7', 'P8')
if ($Phase -notin $activePhases) {
    Write-Error "Unknown or retired phase: $Phase"
    exit 2
}

$runbook = Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs/implementation') -Filter "$Phase`_*.en.md" -File | Select-Object -First 1
$runbookVi = Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs/implementation') -Filter "$Phase`_*.vi.md" -File | Select-Object -First 1
if ($null -eq $runbook -or $null -eq $runbookVi) {
    Write-Error "Bilingual runbook missing for $Phase"
    exit 2
}

$manifest = Get-Content -LiteralPath (Join-Path $repoRoot 'docs/implementation/manifest.json') -Raw | ConvertFrom-Json
$entry = @($manifest.phases | Where-Object { $_.id -eq $Phase })[0]
if ($null -eq $entry) {
    Write-Error "Manifest entry missing for $Phase"
    exit 2
}
foreach ($required in @($entry.en, $entry.vi)) {
    if (-not (Test-Path -LiteralPath (Join-Path $repoRoot "docs/implementation/$required") -PathType Leaf)) {
        Write-Error "Manifest runbook missing: $required"
        exit 2
    }
}

$rustTests = Get-ChildItem -LiteralPath (Join-Path $repoRoot 'crates') -Recurse -Filter "phase_$($Phase.Substring(1)).rs" -File -ErrorAction SilentlyContinue
if (-not $SelfTest -and $null -eq $rustTests) {
    Write-Warning "No focused Rust target named phase_$($Phase.Substring(1)).rs was found; run the workspace gate before accepting this phase."
}

if ($SelfTest) {
    Write-Host "PASS Verify-Phase self-test: $Phase is an active bilingual phase with a manifest entry."
} else {
    Write-Host "PASS Verify-Phase: $Phase runbook and manifest are present."
}
exit 0
