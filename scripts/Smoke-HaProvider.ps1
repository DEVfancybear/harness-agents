<#
.SYNOPSIS
Run one bounded live agent turn against the configured provider, or refuse.

.DESCRIPTION
This is the paid smoke that HA_LAUNCH reserves for an explicit grant. It needs one
credential:

  DEEPSEEK_API_KEY or HA_API_KEY   credential (read at call time, never printed)

The endpoint and the model fall back to DeepSeek's documented values
(`https://api.deepseek.com`, `deepseek-flash`), so one API key is a complete setup.
Set `HA_PROVIDER_ENDPOINT` or `HA_PROVIDER_MODEL` to use another provider or model;
an explicit variable always wins over the default.

Without a credential the script prints `SMOKE_NOT_RUN` and exits 2: it never
substitutes a fixture and never fabricates a result. The budget is bounded on purpose - one turn,
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
One key is enough: the endpoint and the model default to DeepSeek's documented ones.
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

# The values DeepSeek documents, mirrored from the CLI's own defaults so the smoke
# and the app agree on what "configured" means.
$script:DeepSeekEndpoint = 'https://api.deepseek.com'
$script:DeepSeekModel = 'deepseek-flash'

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
    $endpointDefaulted = [string]::IsNullOrWhiteSpace($endpoint)
    $modelDefaulted = [string]::IsNullOrWhiteSpace($model)
    if ($endpointDefaulted) { $endpoint = $script:DeepSeekEndpoint }
    if ($modelDefaulted) { $model = $script:DeepSeekModel }
    $missing = [System.Collections.Generic.List[string]]::new()
    if ([string]::IsNullOrWhiteSpace($credentialName)) { $missing.Add('DEEPSEEK_API_KEY or HA_API_KEY') }
    return [pscustomobject]@{
        Endpoint         = $endpoint
        Model            = $model
        CredentialName   = $credentialName
        EndpointDefaulted = $endpointDefaulted
        ModelDefaulted   = $modelDefaulted
        Missing          = @($missing)
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
        if ($configuration.Missing.Count -ne 1) { $failures.Add("expected one missing input, got $($configuration.Missing.Count)") }
        if (-not $configuration.EndpointDefaulted) { $failures.Add('the endpoint should default when it is unset') }
        if (-not $configuration.ModelDefaulted) { $failures.Add('the model should default when it is unset') }
        if ($configuration.Endpoint -ne $script:DeepSeekEndpoint) { $failures.Add("default endpoint was '$($configuration.Endpoint)'") }
        if ($configuration.Model -ne $script:DeepSeekModel) { $failures.Add("default model was '$($configuration.Model)'") }

        [Environment]::SetEnvironmentVariable('DEEPSEEK_API_KEY', 'selftest-secret')
        $configured = Get-SmokeConfiguration
        if ($configured.Missing.Count -ne 0) { $failures.Add('one credential must be a complete setup') }
        if ($configured.CredentialName -ne 'DEEPSEEK_API_KEY') { $failures.Add("credential name was '$($configured.CredentialName)'") }
        [Environment]::SetEnvironmentVariable('HA_PROVIDER_MODEL', 'explicit-model')
        $explicit = Get-SmokeConfiguration
        if ($explicit.Model -ne 'explicit-model') { $failures.Add('an explicit model must win over the default') }
        if ($explicit.ModelDefaulted) { $failures.Add('an explicit model must not be reported as defaulted') }
        [Environment]::SetEnvironmentVariable('HA_PROVIDER_MODEL', $null)
        [Environment]::SetEnvironmentVariable('DEEPSEEK_API_KEY', $null)

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
$modelNote = if ($configuration.ModelDefaulted) { ' (DeepSeek default)' } else { '' }
$endpointNote = if ($configuration.EndpointDefaulted) { ' (DeepSeek default)' } else { '' }
Write-Host "   model:    $($configuration.Model)$modelNote"
Write-Host "   endpoint: $(Get-RedactedEndpoint -Endpoint $configuration.Endpoint)$endpointNote"
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
