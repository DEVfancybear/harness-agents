#Requires -Version 7.0
[CmdletBinding()]
param([string] $RepositoryRoot = (Join-Path $PSScriptRoot '..'))
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath $RepositoryRoot).Path
& pwsh -NoProfile -File (Join-Path $root 'scripts/Verify-Phase.ps1') -RepositoryRoot $root -Phase P4 -SelfTest
if ($LASTEXITCODE -ne 0) { throw 'P4_GATE_SELFTEST_FAILED' }
& pwsh -NoProfile -File (Join-Path $root 'scripts/Verify-P4Mutations.ps1') -RepositoryRoot $root
if ($LASTEXITCODE -ne 0) { throw 'P4_MUTATIONS_FAILED' }
& pwsh -NoProfile -File (Join-Path $root 'scripts/Verify-Phase.ps1') -RepositoryRoot $root -Phase P4 -Json
if ($LASTEXITCODE -ne 0) { throw 'P4_PHASE_GATE_FAILED' }
Write-Output 'P4_GAUNTLET_OK'
