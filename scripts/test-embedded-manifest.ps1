#requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$MtPath,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$mt = (Get-Item -LiteralPath $MtPath).FullName
$cl = (Get-Command cl.exe -CommandType Application).Source
$link = (Get-Command link.exe -CommandType Application).Source
if (Test-Path -LiteralPath $OutputDirectory) { throw 'OutputDirectory must be new' }
$root = (New-Item -ItemType Directory -Path $OutputDirectory).FullName
$source = Join-Path $root 'fixture.c'
$object = Join-Path $root 'fixture.obj'
$base = Join-Path $root 'marker-only.exe'
Set-Content -LiteralPath $source -Encoding utf8 'volatile const char marker[] = "PerMonitorV2"; int mainCRTStartup(void) { return marker[0]; }'
& $cl /nologo /c /GS- "/Fo$object" $source *> (Join-Path $root 'compile.log')
if ($LASTEXITCODE -ne 0) { throw "cl.exe failed: $LASTEXITCODE" }
& $link /NOLOGO /ENTRY:mainCRTStartup /SUBSYSTEM:CONSOLE /NODEFAULTLIB /MANIFEST:NO "/OUT:$base" $object *> (Join-Path $root 'link.log')
if ($LASTEXITCODE -ne 0) { throw "link.exe failed: $LASTEXITCODE" }
$correct = '<dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>'
$cases = [ordered]@{
    utf8 = $correct
    whitespace = $correct.Replace('>PerMonitorV2<', "> `n PerMonitorV2 `t <")
    system = $correct.Replace('PerMonitorV2', 'System')
    missing = ''
    namespace = $correct.Replace('2016', '2099')
    comment = "<!-- $correct -->"
    duplicate = "$correct$correct"
    case = $correct.Replace('PerMonitorV2', 'permonitorv2')
}
$inputs = [ordered]@{ 'marker-only' = $base }
foreach ($name in $cases.Keys) {
    $manifest = Join-Path $root "$name.manifest"
    $exe = Join-Path $root "$name.exe"
    Set-Content -LiteralPath $manifest -Encoding utf8 ('<?xml version="1.0" encoding="UTF-8"?><assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"><assemblyIdentity name="ManifestFixture" version="1.0.0.0" type="win32"/><application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>' + $cases[$name] + '</windowsSettings></application></assembly>')
    Copy-Item -LiteralPath $base -Destination $exe
    & $mt -nologo -manifest $manifest "-outputresource:$exe;#1" *> (Join-Path $root "$name-embed.log")
    if ($LASTEXITCODE -ne 0) { throw "Embedding $name failed: $LASTEXITCODE" }
    $inputs[$name] = $exe
}
$nonPe = Join-Path $root 'non-pe.exe'
Set-Content -LiteralPath $nonPe 'PerMonitorV2'
$inputs['non-pe'] = $nonPe
$inputs['invalid-mt'] = $inputs['utf8']
foreach ($name in $inputs.Keys) {
    $tool = if ($name -eq 'invalid-mt') { Join-Path $root 'absent/mt.exe' } else { $mt }
    & (Join-Path $PSHOME 'pwsh.exe') -NoProfile -File (Join-Path $PSScriptRoot 'verify-embedded-manifest.ps1') -ExecutablePath $inputs[$name] -MtPath $tool -OutputDirectory (Join-Path $root "$name-result") *> (Join-Path $root "$name-verifier.log")
    $code = $LASTEXITCODE
    $expectedPass = $name -in @('utf8', 'whitespace')
    if (($code -eq 0) -ne $expectedPass) { throw "Unexpected verifier exit code $code for $name (expected pass: $expectedPass)" }
    Write-Host "PASS $name (verifier exit $code)"
}
Write-Host "All $($inputs.Count) manifest regression cases passed; fixtures were never executed. Evidence: $root"
