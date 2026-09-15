#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $RepositoryRoot = (Join-Path $PSScriptRoot '..'),
    [switch] $SelfTest
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-MutantKilled {
    param([int] $Code, [string] $Output, [string] $Test, [string] $Receipt)
    if ($Code -ne 101 -or $Output -notmatch [regex]::Escape("test $Test ... FAILED") -or
        $Output -notmatch [regex]::Escape($Receipt)) {
        throw "MUTATION_NOT_PROVEN: $Test; exit=$Code; receipt=$Receipt`n$Output"
    }
}

function Test-MutationChecker {
    Assert-MutantKilled 101 "receipt`ntest fixture ... FAILED" 'fixture' 'receipt'
    $rejected = 0
    foreach ($control in @(
        @{ Code = 0; Output = 'receipt test fixture ... ok' },
        @{ Code = 101; Output = 'receipt compile error' },
        @{ Code = 101; Output = 'test fixture ... FAILED' }
    )) {
        try { Assert-MutantKilled $control.Code $control.Output 'fixture' 'receipt' }
        catch { $rejected++ }
    }
    if ($rejected -ne 3) { throw 'MUTATION_CHECKER_FALSE_PASS' }
    Write-Output 'MUTATION_CHECKER_OK: survivor, build failure and missing execution receipt rejected'
}
Test-MutationChecker
if ($SelfTest) { exit 0 }

$root = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ('harness-p4-mutations-' + [guid]::NewGuid().ToString('N'))
[void] (New-Item -ItemType Directory -Path $scratch)
$paths = @(& git -C $root ls-files --cached --others --exclude-standard)
if ($LASTEXITCODE -ne 0 -or $paths.Count -eq 0) { throw 'MUTATION_SOURCE_ENUMERATION_FAILED' }
foreach ($relative in $paths) {
    $target = [System.IO.Path]::GetFullPath((Join-Path $scratch $relative))
    if (-not $target.StartsWith($scratch + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'MUTATION_PATH_ESCAPE' }
    [void] (New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
    Copy-Item -LiteralPath (Join-Path $root $relative) -Destination $target
}
$mutants = @(
    @{ Name = 'cas'; Path = 'crates/harness-store-sqlite/src/store/memory.rs'; Before = 'if current != to_i64(commit.expected_version, "expected memory version")? {'; After = 'if { eprintln!("RECEIPT"); current == to_i64(commit.expected_version, "expected memory version")? } {'; Test = 'p4_s02_scoped_assets_and_cas_writes_are_atomic'; Package = 'harness-cli'; Target = 'phase_p4' },
    @{ Name = 'scope'; Path = 'crates/harness-store-sqlite/src/store/memory.rs'; Before = 'if !scope_matches {'; After = 'if { eprintln!("RECEIPT"); false } {'; Test = 'p4_c08_forged_scope_cannot_search_read_or_export'; Package = 'harness-cli'; Target = 'phase_p4' },
    @{ Name = 'cursor'; Path = 'crates/harness-store-sqlite/src/store/memory.rs'; Before = 'if lease.job.start_sequence != cursor.saturating_add(1) {'; After = 'if { eprintln!("RECEIPT"); false } {'; Test = 'p4_c17_out_of_order_range_cannot_advance_contiguous_cursor'; Package = 'harness-cli'; Target = 'phase_p4' },
    @{ Name = 'invalidation'; Path = 'crates/harness-store-sqlite/src/store/memory/advanced.rs'; Before = 'if !include_root && &id == root {'; After = 'if { eprintln!("RECEIPT"); &id != root || !include_root } {'; Test = 'p4_c20_revocation_invalidates_transitive_derived_context'; Package = 'harness-cli'; Target = 'phase_p4' },
    @{ Name = 'normalization'; Path = 'crates/harness-memory/src/lib.rs'; Before = 'split.split_whitespace().collect::<Vec<_>>().join(" ")'; After = '{ eprintln!("RECEIPT"); split.split_whitespace().collect::<Vec<_>>().join("") }'; Test = 'properties::p4_property_normalization_preserves_words_and_is_idempotent'; Package = 'harness-memory'; Target = '' }
)
$priorTarget = $env:CARGO_TARGET_DIR
$env:CARGO_TARGET_DIR = Join-Path $root 'target/p4-mutations'
$nonce = [guid]::NewGuid().ToString('N')
Push-Location -LiteralPath $scratch
try {
    foreach ($mutant in $mutants) {
        $path = Join-Path $scratch $mutant.Path
        $bytes = [System.IO.File]::ReadAllBytes($path)
        $original = [System.IO.File]::ReadAllText($path)
        if ([regex]::Matches($original, [regex]::Escape($mutant.Before)).Count -ne 1) { throw "MUTATION_ANCHOR_NOT_UNIQUE: $($mutant.Name)" }
        $receipt = "P4_MUTANT_EXECUTED_$($mutant.Name)_$nonce"
        try {
            [System.IO.File]::WriteAllText($path, $original.Replace($mutant.Before, $mutant.After.Replace('RECEIPT', $receipt)))
            $mutatedHash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
            $arguments = @('test', '-p', $mutant.Package, '--locked')
            if ($mutant.Target) { $arguments += @('--test', $mutant.Target) } else { $arguments += '--lib' }
            $arguments += @($mutant.Test, '--', '--exact', '--nocapture')
            $output = (& cargo @arguments 2>&1 | Out-String)
            $code = $LASTEXITCODE
            if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $mutatedHash) { throw 'MUTATION_SOURCE_CHANGED_DURING_RUN' }
            Assert-MutantKilled $code $output $mutant.Test $receipt
            Write-Output "MUTANT_KILLED: $($mutant.Name); test=$($mutant.Test); source=$mutatedHash; receipt=$receipt"
        } finally {
            [System.IO.File]::WriteAllBytes($path, $bytes)
            if ([Convert]::ToBase64String([System.IO.File]::ReadAllBytes($path)) -cne [Convert]::ToBase64String($bytes)) { throw 'MUTATION_RESTORE_FAILED' }
        }
    }
    & cargo test -p harness-cli --test phase_p4 --locked
    if ($LASTEXITCODE -ne 0) { throw 'MUTATION_RESTORED_SUITE_FAILED' }
    & cargo test -p harness-memory --lib --locked
    if ($LASTEXITCODE -ne 0) { throw 'MUTATION_RESTORED_PROPERTIES_FAILED' }
    Write-Output "P4_MUTATIONS_OK: 5/5 killed; restored suite passed; scratch=$scratch"
} finally {
    Pop-Location
    $env:CARGO_TARGET_DIR = $priorTarget
}
