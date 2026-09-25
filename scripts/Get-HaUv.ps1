<#
.SYNOPSIS
Place a pinned, checksum-verified `uv` next to `ha`.

.DESCRIPTION
`ha` builds its Python kernel's virtual environment with `uv`. A machine without
`uv` fell back to the system Python, which lacks the kernel's packages. This helper
puts a known `uv` beside `ha.exe`, where `ha` looks for it first:

1. an existing `uv.exe` in the destination of the pinned version is kept;
2. otherwise the pinned release archive is downloaded from GitHub, its SHA-256
   checked against the value pinned below, and `uv.exe` extracted.

Dot-source it and call `Install-HaUv -Destination <dir>`; it returns the path of
the placed `uv.exe`, or throws with a reason. Windows x64 and arm64 only; other
hosts keep using `uv` from PATH.
#>

Set-StrictMode -Version Latest

$script:HaUvVersion = '0.12.19'
$script:HaUvArchives = @{
    'X64'   = @{
        Name   = 'uv-x86_64-pc-windows-msvc.zip'
        Sha256 = '6dbb02d79e419522f1c500f0adb1cddcff0cda7d59b0d66ea7f5e3b4a1b2f5f0'
    }
    'Arm64' = @{
        Name   = 'uv-aarch64-pc-windows-msvc.zip'
        Sha256 = '115b54cb823bc48260670f5782001add6067ac8d98d18c8263a833704e287de9'
    }
}

function Get-HaUvFileDigest {
    param([string] $FilePath)
    return (Get-FileHash -LiteralPath $FilePath -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-HaUvVersion {
    param([string] $UvPath)
    try {
        $text = & $UvPath --version 2>$null
        if ($LASTEXITCODE -eq 0 -and $text -match '(\d+\.\d+\.\d+)') {
            return $Matches[1]
        }
    }
    catch {
    }
    return $null
}

function Install-HaUv {
    param(
        [Parameter(Mandatory)] [string] $Destination,
        # Where to download from; overridable for an internal mirror.
        [string] $BaseUrl = "https://github.com/astral-sh/uv/releases/download/$script:HaUvVersion"
    )
    if ([System.IO.Path]::DirectorySeparatorChar -ne '\') {
        throw 'uv_bundle_windows_only: on this host install uv from https://docs.astral.sh/uv/ and keep it on PATH'
    }
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    $target = Join-Path $Destination 'uv.exe'
    if ((Test-Path -LiteralPath $target -PathType Leaf) -and (Get-HaUvVersion -UvPath $target) -eq $script:HaUvVersion) {
        return $target
    }

    $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    if (-not $script:HaUvArchives.ContainsKey($architecture)) {
        throw "uv_bundle_unsupported_architecture: $architecture"
    }
    $archive = $script:HaUvArchives[$architecture]
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ("ha-uv-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work -Force | Out-Null
    try {
        $zip = Join-Path $work $archive.Name
        $url = "$BaseUrl/$($archive.Name)"
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
        $digest = Get-HaUvFileDigest -FilePath $zip
        if ($digest -ne $archive.Sha256) {
            throw "uv_bundle_checksum_mismatch: $($archive.Name) is $digest, expected $($archive.Sha256)"
        }
        $extracted = Join-Path $work 'extracted'
        Expand-Archive -LiteralPath $zip -DestinationPath $extracted -Force
        $uv = Get-ChildItem -LiteralPath $extracted -Recurse -Filter 'uv.exe' | Select-Object -First 1
        if ($null -eq $uv) {
            throw "uv_bundle_missing_executable: $($archive.Name) holds no uv.exe"
        }
        Copy-Item -LiteralPath $uv.FullName -Destination $target -Force
    }
    finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ((Get-HaUvVersion -UvPath $target) -ne $script:HaUvVersion) {
        throw "uv_bundle_unusable: $target does not report version $script:HaUvVersion"
    }
    return $target
}
