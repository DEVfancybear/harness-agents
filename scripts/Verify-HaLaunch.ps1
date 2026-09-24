<#
.SYNOPSIS
Runtime gate for the HA_LAUNCH track: the exact tests and checks that must pass
before a checkpoint may be called done.

.DESCRIPTION
The gate runs format and clippy over the workspace, the interactive unit tests in
the `ha` binary, the launch acceptance tests (dispatch, non-TTY guard, headless
through a real adapter, resume with recovered context) with one test thread - the
loopback fixtures in this environment flake when tests run in parallel - the
session/turn acceptance tests, the provider streaming tests, the predecessor phase
regressions P0-P7, the installer self test and the documentation checker with its
negative controls.

Items the gate deliberately reports as NOT RUN instead of counting them as passes:
the PTY transcript cases in a real terminal, the live provider smoke, any real
mutation of the user's PATH or profile, and the Windows CMD installer check when
the gate runs on a non-Windows host.

A required selector that disappeared fails the gate instead of silently reducing
coverage.

.PARAMETER Json
Emit the report as JSON on stdout.

.PARAMETER SelfTest
Check the gate's own discovery parsing and required-selector list, then exit.

.EXAMPLE
pwsh -NoProfile -File scripts/Verify-HaLaunch.ps1
Run the whole gate.
#>
[CmdletBinding()]
param(
    [switch] $Json,
    [switch] $SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$script:Steps = [System.Collections.Generic.List[object]]::new()

function Invoke-NativeCapture {
    param([string] $File, [string[]] $Arguments)
    # Windows PowerShell 5.1 wraps native stderr as NativeCommandError. Cargo writes
    # normal progress to stderr, so capture each record as text and judge the command
    # by LASTEXITCODE.
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = (& $File @Arguments 2>&1 | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
        $exit = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    return [pscustomobject]@{ Output = $output; ExitCode = $exit }
}

function Invoke-GateStep {
    param(
        [string] $Name,
        [string] $File,
        [string[]] $Arguments
    )
    Write-Host "== $Name"
    Write-Host "   $File $($Arguments -join ' ')"
    $start = Get-Date
    $result = Invoke-NativeCapture -File $File -Arguments $Arguments
    $output = $result.Output
    $exit = $result.ExitCode
    $elapsed = [int] ((Get-Date) - $start).TotalSeconds
    $tail = ($output -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Last 3) -join ' | '
    $script:Steps.Add([pscustomobject]@{
            Name     = $Name
            ExitCode = $exit
            Seconds  = $elapsed
            Detail   = $tail
        })
    if ($exit -ne 0) {
        Write-Host "   FAILED (exit $exit after $elapsed s): $tail"
    }
    else {
        Write-Host "   ok (after $elapsed s)"
    }
    return $exit
}

function Get-DiscoveredTests {
    param([string] $File, [string[]] $Arguments)
    $result = Invoke-NativeCapture -File $File -Arguments $Arguments
    if ($result.ExitCode -ne 0) {
        throw "test discovery failed for $File $($Arguments -join ' ')"
    }
    $names = [System.Collections.Generic.List[string]]::new()
    foreach ($line in ($result.Output -split "`r?`n")) {
        if ($line -match '^([A-Za-z0-9_:]+): test$') {
            $names.Add($Matches[1])
        }
    }
    return $names
}

function Assert-Selector {
    param([string] $File, [string[]] $DiscoveryArguments, [string] $Selector)
    $discovered = Get-DiscoveredTests -File $File -Arguments $DiscoveryArguments
    if ($discovered.Count -eq 0) {
        throw "gate_test_discovery_empty: $File reported no tests"
    }
    if (-not $discovered.Contains($Selector)) {
        throw "gate_required_test_ignored: $Selector was not discovered in $File"
    }
    return $discovered.Count
}

$requiredSelectors = @(
    @{ Target = 'interactive_launch'; Selector = 'i03_bare_launch_without_a_terminal_exits_two_with_instructions' },
    @{ Target = 'interactive_launch'; Selector = 'i03_headless_turn_runs_through_the_real_adapter_and_keeps_the_key_out_of_output' },
    @{ Target = 'interactive_launch'; Selector = 'i13_resume_continues_the_task_with_recovered_context_and_no_rerun' },
    @{ Target = 'interactive_launch'; Selector = 'i13_a_hard_kill_mid_turn_leaves_one_admitted_input_and_no_claimed_success' },
    @{ Target = 'interactive_launch'; Selector = 'i09_a_corrupt_configuration_stops_the_run_with_an_actionable_error' },
    @{ Target = 'interactive_launch'; Selector = 'i09_a_data_root_that_cannot_be_created_names_the_path_and_writes_nothing' },
    @{ Target = 'interactive_launch'; Selector = 'i09_a_data_directory_without_write_permission_names_the_path_and_writes_nothing' },
    @{ Target = 'interactive_launch'; Selector = 'i04_the_binary_installed_under_a_unicode_path_follows_the_caller_directory' },
    @{ Target = 'interactive_launch'; Selector = 'i16_a_second_run_in_the_same_project_is_refused_while_the_first_holds_the_store' },
    @{ Target = 'interactive_session'; Selector = 'h05_a_settled_receipt_is_not_re_executed_after_the_process_state_is_lost' },
    @{ Target = 'interactive_session'; Selector = 'g2_tool_results_return_to_the_model_and_the_turn_ends_with_the_answer' },
    @{ Target = 'interactive_session'; Selector = 'h05_a_denied_gated_action_is_not_executed_and_the_model_is_told' },
    @{ Target = 'interactive_session'; Selector = 'g3_a_second_input_in_the_same_session_carries_real_context' }
)

$requiredTuiSelectors = @(
    'interactive::controller::tests::t02_plain_transcript_is_byte_identical_to_h03',
    'interactive::input::tests::t03_paste_keeps_newlines_and_submits_once',
    'interactive::controller::tests::t04_history_order_is_user_tool_assistant_run',
    'interactive::controller::tests::t06_y_key_grants_exactly_the_pending_request'
)

$requiredGSelectors = @(
    @{ Selector = 'g01_agents_md_cannot_grant_tools'; Target = @('-p', 'harness-cli', '--test', 'interactive_session') },
    @{ Selector = 'interactive::config::tests::g02_precedence_cli_over_env_over_project_over_user'; Target = @('-p', 'harness-cli', '--bin', 'ha') },
    @{ Selector = 'g04_edit_file_requires_a_unique_match'; Target = @('-p', 'harness-cli', '--test', 'milestone_m4') },
    @{ Selector = 'service::tests::g05_protected_path_beats_every_allow_rule_and_mode'; Target = @('-p', 'harness-tools', '--lib') },
    @{ Selector = 'g06_bang_prefix_goes_through_the_same_approval_gate'; Target = @('-p', 'harness-cli', '--test', 'milestone_m4') },
    @{ Selector = 'g07_auto_compaction_triggers_at_threshold_and_never_loops'; Target = @('-p', 'harness-cli', '--test', 'milestone_m5') },
    @{ Selector = 'interactive::service::tests::g09_hook_cannot_turn_ask_into_allow'; Target = @('-p', 'harness-cli', '--bin', 'ha') },
    @{ Selector = 'g10_mcp_tool_goes_through_the_dispatcher_and_the_approval_gate'; Target = @('-p', 'harness-cli', '--test', 'milestone_m6') }
)

$notRun = @(
    'PTY cases need a real console, which a sandboxed cargo test does not have. Run scripts/Invoke-HaPtyAcceptance.ps1 and keep its transcripts; this gate does not count them as passes.',
    'Live provider smoke (paid model call): no credential or budget is granted for this assignment.',
    'Real User PATH mutation and install into the user profile: not authorized in this assignment.'
)
if (-not $IsLinux) {
    $notRun += 'Linux build and run: this local Windows gate does not establish Linux support; use the Ubuntu CI job.'
}
if (-not $IsWindows) {
    $notRun += 'Windows installer self-test: it verifies CMD PATH resolution and runs on the Windows CI leg; this non-Windows gate does not count it as a pass.'
}

function Invoke-GateSelfTest {
    $failures = [System.Collections.Generic.List[string]]::new()
    $sample = "interactive_launch::i03_thing: test`ninteractive_launch::other: test`n"
    $parsed = [System.Collections.Generic.List[string]]::new()
    foreach ($line in ($sample -split "`r?`n")) {
        if ($line -match '^([A-Za-z0-9_:]+): test$') { $parsed.Add($Matches[1]) }
    }
    if ($parsed.Count -ne 2) { $failures.Add('discovery parsing did not find both sample tests') }
    if (-not ($parsed -contains 'interactive_launch::i03_thing')) { $failures.Add('discovery parsing dropped a selector') }
    if ($requiredSelectors.Count -lt 6) { $failures.Add('the required selector list shrank unexpectedly') }
    if ($requiredTuiSelectors.Count -lt 4) { $failures.Add('the required TUI selector list shrank unexpectedly') }
    if ($requiredGSelectors.Count -lt 8) { $failures.Add('the required G selector list shrank unexpectedly') }
    foreach ($item in $requiredSelectors) {
        if ([string]::IsNullOrWhiteSpace($item.Selector) -or [string]::IsNullOrWhiteSpace($item.Target)) {
            $failures.Add('a required selector entry is incomplete')
        }
    }
    if ($failures.Count -gt 0) {
        foreach ($failure in $failures) { Write-Host "GATE_SELFTEST_FAIL: $failure" }
        exit 1
    }
    Write-Host 'GATE_SELFTEST_OK: selector parsing, required list and report shape verified'
    exit 0
}

if ($SelfTest) {
    Invoke-GateSelfTest
}

Write-Host "HA_LAUNCH gate at $repositoryRoot"
Write-Host ''

$failures = [System.Collections.Generic.List[string]]::new()

if ((Invoke-GateStep -Name 'format' -File 'cargo' -Arguments @('fmt', '--all', '--', '--check')) -ne 0) { $failures.Add('format') }
if ((Invoke-GateStep -Name 'clippy' -File 'cargo' -Arguments @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')) -ne 0) { $failures.Add('clippy') }

foreach ($requirement in $requiredSelectors) {
    try {
        $count = Assert-Selector -File 'cargo' -DiscoveryArguments @('test', '-p', 'harness-cli', '--test', $requirement.Target, '--locked', '--', '--list') -Selector $requirement.Selector
        Write-Host "== discovery $($requirement.Target): $count tests, selector present: $($requirement.Selector)"
        $script:Steps.Add([pscustomobject]@{ Name = "discovery:$($requirement.Selector)"; ExitCode = 0; Seconds = 0; Detail = "$count discovered" })
    }
    catch {
        Write-Host "   FAILED: $($_.Exception.Message)"
        $failures.Add("discovery:$($requirement.Selector)")
        $script:Steps.Add([pscustomobject]@{ Name = "discovery:$($requirement.Selector)"; ExitCode = 1; Seconds = 0; Detail = $_.Exception.Message })
    }
}

$tuiDiscovery = @('test', '-p', 'harness-cli', '--bin', 'ha', '--locked', '--', '--list')
foreach ($selector in $requiredTuiSelectors) {
    try {
        $count = Assert-Selector -File 'cargo' -DiscoveryArguments $tuiDiscovery -Selector $selector
        Write-Host "== discovery ha TUI: $count tests, selector present: $selector"
        $script:Steps.Add([pscustomobject]@{ Name = "discovery:$selector"; ExitCode = 0; Seconds = 0; Detail = "$count discovered" })
    }
    catch {
        Write-Host "   FAILED: $($_.Exception.Message)"
        $failures.Add("discovery:$selector")
        $script:Steps.Add([pscustomobject]@{ Name = "discovery:$selector"; ExitCode = 1; Seconds = 0; Detail = $_.Exception.Message })
    }
}

foreach ($requirement in $requiredGSelectors) {
    $target = @('test') + $requirement.Target + @('--locked')
    try {
        $count = Assert-Selector -File 'cargo' -DiscoveryArguments ($target + @('--', '--list')) -Selector $requirement.Selector
        Write-Host "== discovery G: $count tests, selector present: $($requirement.Selector)"
        $script:Steps.Add([pscustomobject]@{ Name = "discovery:$($requirement.Selector)"; ExitCode = 0; Seconds = 0; Detail = "$count discovered" })
    }
    catch {
        Write-Host "   FAILED: $($_.Exception.Message)"
        $failures.Add("discovery:$($requirement.Selector)")
        $script:Steps.Add([pscustomobject]@{ Name = "discovery:$($requirement.Selector)"; ExitCode = 1; Seconds = 0; Detail = $_.Exception.Message })
    }
    if ((Invoke-GateStep -Name "selector:$($requirement.Selector)" -File 'cargo' -Arguments ($target + @('--', '--exact', $requirement.Selector))) -ne 0) {
        $failures.Add("selector:$($requirement.Selector)")
    }
}

if ((Invoke-GateStep -Name 'unit-interactive' -File 'cargo' -Arguments @('test', '-p', 'harness-cli', '--bin', 'ha', '--locked')) -ne 0) { $failures.Add('unit-interactive') }
if ((Invoke-GateStep -Name 'unit-agent' -File 'cargo' -Arguments @('test', '-p', 'harness-orchestrator', '--lib', '--locked')) -ne 0) { $failures.Add('unit-agent') }
if ((Invoke-GateStep -Name 'acceptance-launch' -File 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', 'interactive_launch', '--locked', '--', '--test-threads=1')) -ne 0) { $failures.Add('acceptance-launch') }
if ((Invoke-GateStep -Name 'acceptance-session' -File 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', 'interactive_session', '--locked', '--', '--test-threads=1')) -ne 0) { $failures.Add('acceptance-session') }
# Serial like every other suite: the streaming fixture binds a loopback port inside the
# test process, and this environment refuses the first connection when tests run in
# parallel (evidence section 14; measured red again in rounds 20 and 21, and 3/3 green
# with --test-threads=1 on the same binary).
if ((Invoke-GateStep -Name 'providers-streaming' -File 'cargo' -Arguments @('test', '-p', 'harness-providers', '--locked', '--', '--test-threads=1')) -ne 0) { $failures.Add('providers-streaming') }

foreach ($phase in @('phase_p0', 'phase_p1', 'phase_p2', 'phase_p3', 'phase_p4', 'phase_p5', 'phase_p6', 'phase_p7')) {
    if ((Invoke-GateStep -Name "regression-$phase" -File 'cargo' -Arguments @('test', '-p', 'harness-cli', '--test', $phase, '--locked', '--', '--test-threads=1')) -ne 0) { $failures.Add("regression-$phase") }
}

if ($IsWindows) {
    if ((Invoke-GateStep -Name 'installer-selftest' -File 'pwsh' -Arguments @('-NoProfile', '-File', (Join-Path $repositoryRoot 'scripts/Install-Ha.ps1'), '-SelfTest')) -ne 0) { $failures.Add('installer-selftest') }
}
else {
    Write-Host '== installer-selftest (not run: Windows CMD installer coverage belongs to the Windows CI leg)'
}
if ((Invoke-GateStep -Name 'release-selftest' -File 'pwsh' -Arguments @('-NoProfile', '-File', (Join-Path $repositoryRoot 'scripts/New-HaRelease.ps1'), '-SelfTest')) -ne 0) { $failures.Add('release-selftest') }
if ((Invoke-GateStep -Name 'docs' -File 'pwsh' -Arguments @('-NoProfile', '-File', (Join-Path $repositoryRoot 'scripts/Verify-Docs.ps1'), '-SelfTest')) -ne 0) { $failures.Add('docs') }

$report = [pscustomobject]@{
    gate           = 'HA_LAUNCH'
    passed         = ($failures.Count -eq 0)
    failures       = @($failures)
    steps          = @($script:Steps)
    not_run        = $notRun
    required_tests = @($requiredSelectors | ForEach-Object { "$($_.Target)::$($_.Selector)" })
    required_tui_tests = @($requiredTuiSelectors)
    required_g_tests = @($requiredGSelectors | ForEach-Object { $_.Selector })
}

if ($Json) {
    $report | ConvertTo-Json -Depth 5
}
else {
    Write-Host ''
    if ($failures.Count -eq 0) {
        Write-Host 'GATE_OK: every required step passed'
    }
    else {
        Write-Host "GATE_FAILED: $($failures -join ', ')"
    }
    Write-Host ''
    Write-Host 'Not run (explicitly not counted as passes):'
    foreach ($item in $notRun) { Write-Host "  - $item" }
}

if ($failures.Count -gt 0) { exit 1 }
exit 0
