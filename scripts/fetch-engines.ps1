<#
.SYNOPSIS
    Download the bundled CLI engines (ffmpeg, pandoc, pdftoppm) into bin/.

.DESCRIPTION
    FileAlchemy ships three third-party executables. They are *build inputs*,
    not runtime downloads: the finished app never touches the network.

    Pinned versions live in engines.lock.json. Every archive is verified against
    the SHA256 recorded there before anything is extracted. If a hash is missing
    the script extracts nothing and prints the hash it computed, so a maintainer
    can record it deliberately (never silently).

.PARAMETER Force
    Re-download even if the executables already exist.

.PARAMETER Update
    Rewrite engines.lock.json with the hashes that were observed. Use this only
    when bumping a version on purpose.
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$Update
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path -Parent $PSScriptRoot
$binDir = Join-Path $root 'bin'
$cacheDir = Join-Path $root '.cache/engines'
$lockFile = Join-Path $root 'engines.lock.json'

if (-not (Test-Path $binDir)) { New-Item -ItemType Directory -Path $binDir | Out-Null }
if (-not (Test-Path $cacheDir)) { New-Item -ItemType Directory -Path $cacheDir | Out-Null }

if (-not (Test-Path $lockFile)) {
    throw "engines.lock.json not found at $lockFile"
}
$lock = Get-Content $lockFile -Raw | ConvertFrom-Json

function Get-FileSha256([string]$Path) {
    return (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Save-Engine([pscustomobject]$Engine) {
    $target = Join-Path $binDir $Engine.output
    if ((Test-Path $target) -and -not $Force) {
        Write-Host "  [skip] $($Engine.output) already present"
        return
    }

    $archive = Join-Path $cacheDir $Engine.archive
    if (-not (Test-Path $archive)) {
        Write-Host "  [get ] $($Engine.url)"
        Invoke-WebRequest -Uri $Engine.url -OutFile $archive -UseBasicParsing
    }

    $actual = Get-FileSha256 $archive
    if ([string]::IsNullOrWhiteSpace($Engine.sha256)) {
        Write-Warning "No SHA256 recorded for $($Engine.output). Computed: $actual"
        if ($Update) {
            $Engine.sha256 = $actual
        } else {
            throw "Refusing to install $($Engine.output) without a recorded hash. Re-run with -Update if $actual is the release you intend to pin."
        }
    } elseif ($actual -ne $Engine.sha256.ToLowerInvariant()) {
        Remove-Item $archive -Force
        throw "SHA256 mismatch for $($Engine.url)`n  expected $($Engine.sha256)`n  actual   $actual"
    }

    $extractDir = Join-Path $cacheDir ($Engine.output + '.d')
    if (Test-Path $extractDir) { Remove-Item $extractDir -Recurse -Force }
    Expand-Archive -Path $archive -DestinationPath $extractDir -Force

    $found = Get-ChildItem -Path $extractDir -Recurse -Filter $Engine.member -File | Select-Object -First 1
    if (-not $found) {
        throw "$($Engine.member) was not found inside $($Engine.archive)"
    }
    Copy-Item $found.FullName $target -Force
    Write-Host "  [ ok ] $($Engine.output) <- $($found.FullName)"

    # Poppler ships DLLs beside the executable; they must travel together.
    if ($Engine.copy_siblings) {
        Get-ChildItem -Path $found.DirectoryName -File |
            Where-Object { $_.FullName -ne $found.FullName } |
            ForEach-Object { Copy-Item $_.FullName (Join-Path $binDir $_.Name) -Force }
    }
}

Write-Host "Fetching FileAlchemy engines into $binDir"
foreach ($engine in $lock.engines) {
    Save-Engine $engine
}

if ($Update) {
    $lock.updated = (Get-Date).ToString('yyyy-MM-dd')
    $lock | ConvertTo-Json -Depth 6 | Set-Content -Path $lockFile -Encoding utf8
    Write-Host "engines.lock.json updated."
}

Write-Host ''
Write-Host 'Installed engines:'
Get-ChildItem $binDir -Filter *.exe | ForEach-Object {
    $version = $null
    try {
        $version = (& $_.FullName -version 2>&1 | Select-Object -First 1)
    } catch { }
    if ($version -is [array]) { $version = $version[0] }
    Write-Host ("  {0,-14} {1,10:N0} bytes   {2}" -f $_.Name, $_.Length, $version)
}
Write-Host ''
Write-Host 'Third-party licences: see bin/README.md. The app itself performs no network access.'
