<#
.SYNOPSIS
Run one bounded live agent turn against the configured provider, or refuse.

.DESCRIPTION
This is the paid smoke that HA_LAUNCH reserves for an explicit grant. It refuses to
run unless the environment already carries everything a real call needs:

  HA_PROVIDER_ENDPOINT   provider endpoint URL
  HA_PROVIDER_MODEL      model name
  DEEPSEEK_API_KEY or HA_API_KEY   credential (read at call time, never printed)

Without them the script prints `SMOKE_NOT_RUN` and exits 2: it never substitutes a
fixture and never fabricates a result. The budget is bounded on purpose - one turn,
a short prompt, a bounded deadline - so a smoke costs one model call, not a session.

The transcript it prints contains the model, the endpoint host (never the full URL
with query data), the exit code, the response text and its length. The credential is
never echoed, and the checks assert that.

.PARAMETER Prompt
Prompt for the single turn. Defaults to a short deterministic instruction.

.PARAMETER DataDirectory
Where the smoke writes its session store. Defaults to a throwaway directory under
the system temp path, so a smoke never touches a real project.

.PARAMETER SelfTest
Prove the refusal path and the redaction logic without calling anything.

.EXAMPLE
$env:DEEPSEEK_API_KEY = '...'; pwsh -NoProfile -File scripts/Smoke-HaProvider.ps1
Run one bounded live turn.
#>
[CmdletBinding()]
param(
    [string] $Prompt = 'Reply with the single word: ready',
    [string] $DataDirectory = '',
    [switch] $SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$executableName = if ([System.IO.Path]::DirectorySeparatorChar -eq '\') { 'ha.exe' } else { 'ha' }

function Get-SmokeConfiguration {
    $endpoint = [string] [Environment]::GetEnvironmentVariable('HA_PROVIDER_ENDPOINT')
    $model = [string] [Environment]::GetEnvironmentVariable('HA_PROVIDER_MODEL')
    $credentialName = ''
    foreach ($name in @('DEEPSEEK_API_KEY', 'HA_API_KEY')) {
        $value = [string] [Environment]::GetEnvironmentVariable($name)
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            $credentialName = $name
            break
        }
    }
    $missing = [System.Collections.Generic.List[string]]::new()
    if ([string]::IsNullOrWhiteSpace($endpoint)) { $missing.Add('HA_PROVIDER_ENDPOINT') }
    if ([string]::IsNullOrWhiteSpace($model)) { $missing.Add('HA_PROVIDER_MODEL') }
    if ([string]::IsNullOrWhiteSpace($credentialName)) { $missing.Add('DEEPSEEK_API_KEY or HA_API_KEY') }
    return [pscustomobject]@{
        Endpoint       = $endpoint
        Model          = $model
        CredentialName = $credentialName
        Missing        = @($missing)
    }
}

<#
Redact anything that could carry a secret before it reaches the transcript: the
full endpoint becomes scheme://host, and a credential value is never printed.
#>
function Get-RedactedEndpoint {
    param([string] $Endpoint)
    try {
        $uri = [System.Uri] $Endpoint
        return "$($uri.Scheme)://$($uri.Host)"
    }
    catch {
        return '<unparseable endpoint>'
    }
}

function Invoke-SmokeSelfTest {
    $failures = [System.Collections.Generic.List[string]]::new()
    $saved = @{}
    foreach ($name in @('HA_PROVIDER_ENDPOINT', 'HA_PROVIDER_MODEL', 'DEEPSEEK_API_KEY', 'HA_API_KEY')) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name)
        [Environment]::SetEnvironmentVariable($name, $null)
    }
    try {
        $configuration = Get-SmokeConfiguration
        if ($configuration.Missing.Count -ne 3) { $failures.Add("expected three missing inputs, got $($configuration.Missing.Count)") }
        $redacted = Get-RedactedEndpoint 'https://api.example.invalid/v1/chat?token=secret-value'
        if ($redacted -ne 'https://api.example.invalid') { $failures.Add("redaction produced '$redacted'") }
        if ($redacted.Contains('secret-value')) { $failures.Add('redaction leaked a query token') }
    }
    finally {
        foreach ($name in $saved.Keys) {
            [Environment]::SetEnvironmentVariable($name, $saved[$name])
        }
    }
    if ($failures.Count -gt 0) {
        foreach ($failure in $failures) { Write-Host "SMOKE_SELFTEST_FAIL: $failure" }
        exit 1
    }
    Write-Host 'SMOKE_SELFTEST_OK: refusal shape and endpoint redaction verified'
    exit 0
}

if ($SelfTest) {
    Invoke-SmokeSelfTest
}

$configuration = Get-SmokeConfiguration
if ($configuration.Missing.Count -gt 0) {
    Write-Host 'SMOKE_NOT_RUN: the environment is not configured for a live call.'
    Write-Host "   missing: $($configuration.Missing -join ', ')"
    Write-Host '   This is deliberate: no fixture is substituted and no paid call is made.'
    Write-Host "   Set those variables and re-run: $($MyInvocation.MyCommand.Path)"
    exit 2
}

$binary = Join-Path $repositoryRoot "target/release/$executableName"
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    $binary = Join-Path $repositoryRoot "target/debug/$executableName"
}
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    Write-Host "SMOKE_FAILED: no built binary at target/release or target/debug. Build first."
    exit 1
}
if ([string]::IsNullOrWhiteSpace($DataDirectory)) {
    $DataDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("ha-smoke-" + [guid]::NewGuid().ToString('n'))
}

Write-Host 'SMOKE_START: one bounded live turn'
Write-Host "   model:    $($configuration.Model)"
Write-Host "   endpoint: $(Get-RedactedEndpoint -Endpoint $configuration.Endpoint)"
Write-Host "   credential: from $($configuration.CredentialName) (value never printed)"
Write-Host "   data dir: $DataDirectory (throwaway)"

$started = Get-Date
$output = & $binary chat --headless --prompt $Prompt --json --cwd $DataDirectory 2>&1 | Out-String
$exit = $LASTEXITCODE
$elapsed = [int] ((Get-Date) - $started).TotalSeconds
$transcript = $output.Trim()

if ($transcript -match [regex]::Escape([string] [Environment]::GetEnvironmentVariable($configuration.CredentialName))) {
    Write-Host 'SMOKE_FAILED: the credential appeared in the output; refusing to record this run.'
    exit 1
}

Write-Host "SMOKE_EXIT: $exit after $elapsed s"
if ($exit -eq 0) {
    try {
        $result = $transcript | ConvertFrom-Json
        Write-Host "SMOKE_RESPONSE_LENGTH: $($result.response.Length)"
        Write-Host "SMOKE_STOP: $($result.stop)"
        Write-Host 'SMOKE_RESPONSE:'
        Write-Host $result.response
        Write-Host 'SMOKE_OK: one live turn completed and was recorded without leaking the credential.'
    }
    catch {
        Write-Host 'SMOKE_FAILED: the turn exited 0 but the output was not the documented JSON.'
        Write-Host $transcript
        exit 1
    }
}
else {
    Write-Host 'SMOKE_FAILED: the live turn did not succeed.'
    Write-Host $transcript
    exit 1
}
