# Shared by Verify-Phase.ps1 and Verify-Milestone.ps1 (dot-sourced).
#
# The workspace suite runs every test of the repository at once on a loaded CI
# runner. Measured 23-24/09/2026: the "Rust foundation checks" workflow was red
# for 30 consecutive pushes while the same revisions passed locally, and the
# failing test moved from run to run - a hook subprocess that timed out
# (`g09_hook_*`), a process-tree heartbeat that had not been written yet
# (`a16_process_tree_env`), an installer copy that Windows Defender still held
# (`m9_04_install_smoke_preserves_existing_data`), a PowerShell fallback probe
# (`g14_fallback_receipt_names_powershell_51`). Each of them passed when run
# alone. The earlier retry only recognized loopback transport phrases, so every
# other kind of load failure turned the whole job red, and the thrown message
# was the *head* of cargo's output - compile lines - so the log never named the
# failing test at all.
#
# The rule here is the one nextest's retries use: a test that fails in the
# crowded run is run again **alone**. If it passes alone it was the load, and the
# gate says so in the log (`GATE_TEST_RETRIED`); if it fails alone it is a real
# failure and the gate fails with that test's own output. A regression cannot
# hide behind this, because a regression also fails when it runs by itself.

Set-StrictMode -Version Latest

function Get-CargoTestSelector {
    # Map one `Running ...` line of `cargo test --workspace` to the arguments that
    # select the same test binary again.
    param([Parameter(Mandatory)] [string] $Line)

    $normalized = $Line.Replace('\', '/')
    if ($normalized -cmatch 'Running\s+tests/(?<target>[A-Za-z0-9_]+)\.rs') {
        return @('--workspace', '--test', $Matches['target'])
    }
    if ($normalized -cmatch 'Running\s+unittests\s+src/(main|bin/[A-Za-z0-9_]+)\.rs\s+\(\S*/(?<bin>[A-Za-z0-9_]+)-[0-9a-f]+(\.exe)?\)') {
        return @('--workspace', '--bin', $Matches['bin'])
    }
    if ($normalized -cmatch 'Running\s+unittests\s+src/lib\.rs\s+\(\S*/(?<crate>[A-Za-z0-9_]+)-[0-9a-f]+(\.exe)?\)') {
        return @('-p', ($Matches['crate'] -replace '_', '-'), '--lib')
    }
    if ($normalized -cmatch '^\s*Doc-tests\s+(?<crate>[A-Za-z0-9_]+)') {
        return @('-p', ($Matches['crate'] -replace '_', '-'), '--doc')
    }
    return $null
}

function Get-FailedCargoTests {
    # Every `test <name> ... FAILED` line, attributed to the binary that ran it.
    param([Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Output)

    $selector = $null
    $failures = [System.Collections.Generic.List[object]]::new()
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($line in $Output) {
        $plain = $line.Replace('`', '')
        # Case-sensitive: cargo's per-binary `running N tests` line is lowercase and
        # must not clear the selector the `Running <path>` line set.
        if ($plain -cmatch '^\s*(Running\s|Doc-tests\s)') {
            $selector = Get-CargoTestSelector -Line $plain
            continue
        }
        if ($plain -match '^\s*test\s+(?<name>\S+)\s+\.\.\.\s+FAILED\b') {
            $name = $Matches['name']
            $key = "$($selector -join ' ')|$name"
            if ($seen.Add($key)) {
                $failures.Add([pscustomobject]@{ Selector = $selector; TestName = $name })
            }
        }
    }
    return $failures.ToArray()
}

function Write-CargoFailureSummary {
    # Name every failing test and show the start of its panic, so the CI log says
    # what failed without anyone downloading an artifact.
    param(
        [Parameter(Mandatory)] [string] $StepName,
        [Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Output
    )

    $failures = @(Get-FailedCargoTests -Output $Output)
    Write-Host "GATE_TEST_SUMMARY: $StepName reported $($failures.Count) failing test(s)"
    foreach ($failure in $failures) {
        $where = if ($null -eq $failure.Selector) { '(unattributed)' } else { $failure.Selector -join ' ' }
        Write-Host "GATE_TEST_FAILED: $where :: $($failure.TestName)"
    }
    $inBlock = $false
    $kept = 0
    foreach ($line in $Output) {
        if ($line -match '^---- \S+ stdout ----') { $inBlock = $true; $kept = 0 }
        elseif ($line -match '^(failures:|test result:)') { $inBlock = $false }
        if ($inBlock -and $kept -lt 12) {
            Write-Host "    $line"
            $kept++
        }
    }
}

function Get-OutputTail {
    param([AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Output, [int] $Lines = 60)

    if ($Output.Count -le $Lines) { return ($Output -join [Environment]::NewLine) }
    return ($Output[($Output.Count - $Lines)..($Output.Count - 1)] -join [Environment]::NewLine)
}

function Invoke-WorkspaceTestsIsolating {
    # Run the workspace suite; re-run each failure alone before calling it a failure.
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string[]] $Arguments,
        [int] $IsolatedAttempts = 2
    )

    $output = @(& cargo @Arguments 2>&1 | ForEach-Object { $_.ToString() })
    $exitCode = $LASTEXITCODE
    if ($exitCode -eq 0) {
        return [pscustomobject]@{ Name = $Name; Output = $output; Retried = @() }
    }
    Write-CargoFailureSummary -StepName $Name -Output $output
    $failures = @(Get-FailedCargoTests -Output $output)
    $unattributed = @($failures | Where-Object { $null -eq $_.Selector })
    if ($failures.Count -eq 0 -or $unattributed.Count -gt 0) {
        # Nothing to isolate: a compile error, a crashed test binary, or a failure
        # this parser cannot place. The tail is where cargo says why.
        throw "gate_test_failure: $Name exited $exitCode and no failing test could be isolated`n$(Get-OutputTail -Output $output)"
    }
    $retried = [System.Collections.Generic.List[string]]::new()
    foreach ($failure in $failures) {
        $label = "$($failure.Selector -join ' ') :: $($failure.TestName)"
        $passed = $false
        $last = @()
        for ($attempt = 1; $attempt -le $IsolatedAttempts -and -not $passed; $attempt++) {
            $isolatedArguments = @('test') + $failure.Selector + @('--locked', $failure.TestName, '--', '--exact')
            $last = @(& cargo @isolatedArguments 2>&1 | ForEach-Object { $_.ToString() })
            $passed = ($LASTEXITCODE -eq 0) -and (($last -join "`n") -match 'test result: ok\. 1 passed')
        }
        if (-not $passed) {
            throw "gate_test_failure: $label fails when run alone`n$(Get-OutputTail -Output $last)"
        }
        Write-Host "GATE_TEST_RETRIED: $label failed in the full run and passed alone"
        $retried.Add($label)
    }
    return [pscustomobject]@{ Name = $Name; Output = $output; Retried = $retried.ToArray() }
}
