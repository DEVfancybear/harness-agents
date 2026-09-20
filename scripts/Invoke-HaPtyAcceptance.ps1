<#
.SYNOPSIS
Run the PTY acceptance tests in a real console and save their transcript.

.DESCRIPTION
The PTY tests are `#[ignore]`d because they need a console: ConPTY only delivers a
transcript when the process that creates the pseudo-console owns one, and a sandboxed
`cargo test` does not. This helper launches the compiled test binary in a new console
window, waits with a hard bound, and writes the result next to the transcript.

Status at the time of writing (HA_TUI CP-D plus the post-CP-D wiring audit): all 16
cases pass here. Ten HA_LAUNCH cases cover bare launch, active-run exit and writer
release, Vietnamese input, idle/running Ctrl-C, backend failure recovery, unreachable
provider behavior, settled-receipt recovery, staged install, and a multiline draft.
Six HA_TUI cases cover the inline viewport, multiline paste, approval by `y`, resize,
plain fallback, and `NO_COLOR`.

Measured in one bounded run: PTY_EXIT 0 and "16 passed; 0 failed" in 21.98 s. The cases stay
`#[ignore]`d because `cargo test` in a sandbox has no console; this helper is how they
are run for evidence.

.PARAMETER Filter
Test-name filter passed to the test binary. Defaults to all 16 PTY cases.

.PARAMETER TimeoutSeconds
Hard bound before the console run is killed. Defaults to 600.
#>
[CmdletBinding()]
param(
    [string] $Filter = '',
    [int] $TimeoutSeconds = 600,
    [string] $OutputDirectory = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot 'target/pty-acceptance'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

Write-Host 'Building the PTY test binary'
& cargo test -p harness-cli --test interactive_terminal --locked --no-run | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "building the PTY test binary failed with exit code $LASTEXITCODE"
}
$candidate = Get-ChildItem -LiteralPath (Join-Path $repositoryRoot 'target/debug/deps') -Filter 'interactive_terminal-*.exe' |
    Sort-Object -Property LastWriteTimeUtc -Descending | Select-Object -First 1
if ($null -eq $candidate) {
    throw 'the compiled PTY test binary was not found'
}

$arguments = @('--ignored', '--test-threads=1', '--nocapture')
if (-not [string]::IsNullOrWhiteSpace($Filter)) {
    $arguments = @($Filter) + $arguments
}
# Name the transcript after the filter: a later run must never overwrite the
# evidence of an earlier one.
$label = if ([string]::IsNullOrWhiteSpace($Filter)) { 'all' } else { ($Filter -replace '[^A-Za-z0-9_]', '') }
$stdout = Join-Path $OutputDirectory "pty-$label.txt"
$stderr = Join-Path $OutputDirectory "pty-$label.err.txt"

Write-Host "Running $($candidate.Name) in a new console (bound $TimeoutSeconds s)"
$process = Start-Process -FilePath $candidate.FullName -ArgumentList $arguments -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
$exited = $process.WaitForExit($TimeoutSeconds * 1000)
if (-not $exited) {
    Write-Host "PTY_TIMEOUT: the console run exceeded $TimeoutSeconds s and was killed (see $stderr)"
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    exit 3
}
Write-Host "PTY_EXIT: $($process.ExitCode)"
Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue | Select-String -Pattern 'test .* \.\.\.|test result:' | ForEach-Object { Write-Host $_.Line }
Get-Content -LiteralPath $stderr -ErrorAction SilentlyContinue | Select-String -Pattern 'panicked|timed out|assertion' | Select-Object -First 5 | ForEach-Object { Write-Host $_.Line }
Write-Host "Transcript: $stdout"
if ($process.ExitCode -ne 0) { exit 1 }
exit 0
