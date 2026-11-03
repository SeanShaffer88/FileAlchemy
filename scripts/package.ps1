<#
.SYNOPSIS
    Produce every release artifact for FileAlchemy.

.DESCRIPTION
    Builds (or reuses) the release binary, verifies it is self-contained, then
    emits:

        dist/FileAlchemy-v{VERSION}-x64-Setup.exe     (Inno Setup, per-user)
        dist/FileAlchemy-v{VERSION}-x64-Setup.msi     (WiX v4, per-user)
        dist/FileAlchemy-v{VERSION}-x64-Portable.zip  (exe + bin + empty data/)
        dist/SHA256SUMS.txt

    Nothing here touches the network except optional tool discovery; the engines
    come from scripts/fetch-engines.ps1 and are pinned by hash.

.PARAMETER SkipBuild
    Package whatever is already in target/release.

.PARAMETER SkipInstallers
    Only produce the portable zip (useful on a machine without WiX/Inno).
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$SkipInstallers
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw 'Could not read the version from Cargo.toml' }

Write-Host "FileAlchemy $version" -ForegroundColor Cyan

# ---------------------------------------------------------------- build ------
if (-not $SkipBuild) {
    Push-Location $root
    try {
        cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    } finally { Pop-Location }
}

$exe = Join-Path $root 'target/release/FileAlchemy.exe'
if (-not (Test-Path $exe)) { throw "Missing $exe - run without -SkipBuild first." }

# --------------------------------------------------- verify static CRT -------
# The spec requires /MT: the shipped binary must not need VCRUNTIME140.dll.
function Test-StaticCrt([string]$Path) {
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $needles = @('VCRUNTIME140.dll', 'MSVCP140.dll', 'api-ms-win-crt-runtime')
    $ascii = [System.Text.Encoding]::ASCII.GetString($bytes)
    $found = @()
    foreach ($needle in $needles) {
        if ($ascii.Contains($needle)) { $found += $needle }
    }
    return $found
}

$runtimeImports = Test-StaticCrt $exe
if ($runtimeImports.Count -gt 0) {
    throw "Static CRT check failed: FileAlchemy.exe still imports $($runtimeImports -join ', '). Check .cargo/config.toml."
}
Write-Host '  [ ok ] static CRT verified (no VCRUNTIME140 / MSVCP140 imports)'

# Also assert the manifest actually made it into the binary.
$exeAscii = [System.Text.Encoding]::Unicode.GetString([System.IO.File]::ReadAllBytes($exe))
if (-not $exeAscii.Contains('PerMonitorV2')) {
    Write-Warning 'Could not find PerMonitorV2 in the embedded manifest. Check build.rs / app.manifest.'
} else {
    Write-Host '  [ ok ] Per-Monitor V2 manifest embedded'
}

# ------------------------------------------------------------- layout --------
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
$portable = Join-Path $dist 'portable'
New-Item -ItemType Directory -Path $portable | Out-Null
New-Item -ItemType Directory -Path (Join-Path $portable 'bin') | Out-Null
New-Item -ItemType Directory -Path (Join-Path $portable 'data/temp') | Out-Null

Copy-Item $exe (Join-Path $portable 'FileAlchemy.exe')

$binDir = Join-Path $root 'bin'
if (-not (Test-Path $binDir)) { throw 'bin/ is missing - run scripts/fetch-engines.ps1 first.' }
Get-ChildItem $binDir -File | Where-Object { $_.Extension -in '.exe', '.dll' } |
    ForEach-Object { Copy-Item $_.FullName (Join-Path $portable 'bin' $_.Name) }
Copy-Item (Join-Path $binDir 'README.md') (Join-Path $portable 'bin/README.md') -ErrorAction SilentlyContinue
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $portable 'LICENSE.txt')

# The portable sentinel makes the build unambiguously portable, even if a user
# later moves the folder somewhere that already has a data\ directory.
Set-Content -Path (Join-Path $portable 'portable.dat') -Value "FileAlchemy portable build $version" -Encoding ascii

# ------------------------------------------------------------- zip -----------
$zipPath = Join-Path $dist "FileAlchemy-v$version-x64-Portable.zip"
Compress-Archive -Path (Join-Path $portable '*') -DestinationPath $zipPath -CompressionLevel Optimal
Write-Host "  [ ok ] $zipPath"

# ------------------------------------------------------- installers ----------
if (-not $SkipInstallers) {
    $iscc = Get-Command 'iscc.exe' -ErrorAction SilentlyContinue
    if (-not $iscc) {
        $candidates = @(
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
        )
        $iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    if ($iscc) {
        & $iscc "/DAppVersion=$version" "/DSourceRoot=$portable" "/DOutputDir=$dist" (Join-Path $root 'installer/filealchemy.iss')
        if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed' }
        Write-Host "  [ ok ] FileAlchemy-v$version-x64-Setup.exe"
    } else {
        Write-Warning 'Inno Setup not found - skipping Setup.exe (install with: choco install innosetup)'
    }

    $wix = Get-Command 'wix.exe' -ErrorAction SilentlyContinue
    if ($wix) {
        Push-Location (Join-Path $root 'installer')
        try {
            & wix build filealchemy.wxs -arch x64 -d "AppVersion=$version" -d "SourceRoot=$portable" -o (Join-Path $dist "FileAlchemy-v$version-x64-Setup.msi")
            if ($LASTEXITCODE -ne 0) { throw 'WiX build failed' }
            Write-Host "  [ ok ] FileAlchemy-v$version-x64-Setup.msi"
        } finally { Pop-Location }
    } else {
        Write-Warning 'WiX v4 not found - skipping MSI (install with: dotnet tool install --global wix)'
    }
}

# ------------------------------------------------------------ hashes ---------
$sums = Join-Path $dist 'SHA256SUMS.txt'
$lines = Get-ChildItem $dist -File |
    Where-Object { $_.Name -ne 'SHA256SUMS.txt' } |
    Sort-Object Name |
    ForEach-Object { "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name }
Set-Content -Path $sums -Value ($lines -join "`n") -Encoding ascii
Write-Host "  [ ok ] $sums"
Write-Host ''
Get-Content $sums
