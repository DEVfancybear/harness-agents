<#
.SYNOPSIS
Packs the npm package for a `ha` release bundle.

.DESCRIPTION
Takes the bundle `New-HaRelease.ps1` wrote, checks every file against the
bundle's `checksums.txt`, and produces one tarball in `target/npm`:
`harness-agents` (the `ha` launcher, ha.exe, uv.exe and the release manifest).
Nothing is published; the publish command is printed for you to run after
`npm login`.

.PARAMETER BundleDirectory
The bundle to pack. Defaults to the newest `target/release-candidate/ha-*-windows-x64`.

.PARAMETER PackageVersion
The npm version. Defaults to the version the bundle was built as; give a higher
one to republish the same build, since npm never accepts a version twice.

.PARAMETER OutputDirectory
Where the tarballs go. Defaults to `target/npm`.

.EXAMPLE
pwsh -NoProfile -File scripts/New-HaNpmPackages.ps1
#>
[CmdletBinding()]
param(
    [string] $BundleDirectory = '',
    [string] $PackageVersion = '',
    [string] $OutputDirectory = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $BundleDirectory) {
    $newest = Get-ChildItem -LiteralPath (Join-Path $repositoryRoot 'target/release-candidate') -Directory -Filter 'ha-*-windows-x64' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $newest) { throw 'no release bundle found; run scripts/New-HaRelease.ps1 first' }
    $BundleDirectory = $newest.FullName
}
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repositoryRoot 'target/npm' }

foreach ($tool in 'npm', 'node') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "$tool is required" }
}

# The bundle is trusted only as far as its own checksums say.
$manifest = Get-Content -LiteralPath (Join-Path $BundleDirectory 'ha.release.json') -Raw | ConvertFrom-Json
$version = ($manifest.version -replace '^ha\s+', '').Trim()
if ($PackageVersion) { $version = $PackageVersion }
if ($version -notmatch '^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$') { throw "unexpected version '$($manifest.version)'" }
$checksums = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $BundleDirectory 'checksums.txt')) {
    if ($line -match '^([0-9a-f]{64})\s+\*?(.+)$') { $checksums[$Matches[2].Trim()] = $Matches[1] }
}
$files = 'ha.exe', 'uv.exe', 'ha.release.json', 'checksums.txt'
foreach ($name in ($files | Where-Object { $_ -ne 'checksums.txt' })) {
    $path = Join-Path $BundleDirectory $name
    if (-not (Test-Path -LiteralPath $path)) { throw "the bundle has no $name" }
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($checksums[$name] -ne $actual) { throw "$name does not match checksums.txt" }
}
if ($manifest.sha256 -ne $checksums['ha.exe']) { throw 'ha.release.json names a different ha.exe than checksums.txt' }

$stage = Join-Path $OutputDirectory 'stage'
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
$packageStage = Join-Path $stage 'harness-agents'
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'npm/harness-agents') -Destination $packageStage -Recurse
foreach ($name in $files) { Copy-Item -LiteralPath (Join-Path $BundleDirectory $name) -Destination $packageStage }
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE') -Destination $packageStage

$packageJson = Join-Path $packageStage 'package.json'
$package = Get-Content -LiteralPath $packageJson -Raw | ConvertFrom-Json
$package.version = $version
$package | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $packageJson -Encoding utf8NoBOM

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
Push-Location $packageStage
try {
    # npm prints warnings on stderr; only stdout is the JSON.
    $packed = & npm pack --pack-destination $OutputDirectory --json
    if ($LASTEXITCODE -ne 0) { throw 'npm pack failed' }
    $tarball = Join-Path $OutputDirectory (($packed | ConvertFrom-Json)[0].filename)
} finally {
    Pop-Location
}

$item = Get-Item -LiteralPath $tarball
Write-Host "Version:  $version (bundle build $($manifest.build_commit))"
Write-Host ('Tarball:  {0} ({1:N1} MB)' -f $item.FullName, ($item.Length / 1MB))
Write-Host ''
Write-Host 'Nothing was published. After `npm login`, publish it:'
Write-Host "  npm publish `"$($item.FullName)`" --access public"
