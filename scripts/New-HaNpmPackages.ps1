<#
.SYNOPSIS
Packs the npm packages for a `ha` release bundle.

.DESCRIPTION
Takes the bundle `New-HaRelease.ps1` wrote, checks every file against the
bundle's `checksums.txt`, and produces two tarballs in `target/npm`:
`harness-agents-win32-x64` (ha.exe, uv.exe, the release manifest) and
`harness-agents` (the `ha` launcher that finds the first one). Both carry the
version the bundle was built as. Nothing is published; the publish commands are
printed for you to run after `npm login`.

.PARAMETER BundleDirectory
The bundle to pack. Defaults to the newest `target/release-candidate/ha-*-windows-x64`.

.PARAMETER OutputDirectory
Where the tarballs go. Defaults to `target/npm`.

.EXAMPLE
pwsh -NoProfile -File scripts/New-HaNpmPackages.ps1
#>
[CmdletBinding()]
param(
    [string] $BundleDirectory = '',
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
$platformName = 'harness-agents-win32-x64'
$platformStage = Join-Path $stage $platformName
$mainStage = Join-Path $stage 'harness-agents'
Copy-Item -LiteralPath (Join-Path $repositoryRoot "npm/$platformName") -Destination $platformStage -Recurse
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'npm/harness-agents') -Destination $mainStage -Recurse
foreach ($name in $files) { Copy-Item -LiteralPath (Join-Path $BundleDirectory $name) -Destination $platformStage }
foreach ($directory in $platformStage, $mainStage) {
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE') -Destination $directory
}

# One version for both, and the launcher asks for exactly that platform build.
$platformJson = Join-Path $platformStage 'package.json'
$package = Get-Content -LiteralPath $platformJson -Raw | ConvertFrom-Json
$package.version = $version
$package | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $platformJson -Encoding utf8NoBOM
$mainJson = Join-Path $mainStage 'package.json'
$package = Get-Content -LiteralPath $mainJson -Raw | ConvertFrom-Json
$package.version = $version
$package.optionalDependencies.$platformName = $version
$package | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $mainJson -Encoding utf8NoBOM

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$tarballs = foreach ($directory in $platformStage, $mainStage) {
    Push-Location $directory
    try {
        $packed = & npm pack --pack-destination $OutputDirectory --json 2>&1
        if ($LASTEXITCODE -ne 0) { throw "npm pack failed in ${directory}: $packed" }
        Join-Path $OutputDirectory (($packed | ConvertFrom-Json)[0].filename)
    } finally {
        Pop-Location
    }
}

Write-Host "Version:  $version (bundle build $($manifest.build_commit))"
foreach ($tarball in $tarballs) {
    $item = Get-Item -LiteralPath $tarball
    Write-Host ('Tarball:  {0} ({1:N1} MB)' -f $item.FullName, ($item.Length / 1MB))
}
Write-Host ''
Write-Host 'Nothing was published. After `npm login`, publish the platform package first:'
Write-Host "  npm publish `"$($tarballs[0])`" --access public"
Write-Host "  npm publish `"$($tarballs[1])`" --access public"
