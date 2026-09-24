#Requires -Version 7.0
#
# Milestone gate (M0-M12). It runs the required checks for one milestone and its
# prerequisite closure, using the registry in tests/acceptance/milestones.json.
#
# The gate reuses the accepted shape of scripts/Verify-Phase.ps1: every step is a
# checked command, required tests are proven by exact discovery and by an `ok`
# result line, and missing, ignored or zero-test selectors exit nonzero.
#
# Usage:
#   pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M0 -SelfTest
#   pwsh -NoProfile -File scripts/Verify-Milestone.ps1 -Milestone M0
[CmdletBinding()]
param(
    [string] $Milestone = 'M0',
    [string] $RepositoryRoot = (Join-Path $PSScriptRoot '..'),
    [switch] $SelfTest,
    [switch] $Json
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'GateTestIsolation.ps1')

# Retry notices from the flake-tolerant step, forwarded to the log by its caller
# so a green run still says whether it needed a retry.
$script:GateRetryNotices = [System.Collections.Generic.List[string]]::new()

# The combined output of the last tolerant step that gave up. Set by
# `Invoke-FlakeTolerantCommand` on its final failure, because the thrown message
# is a formatted rendering of it: truncated when long and with collapsed
# whitespace, which is enough to hide the failing test names from the fallback.
$script:GateLastFailureOutput = @()

function Write-GateRetryNotices {
    foreach ($notice in $script:GateRetryNotices) { Write-Output $notice }
    $script:GateRetryNotices.Clear()
}

function New-GateError {
    param(
        [Parameter(Mandatory)] [string] $Code,
        [Parameter(Mandatory)] [string] $Message
    )
    return "$Code`: $Message"
}

function Invoke-CheckedCommand {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $FilePath,
        [Parameter(Mandatory)] [string[]] $Arguments
    )

    $output = @(& $FilePath @Arguments 2>&1 | ForEach-Object { $_.ToString() })
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message "$Name exited $exitCode`n$($output -join [Environment]::NewLine)")
    }
    return [pscustomobject]@{
        Name = $Name
        Arguments = $Arguments
        Output = $output
        ExitCode = $exitCode
    }
}

function Invoke-FlakeTolerantCommand {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $FilePath,
        [Parameter(Mandatory)] [string[]] $Arguments,
        [int] $MaxAttempts = 3
    )

    # Measured on 21-22/09/2026: this Windows host intermittently refuses a
    # loopback connection or a credential-file rename while the whole workspace
    # suite runs, and the same suites pass when run alone (the failure moves
    # between suites from run to run). Only these exact signatures are retried,
    # and only for the workspace test step; anything else rethrows at once, so a
    # real regression can never be hidden by this retry.
    #
    # 23/09/2026 (M6): a refusal arrived as
    # `service_unavailable: ... provider request failed: error sending request`
    # with no ` for url` suffix, which the narrower pattern missed, so the
    # workspace step failed on a flake the retry existed to absorb. The
    # signature now matches the transport-level phrase itself; it still cannot
    # match an assertion failure, and a genuinely unreachable provider is
    # retried at most MaxAttempts times and then rethrown.
    #
    # The retry notice is collected here and forwarded by the caller, because a
    # discarded `[void]` call hid whether a green step had needed a retry at all
    # — and an evidence claim about a gate run has to say that.
    $attempt = 0
    $lastOutput = @()
    while ($true) {
        $attempt++
        try {
            $command = Invoke-CheckedCommand -Name $Name -FilePath $FilePath -Arguments $Arguments
            return [pscustomobject]@{ Command = $command; Attempts = $attempt }
        } catch {
            $message = $_.Exception.Message
            # The thrown message is `code: name exited N` followed by the combined
            # output, so the output is recoverable from it - but only in full. A
            # caller that needs to read cargo's report uses this instead of the
            # message, because `Invoke-CheckedCommand` keeps the stream and the
            # message is for humans.
            #
            # It is recorded on every failure, not only when the retries run out:
            # an isolated re-run that fails for a reason that is not the flake
            # rethrows on its first attempt, and a caller that only saw the
            # exhausted path would have nothing to report but a truncated message.
            $lastOutput = @($message -split "`n")
            $script:GateLastFailureOutput = $lastOutput
            if ($null -eq (Get-FlakeSignature -Output $lastOutput)) { throw }
            if ($attempt -ge $MaxAttempts) { throw }
            $script:GateRetryNotices.Add("GATE_RETRY: $Name failed with the known host flake (attempt $attempt of $MaxAttempts); rerunning")
        }
    }
}

function Get-FlakeSignature {
    # The exact transport phrases this host produces when a loopback connection
    # to a freshly bound fixture listener is refused or reset under load. A
    # genuinely unreachable provider produces the same phrase, which is why the
    # retry is bounded and why the notice is printed.
    param([Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Output)

    $text = $Output -join "`n"
    # PowerShell's error rendering collapses runs of spaces, so the phrase arrives
    # as `error  sending request` in a formatted message while the raw cargo line
    # has one space. Normalizing whitespace is what lets one signature match both
    # readings of the same event.
    $text = $text -replace '\s+', ' '
    $signatures = @(
        'error sending request',
        'error decoding response body',
        'connection reset',
        'broken pipe',
        'credential file .* could not be (written|replaced)'
    )
    foreach ($signature in $signatures) {
        if ($text -match $signature) { return $signature }
    }
    return $null
}

function Get-FailedIntegrationTests {
    # `cargo test --workspace` reports one `Running <path>` line per test binary
    # and one `test <name> ... FAILED` line per failing test. Separate failures
    # share one process, so the failing test has to be re-run alone: that is the
    # whole point of this step, and it is why the binary is mapped back to the
    # `--test <target>` file name cargo accepts.
    #
    # 23/09/2026 (M7): cargo's own output marks the failing test name with
    # backticks (`test \`name\` ... FAILED`). A parser that only matched a bare
    # name found nothing, so the per-test fallback threw the whole workspace
    # failure instead of isolating it - measured on this host, where two
    # `interactive_launch` tests hit the loopback flake and the fallback reported
    # "no failing integration test to isolate". The backticks are stripped before
    # matching, which is also what makes the reported name the name `cargo test
    # --exact` accepts.
    #
    # 23/09/2026 (M8): the flake then landed in a *milestone* target
    # (`milestone_m2::m2_04_text_arrives_before_the_terminal_barrier`), and the
    # parser - which only recognized a target under `tests/` - refused to
    # isolate it. Every integration-test binary has the same shape
    # (`tests/<name>.rs (target/.../<name>-<hash>.exe)`), so the rule is now the
    # file name itself rather than where it sits: a milestone target is exactly
    # as re-runnable by name as the workspace's own suites.
    param([Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Output)

    $target = $null
    $failures = [System.Collections.Generic.List[object]]::new()
    foreach ($line in $Output) {
        # `$Matches` is a single script-wide hashtable: the `.Replace` call below
        # runs a regex match of its own and overwrites it. The name therefore has
        # to be captured into a variable in the same statement that matches it,
        # or a backticked failure line reports whatever matched last - measured on
        # this host, where that made the parser find nothing and the step give up
        # on a failure it could have isolated.
        $normalized = $line.Replace('`', '')
        if ($normalized -match 'Running\s+\S*tests[/\\](?<name>[A-Za-z0-9_]+)\.rs') {
            $target = $Matches['name']
            continue
        }
        # Cargo's own summary line for the target under test is `running N tests`,
        # lowercase and with no path. It names no target, so it must not clear the
        # one the preceding `Running` line set: doing that made every failure in a
        # milestone target unattributable, which is exactly where the flake lands.
        if ($normalized -match '^\s*running\s+\d+\s+test') {
            continue
        }
        # Any other `Running` line (unit binary, doc test, example) clears the
        # target: a failure under it is not an integration test this step may
        # re-run by file name, and guessing one would misattribute the failure.
        if ($normalized -match '^\s*Running\s') {
            $target = $null
            continue
        }
        if ($normalized -match '^\s*test\s+(?<name>[^\s]+)\s+\.\.\.\s+FAILED\b') {
            if ($null -ne $target) {
                $failures.Add([pscustomobject]@{ Target = $target; TestName = $Matches['name'] })
            }
        }
    }
    return @($failures)
}

function Get-DiscoveredTestNames {
    param([Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Output)

    $names = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($line in $Output) {
        if ($line -match '^\s*(?<name>[A-Za-z0-9_:-]+): test$') {
            [void] $names.Add($Matches.name)
        }
    }
    return @($names | Sort-Object)
}

function Assert-RequiredTestDiscovery {
    param(
        [Parameter(Mandatory)] [AllowEmptyCollection()] [string[]] $Discovered,
        [Parameter(Mandatory)] [string[]] $Required
    )

    # A single-element array arrives as a scalar, and StrictMode forbids
    # `.Count` on a scalar; wrap before counting.
    $Discovered = @($Discovered)
    $Required = @($Required)
    if ($Required.Count -eq 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'milestone registry has no required tests')
    }
    if ($Discovered.Count -eq 0) {
        throw (New-GateError -Code 'gate_test_discovery_empty' -Message 'cargo test discovery returned zero tests')
    }
    foreach ($testName in $Required) {
        if ($testName -notin $Discovered) {
            throw (New-GateError -Code 'gate_configuration_error' -Message "required test was not discovered: $testName")
        }
    }
}

function Assert-RequiredTestResult {
    param(
        [Parameter(Mandatory)] [string] $TestName,
        [Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Output
    )

    $escaped = [regex]::Escape($TestName)
    if (($Output -join "`n") -match "(?m)^test\s+$escaped\s+\.\.\.\s+ignored\b") {
        throw (New-GateError -Code 'gate_required_test_ignored' -Message "required test is ignored: $TestName")
    }
    if (($Output -join "`n") -notmatch "(?m)^test\s+$escaped\s+\.\.\.\s+ok\b") {
        throw (New-GateError -Code 'gate_configuration_error' -Message "required test did not report success: $TestName")
    }
}

function Get-TestSelector {
    param(
        [Parameter(Mandatory)] [string] $Selector,
        [Parameter(Mandatory)] [string] $DefaultTarget
    )

    # A selector is either "test_name" (proven by the milestone's own target) or
    # "target::test_name" (proven by an earlier accepted target, e.g. phase_p1).
    $separator = $Selector.IndexOf('::')
    if ($separator -gt 0) {
        return [pscustomobject]@{
            Selector = $Selector
            Target = $Selector.Substring(0, $separator)
            TestName = $Selector.Substring($separator + 2)
        }
    }
    return [pscustomobject]@{
        Selector = $Selector
        Target = $DefaultTarget
        TestName = $Selector
    }
}

function Resolve-RepositoryFile {
    param(
        [Parameter(Mandatory)] [string] $Root,
        [Parameter(Mandatory)] [string] $RelativePath
    )

    $candidate = [System.IO.Path]::GetFullPath((Join-Path $Root $RelativePath))
    $relative = [System.IO.Path]::GetRelativePath($Root, $candidate)
    if ($relative -eq '..' -or $relative.StartsWith("..$([System.IO.Path]::DirectorySeparatorChar)") -or [System.IO.Path]::IsPathRooted($relative)) {
        throw (New-GateError -Code 'gate_configuration_error' -Message "registry path escapes repository: $RelativePath")
    }
    if (-not (Test-Path -LiteralPath $candidate)) {
        throw (New-GateError -Code 'gate_configuration_error' -Message "required fixture or artifact is missing: $RelativePath")
    }
    return $candidate
}

function Get-SourceTreeDigest {
    param(
        [Parameter(Mandatory)] [string] $Root,
        [Parameter(Mandatory)] [string] $MilestoneId
    )

    $paths = @(& git -C $Root ls-files --cached --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'git could not enumerate the source tree')
    }
    if ($paths.Count -eq 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'source tree enumeration returned zero files')
    }
    [string[]] $metadataPaths = @(
        "docs/evidence/$MilestoneId.vi.md",
        "docs/handoffs/CURRENT.vi.md"
    )
    [string[]] $normalizedPaths = @($paths | ForEach-Object { ([string] $_).Replace('\', '/') })
    [string[]] $excludedPaths = @($normalizedPaths | Where-Object { $metadataPaths -contains $_ })
    [string[]] $orderedPaths = @($normalizedPaths | Where-Object { $metadataPaths -notcontains $_ })
    if ($orderedPaths.Count -eq 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'source tree has no non-metadata files to hash')
    }
    [System.Array]::Sort($orderedPaths, [System.StringComparer]::Ordinal)
    [System.Array]::Sort($excludedPaths, [System.StringComparer]::Ordinal)

    $hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        $fileCount = 0
        foreach ($relativePath in $orderedPaths) {
            if ([string]::IsNullOrWhiteSpace($relativePath)) {
                throw (New-GateError -Code 'gate_configuration_error' -Message 'source tree contains an empty path')
            }
            $absolutePath = [System.IO.Path]::GetFullPath((Join-Path $Root $relativePath))
            $relativeCheck = [System.IO.Path]::GetRelativePath($Root, $absolutePath)
            if ($relativeCheck -eq '..' -or $relativeCheck.StartsWith("..$([System.IO.Path]::DirectorySeparatorChar)") -or
                [System.IO.Path]::IsPathRooted($relativeCheck) -or -not (Test-Path -LiteralPath $absolutePath -PathType Leaf)) {
                throw (New-GateError -Code 'gate_configuration_error' -Message "source tree path is unsafe or missing: $relativePath")
            }
            $contentHash = (Get-FileHash -LiteralPath $absolutePath -Algorithm SHA256).Hash.ToLowerInvariant()
            $record = $relativePath.Replace('\', '/') + [char]0 + $contentHash + "`n"
            $bytes = $utf8.GetBytes($record)
            $buffer = [byte[]]::new($bytes.Length)
            [void] $hasher.TransformBlock($bytes, 0, $bytes.Length, $buffer, 0)
            $fileCount++
        }
        [void] $hasher.TransformFinalBlock([byte[]]::new(0), 0, 0)
        $digest = [Convert]::ToHexString($hasher.Hash).ToLowerInvariant()
        return [pscustomobject]@{
            algorithm = 'sha256'
            file_count = $fileCount
            digest = "sha256:$digest"
            scope = 'workspace source excluding milestone evidence and CURRENT handoff'
            excluded_paths = $excludedPaths
        }
    } finally {
        $hasher.Dispose()
    }
}

function Invoke-NegativeControl {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $ExpectedCode,
        [Parameter(Mandatory)] [scriptblock] $Action
    )

    try {
        & $Action
    } catch {
        if ($_.Exception.Message.StartsWith("$ExpectedCode`:")) {
            Write-Output "NEGATIVE_CONTROL_OK: $Name"
            return
        }
        throw
    }
    throw "NEGATIVE_CONTROL_FAILED: $Name"
}

function Read-MilestoneRegistry {
    param([Parameter(Mandatory)] [string] $Root)

    $registryPath = Resolve-RepositoryFile -Root $Root -RelativePath 'tests/acceptance/milestones.json'
    try {
        $registry = Get-Content -LiteralPath $registryPath -Raw -Encoding utf8 | ConvertFrom-Json
    } catch {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'milestone registry is not valid JSON')
    }
    if ($registry.schema_version -ne 1 -or $registry.registry_kind -cne 'milestone-registry') {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'milestone registry has an unsupported schema or kind')
    }
    return $registry
}

function Resolve-Milestone {
    param(
        [Parameter(Mandatory)] $Registry,
        [Parameter(Mandatory)] [string] $MilestoneId
    )

    $milestone = @($Registry.milestones | Where-Object { $_.id -ceq $MilestoneId })
    if ($milestone.Count -ne 1) {
        throw (New-GateError -Code 'gate_configuration_error' -Message "unknown milestone: $MilestoneId")
    }
    return $milestone[0]
}

function Resolve-MilestoneClosure {
    param(
        [Parameter(Mandatory)] $Registry,
        [Parameter(Mandatory)] [string] $MilestoneId
    )

    $closure = [System.Collections.Generic.List[string]]::new()
    $pending = [System.Collections.Generic.Queue[string]]::new()
    $pending.Enqueue($MilestoneId)
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    while ($pending.Count -gt 0) {
        $current = $pending.Dequeue()
        if (-not $seen.Add($current)) {
            continue
        }
        $entry = Resolve-Milestone -Registry $Registry -MilestoneId $current
        $closure.Add($current)
        foreach ($prerequisite in @($entry.prerequisites)) {
            $pending.Enqueue([string] $prerequisite)
        }
    }
    return @($closure)
}

function Invoke-DependencyCheck {
    param(
        [Parameter(Mandatory)] [string] $Root,
        [string[]] $ExtraEdges = @()
    )

    $arguments = @('run', '--quiet', '-p', 'harness-cli', '--bin', 'dependency_check', '--locked', '--', '--root', $Root)
    foreach ($edge in $ExtraEdges) {
        $arguments += @('--extra-edge', $edge)
    }
    $output = @(& cargo @arguments 2>&1 | ForEach-Object { $_.ToString() })
    $exitCode = $LASTEXITCODE
    return [pscustomobject]@{ ExitCode = $exitCode; Output = $output }
}

function Invoke-GateSelfTest {
    param(
        [Parameter(Mandatory)] [string] $Root,
        [Parameter(Mandatory)] [string] $MilestoneId
    )

    $expectedTest = 'm0_01_retry_class_and_exit_codes_match_the_contract'
    Invoke-NegativeControl -Name 'missing-selector' -ExpectedCode 'gate_configuration_error' -Action {
        Assert-RequiredTestDiscovery -Discovered @('some_other_test') -Required @($expectedTest)
    }
    Invoke-NegativeControl -Name 'ignored-required-test' -ExpectedCode 'gate_required_test_ignored' -Action {
        Assert-RequiredTestResult -TestName $expectedTest -Output @("test $expectedTest ... ignored")
    }
    Invoke-NegativeControl -Name 'command-nonzero' -ExpectedCode 'gate_configuration_error' -Action {
        Invoke-CheckedCommand -Name 'synthetic-test' -FilePath (Join-Path $PSHOME 'pwsh') -Arguments @('-NoProfile', '-Command', 'exit 17')
    }
    Invoke-NegativeControl -Name 'zero-test-discovery' -ExpectedCode 'gate_test_discovery_empty' -Action {
        Assert-RequiredTestDiscovery -Discovered @() -Required @($expectedTest)
    }
    Invoke-NegativeControl -Name 'unknown-milestone' -ExpectedCode 'gate_configuration_error' -Action {
        $registry = Read-MilestoneRegistry -Root $Root
        [void] (Resolve-Milestone -Registry $registry -MilestoneId 'M99')
    }
    Invoke-NegativeControl -Name 'qualified-selector' -ExpectedCode 'gate_configuration_error' -Action {
        $parsed = Get-TestSelector -Selector 'phase_p1::p1_c01_some_test' -DefaultTarget 'milestone_m1'
        if ($parsed.Target -cne 'phase_p1' -or $parsed.TestName -cne 'p1_c01_some_test') {
            throw (New-GateError -Code 'gate_configuration_error' -Message "selector split is wrong: $($parsed.Target) :: $($parsed.TestName)")
        }
        Assert-RequiredTestDiscovery -Discovered @('p1_c01_some_test') -Required @('p1_c01_other_test')
    }
    $forbidden = Invoke-DependencyCheck -Root $Root -ExtraEdges @('harness-runtime->harness-tools')
    if ($forbidden.ExitCode -eq 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'dependency checker accepted a forbidden edge')
    }
    Write-Output 'NEGATIVE_CONTROL_OK: dependency-edge'

    # The per-test fallback of the workspace step reads cargo's own output. It
    # must name the target file and the failing test, and it must find nothing
    # in output that carries no failure - a fallback that guessed would re-run
    # an innocent test and report it as the flake.
    $synthetic = @(
        '     Running tests\interactive_launch.rs (target\debug\deps\interactive_launch-e569c194625e4b56.exe)',
        'test i02_help_and_version_stay_fast_paths_that_write_nothing ... ok',
        'test i04_the_binary_installed_under_a_unicode_path_follows_the_caller_directory ... FAILED',
        'test result: FAILED. 18 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'
    )
    $parsed = @(Get-FailedIntegrationTests -Output $synthetic)
    if ($parsed.Count -ne 1 -or $parsed[0].Target -cne 'interactive_launch' -or
        $parsed[0].TestName -cne 'i04_the_binary_installed_under_a_unicode_path_follows_the_caller_directory') {
        throw (New-GateError -Code 'gate_configuration_error' -Message "cargo output parsing is wrong: $($parsed | ConvertTo-Json -Compress)")
    }
    # Cargo brackets the failing name with backticks in its own report, and the
    # replacement that strips them runs a match of its own: this case is what
    # catches a parser that reads `$Matches` after the replace instead of
    # capturing the name when it matched.
    $backticked = @(
        '     Running tests/milestone_m7.rs (target/debug/deps/milestone_m7-0123456789abcdef.exe)',
        'test `a25_memory_job_cas` ... FAILED',
        'test `m7_04_cli_propose_confirm_reject_export` ... FAILED'
    )
    $parsed = @(Get-FailedIntegrationTests -Output $backticked)
    if ($parsed.Count -ne 2 -or $parsed[0].Target -cne 'milestone_m7' -or
        $parsed[0].TestName -cne 'a25_memory_job_cas' -or
        $parsed[1].TestName -cne 'm7_04_cli_propose_confirm_reject_export') {
        throw (New-GateError -Code 'gate_configuration_error' -Message "backticked cargo output parsing is wrong: $($parsed | ConvertTo-Json -Compress)")
    }
    # A milestone target under crates/ is just as re-runnable by name as the
    # workspace's own tests/ suites, and the flake does land in it.
    $nested = @(
        '     Running crates\harness-cli\tests\milestone_m2.rs (target\debug\deps\milestone_m2-d92cb5d5c0ffb6ac.exe)',
        'test m2_04_text_arrives_before_the_terminal_barrier ... FAILED'
    )
    $parsed = @(Get-FailedIntegrationTests -Output $nested)
    if ($parsed.Count -ne 1 -or $parsed[0].Target -cne 'milestone_m2' -or
        $parsed[0].TestName -cne 'm2_04_text_arrives_before_the_terminal_barrier') {
        throw (New-GateError -Code 'gate_configuration_error' -Message "nested cargo output parsing is wrong: $($parsed | ConvertTo-Json -Compress)")
    }
    # A unit-test binary has no target file to re-run, so a failure under it must
    # not be attributed to whichever integration target ran last.
    $after_unit = @(
        '     Running tests/milestone_m7.rs (target/debug/deps/milestone_m7-0123456789abcdef.exe)',
        'test a25_memory_job_cas ... FAILED',
        '     Running unittests src\main.rs (target\debug\deps\ha-dd3932fc9bd5f0e9.exe)',
        'test interactive::memory::tests::something ... FAILED'
    )
    $parsed = @(Get-FailedIntegrationTests -Output $after_unit)
    if ($parsed.Count -ne 1 -or $parsed[0].Target -cne 'milestone_m7') {
        throw (New-GateError -Code 'gate_configuration_error' -Message "a unit failure was attributed to an integration target: $($parsed | ConvertTo-Json -Compress)")
    }
    # Cargo's own `running N tests` summary line names no target. Clearing the
    # target on it made every failure in a milestone target unattributable, which
    # is where the host flake actually lands.
    $with_summary = @(
        '     Running tests\milestone_m2.rs (target\debug\deps\milestone_m2-d92cb5d5c0ffb6ac.exe)',
        'running 10 tests',
        'test `m2_04_text_arrives_before_the_terminal_barrier` ... FAILED',
        'test `a07_401_is_not_retried_and_transient_is_bounded` ... FAILED'
    )
    $parsed = @(Get-FailedIntegrationTests -Output $with_summary)
    if ($parsed.Count -ne 2 -or $parsed[0].Target -cne 'milestone_m2' -or
        $parsed[0].TestName -cne 'm2_04_text_arrives_before_the_terminal_barrier' -or
        $parsed[1].TestName -cne 'a07_401_is_not_retried_and_transient_is_bounded') {
        throw (New-GateError -Code 'gate_configuration_error' -Message "a summary line hid the target: $($parsed | ConvertTo-Json -Compress)")
    }
    Invoke-NegativeControl -Name 'no-failure-no-isolation' -ExpectedCode 'gate_configuration_error' -Action {
        $none = @(Get-FailedIntegrationTests -Output @('     Running tests/phase_p1.rs (target/debug/deps/phase_p1-abcdef0123456789.exe)', 'test result: ok. 21 passed; 0 failed'))
        if ($none.Count -ne 0) {
            throw (New-GateError -Code 'gate_configuration_error' -Message 'parser invented a failing test')
        }
        throw (New-GateError -Code 'gate_configuration_error' -Message 'no failing integration test to isolate')
    }
    Invoke-NegativeControl -Name 'assertion-is-not-a-flake' -ExpectedCode 'gate_configuration_error' -Action {
        $assertion = @('test m7_01_something ... FAILED', "assertion `left == right` failed")
        if ($null -ne (Get-FlakeSignature -Output $assertion)) {
            throw (New-GateError -Code 'gate_configuration_error' -Message 'an assertion failure matched the transport signature')
        }
        throw (New-GateError -Code 'gate_configuration_error' -Message 'assertion failure is not retryable')
    }
    Write-Output "MILESTONE_GATE_SELFTEST_OK: $MilestoneId"
}

if ($SelfTest) {
    $selfTestRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
    Invoke-GateSelfTest -Root $selfTestRoot -MilestoneId $Milestone
    exit 0
}

$repoRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$registry = Read-MilestoneRegistry -Root $repoRoot
$milestoneEntry = Resolve-Milestone -Registry $registry -MilestoneId $Milestone
$closure = Resolve-MilestoneClosure -Registry $registry -MilestoneId $Milestone
$integrationTarget = [string] $milestoneEntry.integration_target
if ([string]::IsNullOrWhiteSpace($integrationTarget)) {
    throw (New-GateError -Code 'gate_configuration_error' -Message "$Milestone has no integration target")
}
$requiredTests = @($milestoneEntry.required_tests | ForEach-Object { [string] $_ } | Sort-Object -Unique)
if ($requiredTests.Count -eq 0) {
    throw (New-GateError -Code 'gate_configuration_error' -Message "$Milestone has no required tests")
}
foreach ($fixture in @($milestoneEntry.fixtures)) {
    [void] (Resolve-RepositoryFile -Root $repoRoot -RelativePath ([string] $fixture))
}

$results = [System.Collections.Generic.List[object]]::new()
Push-Location -LiteralPath $repoRoot
try {
    foreach ($step in @(
        @{ Name = 'format'; File = 'cargo'; Arguments = @('fmt', '--all', '--', '--check') },
        @{ Name = 'clippy'; File = 'cargo'; Arguments = @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings') },
        @{ Name = 'build'; File = 'cargo'; Arguments = @('build', '--workspace', '--locked') },
        # The release-candidate acceptance case depends on an artifact created by
        # the dedicated M9 job. Keep it out of broad regression runs; M9 executes
        # it below as a required test after the artifact-preparation step.
        @{ Name = 'workspace-tests'; File = 'cargo'; Arguments = @('test', '--workspace', '--all-targets', '--locked', '--', '--skip', 'm9_04_release_candidate_has_checksums_and_is_not_published') }
    )) {
        if ($step.Name -ceq 'workspace-tests') {
            # 24/09/2026: the transport-signature retry above absorbed only
            # loopback refusals, and the CI suite went red for 30 pushes on other
            # load failures that pass alone. Every failure is now re-run by itself
            # first; see GateTestIsolation.ps1. A test that also fails alone still
            # fails the gate, with its own output.
            $isolated = Invoke-WorkspaceTestsIsolating -Name $step.Name -Arguments $step.Arguments
            foreach ($label in $isolated.Retried) {
                Write-Output "GATE_STEP_RETRIED: workspace-tests tolerated $label alone after the full run failed it"
            }
        } else {
            [void] (Invoke-CheckedCommand -Name $step.Name -FilePath $step.File -Arguments $step.Arguments)
        }
        $results.Add([pscustomobject]@{ name = $step.Name; result = 'passed' })
        if (-not $Json) { Write-Output "GATE_STEP_OK: $($step.Name)" }
    }

    $dependency = Invoke-DependencyCheck -Root $repoRoot
    if ($dependency.ExitCode -ne 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message "dependency allowlist rejected the workspace`n$($dependency.Output -join [Environment]::NewLine)")
    }
    $dependencyReport = ($dependency.Output -join "`n") | ConvertFrom-Json
    if ($dependencyReport.status -cne 'ok') {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'dependency checker did not report ok')
    }
    $results.Add([pscustomobject]@{ name = 'dependency-allowlist'; result = 'passed'; edges = $dependencyReport.edge_count })
    if (-not $Json) { Write-Output "GATE_STEP_OK: dependency-allowlist ($($dependencyReport.edge_count) edges)" }

    # Unit suites: every declared unit test must exist and pass, so a milestone
    # cannot pass with a zero-test unit suite.
    $unitTests = @($milestoneEntry.unit_tests)
    $unitPassed = 0
    foreach ($unit in $unitTests) {
        $package = [string] $unit.package
        $testName = [string] $unit.test_name
        $result = Invoke-CheckedCommand -Name "unit-test:$package::$testName" -FilePath 'cargo' -Arguments @('test', '-p', $package, '--locked', $testName, '--', '--exact')
        Assert-RequiredTestResult -TestName $testName -Output $result.Output
        $unitPassed++
    }
    $results.Add([pscustomobject]@{ name = 'unit-tests'; result = 'passed'; count = $unitPassed })
    if (-not $Json) { Write-Output "GATE_STEP_OK: unit-tests ($unitPassed tests)" }

    $selectors = @($requiredTests | ForEach-Object { Get-TestSelector -Selector $_ -DefaultTarget $integrationTarget })
    $targets = @($selectors | ForEach-Object { $_.Target } | Sort-Object -Unique)
    $discoveredByTarget = @{}
    foreach ($target in $targets) {
        $targetRequired = @($selectors | Where-Object { $_.Target -ceq $target } | ForEach-Object { $_.TestName } | Sort-Object -Unique)
        $discovery = Invoke-CheckedCommand -Name "test-discovery:$target" -FilePath 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', $target, '--locked', '--', '--list')
        $discoveredTarget = @(Get-DiscoveredTestNames -Output $discovery.Output)
        Assert-RequiredTestDiscovery -Discovered $discoveredTarget -Required $targetRequired
        $discoveredByTarget[$target] = $discoveredTarget.Count
        if (-not $Json) { Write-Output "GATE_STEP_OK: test-discovery:$target ($($discoveredTarget.Count) tests)" }
    }
    $discoveredCount = 0
    foreach ($targetCount in $discoveredByTarget.Values) { $discoveredCount += [int] $targetCount }
    $results.Add([pscustomobject]@{ name = 'milestone-test-discovery'; result = 'passed'; discovered = $discoveredCount; targets = $targets })
    if (-not $Json) { Write-Output "GATE_STEP_OK: milestone-test-discovery ($discoveredCount tests over $(@($targets).Count) target(s))" }

    foreach ($selector in $selectors) {
        $result = Invoke-CheckedCommand -Name "required-test:$($selector.Selector)" -FilePath 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', $selector.Target, '--locked', $selector.TestName, '--', '--exact')
        Assert-RequiredTestResult -TestName $selector.TestName -Output $result.Output
    }
    $results.Add([pscustomobject]@{ name = 'required-tests'; result = 'passed'; count = $requiredTests.Count })
    if (-not $Json) { Write-Output "GATE_STEP_OK: required-tests ($($requiredTests.Count) tests)" }

    # The closure contains the milestone itself, whose required tests already
    # ran above; only its prerequisites are re-run as regressions. A closure
    # entry may prove a case with a qualified selector (`phase_p1::name`), so
    # the closure splits selectors by target exactly like the milestone's own
    # required tests: a qualified selector is discovered and run in the target
    # it names, not assumed to live in the prerequisite's milestone target.
    foreach ($closureMilestone in @($closure | Where-Object { $_ -cne $Milestone })) {
        $entry = Resolve-Milestone -Registry $registry -MilestoneId $closureMilestone
        $entryTests = @($entry.required_tests | ForEach-Object { [string] $_ } | Sort-Object -Unique)
        if ($entryTests.Count -eq 0) {
            throw (New-GateError -Code 'gate_configuration_error' -Message "closure milestone $closureMilestone has no required tests")
        }
        $entryTarget = [string] $entry.integration_target
        $entrySelectors = @($entryTests | ForEach-Object { Get-TestSelector -Selector $_ -DefaultTarget $entryTarget })
        foreach ($target in @($entrySelectors | ForEach-Object { $_.Target } | Sort-Object -Unique)) {
            $targetRequired = @($entrySelectors | Where-Object { $_.Target -ceq $target } | ForEach-Object { $_.TestName } | Sort-Object -Unique)
            $entryDiscovery = Invoke-CheckedCommand -Name "closure-$closureMilestone-discovery:$target" -FilePath 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', $target, '--locked', '--', '--list')
            $entryDiscovered = @(Get-DiscoveredTestNames -Output $entryDiscovery.Output)
            Assert-RequiredTestDiscovery -Discovered $entryDiscovered -Required $targetRequired
        }
        foreach ($selector in $entrySelectors) {
            # Closure tests are predecessor proofs. They still run, but a
            # transport-level host flake (the same signatures retried for the
            # workspace step) may be retried once more here; a typed or
            # assertion failure rethrows at once, and every milestone's own
            # required tests above stay single-shot.
            $tolerant = $null
            try {
                $tolerant = Invoke-FlakeTolerantCommand -Name "closure-$closureMilestone-test:$($selector.Selector)" -FilePath 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', $selector.Target, '--locked', $selector.TestName, '--', '--exact')
            } finally {
                Write-GateRetryNotices
            }
            $result = $tolerant.Command
            Assert-RequiredTestResult -TestName $selector.TestName -Output $result.Output
        }
        $results.Add([pscustomobject]@{ name = "closure-$closureMilestone"; result = 'passed'; count = $entryTests.Count })
        if (-not $Json) { Write-Output "GATE_STEP_OK: closure-$closureMilestone ($($entryTests.Count) tests)" }
    }
} finally {
    Pop-Location
}

$sourceTree = Get-SourceTreeDigest -Root $repoRoot -MilestoneId $Milestone
$summary = [ordered]@{
    schema_version = 1
    milestone = $Milestone
    result = 'passed'
    closure = $closure
    source_tree = $sourceTree
    required_test_count = @($requiredTests).Count
    discovered_test_count = $discoveredCount
    steps = @($results)
}
if ($Json) {
    $summary | ConvertTo-Json -Depth 8 -Compress
} else {
    Write-Output "GATE_RESULT_JSON: $($summary | ConvertTo-Json -Depth 8 -Compress)"
}
