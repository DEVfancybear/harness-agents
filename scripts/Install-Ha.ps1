<#
.SYNOPSIS
Build the `ha` CLI and put it on the terminal PATH.

.DESCRIPTION
Installs the `ha` binary so that typing `ha` works from any directory, in any
shell, the way `cargo` and `rustc` already do. Two routes are supported, and both
install the same tested source tree:

  - Copy (default): `cargo build --release -p harness-cli --bin ha` once, then copy
    the real compiled artifact into the destination directory. The destination
    defaults to `$HOME/.cargo/bin`, which the Rust installer already put on PATH.

  - Cargo route (`-UseCargoInstall`): `cargo install --path crates/harness-cli`,
    which records the install so `cargo uninstall harness-cli` removes it. The
    workspace `target` directory is reused as the build cache, because `cargo
    install` otherwise rebuilds every dependency from scratch.

Nothing is downloaded, no registry is published to, and no environment variable is
changed: the destination directory must already be on PATH. When it is not, the
script says exactly which directory to add instead of editing your profile.

.PARAMETER Profile
`Release` (default) builds the optimized binary; `Debug` builds faster and is
intended for local iteration. The installed binary is the same artifact the phase
gate exercises, in both cases.

.PARAMETER Destination
Directory to install into. When omitted, the copy route installs into
`$HOME/.cargo/bin` and the `-UseCargoInstall` route uses `$HOME/.cargo` as its
cargo root, which also places the binary in `$HOME/.cargo/bin`. Pass a temporary
directory to prove the install without touching the real PATH.

.PARAMETER Force
Rebuild even when the installed binary is newer than every source file.

.PARAMETER SkipBuild
Install an already built binary without invoking cargo. Fails when the binary is
missing rather than silently doing nothing.

.PARAMETER UseCargoInstall
Install through `cargo install --path crates/harness-cli --locked` instead of
copying the built artifact.

.EXAMPLE
pwsh -NoProfile -File scripts/Install-Ha.ps1
Build a release binary and install it as `ha` on PATH.

.EXAMPLE
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Profile Debug
Install the debug binary, for a fast edit/build/run loop.

.EXAMPLE
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Destination $env:TEMP -Force
Prove the install into a throwaway directory.
#>
[CmdletBinding()]
param(
    [ValidateSet('Release', 'Debug')]
    [string] $Profile = 'Release',
    [string] $Destination = '',
    [switch] $Force,
    [switch] $SkipBuild,
    [switch] $UseCargoInstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$profileDirectory = if ($Profile -eq 'Release') { 'release' } else { 'debug' }
$executableName = if ($env:OS -eq 'Windows_NT') { 'ha.exe' } else { 'ha' }
$sourceBinary = Join-Path $repositoryRoot "target/$profileDirectory/$executableName"

# The two routes have different roots: a plain copy goes into the PATH directory,
# while `cargo install --root` owns a `bin` subdirectory and records the install so
# `cargo uninstall harness-cli` can remove it.
if ([string]::IsNullOrWhiteSpace($Destination)) {
    $Destination = if ($UseCargoInstall) { Join-Path $HOME '.cargo' } else { Join-Path $HOME '.cargo/bin' }
}
$installDirectory = if ($UseCargoInstall) { Join-Path $Destination 'bin' } else { $Destination }
$installedBinary = Join-Path $installDirectory $executableName

function Invoke-Cargo {
    param([string[]] $Arguments)
    Write-Host "cargo $($Arguments -join ' ')"
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Test-BuildStale {
    param([string] $Binary)
    if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
        return $true
    }
    $binaryTime = (Get-Item -LiteralPath $Binary).LastWriteTimeUtc
    $sourceRoot = Join-Path $repositoryRoot 'crates'
    $newest = Get-ChildItem -LiteralPath $sourceRoot -Recurse -File -Include '*.rs', '*.toml' |
        Sort-Object -Property LastWriteTimeUtc -Descending |
        Select-Object -First 1
    if ($null -eq $newest) {
        return $false
    }
    return $newest.LastWriteTimeUtc -gt $binaryTime
}

Write-Host "Repository: $repositoryRoot"
Write-Host "Profile:    $Profile"
Write-Host "Install to: $installDirectory"

if (-not $UseCargoInstall -and -not (Test-Path -LiteralPath $installDirectory -PathType Container)) {
    New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null
    Write-Host "Created $installDirectory"
}

if ($UseCargoInstall) {
    # `--target-dir` reuses the workspace build cache. Without it, `cargo install`
    # builds every dependency again in its own temporary directory.
    $arguments = @(
        'install', '--path', (Join-Path $repositoryRoot 'crates/harness-cli'),
        '--locked', '--force', '--root', $Destination,
        '--target-dir', (Join-Path $repositoryRoot 'target')
    )
    if ($Profile -eq 'Debug') {
        $arguments += '--debug'
    }
    Invoke-Cargo -Arguments $arguments
}
else {
    $needsBuild = $Force -or (Test-BuildStale -Binary $sourceBinary)
    if ($SkipBuild) {
        $needsBuild = $false
    }
    if ($needsBuild) {
        $arguments = @('build', '-p', 'harness-cli', '--bin', 'ha', '--locked')
        if ($Profile -eq 'Release') {
            $arguments += '--release'
        }
        Invoke-Cargo -Arguments $arguments
    }
    else {
        Write-Host "Using the existing binary at $sourceBinary"
    }
    if (-not (Test-Path -LiteralPath $sourceBinary -PathType Leaf)) {
        throw "The expected binary is missing at $sourceBinary. Run without -SkipBuild."
    }
    # Windows cannot replace a running image, so a locked file is reported instead
    # of being worked around by killing a process.
    try {
        Copy-Item -LiteralPath $sourceBinary -Destination $installedBinary -Force
    }
    catch {
        throw "Cannot replace $installedBinary because it is in use. Close every running 'ha' process and run this script again. ($($_.Exception.Message))"
    }
}

if (-not (Test-Path -LiteralPath $installedBinary -PathType Leaf)) {
    throw "Install finished but $installedBinary does not exist."
}

$version = & $installedBinary --version
if ($LASTEXITCODE -ne 0) {
    throw "$installedBinary --version failed with exit code $LASTEXITCODE"
}

Write-Host ''
Write-Host "Installed: $installedBinary"
Write-Host "Version:   $version"

$pathEntries = @($env:PATH -split [System.IO.Path]::PathSeparator | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
$destinationOnPath = $false
foreach ($entry in $pathEntries) {
    try {
        $resolved = [System.IO.Path]::GetFullPath($entry)
    }
    catch {
        continue
    }
    if ($resolved.TrimEnd([System.IO.Path]::DirectorySeparatorChar) -ieq $installDirectory.TrimEnd([System.IO.Path]::DirectorySeparatorChar)) {
        $destinationOnPath = $true
        break
    }
}

if ($destinationOnPath) {
    Write-Host 'PATH:      the install directory is on PATH; `ha` works in a new terminal.'
    $command = Get-Command ha -ErrorAction SilentlyContinue
    if ($null -ne $command -and $command.Source -ine $installedBinary) {
        Write-Host "WARNING:   'ha' currently resolves to $($command.Source), not to the binary just installed."
    }
}
else {
    Write-Host "PATH:      $installDirectory is NOT on PATH. Add it to your user PATH, for example:"
    Write-Host "           [Environment]::SetEnvironmentVariable('PATH', `$env:PATH + ';$installDirectory', 'User')"
}
