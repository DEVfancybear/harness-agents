#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $RepositoryRoot = (Join-Path $PSScriptRoot '..'),
    [switch] $SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$packRoot = Join-Path $repoRoot 'docs/implementation-next'
$plan = Get-Content -LiteralPath (Join-Path $packRoot 'manifest.json') -Raw | ConvertFrom-Json -AsHashtable
$documents = @{}
Get-ChildItem -LiteralPath $packRoot -Filter '*.md' -File | ForEach-Object {
    $documents[$_.Name] = Get-Content -LiteralPath $_.FullName -Raw
}

function Get-NextPlanErrors {
    param([hashtable] $Plan, [hashtable] $Docs)
    $errors = [System.Collections.Generic.List[string]]::new()
    if ($Plan.schema_version -ne 1 -or $Plan.status -cne 'planning_only' -or
        $Plan.workspace_root -cne 'vnext' -or $Plan.runtime_gate -cne 'vnext/scripts/Verify-Milestone.ps1') {
        $errors.Add('PLAN_HEADER')
    }
    $required = @('README.vi.md', 'CONTRACTS.vi.md', 'ACCEPTANCE.vi.md', 'PROMPTS.vi.md', 'TEMPLATES.vi.md') +
        @(0..12 | ForEach-Object { "M$_.vi.md" })
    foreach ($name in $required) {
        if (-not $Docs.ContainsKey($name)) { $errors.Add("MISSING_RUNBOOK:$name") }
    }
    if ($errors.Count -gt 0) { return $errors.ToArray() }

    $milestones = @($Plan.milestones)
    $expectedM = @(0..12 | ForEach-Object { "M$_" })
    if (($milestones.id -join ',') -cne ($expectedM -join ',')) { $errors.Add('MILESTONE_IDS') }
    $dependencies = @{
        M0 = @(); M1 = @('M0'); M2 = @('M0'); M3 = @('M1', 'M2'); M4 = @('M3')
        M5 = @('M4'); M6 = @('M5'); M7 = @('M6'); M8 = @('M7'); M9 = @('M8')
        M10 = @('M9'); M11 = @('M9'); M12 = @('M4')
    }
    $byId = @{}
    foreach ($m in $milestones) {
        if ($byId.ContainsKey($m.id)) { $errors.Add("DUPLICATE_MILESTONE:$($m.id)"); continue }
        $byId[$m.id] = $m
        if (-not $dependencies.ContainsKey($m.id)) { $errors.Add("UNKNOWN_MILESTONE:$($m.id)"); continue }
        if ((@($m.dependencies) -join ',') -cne ($dependencies[$m.id] -join ',')) { $errors.Add("DEPENDENCIES:$($m.id)") }
        if ($m.optional -ne ([int]$m.id.Substring(1) -ge 10)) { $errors.Add("OPTIONAL_FLAG:$($m.id)") }
        if ($m.runbook -cne "$($m.id).vi.md" -or -not $Docs.ContainsKey($m.runbook)) {
            $errors.Add("RUNBOOK_MAPPING:$($m.id)"); continue
        }
        $body = [string]$Docs[$m.runbook]
        $expectedItems = @(1..4 | ForEach-Object { '{0}-{1:D2}' -f $m.id, $_ })
        if ((@($m.work_items.id) -join ',') -cne ($expectedItems -join ',')) { $errors.Add("WORK_ITEMS:$($m.id)") }
        $actualHeadings = @([regex]::Matches($body, '(?m)^### (M\d+-\d{2}) —') | ForEach-Object { $_.Groups[1].Value })
        if (($actualHeadings -join ',') -cne ($expectedItems -join ',')) { $errors.Add("ITEM_HEADINGS:$($m.id)") }
        foreach ($label in @('**Files/ownership:**', '**Test/oracle:**', '**Invariant và lỗi cần tránh:**', '**Done cho item:**')) {
            if ([regex]::Matches($body, [regex]::Escape($label)).Count -ne 4) { $errors.Add("ITEM_DETAIL:$($m.id):$label") }
        }
        if (-not $body.Contains("-Milestone $($m.id)") -or -not $body.Contains('## 6. Prompt giao trực tiếp')) { $errors.Add("RUNBOOK_GATE_PROMPT:$($m.id)") }
        $knownRefs = @([regex]::Matches($body, '\bA\d{2}\b') | ForEach-Object { $_.Value } | Sort-Object -Unique)
        foreach ($caseId in $m.acceptance_ids) {
            if ($caseId -notin $knownRefs) { $errors.Add("MISSING_CASE_REFERENCE:$($m.id):$caseId") }
        }
    }

    $cases = @($Plan.acceptance)
    $expectedA = @(1..36 | ForEach-Object { 'A{0:D2}' -f $_ })
    if (($cases.id -join ',') -cne ($expectedA -join ',')) { $errors.Add('ACCEPTANCE_IDS') }
    $actualA = @([regex]::Matches([string]$Docs['ACCEPTANCE.vi.md'], '(?m)^### (A\d{2}) —') | ForEach-Object { $_.Groups[1].Value })
    if (($actualA -join ',') -cne ($expectedA -join ',')) { $errors.Add('ACCEPTANCE_HEADINGS') }
    foreach ($label in @('**Setup:**', '**Trigger:**', '**Oracle bắt buộc:**', '**Negative control:**')) {
        if ([regex]::Matches([string]$Docs['ACCEPTANCE.vi.md'], [regex]::Escape($label)).Count -ne 36) { $errors.Add("ACCEPTANCE_DETAIL:$label") }
    }
    foreach ($case in $cases) {
        $actualOwners = @($milestones | Where-Object { $case.id -in $_.acceptance_ids } | ForEach-Object { $_.id })
        if ($actualOwners.Count -eq 0 -or ($actualOwners -join ',') -cne (@($case.owners) -join ',')) { $errors.Add("CASE_OWNERS:$($case.id)") }
        if ($actualOwners.Count -gt 0 -and $case.completion_milestone -cne $actualOwners[-1]) { $errors.Add("CASE_COMPLETION:$($case.id)") }
        if ($case.status -cne 'planned') { $errors.Add("FALSE_RUNTIME_STATUS:$($case.id)") }
        if ($case.planned_test_name -cne ($case.id.ToLowerInvariant() + '_' + $case.slug) -or
            -not ([string]$Docs['ACCEPTANCE.vi.md']).Contains('`' + $case.planned_test_name + '`')) { $errors.Add("CASE_SELECTOR:$($case.id)") }
    }
    foreach ($m in $milestones) {
        foreach ($id in $m.acceptance_ids) { if ($id -notin $expectedA) { $errors.Add("UNKNOWN_CASE:$id") } }
    }

    foreach ($entry in $Docs.GetEnumerator()) {
        $body = [string]$entry.Value
        if ([string]::IsNullOrWhiteSpace($body) -or $body.Contains([char]0xFFFD)) { $errors.Add("INVALID_DOCUMENT:$($entry.Key)") }
        if ([regex]::Matches($body, '(?m)^```').Count % 2 -ne 0) { $errors.Add("UNCLOSED_FENCE:$($entry.Key)") }
        foreach ($match in [regex]::Matches($body, '\[[^\]\r\n]+\]\(([^)\r\n]+)\)')) {
            $target = $match.Groups[1].Value
            if ($target -match '^(https?://|#)') { continue }
            $relative = [uri]::UnescapeDataString(($target -split '#', 2)[0])
            $absolute = [System.IO.Path]::GetFullPath((Join-Path $packRoot $relative))
            $withinRepo = [System.IO.Path]::GetRelativePath($repoRoot, $absolute)
            if ($withinRepo -eq '..' -or $withinRepo.StartsWith('../') -or $withinRepo.StartsWith('..\') -or
                [System.IO.Path]::IsPathRooted($withinRepo) -or -not (Test-Path -LiteralPath $absolute -PathType Leaf)) {
                $errors.Add("BROKEN_OR_OUTSIDE_LINK:$($entry.Key):$target")
            }
        }
    }
    return $errors.ToArray()
}

$problems = @(Get-NextPlanErrors -Plan $plan -Docs $documents)
if ($problems.Count -gt 0) { $problems | ForEach-Object { Write-Output $_ }; exit 1 }

if ($SelfTest) {
    $controls = @(
        @{ Name = 'missing-runbook'; Prefix = 'MISSING_RUNBOOK'; Change = { param($p, $d) $d.Remove('M4.vi.md') } },
        @{ Name = 'duplicate-work-item'; Prefix = 'WORK_ITEMS'; Change = { param($p, $d) $p.milestones[0].work_items[1].id = 'M0-01' } },
        @{ Name = 'wrong-dependency'; Prefix = 'DEPENDENCIES'; Change = { param($p, $d) $p.milestones[3].dependencies = @('M3') } },
        @{ Name = 'unowned-case'; Prefix = 'CASE_OWNERS'; Change = { param($p, $d) $p.acceptance[0].owners = @('M0') } },
        @{ Name = 'premature-completion'; Prefix = 'CASE_COMPLETION'; Change = { param($p, $d) $p.acceptance[10].completion_milestone = 'M3' } },
        @{ Name = 'false-pass-status'; Prefix = 'FALSE_RUNTIME_STATUS'; Change = { param($p, $d) $p.acceptance[0].status = 'passed' } },
        @{ Name = 'missing-oracle'; Prefix = 'ACCEPTANCE_DETAIL'; Change = { param($p, $d) $d['ACCEPTANCE.vi.md'] = $d['ACCEPTANCE.vi.md'].Replace('**Oracle bắt buộc:**', '**Assertion:**') } },
        @{ Name = 'broken-link'; Prefix = 'BROKEN_OR_OUTSIDE_LINK'; Change = { param($p, $d) $d['README.vi.md'] += "`n[missing](missing.md)" } },
        @{ Name = 'unclosed-fence'; Prefix = 'UNCLOSED_FENCE'; Change = { param($p, $d) $d['README.vi.md'] += "`n" + '```text' } }
    )
    foreach ($control in $controls) {
        $mutatedPlan = $plan | ConvertTo-Json -Depth 20 | ConvertFrom-Json -AsHashtable
        $mutatedDocs = $documents.Clone()
        & $control.Change $mutatedPlan $mutatedDocs
        $found = @(Get-NextPlanErrors -Plan $mutatedPlan -Docs $mutatedDocs)
        if (-not @($found | Where-Object { $_.StartsWith($control.Prefix) }).Count) { throw "Negative control did not fail: $($control.Name)" }
        Write-Output "NEGATIVE_CONTROL_OK: $($control.Name)"
    }
}
Write-Output 'NEXT_PLAN_OK: M0-M12; 52 work items; A01-A36; ownership/dependencies/selectors/details/links validated'
Write-Output 'Scope: planning structure only. No vnext runtime tests were executed or accepted.'
