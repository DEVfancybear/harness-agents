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

# Retry notices from the flake-tolerant step, forwarded to the log by its caller
# so a green run still says whether it needed a retry.
$script:GateRetryNotices = [System.Collections.Generic.List[string]]::new()

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
    while ($true) {
        $attempt++
        try {
            $command = Invoke-CheckedCommand -Name $Name -FilePath $FilePath -Arguments $Arguments
            return [pscustomobject]@{ Command = $command; Attempts = $attempt }
        } catch {
            $message = $_.Exception.Message
            $isFlake = $message -match 'error sending request' -or
                $message -match 'error decoding response body' -or
                $message -match 'connection reset|broken pipe' -or
                $message -match 'credential file .* could not be (written|replaced)'
            if (-not $isFlake -or $attempt -ge $MaxAttempts) { throw }
            $script:GateRetryNotices.Add("GATE_RETRY: $Name failed with the known host flake (attempt $attempt of $MaxAttempts); rerunning")
        }
    }
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
        @{ Name = 'workspace-tests'; File = 'cargo'; Arguments = @('test', '--workspace', '--all-targets', '--locked') }
    )) {
        if ($step.Name -ceq 'workspace-tests') {
            # The whole-workspace regression run is the only step retried for the
            # host flake; required milestone tests below stay single-shot. The
            # retry notices are forwarded so the log says whether a green step
            # needed one.
            $tolerant = $null
            try {
                $tolerant = Invoke-FlakeTolerantCommand -Name $step.Name -FilePath $step.File -Arguments $step.Arguments -MaxAttempts 3
            } finally {
                # Forwarded whether the step passed or gave up, so the log always
                # says how many attempts a result cost.
                Write-GateRetryNotices
            }
            if ($tolerant.Attempts -gt 1) {
                Write-Output "GATE_STEP_RETRIED: $($step.Name) needed $($tolerant.Attempts) attempts"
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
