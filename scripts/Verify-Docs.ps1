#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $RepositoryRoot = (Join-Path $PSScriptRoot '..'),
    [switch] $SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$pairs = @('ARCHITECTURE_REVIEW', 'RUST_HARNESS_PLAN', 'PLUGIN_ARCHITECTURE', 'MEMORY_AND_CONTINUITY')

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
        if (-not $en.Contains("$pair.vi.md") -or -not $vi.Contains("$pair.en.md")) {
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

$documents = @{}
$files = @(Get-Item -LiteralPath (Join-Path $repoRoot 'README.md')) + @(Get-ChildItem -LiteralPath (Join-Path $repoRoot 'docs') -Filter '*.md' -Recurse -File)
foreach ($file in $files) {
    $relative = [System.IO.Path]::GetRelativePath($repoRoot, $file.FullName).Replace('\', '/')
    $documents[$relative] = Get-Content -LiteralPath $file.FullName -Raw -Encoding utf8
}
$failures = @(Get-DocumentErrors $documents)
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
    # Recheck the untouched inputs; controls never edit files.
    $failures = @(Get-DocumentErrors $documents)
    if ($failures.Count -gt 0) { throw ($failures -join "`n") }
}

Write-Output "DOCS_OK: $($documents.Count) Markdown files; 4 language pairs; C01-C30; K01-K14; R01-R12; P0-P7=53-76 person-days"
Write-Output "PowerShell: $($PSVersionTable.PSVersion)"
Write-Output 'Scope: structural documentation checks only; no Rust runtime tests executed.'
