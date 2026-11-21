# Update ZephyraCLI: re-run install and force-refresh the MCP config and agent skill.
# Same parameters as install.ps1 (-Version / -BaseUrl / -NoPath / -NoMcp); -Update is implied.
#
#   irm https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/update.ps1 | iex
#   .\update.ps1 -Version v0.1.0
#   .\update.ps1 -BaseUrl file:///C:/pkg
param(
    [string]$Version = '',
    [string]$BaseUrl = '',
    [switch]$NoPath,
    [switch]$NoMcp
)

$ErrorActionPreference = 'Stop'
$local = Join-Path $PSScriptRoot 'install.ps1'
if (Test-Path $local) {
    $installer = $local
} else {
    $installer = Join-Path ([System.IO.Path]::GetTempPath()) ("zephyra-install-" + [guid]::NewGuid().ToString('N') + ".ps1")
    Invoke-WebRequest -Uri 'https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.ps1' -OutFile $installer
}

# Forward via environment variables (avoids PowerShell 5.1 array-splat binding pitfalls).
if ($Version) { $env:ZEPHYRA_VERSION = $Version }
if ($BaseUrl) { $env:ZEPHYRA_BASE_URL = $BaseUrl }
if ($NoPath)  { $env:ZEPHYRA_NO_PATH = '1' }
if ($NoMcp)   { $env:ZEPHYRA_NO_MCP = '1' }

# Forward the skip switches explicitly as well (env fallback covers direct callers).
if ($NoPath -or $env:ZEPHYRA_NO_PATH -eq '1') {
    if ($NoMcp -or $env:ZEPHYRA_NO_MCP -eq '1') { & $installer -Update -NoPath -NoMcp }
    else { & $installer -Update -NoPath }
} elseif ($NoMcp -or $env:ZEPHYRA_NO_MCP -eq '1') {
    & $installer -Update -NoMcp
} else {
    & $installer -Update
}
exit $LASTEXITCODE
