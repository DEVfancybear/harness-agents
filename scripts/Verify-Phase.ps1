#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('P0')]
    [string] $Phase = 'P0',
    [string] $RepositoryRoot = (Join-Path $PSScriptRoot '..'),
    [switch] $SelfTest,
    [switch] $Json
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

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

    if ($Required.Count -eq 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'registry has no required P0 tests')
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
        [Parameter(Mandatory)] [string] $Phase
    )

    $paths = @(& git -C $Root ls-files --cached --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'git could not enumerate the source tree')
    }
    if ($paths.Count -eq 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message 'source tree enumeration returned zero files')
    }
    [string[]] $metadataPaths = @(
        "docs/evidence/$Phase.en.md",
        "docs/evidence/$Phase.vi.md",
        "docs/handoffs/$Phase.en.md",
        "docs/handoffs/$Phase.vi.md"
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
            scope = 'workspace source excluding phase evidence and handoff metadata'
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

function Invoke-GateSelfTest {
    $expectedTest = 'p0_f01_cli_help_and_version_run_without_credentials'
    Invoke-NegativeControl -Name 'zero-test-discovery' -ExpectedCode 'gate_test_discovery_empty' -Action {
        Assert-RequiredTestDiscovery -Discovered @() -Required @($expectedTest)
    }
    Invoke-NegativeControl -Name 'test-command-failure' -ExpectedCode 'gate_configuration_error' -Action {
        Invoke-CheckedCommand -Name 'synthetic-test' -FilePath (Join-Path $PSHOME 'pwsh') -Arguments @('-NoProfile', '-Command', 'exit 17')
    }
    Invoke-NegativeControl -Name 'missing-fixture' -ExpectedCode 'gate_configuration_error' -Action {
        $missingRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('harness-agents-p0-missing-' + [guid]::NewGuid().ToString('N'))
        Resolve-RepositoryFile -Root $missingRoot -RelativePath 'missing-fixture.json'
    }
    Invoke-NegativeControl -Name 'required-test-ignored' -ExpectedCode 'gate_required_test_ignored' -Action {
        Assert-RequiredTestResult -TestName $expectedTest -Output @("test $expectedTest ... ignored")
    }
    Write-Output 'PHASE_GATE_SELFTEST_OK: P0'
}

if ($SelfTest) {
    Invoke-GateSelfTest
    exit 0
}

$repoRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$registryPath = Resolve-RepositoryFile -Root $repoRoot -RelativePath 'tests/acceptance/registry.json'
try {
    $registry = Get-Content -LiteralPath $registryPath -Raw -Encoding utf8 | ConvertFrom-Json
} catch {
    throw (New-GateError -Code 'gate_configuration_error' -Message 'acceptance registry is not valid JSON')
}
if ($registry.schema_version -ne 1 -or $registry.registry_kind -cne 'acceptance') {
    throw (New-GateError -Code 'gate_configuration_error' -Message 'acceptance registry has an unsupported schema or kind')
}

$phaseCases = @($registry.cases | Where-Object { $_.phase -ceq $Phase -and $_.required -eq $true })
if ($phaseCases.Count -eq 0) {
    throw (New-GateError -Code 'gate_configuration_error' -Message "registry has no required cases for $Phase")
}
foreach ($case in $phaseCases) {
    if ($case.readiness -cne 'implemented') {
        throw (New-GateError -Code 'gate_configuration_error' -Message "required case is not implemented: $($case.id)")
    }
    if ([string]::IsNullOrWhiteSpace($case.target) -or @($case.test_names).Count -eq 0) {
        throw (New-GateError -Code 'gate_configuration_error' -Message "required case lacks target or test name: $($case.id)")
    }
    if ($null -ne $case.fixture) {
        [void] (Resolve-RepositoryFile -Root $repoRoot -RelativePath $case.fixture)
    }
}

$futureCases = @($registry.cases | Where-Object { $_.id -match '^[CK]\d{2}$' })
if ($futureCases.Count -ne 44) {
    throw (New-GateError -Code 'gate_configuration_error' -Message 'registry must contain all 44 C/K future cases')
}
foreach ($futureCase in $futureCases) {
    if ($futureCase.readiness -cne 'not_implemented' -or $futureCase.required -ne $false) {
        throw (New-GateError -Code 'gate_configuration_error' -Message "future case is falsely marked ready: $($futureCase.id)")
    }
}

$targets = @($phaseCases | ForEach-Object { $_.target } | Sort-Object -Unique)
if ($targets.Count -ne 1 -or $targets[0] -cne 'phase_p0') {
    throw (New-GateError -Code 'gate_configuration_error' -Message 'P0 registry must use the phase_p0 target only')
}
$requiredTests = @($phaseCases | ForEach-Object { $_.test_names } | Sort-Object -Unique)

$results = [System.Collections.Generic.List[object]]::new()
Push-Location -LiteralPath $repoRoot
try {
    foreach ($step in @(
        @{ Name = 'format'; File = 'cargo'; Arguments = @('fmt', '--all', '--', '--check') },
        @{ Name = 'clippy'; File = 'cargo'; Arguments = @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings') },
        @{ Name = 'workspace-tests'; File = 'cargo'; Arguments = @('test', '--workspace', '--all-targets', '--locked') }
    )) {
        $result = Invoke-CheckedCommand -Name $step.Name -FilePath $step.File -Arguments $step.Arguments
        $results.Add([pscustomobject]@{ name = $step.Name; result = 'passed' })
        if (-not $Json) { Write-Output "GATE_STEP_OK: $($step.Name)" }
    }

    $discovery = Invoke-CheckedCommand -Name 'phase-test-discovery' -FilePath 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', 'phase_p0', '--locked', '--', '--list')
    $discoveredTests = Get-DiscoveredTestNames -Output $discovery.Output
    Assert-RequiredTestDiscovery -Discovered $discoveredTests -Required $requiredTests
    $results.Add([pscustomobject]@{ name = 'phase-test-discovery'; result = 'passed'; tests = $discoveredTests })
    if (-not $Json) { Write-Output "GATE_STEP_OK: phase-test-discovery ($($discoveredTests.Count) tests)" }

    foreach ($testName in $requiredTests) {
        $result = Invoke-CheckedCommand -Name "required-test:$testName" -FilePath 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', 'phase_p0', '--locked', $testName, '--', '--exact')
        Assert-RequiredTestResult -TestName $testName -Output $result.Output
    }
    $results.Add([pscustomobject]@{ name = 'required-tests'; result = 'passed'; count = $requiredTests.Count })
    if (-not $Json) { Write-Output "GATE_STEP_OK: required-tests ($($requiredTests.Count) tests)" }

    $docs = Invoke-CheckedCommand -Name 'documentation-checks' -FilePath 'pwsh' -Arguments @('-NoProfile', '-File', 'scripts/Verify-Docs.ps1', '-SelfTest')
    $results.Add([pscustomobject]@{ name = 'documentation-checks'; result = 'passed' })
    if (-not $Json) { Write-Output 'GATE_STEP_OK: documentation-checks' }
} finally {
    Pop-Location
}

$sourceTree = Get-SourceTreeDigest -Root $repoRoot -Phase $Phase
$summary = [ordered]@{
    schema_version = 1
    phase = $Phase
    result = 'passed'
    source_tree = $sourceTree
    required_case_ids = @($phaseCases | ForEach-Object { $_.id } | Sort-Object)
    required_test_count = $requiredTests.Count
    discovered_test_count = $discoveredTests.Count
    steps = @($results)
}
if ($Json) {
    $summary | ConvertTo-Json -Depth 8 -Compress
} else {
    Write-Output "GATE_RESULT_JSON: $($summary | ConvertTo-Json -Depth 8 -Compress)"
}
