#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $RepositoryRoot = (Join-Path $PSScriptRoot '..'),
    [switch] $SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$phaseStems = @(
    'P0_FOUNDATION', 'P1_KERNEL_STORAGE', 'P2_RUNTIME_CONTEXT', 'P3_CODING_TOOLS',
    'P4_MEMORY', 'P5_MULTI_AGENT', 'P6_EXTENSIONS', 'P7_RELEASE', 'P8_WEB'
)
$pairs = @(
    'ARCHITECTURE_REVIEW', 'RUST_HARNESS_PLAN', 'PLUGIN_ARCHITECTURE', 'MEMORY_AND_CONTINUITY',
    'implementation/README', 'implementation/ACCEPTANCE_MAP'
) + @($phaseStems | ForEach-Object { "implementation/$_" })

function Get-DocumentErrors {
    param([hashtable] $Documents)
    $problems = [System.Collections.Generic.List[string]]::new()
    $required = @('README.md') + @($pairs | ForEach-Object { "docs/$_.en.md"; "docs/$_.vi.md" })
    foreach ($name in $required) {
        if (-not $Documents.ContainsKey($name)) { $problems.Add("MISSING_FILE: $name") }
    }
    if ($problems.Count -gt 0) { return $problems.ToArray() }

    foreach ($name in ($Documents.Keys | Sort-Object)) {
        $body = [string] $Documents[$name]
        if ([string]::IsNullOrWhiteSpace($body)) { $problems.Add("EMPTY_FILE: $name") }
        if ($body.Contains([char]0xFFFD)) { $problems.Add("INVALID_UTF8: $name") }
        $insideFence = $false
        $prose = [System.Collections.Generic.List[string]]::new()
        foreach ($line in ($body -split '\r?\n')) {
            if ($line -match '^\s*```') { $insideFence = -not $insideFence; continue }
            if (-not $insideFence) { $prose.Add($line) }
        }
        if ($insideFence) { $problems.Add("UNCLOSED_FENCE: $name") }
        $proseText = $prose -join "`n"
        foreach ($link in [regex]::Matches($proseText, '\[[^\]\r\n]+\]\(([^)\r\n]+)\)')) {
            $target = $link.Groups[1].Value.Trim('<', '>')
            if ($target -match '^(https?://|mailto:|#)') { continue }
            $target = [uri]::UnescapeDataString(($target -split '#', 2)[0])
            $parent = Split-Path -Parent (Join-Path $repoRoot $name)
            $absolute = [System.IO.Path]::GetFullPath((Join-Path $parent $target))
            $relative = [System.IO.Path]::GetRelativePath($repoRoot, $absolute).Replace('\', '/')
            if ($relative -eq '..' -or $relative.StartsWith('../') -or [System.IO.Path]::IsPathRooted($relative)) {
                $problems.Add("OUTSIDE_REPO_LINK: $name -> $target")
            } elseif ($relative.EndsWith('.md')) {
                if (-not $Documents.ContainsKey($relative)) { $problems.Add("BROKEN_LINK: $name -> $target") }
            } elseif (-not (Test-Path -LiteralPath $absolute -PathType Leaf)) {
                $problems.Add("BROKEN_LINK: $name -> $target")
            }
        }
        foreach ($link in [regex]::Matches($proseText, 'https://github.com/(deepseek-ai/deepseek-harness|TencentCloud/TencentDB-Agent-Memory)/(blob|tree)/([^/\s)]+)')) {
            $expected = if ($link.Groups[1].Value -eq 'deepseek-ai/deepseek-harness') {
                '2377c272a8e839e0a84c9f0e623b867a1dce2014'
            } else { '906b5823b5106eed8f842b62f16d23228838149a' }
            if ($link.Groups[3].Value -cne $expected) { $problems.Add("UNPINNED_SOURCE: $name") }
        }
    }

    foreach ($pair in $pairs) {
        $en = [string] $Documents["docs/$pair.en.md"]
        $vi = [string] $Documents["docs/$pair.vi.md"]
        $enSections = @([regex]::Matches($en, '(?m)^(#{2,3}) (\d+(?:\.\d+)*)\. ') | ForEach-Object { $_.Groups[1].Value + $_.Groups[2].Value })
        $viSections = @([regex]::Matches($vi, '(?m)^(#{2,3}) (\d+(?:\.\d+)*)\. ') | ForEach-Object { $_.Groups[1].Value + $_.Groups[2].Value })
        if ($enSections.Count -eq 0 -or ($enSections -join ',') -cne ($viSections -join ',')) {
            $problems.Add("SECTION_PARITY: $pair")
        }
        $leaf = ($pair -split '/')[-1]
        if (-not $en.Contains("$leaf.vi.md") -or -not $vi.Contains("$leaf.en.md")) {
            $problems.Add("LANGUAGE_SWITCH: $pair")
        }
    }

    foreach ($spec in @(
        @{ Name = 'MEMORY_AND_CONTINUITY'; Prefix = 'C'; Count = 30 },
        @{ Name = 'PLUGIN_ARCHITECTURE'; Prefix = 'K'; Count = 14 },
        @{ Name = 'ARCHITECTURE_REVIEW'; Prefix = 'R'; Count = 12 }
    )) {
        $expectedIds = @(1..$spec.Count | ForEach-Object { '{0}{1:D2}' -f $spec.Prefix, $_ }) -join ','
        foreach ($lang in @('en', 'vi')) {
            $name = "docs/$($spec.Name).$lang.md"
            $pattern = '(?m)^\| (' + $spec.Prefix + '\d{2}) \|'
            $actualIds = @([regex]::Matches([string] $Documents[$name], $pattern) | ForEach-Object { $_.Groups[1].Value }) -join ','
            if ($actualIds -cne $expectedIds) { $problems.Add("CASE_IDS: $name") }
        }
    }

    $dayLists = @{}
    foreach ($lang in @('en', 'vi')) {
        $name = "docs/RUST_HARNESS_PLAN.$lang.md"
        $rows = @([regex]::Matches([string] $Documents[$name], '(?m)^\| (P[0-8]) \|[^\r\n]+'))
        if (($rows | ForEach-Object { $_.Groups[1].Value }) -join ',' -cne 'P0,P1,P2,P3,P4,P5,P6,P7,P8') {
            $problems.Add("MILESTONE_IDS: $name")
            continue
        }
        $minimum = 0; $maximum = 0
        $days = [System.Collections.Generic.List[string]]::new()
        foreach ($row in $rows) {
            $range = [regex]::Match($row.Value, '(\d+)–(\d+) \|\s*$')
            if (-not $range.Success) { $problems.Add("MILESTONE_RANGE: $name"); continue }
            $days.Add($range.Groups[1].Value + '-' + $range.Groups[2].Value)
            if ($row.Groups[1].Value -ne 'P8') {
                $minimum += [int] $range.Groups[1].Value
                $maximum += [int] $range.Groups[2].Value
            }
        }
        $dayLists[$lang] = $days -join ','
        if ($minimum -ne 53 -or $maximum -ne 76 -or -not $Documents[$name].Contains('53–76')) {
            $problems.Add("MILESTONE_TOTAL: $name ($minimum-$maximum)")
        }
    }
    if ($dayLists.ContainsKey('en') -and $dayLists.ContainsKey('vi') -and $dayLists.en -cne $dayLists.vi) {
        $problems.Add('MILESTONE_PARITY: en/vi')
    }
    return $problems.ToArray()
}

function Get-ImplementationErrors {
    param([hashtable] $Documents, [System.Collections.IDictionary] $Manifest)
    $problems = [System.Collections.Generic.List[string]]::new()
    if ($Manifest['schema_version'] -ne 1 -or $Manifest['status'] -cne 'planning_only' -or
        $Manifest['architecture_base_commit'] -cne 'b636208c7d81464525b38e0f32f5e2743c083357' -or
        $Manifest['execution_policy'] -cne 'sequential_phase_gates; parallel_tasks_only_when_explicitly_assigned') {
        $problems.Add('PHASE_MANIFEST: unsupported schema, planning status, base revision or execution policy')
    }
    $phases = @($Manifest['phases'])
    if (@($phases | Where-Object { $_ -isnot [System.Collections.IDictionary] }).Count -gt 0) {
        $problems.Add('PHASE_MANIFEST: phase entries must be objects')
        return $problems.ToArray()
    }
    $ids = @($phases | ForEach-Object { $_['id'] })
    if (($ids -join ',') -cne 'P0,P1,P2,P3,P4,P5,P6,P7,P8') {
        $problems.Add('PHASE_IDS: expected P0-P8 exactly once in order')
        return $problems.ToArray()
    }
    $allCases = @(1..30 | ForEach-Object { 'C{0:D2}' -f $_ }) + @(1..14 | ForEach-Object { 'K{0:D2}' -f $_ })
    $webCases = @(1..6 | ForEach-Object { 'W{0:D2}' -f $_ })
    $owners = @{}
    $assigned = [System.Collections.Generic.List[string]]::new()
    for ($n = 0; $n -lt $phases.Count; $n++) {
        $phase = $phases[$n]
        $id = [string] $phase['id']
        $expectedDependency = if ($n -eq 0) { '' } else { 'P' + ($n - 1) }
        if ((@($phase['depends_on']) -join ',') -cne $expectedDependency) {
            $problems.Add("PHASE_DEPENDENCY: $id must follow its accepted predecessor")
        }
        if ($phase['status'] -cne 'not_started' -or $phase['optional'] -isnot [bool] -or $phase['optional'] -ne ($n -eq 8)) {
            $problems.Add("PHASE_STATUS: $id is a planning entry; only P8 is optional")
        }
        $expectedSteps = @(1..7 | ForEach-Object { '{0}-S{1:D2}' -f $id, $_ })
        if ((@($phase['steps']) -join ',') -cne ($expectedSteps -join ',')) {
            $problems.Add("PHASE_STEPS: $id must contain seven ordered unique steps")
        }
        $days = @($phase['person_days']) -join '–'
        $dependencyLists = @{}
        foreach ($lang in @('en', 'vi')) {
            $expectedFile = $phaseStems[$n] + '.' + $lang + '.md'
            $name = 'docs/implementation/' + $expectedFile
            if ($phase[$lang] -cne $expectedFile -or -not $Documents.ContainsKey($name)) {
                $problems.Add("PHASE_FILE: $id / $lang")
                continue
            }
            $body = [string] $Documents[$name]
            $steps = @([regex]::Matches($body, '(?m)^### 4\.\d+\. (P\d-S\d{2}) — ') | ForEach-Object { $_.Groups[1].Value })
            if (($steps -join ',') -cne ($expectedSteps -join ',')) { $problems.Add("PHASE_STEP_DOC: $name") }
            $sections = @([regex]::Matches($body, '(?m)^## (\d+)\. ') | ForEach-Object { $_.Groups[1].Value })
            if (($sections -join ',') -cne '1,2,3,4,5,6,7,8,9') { $problems.Add("PHASE_SECTIONS: $name") }
            $unit = if ($lang -eq 'en') { 'person-days' } else { 'ngày công' }
            if (-not $body.Contains("$days $unit")) { $problems.Add("PHASE_ESTIMATE: $name") }
            if (-not $body.Contains("cargo test -p harness-cli --test phase_$($id.ToLowerInvariant()) --locked") -or
                -not $body.Contains("scripts/Verify-Phase.ps1 -Phase $id")) {
                $problems.Add("PHASE_COMMAND: $name")
            }
            $depLines = @([regex]::Matches($body, '(?m)^(?:Depends on|Phụ thuộc): ([^\r\n]+)'))
            if ($depLines.Count -ne 7) {
                $problems.Add("STEP_DEPENDENCY: $name requires seven dependency lines")
            } else {
                $normalized = [System.Collections.Generic.List[string]]::new()
                for ($s = 0; $s -lt 7; $s++) {
                    $tokens = @([regex]::Matches($depLines[$s].Groups[1].Value, 'P[0-8](?:-S\d{2})?') | ForEach-Object { $_.Value })
                    $normalized.Add($tokens -join ',')
                    if ($s -eq 0) {
                        if (($tokens -join ',') -cne $expectedDependency) { $problems.Add("STEP_DEPENDENCY: $name / first step") }
                    } else {
                        if ($tokens.Count -eq 0 -or @($tokens | Where-Object { $_ -cnotin $expectedSteps[0..($s - 1)] }).Count -gt 0) {
                            $problems.Add("STEP_DEPENDENCY: $name / $($expectedSteps[$s]) must reference earlier steps")
                        }
                    }
                }
                $dependencyLists[$lang] = $normalized -join ';'
            }
            foreach ($tableName in @("docs/RUST_HARNESS_PLAN.$lang.md", "docs/implementation/README.$lang.md")) {
                $row = [regex]::Match([string] $Documents[$tableName], '(?m)^\| ' + $id + ' \|[^\r\n]+')
                $range = [regex]::Match($row.Value, '\b(\d+–\d+) \|')
                if (-not $range.Success -or $range.Groups[1].Value -cne $days) { $problems.Add("PHASE_ESTIMATE: $id / $tableName") }
            }
        }
        if ($dependencyLists.ContainsKey('en') -and $dependencyLists.ContainsKey('vi') -and $dependencyLists.en -cne $dependencyLists.vi) {
            $problems.Add("STEP_DEPENDENCY_PARITY: $id")
        }
        foreach ($case in @($phase['primary_cases'])) {
            $assigned.Add([string] $case)
            if ($case -cnotin $allCases -or $owners.ContainsKey($case) -or $n -in @(0, 8)) {
                $problems.Add("PHASE_CASE_COVERAGE: invalid or duplicate primary case $case / $id")
            } else { $owners[$case] = $id }
        }
        foreach ($case in @($phase['strengthen'])) {
            if ($case -cnotin ($allCases + @('ALL_C', 'ALL_K'))) { $problems.Add("PHASE_STRENGTHEN: unknown $case / $id") }
        }
        if ($n -in @(7, 8) -and (@($phase['strengthen']) -join ',') -cne 'ALL_C,ALL_K') {
            $problems.Add("PHASE_STRENGTHEN: $id must retain every C/K regression")
        }
    }
    if ((@($assigned | Sort-Object) -join ',') -cne ($allCases -join ',')) {
        $problems.Add('PHASE_CASE_COVERAGE: all 44 C/K cases need exactly one primary owner')
    }
    foreach ($lang in @('en', 'vi')) {
        $name = "docs/implementation/ACCEPTANCE_MAP.$lang.md"
        $rows = @([regex]::Matches([string] $Documents[$name], '(?m)^\| ([CKW]\d{2}) \| (P[0-8]) \|'))
        if ((@($rows | ForEach-Object { $_.Groups[1].Value }) -join ',') -cne (($allCases + $webCases) -join ',')) {
            $problems.Add("ACCEPTANCE_MAP_IDS: $name")
        }
        foreach ($row in $rows) {
            $case = $row.Groups[1].Value
            $expectedOwner = if ($case.StartsWith('W')) { 'P8' } else { $owners[$case] }
            if ($row.Groups[2].Value -cne $expectedOwner) { $problems.Add("ACCEPTANCE_OWNER: $name / $case") }
        }
    }
    return $problems.ToArray()
}

$documents = @{}
$files = @(Get-Item -LiteralPath (Join-Path $repoRoot 'README.md')) + @(Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs') -Filter '*.md' -Recurse -File)
foreach ($file in $files) {
    $relative = [System.IO.Path]::GetRelativePath($repoRoot, $file.FullName).Replace('\', '/')
    $documents[$relative] = Get-Content -LiteralPath $file.FullName -Raw -Encoding utf8
}
$manifestPath = Join-Path $repoRoot 'docs/implementation/manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw 'PHASE_MANIFEST: missing manifest.json' }
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding utf8 | ConvertFrom-Json -AsHashtable
$failures = @(Get-DocumentErrors $documents) + @(Get-ImplementationErrors $documents $manifest)
if ($failures.Count -gt 0) { throw ($failures -join "`n") }

if ($SelfTest) {
    $controls = @(
        @{ Name = 'missing-translation'; Expected = 'MISSING_FILE'; Mutate = { param($d) $d.Remove('docs/PLUGIN_ARCHITECTURE.vi.md') } },
        @{ Name = 'broken-link'; Expected = 'BROKEN_LINK'; Mutate = { param($d) $d['README.md'] += "`n[broken](missing-document.md)`n" } },
        @{ Name = 'unclosed-fence'; Expected = 'UNCLOSED_FENCE'; Mutate = { param($d) $d['README.md'] += "`n" + '```text' + "`n" } },
        @{ Name = 'missing-case'; Expected = 'CASE_IDS'; Mutate = { param($d) $d['docs/MEMORY_AND_CONTINUITY.en.md'] = $d['docs/MEMORY_AND_CONTINUITY.en.md'].Replace('| C30 |', '| X30 |') } },
        @{ Name = 'changed-estimate'; Expected = 'MILESTONE_TOTAL'; Mutate = { param($d) $d['docs/RUST_HARNESS_PLAN.en.md'] = $d['docs/RUST_HARNESS_PLAN.en.md'].Replace('| 3–4 |', '| 30–40 |') } },
        @{ Name = 'unpinned-source'; Expected = 'UNPINNED_SOURCE'; Mutate = { param($d) $d['README.md'] = $d['README.md'].Replace('/tree/2377c272a8e839e0a84c9f0e623b867a1dce2014', '/tree/master') } }
    )
    foreach ($control in $controls) {
        $mutated = $documents.Clone()
        & $control.Mutate $mutated
        $rejections = @(Get-DocumentErrors $mutated)
        if (@($rejections | Where-Object { $_.StartsWith($control.Expected + ':') }).Count -eq 0) {
            throw "NEGATIVE_CONTROL_FAILED: $($control.Name)"
        }
        Write-Output "NEGATIVE_CONTROL_OK: $($control.Name)"
    }
    $phaseControls = @(
        @{ Name = 'missing-phase'; Expected = 'PHASE_IDS'; Mutate = { param($d, $m) $m.phases = @($m.phases | Where-Object { $_.id -ne 'P8' }) } },
        @{ Name = 'cyclic-phase-dependency'; Expected = 'PHASE_DEPENDENCY'; Mutate = { param($d, $m) $m.phases[1].depends_on = @('P2') } },
        @{ Name = 'unowned-continuity-case'; Expected = 'PHASE_CASE_COVERAGE'; Mutate = { param($d, $m) $m.phases[4].primary_cases = @($m.phases[4].primary_cases | Where-Object { $_ -ne 'C30' }) } },
        @{ Name = 'missing-phase-step'; Expected = 'PHASE_STEP_DOC'; Mutate = { param($d, $m) $d['docs/implementation/P0_FOUNDATION.en.md'] = $d['docs/implementation/P0_FOUNDATION.en.md'].Replace('### 4.7. P0-S07', '### 4.7. P0-S08') } },
        @{ Name = 'wrong-case-owner'; Expected = 'ACCEPTANCE_OWNER'; Mutate = { param($d, $m) $d['docs/implementation/ACCEPTANCE_MAP.vi.md'] = $d['docs/implementation/ACCEPTANCE_MAP.vi.md'].Replace('| C01 | P1 |', '| C01 | P2 |') } },
        @{ Name = 'forward-step-dependency'; Expected = 'STEP_DEPENDENCY'; Mutate = { param($d, $m) $d['docs/implementation/P0_FOUNDATION.en.md'] = $d['docs/implementation/P0_FOUNDATION.en.md'].Replace('Depends on: P0-S01.', 'Depends on: P0-S07.') } }
    )
    foreach ($control in $phaseControls) {
        $mutated = $documents.Clone()
        $mutatedManifest = $manifest | ConvertTo-Json -Depth 20 | ConvertFrom-Json -AsHashtable
        & $control.Mutate $mutated $mutatedManifest
        $rejections = @(Get-ImplementationErrors $mutated $mutatedManifest)
        if (@($rejections | Where-Object { $_.StartsWith($control.Expected + ':') }).Count -eq 0) {
            throw "NEGATIVE_CONTROL_FAILED: $($control.Name)"
        }
        Write-Output "NEGATIVE_CONTROL_OK: $($control.Name)"
    }
    # Recheck the untouched inputs; controls never edit files.
    $failures = @(Get-DocumentErrors $documents) + @(Get-ImplementationErrors $documents $manifest)
    if ($failures.Count -gt 0) { throw ($failures -join "`n") }
}

Write-Output "DOCS_OK: $($documents.Count) Markdown files; $($pairs.Count) language pairs; C01-C30; K01-K14; R01-R12; W01-W06; P0-P8=63 steps; P0-P7=53-76 person-days"
Write-Output "PowerShell: $($PSVersionTable.PSVersion)"
Write-Output 'Scope: structural documentation checks only; no Rust runtime tests executed.'
