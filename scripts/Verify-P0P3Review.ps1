#Requires -Version 7.0
[CmdletBinding()]
param([switch] $SkipMutations)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repository = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$baseline = 'c9bb106cce67c5f0b7e3c02fafe1484e5f69d379'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ('harness-p0-p3-review-' + [guid]::NewGuid().ToString('N'))
$archive = "$scratch.zip"
$owned = @(
    'crates/harness-types/src/ids.rs', 'crates/harness-types/tests/contracts.rs',
    'crates/harness-kernel/src/lib.rs', 'crates/harness-session/src/context.rs',
    'crates/harness-cli/tests/phase_p1.rs', 'crates/harness-cli/tests/phase_p2.rs',
    'crates/harness-cli/tests/phase_p3.rs', 'crates/harness-tools/src/contracts.rs',
    'crates/harness-tools/src/policy.rs', 'crates/harness-tools/src/workspace.rs',
    'docs/specs/P0-P3_REVIEW.en.md', 'docs/specs/P0-P3_REVIEW.vi.md',
    'docs/evidence/P0-P3_REVIEW.en.md', 'docs/evidence/P0-P3_REVIEW.vi.md',
    'scripts/Verify-P0P3Review.ps1', 'scripts/Verify-P0P3ReviewMutations.ps1'
)

& git -C $repository archive --format=zip "--output=$archive" $baseline
if ($LASTEXITCODE -ne 0) { throw 'Could not export the accepted P3 baseline.' }
Expand-Archive -LiteralPath $archive -DestinationPath $scratch
Remove-Item -LiteralPath $archive
foreach ($relative in $owned) {
    Copy-Item -LiteralPath (Join-Path $repository $relative) -Destination (Join-Path $scratch $relative)
}
& git -C $scratch init --quiet
if ($LASTEXITCODE -ne 0) { throw 'Could not initialize source enumeration in the scratch copy.' }
Write-Output "Review source copy: $scratch"
$priorTarget = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = Join-Path $repository 'target'
    if (-not $SkipMutations) {
        & (Join-Path $scratch 'scripts/Verify-P0P3ReviewMutations.ps1') -ScratchRoot $scratch
    }
    & pwsh -NoProfile -File (Join-Path $scratch 'scripts/Verify-Phase.ps1') -RepositoryRoot $scratch -Phase P3 -Json
    if ($LASTEXITCODE -ne 0) { throw 'P3 review gate failed.' }
    & pwsh -NoProfile -File (Join-Path $scratch 'scripts/Verify-Docs.ps1') -RepositoryRoot $scratch -SelfTest
    if ($LASTEXITCODE -ne 0) { throw 'Review documentation self-test failed.' }
    Push-Location $scratch
    try {
        & cargo run -p harness-cli --bin ha --locked -- code capabilities --json
        if ($LASTEXITCODE -ne 0) { throw 'Review CLI smoke failed.' }
    } finally { Pop-Location }
    foreach ($relative in $owned) {
        $originalHash = (Get-FileHash -LiteralPath (Join-Path $repository $relative)).Hash
        $testedHash = (Get-FileHash -LiteralPath (Join-Path $scratch $relative)).Hash
        if ($originalHash -ne $testedHash) { throw "Source changed while verifying: $relative" }
    }
    Write-Output 'P0_P3_REVIEW_OK'
} finally {
    $env:CARGO_TARGET_DIR = $priorTarget
}
