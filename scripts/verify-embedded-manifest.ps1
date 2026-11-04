#requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ExecutablePath,
    [Parameter(Mandatory)][string]$MtPath,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$stage = 'setup'
$manifestPath = Join-Path ([IO.Path]::GetFullPath($OutputDirectory)) 'embedded.manifest'
try {
    $exe = (Get-Item -LiteralPath $ExecutablePath).FullName
    $mt = Get-Item -LiteralPath $MtPath
    if ($mt.PSIsContainer -or $mt.Name -ine 'mt.exe') { throw 'MtPath must identify SDK mt.exe' }
    if (Test-Path -LiteralPath $OutputDirectory) { throw 'OutputDirectory must be new' }
    $null = New-Item -ItemType Directory -Path $OutputDirectory
    $stage = 'extract resource #1'
    & $mt.FullName -nologo "-inputresource:$exe;#1" "-out:$manifestPath" *> (Join-Path $OutputDirectory 'extract.log')
    $code = $LASTEXITCODE
    if ($code -ne 0) { throw "mt.exe exit code $code" }
    if (-not (Test-Path -LiteralPath $manifestPath) -or (Get-Item -LiteralPath $manifestPath).Length -eq 0) { throw 'Extraction produced no manifest' }
    $stage = 'validate manifest'
    & $mt.FullName -nologo -manifest $manifestPath -validate_manifest *> (Join-Path $OutputDirectory 'validate.log')
    $code = $LASTEXITCODE
    if ($code -ne 0) { throw "mt.exe exit code $code" }
    $stage = 'assert DPI declaration'
    $settings = [Xml.XmlReaderSettings]::new()
    $settings.DtdProcessing = [Xml.DtdProcessing]::Prohibit
    $settings.XmlResolver = $null
    $reader = [Xml.XmlReader]::Create($manifestPath, $settings)
    try {
        $xml = [Xml.XmlDocument]::new()
        $xml.XmlResolver = $null
        $xml.Load($reader)
    } finally { $reader.Dispose() }
    $ns = [Xml.XmlNamespaceManager]::new($xml.NameTable)
    $ns.AddNamespace('asm', 'urn:schemas-microsoft-com:asm.v1')
    $ns.AddNamespace('v3', 'urn:schemas-microsoft-com:asm.v3')
    $ns.AddNamespace('ws2016', 'http://schemas.microsoft.com/SMI/2016/WindowsSettings')
    $dpi = $xml.SelectNodes('/asm:assembly/v3:application/v3:windowsSettings/ws2016:dpiAwareness', $ns)
    if ($dpi.Count -ne 1 -or $dpi[0].InnerText.Trim() -cne 'PerMonitorV2') { throw 'Expected exactly one PerMonitorV2 DPI declaration' }
    Write-Host "Embedded manifest verified: $exe -> $manifestPath"
    exit 0
} catch {
    Write-Error "Manifest verification failed at ${stage}: executable=$ExecutablePath; output=$manifestPath; $($_.Exception.Message)" -ErrorAction Continue
    exit 1
}
