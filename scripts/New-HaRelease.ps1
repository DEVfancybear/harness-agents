<#
.SYNOPSIS
Build a local release candidate for `ha` with checksums, without publishing it.

.DESCRIPTION
This script produces the artifact an end user would download, but it does not
publish anything: there is no registry, no GitHub release and no URL. Publishing
requires an explicit grant that this assignment does not carry.

A candidate bundle contains exactly:

  - `ha` (the release executable the tested revision produced);
  - `ha.release.json` (target, version, rustc, source revision, artifact digest);
  - `checksums.txt` (sha256 of every file in the bundle).

Nothing else: no fixture executables, no test secrets, no build cache. The bundle
checker fails when an unexpected file appears, so a stray fixture cannot ship.

.PARAMETER Profile
`Release` (default) or `Debug` for a fast local candidate.

.PARAMETER OutputDirectory
Where candidates are written. Defaults to `target/release-candidate`.

.PARAMETER SkipBuild
Package the artifact that already exists instead of rebuilding.

.PARAMETER SelfTest
Check the bundle rules against in-memory/disposable fixtures and exit.

.EXAMPLE
pwsh -NoProfile -File scripts/New-HaRelease.ps1
Build and package a Windows x64 candidate.
#>
[CmdletBinding()]
param(
    [ValidateSet('Release', 'Debug')]
    [string] $Profile = 'Release',
    [string] $OutputDirectory = '',
    [switch] $SkipBuild,
    [switch] $SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$profileDirectory = if ($Profile -eq 'Release') { 'release' } else { 'debug' }
$isWindowsHost = [System.IO.Path]::DirectorySeparatorChar -eq '\'
$executableName = if ($isWindowsHost) { 'ha.exe' } else { 'ha' }
$expectedArtifact = Join-Path $repositoryRoot "target/$profileDirectory/$executableName"
$script:SelfTestFailures = [System.Collections.Generic.List[string]]::new()

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
    try {
        return ([string] (& git -C $repositoryRoot rev-parse HEAD 2>$null)).Trim()
    }
    catch {
        return ''
    }
}

function Get-RustcVersion {
    try {
        return ([string] (& rustc --version 2>$null)).Trim()
    }
    catch {
        return ''
    }
}

function Get-TargetTriple {
    try {
        return ([string] (& rustc -vV 2>$null | Select-String -Pattern '^host: ' | ForEach-Object { $_.Line -replace '^host: ', '' })).Trim()
    }
    catch {
        return 'unknown'
    }
}

<#
The bundle contract: exactly the executable, the release manifest and the
checksums. Any other file is a packaging defect, because bundles must never carry
fixture executables, test secrets or build output.
#>
function Assert-BundleContents {
    param([string] $BundleDirectory, [string] $Executable, [string[]] $ExpectedNames)
    $files = @(Get-ChildItem -LiteralPath $BundleDirectory -File | Sort-Object -Property Name | ForEach-Object { $_.Name })
    $unexpected = @($files | Where-Object { $ExpectedNames -notcontains $_ })
    if ($unexpected.Count -gt 0) {
        throw "bundle_contains_unexpected_files: $($unexpected -join ', ')"
    }
    if ($files -notcontains $Executable) {
        throw "bundle_missing_executable: $Executable is not in $BundleDirectory"
    }
    return $files
}

function New-Bundle {
    param([string] $Artifact, [string] $BundleDirectory, [string] $Version, [string] $Target)
    if (Test-Path -LiteralPath $BundleDirectory) {
        Remove-Item -LiteralPath $BundleDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $BundleDirectory -Force | Out-Null
    $bundleBinary = Join-Path $BundleDirectory $executableName
    Copy-Item -LiteralPath $Artifact -Destination $bundleBinary -Force
    $digest = Get-FileDigest -FilePath $bundleBinary
    $manifest = [ordered]@{
        schema_version = 1
        name           = 'ha'
        version        = $Version
        target         = $Target
        executable     = $executableName
        sha256         = $digest
        profile        = $Profile
        build_commit   = (Get-BuildCommit)
        rustc          = (Get-RustcVersion)
        built_at       = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        published      = $false
    }
    $manifestPath = Join-Path $BundleDirectory 'ha.release.json'
    $manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $manifestPath -Encoding utf8

    # checksums.txt covers every bundled file except itself: a file cannot hash
    # its own hash.
    $checksumLines = @()
    foreach ($file in (Get-ChildItem -LiteralPath $BundleDirectory -File | Sort-Object -Property Name)) {
        $checksumLines += "$(Get-FileDigest -FilePath $file.FullName)  $($file.Name)"
    }
    Set-Content -LiteralPath (Join-Path $BundleDirectory 'checksums.txt') -Value $checksumLines -Encoding utf8

    $names = Assert-BundleContents -BundleDirectory $BundleDirectory -Executable $executableName -ExpectedNames @($executableName, 'ha.release.json', 'checksums.txt')
    return [pscustomobject]@{
        Directory = $BundleDirectory
        Files     = $names
        Digest    = $digest
        Version   = $Version
    }
}

function Invoke-ReleaseSelfTest {
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) ("ha-bundle-" + [guid]::NewGuid().ToString('n'))
    New-Item -ItemType Directory -Path $temp -Force | Out-Null
    try {
        $binary = Join-Path $temp $executableName
        Set-Content -LiteralPath $binary -Value 'placeholder' -Encoding utf8
        $bundle = Join-Path $temp 'bundle'
        $result = New-Bundle -Artifact $binary -BundleDirectory $bundle -Version '0.1.0-test' -Target 'test-target'
        if ($result.Files.Count -ne 3) { $script:SelfTestFailures.Add("bundle_files_expected_3 got $($result.Files.Count)") }
        $manifest = Get-Content -LiteralPath (Join-Path $bundle 'ha.release.json') -Raw | ConvertFrom-Json
        if ($manifest.sha256 -ne (Get-FileDigest -FilePath (Join-Path $bundle $executableName))) {
            $script:SelfTestFailures.Add('bundle_manifest_digest_mismatch')
        }
        if ($manifest.published -ne $false) { $script:SelfTestFailures.Add('bundle_must_not_claim_publication') }
        $checksums = Get-Content -LiteralPath (Join-Path $bundle 'checksums.txt')
        if ($checksums.Count -ne 2) { $script:SelfTestFailures.Add("checksums_expected_2 got $($checksums.Count)") }
        foreach ($expected in @($executableName, 'ha.release.json')) {
            if (-not ($checksums | Where-Object { $_ -like "*  $expected" })) {
                $script:SelfTestFailures.Add("checksums_missing_entry_for_$expected")
            }
        }

        # Negative control: an extra file (a fixture would look like this) must fail.
        $stray = Join-Path $bundle 'p6_fixture_plugin.exe'
        Set-Content -LiteralPath $stray -Value 'fixture' -Encoding utf8
        $rejected = $false
        try {
            Assert-BundleContents -BundleDirectory $bundle -Executable $executableName -ExpectedNames @($executableName, 'ha.release.json', 'checksums.txt') | Out-Null
        }
        catch {
            $rejected = $_.Exception.Message -like 'bundle_contains_unexpected_files*'
        }
        if (-not $rejected) { $script:SelfTestFailures.Add('unexpected_file_was_not_rejected') }
        Remove-Item -LiteralPath $stray -Force

        if ($script:SelfTestFailures.Count -gt 0) {
            foreach ($failure in $script:SelfTestFailures) { Write-Host "RELEASE_SELFTEST_FAIL: $failure" }
            exit 1
        }
        Write-Host 'RELEASE_SELFTEST_OK: bundle contents, manifest digest and the no-extra-files rule verified'
        exit 0
    }
    finally {
        Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($SelfTest) {
    Invoke-ReleaseSelfTest
}

$target = Get-TargetTriple
$version = ''
$artifact = $expectedArtifact
if (-not $SkipBuild) {
    $reported = Resolve-CargoArtifact -BuildProfile $Profile
    if (-not [string]::IsNullOrWhiteSpace($reported)) { $artifact = $reported }
}
if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
    throw "the artifact is missing at $artifact; run without -SkipBuild"
}
$version = [string] (& $artifact --version)
if ($LASTEXITCODE -ne 0) {
    throw "the artifact at $artifact does not run"
}
$version = $version.Trim()
# `ha --version` prints "ha <version>"; the bundle name uses only the version token.
$versionToken = ($version -replace '^ha\s+', '').Trim()

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot 'target/release-candidate'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$platform = if ($isWindowsHost) { 'windows' } else { 'linux' }
$bundleName = "ha-$versionToken-$platform-x64"
$bundleDirectory = Join-Path $OutputDirectory $bundleName

Write-Host "Artifact: $artifact"
Write-Host "Version:  $version"
Write-Host "Target:   $target"
Write-Host "Bundle:   $bundleDirectory"

$result = New-Bundle -Artifact $artifact -BundleDirectory $bundleDirectory -Version $version -Target $target
$archive = Join-Path $OutputDirectory "$bundleName.zip"
if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
Compress-Archive -Path (Join-Path $bundleDirectory '*') -DestinationPath $archive
$archiveDigest = Get-FileDigest -FilePath $archive

Write-Host ''
Write-Host "Candidate:  $bundleDirectory"
Write-Host "Archive:    $archive"
Write-Host "SHA-256:    $archiveDigest"
Write-Host "Contents:   $($result.Files -join ', ')"
Write-Host 'Published:  no - publishing a release is not authorized in this assignment.'
