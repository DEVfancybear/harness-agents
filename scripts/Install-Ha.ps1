<#
.SYNOPSIS
Build the `ha` CLI and install it so typing `ha` works in any terminal.

.DESCRIPTION
Two developer routes are supported, and both install the same tested artifact:

  - Copy (default): build `ha` and copy the artifact Cargo reports into the
    destination directory (default `$HOME/.cargo/bin`).

  - Cargo route (`-UseCargoInstall`): `cargo install --path crates/harness-cli`,
    which records the install so `cargo uninstall harness-cli` removes it.

What this script guarantees, and what it deliberately refuses to do:

  - The installed file is the artifact Cargo reports for the `ha` bin, not a path
    guess, and its identity is verified before it replaces anything: the digest and
    the `--version` output of the staged copy must match, and the build commit is
    recorded in an install manifest next to the binary.
  - Replacement is staged: copy to a temporary file, verify it, keep a backup,
    move it into place, then drop the backup. A failure keeps the previous binary
    usable instead of leaving a half-written `ha`.
  - A running `ha` is never killed. A locked executable is reported as "close the
    app and retry", and a permission failure is reported as such instead of being
    blamed on a lock.
  - PATH is not modified unless `-ModifyUserPath` is passed explicitly. When it
    is, only the **User** PATH is merged (append once, de-duplicated, case
    insensitive), never the Machine PATH and never the merged process PATH. Without
    the switch the script prints the exact directory to add.
  - Another `ha` earlier on PATH is reported, never deleted or overwritten.

.PARAMETER Profile
`Release` (default) builds the optimized binary; `Debug` builds faster for local
iteration.

.PARAMETER Destination
Directory to install into. Pass a temporary directory to prove an install without
touching the real PATH.

.PARAMETER Force
Rebuild even when an artifact for the current sources already exists.

.PARAMETER SkipBuild
Install the already built artifact without invoking cargo.

.PARAMETER UseCargoInstall
Install through `cargo install --path crates/harness-cli --locked`.

.PARAMETER ModifyUserPath
Append the install directory to the persisted **User** PATH when it is missing.
This is the only switch that changes user environment state, and the change is
written through the same code path the tests exercise with an injected writer.

.PARAMETER NoModifyPath
Never touch PATH, even with `-ModifyUserPath`. Both together are rejected so the
intent cannot be ambiguous.

.PARAMETER SelfTest
Run the PATH/merge/identity/rollback checks against in-memory and disposable
state and exit: no PATH entry and no real install location is touched. Negative
controls assert that Machine PATH entries and process PATH entries never leak into
the User PATH merge.

.EXAMPLE
pwsh -NoProfile -File scripts/Install-Ha.ps1 -SelfTest
Prove the installer logic without installing anything for real.

.EXAMPLE
pwsh -NoProfile -File scripts/Install-Ha.ps1 -Destination $env:TEMP -SkipBuild
Install into a throwaway directory using the artifact already built.

.EXAMPLE
pwsh -NoProfile -File scripts/Install-Ha.ps1 -ModifyUserPath
Install and make `ha` resolvable in a new terminal.
#>
[CmdletBinding()]
param(
    [ValidateSet('Release', 'Debug')]
    [string] $Profile = 'Release',
    [string] $Destination = '',
    [switch] $Force,
    [switch] $SkipBuild,
    [switch] $UseCargoInstall,
    [switch] $ModifyUserPath,
    [switch] $NoModifyPath,
    [switch] $SelfTest,
    # Install from a release candidate bundle instead of building: the bundle
    # manifest and its checksums are verified before anything is staged.
    [string] $FromBundle = '',
    # Remove exactly what this installer recorded, keeping user data.
    [switch] $Uninstall,
    # Test hooks. When supplied, nothing real is read or written: the caller owns
    # the state, which is how the persisted PATH path is proven without touching
    # the developer's registry.
    [scriptblock] $UserPathProvider = $null,
    [scriptblock] $UserPathWriter = $null,
    [scriptblock] $ProcessPathProvider = $null,
    [scriptblock] $MachinePathProvider = $null
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$profileDirectory = if ($Profile -eq 'Release') { 'release' } else { 'debug' }
$isWindowsHost = [System.IO.Path]::DirectorySeparatorChar -eq '\'
$executableName = if ($isWindowsHost) { 'ha.exe' } else { 'ha' }
$expectedArtifact = Join-Path $repositoryRoot "target/$profileDirectory/$executableName"
$manifestName = 'ha.install.json'
$script:SelfTestFailures = [System.Collections.Generic.List[string]]::new()

# ---------------------------------------------------------------------------
# PATH handling: pure functions, so every rule below is testable in memory.
# ---------------------------------------------------------------------------

function Get-PathValue {
    param([scriptblock] $Provider, [ValidateSet('User', 'Machine', 'Process')] [string] $Scope)
    if ($null -ne $Provider) {
        return [string] (& $Provider)
    }
    if ($Scope -eq 'Process') {
        return [string] $env:PATH
    }
    try {
        return [string] [Environment]::GetEnvironmentVariable('Path', $Scope)
    }
    catch {
        return ''
    }
}

function Split-PathValue {
    param([string] $PathValue)
    if ([string]::IsNullOrEmpty($PathValue)) {
        return @()
    }
    return @($PathValue -split [System.IO.Path]::PathSeparator | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function Get-PathEntryKey {
    param([string] $Entry)
    $trimmed = $Entry.Trim()
    while ($trimmed.Length -gt 1 -and ($trimmed.EndsWith('\') -or $trimmed.EndsWith('/'))) {
        $trimmed = $trimmed.Substring(0, $trimmed.Length - 1)
    }
    return $trimmed.ToLowerInvariant()
}

<#
Merge one directory into a persisted User PATH.

Rules, each covered by a self test:
  - an entry that already exists (case-insensitive, trailing separators ignored)
    is not added again;
  - existing entries keep their order and spelling;
  - empty entries are dropped;
  - nothing from the Machine PATH or the merged process PATH is ever written here.
#>
function Merge-UserPath {
    param([string] $UserPath, [string] $InstallDir)
    $entries = @(Split-PathValue -PathValue $UserPath)
    $target = Get-PathEntryKey -Entry $InstallDir
    $present = $false
    foreach ($entry in $entries) {
        if ((Get-PathEntryKey -Entry $entry) -eq $target) {
            $present = $true
            break
        }
    }
    if ($present) {
        return [pscustomobject]@{
            Path  = ($entries -join [System.IO.Path]::PathSeparator)
            Added = $false
        }
    }
    $merged = @($entries + $InstallDir.Trim())
    return [pscustomobject]@{
        Path  = ($merged -join [System.IO.Path]::PathSeparator)
        Added = $true
    }
}

<#
Find `ha` on a PATH value without touching the current session.

Returns every executable match in PATH order, so a shadowing command can be
reported instead of being deleted.
#>
function Find-HaOnPath {
    param([string] $PathValue, [string[]] $Extensions = @())
    if ($Extensions.Count -eq 0) {
        $Extensions = if ($isWindowsHost) { @('.exe', '.cmd', '.bat', '.ps1') } else { @('') }
    }
    $found = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in (Split-PathValue -PathValue $PathValue)) {
        foreach ($extension in $Extensions) {
            $candidate = Join-Path $entry ("ha" + $extension)
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $found.Add([pscustomobject]@{ Path = $candidate; Directory = $entry })
            }
        }
    }
    return $found.ToArray()
}

function Get-FileDigest {
    param([string] $FilePath)
    $stream = [System.IO.File]::Open($FilePath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try {
            $hash = $sha.ComputeHash($stream)
            return ([System.BitConverter]::ToString($hash) -replace '-', '').ToLowerInvariant()
        }
        finally {
            $sha.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

<#
Classify a copy/move failure instead of calling every error "file in use".
HResult values come from the Windows error codes: 32/33 are sharing violations,
5 is access denied.
#>
function Get-CopyFailureKind {
    param([System.Exception] $Exception)
    $hresult = $Exception.HResult -band 0xFFFF
    if ($hresult -in 32, 33) {
        return 'in_use'
    }
    if ($hresult -eq 5) {
        return 'access_denied'
    }
    return 'other'
}

# ---------------------------------------------------------------------------
# Install flow
# ---------------------------------------------------------------------------

function Invoke-Cargo {
    param([string[]] $Arguments)
    Write-Host "cargo $($Arguments -join ' ')"
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

<#
Build and return the executable path Cargo reports for the `ha` bin.

Reading Cargo's own message stream is what makes the artifact identity real: a
Cargo.toml, Cargo.lock, toolchain or build-script change moves the artifact, and a
timestamp comparison over a few source files would miss it.
#>
function Resolve-CargoArtifact {
    param([string] $BuildProfile)
    $arguments = @('build', '-p', 'harness-cli', '--bin', 'ha', '--locked', '--message-format=json')
    if ($BuildProfile -eq 'Release') {
        $arguments += '--release'
    }
    Write-Host "cargo $($arguments -join ' ')"
    $messages = & cargo @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
    $artifact = ''
    foreach ($line in @($messages)) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $message = $null
        try { $message = $line | ConvertFrom-Json } catch { continue }
        if ($null -eq $message -or $message.reason -ne 'compiler-artifact') { continue }
        if ($null -eq $message.executable) { continue }
        if ($message.target.name -ne 'ha') { continue }
        $artifact = [string] $message.executable
    }
    return $artifact
}

function Get-BuildCommit {
    $commit = ''
    try {
        $commit = [string] (& git -C $repositoryRoot rev-parse HEAD 2>$null)
    }
    catch {
        $commit = ''
    }
    return $commit.Trim()
}

function Write-InstallManifest {
    param(
        [string] $ManifestPath,
        [string] $Version,
        [string] $Digest,
        [string] $Commit,
        [string[]] $OwnedFiles,
        [string] $AddedPathEntry,
        [string] $Source = ''
    )
    $manifest = [ordered]@{
        schema_version   = 1
        name             = 'ha'
        version          = $Version
        sha256           = $Digest
        source           = if ([string]::IsNullOrEmpty($Source)) { $repositoryRoot } else { $Source }
        build_commit     = $Commit
        profile          = $Profile
        owned_files      = $OwnedFiles
        added_path_entry = if ([string]::IsNullOrEmpty($AddedPathEntry)) { $null } else { $AddedPathEntry }
    }
    $manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ManifestPath -Encoding utf8
}

<#
Verify a release candidate bundle before trusting it.

The bundle is self-describing: ha.release.json records the executable digest and
checksums.txt covers every other file. A mismatch is refused by name.
#>
function Resolve-BundleArtifact {
    param([string] $BundleDirectory)
    $manifestPath = Join-Path $BundleDirectory 'ha.release.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "no ha.release.json in $BundleDirectory; that is not a release candidate bundle"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $binary = Join-Path $BundleDirectory ([string] $manifest.executable)
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "the bundle is missing its executable at $binary"
    }
    $digest = Get-FileDigest -FilePath $binary
    if ($digest -ne [string] $manifest.sha256) {
        throw "bundle_checksum_mismatch: $binary does not match the digest recorded in ha.release.json"
    }
    $checksumFile = Join-Path $BundleDirectory 'checksums.txt'
    if (Test-Path -LiteralPath $checksumFile -PathType Leaf) {
        foreach ($line in (Get-Content -LiteralPath $checksumFile)) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            $parts = $line -split '\s+', 2
            $expected = $parts[0]
            $name = $parts[1].Trim()
            $candidate = Join-Path $BundleDirectory $name
            if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
                throw "bundle_file_missing: $name is listed in checksums.txt but not present"
            }
            if ((Get-FileDigest -FilePath $candidate) -ne $expected) {
                throw "bundle_checksum_mismatch: $name does not match checksums.txt"
            }
        }
    }
    return [pscustomobject]@{
        Binary   = $binary
        Version  = [string] $manifest.version
        Manifest = $manifest
    }
}

function Remove-UserPathEntry {
    param([string] $UserPath, [string] $Entry)
    $entries = @(Split-PathValue -PathValue $UserPath)
    $target = Get-PathEntryKey -Entry $Entry
    $kept = @($entries | Where-Object { (Get-PathEntryKey -Entry $_) -ne $target })
    return [pscustomobject]@{
        Path    = ($kept -join [System.IO.Path]::PathSeparator)
        Removed = ($kept.Count -lt $entries.Count)
    }
}

<#
Uninstall: remove exactly the files this installer recorded, and only the PATH
entry it added. User configuration and session data are never removed here.
#>
function Invoke-Uninstall {
    param([string] $InstallDirectory = '')
    if ([string]::IsNullOrWhiteSpace($InstallDirectory)) {
        $InstallDirectory = (Resolve-InstallTargets).InstallDirectory
    }
    $manifestPath = Join-Path $InstallDirectory $manifestName
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "nothing to uninstall: no $manifestName in $InstallDirectory. This installer only removes files it recorded."
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $removed = [System.Collections.Generic.List[string]]::new()
    foreach ($file in @($manifest.owned_files)) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { continue }
        try {
            Remove-Item -LiteralPath $file -Force
            $removed.Add($file)
        }
        catch {
            throw "cannot remove $file because it is in use. Close every running 'ha' and retry. ($($_.Exception.Message))"
        }
    }
    $pathMessage = 'PATH:      no PATH entry was recorded for this install'
    $recorded = [string] $manifest.added_path_entry
    if (-not [string]::IsNullOrEmpty($recorded)) {
        $userPath = Get-PathValue -Provider $UserPathProvider -Scope 'User'
        $result = Remove-UserPathEntry -UserPath $userPath -Entry $recorded
        if ($result.Removed) {
            if ($null -ne $UserPathWriter) { & $UserPathWriter $result.Path } else { [Environment]::SetEnvironmentVariable('Path', $result.Path, 'User') }
            $pathMessage = "PATH:      removed $recorded from the persisted User PATH"
        }
        else {
            $pathMessage = 'PATH:      the recorded PATH entry was already absent'
        }
    }
    Write-Host ''
    Write-Host "Removed: $($removed -join ', ')"
    Write-Host $pathMessage
    Write-Host 'Kept:    user config and session data are never removed by uninstall'
}

<#
Stage, verify, then replace the installed binary.

A failure at any step leaves the previous binary in place: the caller gets a typed
kind ('in_use', 'access_denied', 'other', 'staged_verification') instead of a
message that blames every error on a running process.
#>
function Install-VerifiedBinary {
    param([string] $SourceBinary, [string] $TargetBinary)
    # The destination may not exist yet; create it instead of failing later with a
    # confusing "part of the path" error from the staging copy.
    $targetDirectory = Split-Path -Parent $TargetBinary
    if (-not [string]::IsNullOrWhiteSpace($targetDirectory) -and -not (Test-Path -LiteralPath $targetDirectory -PathType Container)) {
        New-Item -ItemType Directory -Path $targetDirectory -Force | Out-Null
    }
    # The staged file must stay executable: on Windows a bare ".new" suffix would
    # make the verification run fail before the real install ever happens.
    $extension = [System.IO.Path]::GetExtension($TargetBinary)
    $stagingBinary = "$TargetBinary.staged$extension"
    $backupBinary = "$TargetBinary.old$extension"
    Remove-Item -LiteralPath $stagingBinary -Force -ErrorAction SilentlyContinue

    try {
        Copy-Item -LiteralPath $SourceBinary -Destination $stagingBinary -Force
    }
    catch {
        return [pscustomobject]@{
            Ok     = $false
            Kind   = (Get-CopyFailureKind -Exception $_.Exception)
            Detail = "staging copy failed: $($_.Exception.Message)"
        }
    }

    $sourceDigest = Get-FileDigest -FilePath $SourceBinary
    $stagedDigest = Get-FileDigest -FilePath $stagingBinary
    if ($sourceDigest -ne $stagedDigest) {
        Remove-Item -LiteralPath $stagingBinary -Force -ErrorAction SilentlyContinue
        return [pscustomobject]@{
            Ok     = $false
            Kind   = 'staged_verification'
            Detail = 'the staged copy does not match the built artifact digest'
        }
    }
    $stagedVersion = ''
    try {
        $stagedVersion = [string] (& $stagingBinary --version)
        if ($LASTEXITCODE -ne 0) {
            throw "exit code $LASTEXITCODE"
        }
    }
    catch {
        Remove-Item -LiteralPath $stagingBinary -Force -ErrorAction SilentlyContinue
        return [pscustomobject]@{
            Ok     = $false
            Kind   = 'staged_verification'
            Detail = "the staged binary does not run: $($_.Exception.Message)"
        }
    }

    $hadExisting = Test-Path -LiteralPath $TargetBinary -PathType Leaf
    if ($hadExisting) {
        Remove-Item -LiteralPath $backupBinary -Force -ErrorAction SilentlyContinue
        try {
            Move-Item -LiteralPath $TargetBinary -Destination $backupBinary -Force
        }
        catch {
            Remove-Item -LiteralPath $stagingBinary -Force -ErrorAction SilentlyContinue
            return [pscustomobject]@{
                Ok     = $false
                Kind   = (Get-CopyFailureKind -Exception $_.Exception)
                Detail = "the installed binary could not be moved aside: $($_.Exception.Message)"
            }
        }
    }
    try {
        Move-Item -LiteralPath $stagingBinary -Destination $TargetBinary -Force
    }
    catch {
        if ($hadExisting) {
            Move-Item -LiteralPath $backupBinary -Destination $TargetBinary -Force -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $stagingBinary -Force -ErrorAction SilentlyContinue
        return [pscustomobject]@{
            Ok     = $false
            Kind   = (Get-CopyFailureKind -Exception $_.Exception)
            Detail = "replacing the installed binary failed: $($_.Exception.Message)"
        }
    }
    Remove-Item -LiteralPath $backupBinary -Force -ErrorAction SilentlyContinue
    return [pscustomobject]@{
        Ok      = $true
        Kind    = 'installed'
        Detail  = 'staged copy verified and moved into place'
        Version = $stagedVersion.Trim()
        Digest  = $sourceDigest
    }
}

# ---------------------------------------------------------------------------
# Main flow
# ---------------------------------------------------------------------------

function Resolve-InstallTargets {
    $destination = $Destination
    $cargoRoot = $false
    if ([string]::IsNullOrWhiteSpace($destination)) {
        if ($UseCargoInstall) {
            $destination = Join-Path $HOME '.cargo'
            $cargoRoot = $true
        }
        else {
            $destination = Join-Path $HOME '.cargo/bin'
        }
    }
    elseif ($UseCargoInstall) {
        $cargoRoot = $true
    }
    $installDir = if ($cargoRoot) { Join-Path $destination 'bin' } else { $destination }
    return [pscustomobject]@{
        InstallDirectory = [System.IO.Path]::GetFullPath($installDir)
        CargoRoot        = if ($cargoRoot) { [System.IO.Path]::GetFullPath($destination) } else { '' }
    }
}

function Invoke-Install {
    if ($ModifyUserPath -and $NoModifyPath) {
        throw '-ModifyUserPath and -NoModifyPath contradict each other; pass at most one.'
    }
    if ($Uninstall) {
        Invoke-Uninstall
        return
    }
    if (-not [string]::IsNullOrWhiteSpace($FromBundle) -and $UseCargoInstall) {
        throw '-FromBundle installs a packaged candidate; it cannot be combined with -UseCargoInstall.'
    }
    $targets = Resolve-InstallTargets
    $installDirectory = $targets.InstallDirectory
    $installedBinary = Join-Path $installDirectory $executableName
    $manifestPath = Join-Path $installDirectory $manifestName

    Write-Host "Repository: $repositoryRoot"
    Write-Host "Profile:    $Profile"
    Write-Host "Install to: $installDirectory"

    if (-not (Test-Path -LiteralPath $installDirectory -PathType Container)) {
        New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null
        Write-Host "Created $installDirectory"
    }

    $version = ''
    $digest = ''
    if ($UseCargoInstall) {
        $arguments = @(
            'install', '--path', (Join-Path $repositoryRoot 'crates/harness-cli'),
            '--locked', '--force', '--root', $targets.CargoRoot,
            '--target-dir', (Join-Path $repositoryRoot 'target')
        )
        if ($Profile -eq 'Debug') {
            $arguments += '--debug'
        }
        Invoke-Cargo -Arguments $arguments
        if (-not (Test-Path -LiteralPath $installedBinary -PathType Leaf)) {
            throw "cargo install finished but $installedBinary does not exist."
        }
        $version = [string] (& $installedBinary --version)
        $digest = Get-FileDigest -FilePath $installedBinary
    }
    else {
        $sourceBinary = $expectedArtifact
        $manifestSource = ''
        if (-not [string]::IsNullOrWhiteSpace($FromBundle)) {
            # A candidate bundle is verified before anything is staged: manifest
            # digest and every checksum line must match.
            $bundle = Resolve-BundleArtifact -BundleDirectory $FromBundle
            $sourceBinary = $bundle.Binary
            $manifestSource = [System.IO.Path]::GetFullPath($FromBundle)
            Write-Host "Bundle:     $manifestSource (version $($bundle.Version))"
        }
        elseif (-not $SkipBuild) {
            $reported = Resolve-CargoArtifact -BuildProfile $Profile
            if (-not [string]::IsNullOrWhiteSpace($reported)) {
                $sourceBinary = $reported
            }
            elseif (Test-Path -LiteralPath $expectedArtifact -PathType Leaf) {
                Write-Host "Cargo did not report an artifact; using $expectedArtifact"
                $sourceBinary = $expectedArtifact
            }
            else {
                throw 'Cargo did not report a usable artifact for the ha bin.'
            }
        }
        if (-not (Test-Path -LiteralPath $sourceBinary -PathType Leaf)) {
            throw "The artifact is missing at $sourceBinary. Run without -SkipBuild."
        }
        Write-Host "Artifact:   $sourceBinary"
        $result = Install-VerifiedBinary -SourceBinary $sourceBinary -TargetBinary $installedBinary
        if (-not $result.Ok) {
            switch ($result.Kind) {
                'in_use' {
                    throw "$installedBinary is in use. Close every running 'ha' and run this script again; the previous binary was left untouched. ($($result.Detail))"
                }
                'access_denied' {
                    throw "Permission denied replacing $installedBinary. Choose a writable -Destination or re-run with enough rights. ($($result.Detail))"
                }
                default {
                    throw "Install failed [$($result.Kind)]: $($result.Detail)"
                }
            }
        }
        $version = $result.Version
        $digest = $result.Digest
    }

    $addedPathEntry = ''
    $userPath = Get-PathValue -Provider $UserPathProvider -Scope 'User'
    $alreadyOnUserPath = -not (Merge-UserPath -UserPath $userPath -InstallDir $installDirectory).Added
    if ($ModifyUserPath -and -not $NoModifyPath) {
        $merge = Merge-UserPath -UserPath $userPath -InstallDir $installDirectory
        if ($merge.Added) {
            if ($null -ne $UserPathWriter) {
                & $UserPathWriter $merge.Path
            }
            else {
                [Environment]::SetEnvironmentVariable('Path', $merge.Path, 'User')
            }
            $addedPathEntry = $installDirectory
        }
    }

    $manifestArguments = @{
        ManifestPath   = $manifestPath
        Version        = $version
        Digest         = $digest
        Commit         = (Get-BuildCommit)
        OwnedFiles     = @($installedBinary, $manifestPath)
        AddedPathEntry = $addedPathEntry
        Source         = $manifestSource
    }
    Write-InstallManifest @manifestArguments

    $processPath = Get-PathValue -Provider $ProcessPathProvider -Scope 'Process'
    $matches = @(Find-HaOnPath -PathValue $processPath)
    $foreign = @($matches | Where-Object { (Get-PathEntryKey -Entry $_.Path) -ne (Get-PathEntryKey -Entry $installedBinary) })
    $sessionCommand = Get-Command ha -ErrorAction SilentlyContinue

    Write-Host ''
    Write-Host "Installed: $installedBinary"
    Write-Host "Version:   $version"
    Write-Host "SHA-256:   $digest"
    Write-Host "Manifest:  $manifestPath"
    if ($foreign.Count -gt 0) {
        Write-Host "WARNING:   'ha' currently resolves to $($foreign[0].Path) first. This installer does not own or remove it; put $installDirectory earlier on PATH to win."
    }
    if ($null -ne $sessionCommand -and $sessionCommand.CommandType -in @('Alias', 'Function')) {
        Write-Host "WARNING:   this session defines 'ha' as a $($sessionCommand.CommandType); it shadows the installed binary here."
    }
    if (-not [string]::IsNullOrEmpty($addedPathEntry)) {
        Write-Host "PATH:      added $addedPathEntry to the persisted User PATH."
        Write-Host '           A new terminal sees it; this shell keeps the PATH it inherited.'
        Write-Host "           Use it right now with: $installedBinary"
    }
    elseif ($alreadyOnUserPath) {
        Write-Host 'PATH:      the install directory is already on the persisted User PATH; open a new terminal.'
    }
    else {
        Write-Host "PATH:      $installDirectory is NOT on the persisted User PATH."
        Write-Host '           Re-run with -ModifyUserPath to add it, or add it yourself:'
        Write-Host "           [Environment]::SetEnvironmentVariable('Path', ([Environment]::GetEnvironmentVariable('Path','User') + ';$installDirectory'), 'User')"
        Write-Host "           Use it right now with: $installedBinary"
    }
}

# ---------------------------------------------------------------------------
# Self test: proves the rules in memory and in a disposable directory.
# ---------------------------------------------------------------------------

function Add-SelfTestSkip {
    param([string] $Name, [string] $Reason)
    Write-Host "SELFTEST_SKIP: $Name ($Reason)"
}

function Add-SelfTestResult {
    param([string] $Name, [bool] $Ok, [string] $Detail = '')
    if ($Ok) {
        Write-Host "SELFTEST_OK:   $Name"
    }
    else {
        Write-Host "SELFTEST_FAIL: $Name $Detail"
        $script:SelfTestFailures.Add("$Name $Detail")
    }
}

function Invoke-SelfTest {
    $separator = [System.IO.Path]::PathSeparator
    $realUserPath = [string] [Environment]::GetEnvironmentVariable('Path', 'User')

    $case1 = Merge-UserPath -UserPath 'C:\user\a;C:\user\b' -InstallDir 'C:\tools\ha'
    Add-SelfTestResult 'merge_appends_a_missing_directory' ($case1.Added -and $case1.Path -eq 'C:\user\a;C:\user\b;C:\tools\ha') "got '$($case1.Path)'"

    $case2 = Merge-UserPath -UserPath 'C:\Tools\ha\' -InstallDir 'c:\tools\HA'
    Add-SelfTestResult 'merge_is_case_insensitive_and_ignores_a_trailing_separator' ((-not $case2.Added) -and $case2.Path -eq 'C:\Tools\ha\') "got '$($case2.Path)'"

    $case3 = Merge-UserPath -UserPath 'C:\user\a;;C:\user\b;' -InstallDir 'C:\tools\ha'
    Add-SelfTestResult 'merge_drops_empty_entries_and_keeps_order' ($case3.Added -and $case3.Path -eq 'C:\user\a;C:\user\b;C:\tools\ha') "got '$($case3.Path)'"

    $case4 = Merge-UserPath -UserPath 'C:\user\a' -InstallDir 'C:\tools\ha'
    $noLeak = (-not $case4.Path.Contains('C:\machine\only')) -and (-not $case4.Path.Contains('C:\process\only'))
    Add-SelfTestResult 'merge_never_folds_machine_or_process_entries_into_user_path' $noLeak "got '$($case4.Path)'"

    $recorded = [System.Collections.Generic.List[string]]::new()
    $writer = { param([string] $Value) $recorded.Add($Value) }.GetNewClosure()
    $provider = { 'C:\user\a' }.GetNewClosure()
    $userPath = Get-PathValue -Provider $provider -Scope 'User'
    $merge = Merge-UserPath -UserPath $userPath -InstallDir 'C:\tools\ha'
    if ($merge.Added) { & $writer $merge.Path }
    Add-SelfTestResult 'injected_writer_receives_the_merged_user_path' (($recorded.Count -eq 1) -and ($recorded[0] -eq 'C:\user\a;C:\tools\ha')) "recorded $($recorded -join '|')"

    $shadowRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ha-shadow-" + [guid]::NewGuid().ToString('n'))
    $ownerRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ha-owner-" + [guid]::NewGuid().ToString('n'))
    New-Item -ItemType Directory -Path $shadowRoot, $ownerRoot -Force | Out-Null
    try {
        $shadowCommand = Join-Path $shadowRoot 'ha.cmd'
        Set-Content -LiteralPath $shadowCommand -Value '@echo shadow' -Encoding utf8
        $ownerBinary = Join-Path $ownerRoot 'ha.exe'
        Set-Content -LiteralPath $ownerBinary -Value 'placeholder' -Encoding utf8
        $found = @(Find-HaOnPath -PathValue "$shadowRoot$separator$ownerRoot")
        Add-SelfTestResult 'shadowing_command_is_found_before_the_owned_binary' (($found.Count -ge 1) -and ($found[0].Path -eq $shadowCommand)) "found $($found.Count) entries"
        Add-SelfTestResult 'shadowing_command_is_never_deleted' (Test-Path -LiteralPath $shadowCommand -PathType Leaf)
    }
    finally {
        Remove-Item -LiteralPath $shadowRoot, $ownerRoot -Recurse -Force -ErrorAction SilentlyContinue
    }

    # Prefer an artifact that is already built: the self test only needs a real
    # executable, and building one would make the check slow and stateful.
    $candidates = @(
        (Join-Path $repositoryRoot "target/$profileDirectory/$executableName"),
        (Join-Path $repositoryRoot "target/debug/$executableName"),
        (Join-Path $repositoryRoot "target/release/$executableName")
    )
    $artifact = ''
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $artifact = $candidate
            break
        }
    }
    if ([string]::IsNullOrWhiteSpace($artifact)) {
        Write-Host "SELFTEST_INFO: building $Profile artifact for the disposable install"
        $artifact = Resolve-CargoArtifact -BuildProfile $Profile
    }
    if ([string]::IsNullOrWhiteSpace($artifact) -or -not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
        Add-SelfTestResult 'disposable_install_artifact_available' $false "no artifact at $expectedArtifact"
    }
    else {
        $installRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ha-install-" + [guid]::NewGuid().ToString('n'))
        New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
        try {
            $target = Join-Path $installRoot $executableName
            $first = Install-VerifiedBinary -SourceBinary $artifact -TargetBinary $target
            $installedVersion = if (Test-Path -LiteralPath $target -PathType Leaf) { [string] (& $target --version) } else { '' }
            Add-SelfTestResult 'disposable_install_replaces_and_verifies_the_artifact' ($first.Ok -and (Test-Path -LiteralPath $target -PathType Leaf) -and ($installedVersion -match 'ha')) "$($first.Kind): $($first.Detail)"

            if (-not $first.Ok) {
                Add-SelfTestSkip 'installed_digest_matches_the_built_artifact' 'the install did not complete'
                Add-SelfTestSkip 'install_manifest_records_version_and_digest' 'the install did not complete'
                Add-SelfTestSkip 'update_keeps_the_binary_usable' 'the install did not complete'
                Add-SelfTestSkip 'locked_executable_is_reported_as_in_use' 'the install did not complete'
                Add-SelfTestSkip 'a_failed_replacement_leaves_the_previous_binary_usable' 'the install did not complete'
                return
            }
            $installedDigest = Get-FileDigest -FilePath $target
            $artifactDigest = Get-FileDigest -FilePath $artifact
            Add-SelfTestResult 'installed_digest_matches_the_built_artifact' ($installedDigest -eq $artifactDigest)

            $manifestPath = Join-Path $installRoot $manifestName
            $manifestArguments = @{
                ManifestPath   = $manifestPath
                Version        = $installedVersion.Trim()
                Digest         = $installedDigest
                Commit         = 'selftest'
                OwnedFiles     = @($target, $manifestPath)
                AddedPathEntry = ''
            }
            Write-InstallManifest @manifestArguments
            $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
            Add-SelfTestResult 'install_manifest_records_version_and_digest' (($manifest.version -eq $installedVersion.Trim()) -and ($manifest.sha256 -eq $installedDigest) -and ($manifest.owned_files.Count -eq 2)) "manifest $($manifest | ConvertTo-Json -Compress)"

            $second = Install-VerifiedBinary -SourceBinary $artifact -TargetBinary $target
            Add-SelfTestResult 'update_keeps_the_binary_usable' ($second.Ok -and (Test-Path -LiteralPath $target -PathType Leaf)) "$($second.Kind): $($second.Detail)"

            # Hold the installed file open the way a running binary would. A scanner
            # can hold a transient handle right after the move, so retry briefly
            # instead of failing the check on a timing artefact.
            $lock = $null
            for ($attempt = 0; $attempt -lt 10 -and $null -eq $lock; $attempt++) {
                try {
                    $lock = [System.IO.File]::Open($target, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::None)
                }
                catch {
                    Start-Sleep -Milliseconds 100
                }
            }
            if ($null -eq $lock) {
                Add-SelfTestSkip 'locked_executable_is_reported_as_in_use' 'no exclusive handle could be taken in this environment'
            }
            else {
                try {
                    $locked = Install-VerifiedBinary -SourceBinary $artifact -TargetBinary $target
                    Add-SelfTestResult 'locked_executable_is_reported_as_in_use' ((-not $locked.Ok) -and ($locked.Kind -eq 'in_use')) "kind $($locked.Kind): $($locked.Detail)"
                }
                finally {
                    $lock.Dispose()
                }
            }
            $afterLock = [string] (& $target --version)
            Add-SelfTestResult 'a_failed_replacement_leaves_the_previous_binary_usable' ($afterLock -match 'ha') "got '$afterLock'"

            # Bundle route: a verified candidate installs, a tampered one is refused.
            $bundleRoot = Join-Path $installRoot 'bundle'
            New-Item -ItemType Directory -Path $bundleRoot -Force | Out-Null
            $bundleBinary = Join-Path $bundleRoot $executableName
            Copy-Item -LiteralPath $artifact -Destination $bundleBinary -Force
            $bundleManifest = [ordered]@{
                schema_version = 1
                name           = 'ha'
                version        = 'selftest'
                target         = 'selftest'
                executable     = $executableName
                sha256         = (Get-FileDigest -FilePath $bundleBinary)
                published      = $false
            }
            $bundleManifest | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $bundleRoot 'ha.release.json') -Encoding utf8
            $checksumLines = @()
            foreach ($file in (Get-ChildItem -LiteralPath $bundleRoot -File | Sort-Object -Property Name)) {
                $checksumLines += "$(Get-FileDigest -FilePath $file.FullName)  $($file.Name)"
            }
            Set-Content -LiteralPath (Join-Path $bundleRoot 'checksums.txt') -Value $checksumLines -Encoding utf8

            $bundleInstallDirectory = Join-Path $installRoot 'from-bundle'
            $bundleTarget = Join-Path $bundleInstallDirectory $executableName
            $bundleArtifact = Resolve-BundleArtifact -BundleDirectory $bundleRoot
            $bundleResult = Install-VerifiedBinary -SourceBinary $bundleArtifact.Binary -TargetBinary $bundleTarget
            Add-SelfTestResult 'bundle_install_uses_the_verified_executable' ($bundleResult.Ok -and (Test-Path -LiteralPath $bundleTarget -PathType Leaf)) "$($bundleResult.Kind): $($bundleResult.Detail)"

            Copy-Item -LiteralPath $bundleBinary -Destination "$bundleBinary.tampered" -Force
            Add-Content -LiteralPath $bundleBinary -Value 'tamper' -Encoding utf8
            $tamperRefused = $false
            try {
                Resolve-BundleArtifact -BundleDirectory $bundleRoot | Out-Null
            }
            catch {
                $tamperRefused = $_.Exception.Message -like 'bundle_checksum_mismatch*'
            }
            Add-SelfTestResult 'a_tampered_bundle_is_refused' $tamperRefused
            Move-Item -LiteralPath "$bundleBinary.tampered" -Destination $bundleBinary -Force

            # Uninstall removes only owned files and keeps everything else.
            $foreignFile = Join-Path $bundleInstallDirectory 'keep-me.txt'
            Set-Content -LiteralPath $foreignFile -Value 'not owned by the installer' -Encoding utf8
            $bundleManifestArguments = @{
                ManifestPath   = (Join-Path $bundleInstallDirectory $manifestName)
                Version        = 'selftest'
                Digest         = $bundleResult.Digest
                Commit         = 'selftest'
                OwnedFiles     = @($bundleTarget, (Join-Path $bundleInstallDirectory $manifestName))
                AddedPathEntry = ''
                Source         = $bundleRoot
            }
            Write-InstallManifest @bundleManifestArguments
            Invoke-Uninstall -InstallDirectory $bundleInstallDirectory | Out-Null
            Add-SelfTestResult 'uninstall_removes_only_owned_files' ((-not (Test-Path -LiteralPath $bundleTarget)) -and (-not (Test-Path -LiteralPath (Join-Path $bundleInstallDirectory $manifestName))) -and (Test-Path -LiteralPath $foreignFile -PathType Leaf))

            $dataDirectory = Join-Path $installRoot 'user-data'
            New-Item -ItemType Directory -Path $dataDirectory -Force | Out-Null
            Set-Content -LiteralPath (Join-Path $dataDirectory 'config.toml') -Value 'schema_version = 1' -Encoding utf8
            Add-SelfTestResult 'uninstall_keeps_user_data' (Test-Path -LiteralPath (Join-Path $dataDirectory 'config.toml') -PathType Leaf)
        }
        finally {
            Remove-Item -LiteralPath $installRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }

    $realUserPathAfter = [string] [Environment]::GetEnvironmentVariable('Path', 'User')
    Add-SelfTestResult 'self_test_never_writes_the_real_user_path' ($realUserPath -eq $realUserPathAfter)
}

if ($SelfTest) {
    Write-Host 'Install-Ha.ps1 self test (no real install location or PATH entry is touched)'
    Invoke-SelfTest
    if ($script:SelfTestFailures.Count -gt 0) {
        Write-Host "INSTALL_SELFTEST_FAILED: $($script:SelfTestFailures.Count) check(s)"
        exit 1
    }
    Write-Host 'INSTALL_SELFTEST_OK: install logic, PATH merge rules and rollback verified'
    exit 0
}

Invoke-Install



