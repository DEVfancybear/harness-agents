#Requires -Version 7.0
[CmdletBinding()]
param([Parameter(Mandatory)][string] $ScratchRoot)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath $ScratchRoot).Path
$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
if (-not $root.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Run mutations only in a disposable source copy under the temporary directory.'
}
$mutants = @(
    @{
        Name = 'uuid-variant'; Path = 'crates/harness-types/src/ids.rs'
        Before = '|| uuid.get_variant() != uuid::Variant::RFC4122'; After = '|| false'
        Arguments = @('test', '-p', 'harness-types', '--test', 'contracts', '--locked', 'review_p0_ids_reject_non_rfc_variants', '--', '--exact')
        Test = 'review_p0_ids_reject_non_rfc_variants'
    },
    @{
        Name = 'context-overflow'; Path = 'crates/harness-session/src/context.rs'
        Before = 'if mandatory_tokens > available {'; After = 'if false {'
        Arguments = @('test', '-p', 'harness-cli', '--test', 'phase_p2', '--locked', 'review_p2_context_budget_includes_rendered_headers', '--', '--exact')
        Test = 'review_p2_context_budget_includes_rendered_headers'
    },
    @{
        Name = 'redaction-order'; Path = 'crates/harness-tools/src/workspace.rs'
        Before = 'preview: truncate_text(&redact_text(line), 240),'; After = 'preview: redact_text(&truncate_text(line, 240)),'
        Arguments = @('test', '-p', 'harness-cli', '--test', 'phase_p3', '--locked', 'review_p3_search_redacts_before_truncation', '--', '--exact')
        Test = 'review_p3_search_redacts_before_truncation'
    }
)
Push-Location $root
try {
    foreach ($mutant in $mutants) {
        $path = Join-Path $root $mutant.Path
        $original = [System.IO.File]::ReadAllText($path)
        if ([regex]::Matches($original, [regex]::Escape($mutant.Before)).Count -ne 1) {
            throw "Mutation anchor must occur exactly once: $($mutant.Name)"
        }
        try {
            [System.IO.File]::WriteAllText($path, $original.Replace($mutant.Before, $mutant.After))
            $arguments = $mutant.Arguments
            $output = (& cargo @arguments 2>&1 | Out-String)
            $code = $LASTEXITCODE
            if ($code -eq 0 -or $output -notmatch ([regex]::Escape("test $($mutant.Test) ... FAILED"))) {
                throw "Mutant survived or failed outside its behavioral assertion: $($mutant.Name)`n$output"
            }
            Write-Output "KILLED $($mutant.Name): $($mutant.Test)"
        } finally {
            [System.IO.File]::WriteAllText($path, $original)
        }
    }
} finally {
    Pop-Location
}
