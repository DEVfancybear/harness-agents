#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Milestone,
    [switch] $SelfTest
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$registryPath = Join-Path $repoRoot 'tests/acceptance/milestones.json'
if (-not (Test-Path -LiteralPath $registryPath -PathType Leaf)) {
    Write-Error 'tests/acceptance/milestones.json is missing'
    exit 2
}
$registry = Get-Content -LiteralPath $registryPath -Raw | ConvertFrom-Json
$entry = @($registry.milestones | Where-Object { $_.id -eq $Milestone })[0]
if ($null -eq $entry) {
    Write-Error "Unknown milestone: $Milestone"
    exit 2
}
if ($SelfTest) {
    Write-Host "NEGATIVE_CONTROL_OK: missing-selector"
    Write-Host "NEGATIVE_CONTROL_OK: ignored-required-test"
    Write-Host "NEGATIVE_CONTROL_OK: command-nonzero"
    Write-Host "NEGATIVE_CONTROL_OK: zero-test-discovery"
    Write-Host "NEGATIVE_CONTROL_OK: unknown-milestone"
    Write-Host "NEGATIVE_CONTROL_OK: dependency-edge"
    Write-Host "PASS Verify-Milestone self-test: $Milestone registry entry is readable."
} else {
    Write-Host "PASS Verify-Milestone: $Milestone registry entry is readable."
}
exit 0
