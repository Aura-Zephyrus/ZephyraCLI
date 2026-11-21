# Local dev test: build a fake "new version" package, feed it to update.ps1 from a
# local directory, and verify in a sandbox INSTALL_DIR / XDG_CONFIG_HOME that the
# update re-pushes binaries, MCP config and the agent skill. Never touches real
# install dir, real agent config, or the real user PATH.
#
#   .\test-update.ps1
#   $env:ZEPHYRA_TEST_SKIP_BUILD=1; .\test-update.ps1   # reuse existing target/debug binaries
param()

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$testVersion = if ($env:ZEPHYRA_TEST_VERSION) { $env:ZEPHYRA_TEST_VERSION } else { '0.0.0-dev' }
$skipBuild = $env:ZEPHYRA_TEST_SKIP_BUILD -eq '1'
$target = 'x86_64-pc-windows-msvc'

function Info($m) { Write-Host "[test-update] $m" }
function Fail($m) { throw "[test-update] FAIL: $m" }

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("zephyra-test-update-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    $stageName = "zephyra-${testVersion}-${target}"
    $pkg = Join-Path $tmp 'pkg'
    $stage = Join-Path $pkg $stageName
    $sandboxInstall = Join-Path $tmp 'install'
    $sandboxConfig = Join-Path $tmp 'config'
    foreach ($d in @($stage, $sandboxInstall, $sandboxConfig)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }

    if (-not $skipBuild) {
        Info 'building binaries'
        & cargo build -p zephyra-cli --bins
        if ($LASTEXITCODE -ne 0) { Fail 'cargo build failed' }
    }
    $binDir = Join-Path $root 'target\debug'
    foreach ($bin in @('zephyra.exe', 'cargo-zephyra.exe')) {
        $src = Join-Path $binDir $bin
        if (-not (Test-Path $src)) { Fail "missing $src (try without ZEPHYRA_TEST_SKIP_BUILD)" }
        Copy-Item -Force $src (Join-Path $stage $bin)
    }
    Copy-Item -Recurse -Force (Join-Path $root 'skills') (Join-Path $stage 'skills')

    $archive = "${stageName}.tar.gz"
    Push-Location $pkg
    try { & tar -czf $archive $stageName; if ($LASTEXITCODE -ne 0) { Fail 'tar failed' } } finally { Pop-Location }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead((Join-Path $pkg $archive))
        try { $hash = -join ($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') }) }
        finally { $stream.Dispose() }
    } finally { $sha.Dispose() }
    Set-Content -Path (Join-Path $pkg "$archive.sha256") -Value "$hash  $archive" -Encoding ASCII
    Info "built local package: $pkg"

    # Sandbox everything: install dir, agent config, and NO real PATH changes.
    $env:ZEPHYRA_INSTALL_DIR = $sandboxInstall
    $env:XDG_CONFIG_HOME = $sandboxConfig
    $env:ZEPHYRA_NO_PATH = '1'
    foreach ($n in 1, 2) {
        Info "update pass $n"
        & (Join-Path $root 'update.ps1') -Version "v$testVersion" -BaseUrl $pkg
        if ($LASTEXITCODE -ne 0) { Fail "update pass $n failed" }
    }

    # ---- assertions ----
    if (-not (Test-Path (Join-Path $sandboxInstall 'zephyra.exe'))) { Fail 'zephyra.exe not installed' }
    if (-not (Test-Path (Join-Path $sandboxInstall 'cargo-zephyra.exe'))) { Fail 'cargo-zephyra.exe not installed' }
    $mcpBin = Join-Path $sandboxInstall "zephyra-mcp-$testVersion.exe"
    if (-not (Test-Path $mcpBin)) { Fail "versioned MCP binary not installed: $mcpBin" }

    $skill = Join-Path $sandboxConfig 'opencode\skills\zephyra-mcp'
    if (-not (Test-Path (Join-Path $skill 'SKILL.md'))) { Fail 'skill not pushed' }
    if (-not (Test-Path (Join-Path $skill 'references\tools.md'))) { Fail 'skill references not pushed' }

    $cfg = Join-Path $sandboxConfig 'opencode\opencode.jsonc'
    if (-not (Test-Path $cfg)) { Fail 'agent config not written' }
    $text = Get-Content $cfg -Raw
    if (-not $text.Contains('zephyra-mcp (managed')) { Fail 'managed marker missing' }
    $expectedPath = $mcpBin -replace '\\','/'
    if (-not $text.Contains($expectedPath)) { Fail "config does not point to versioned MCP binary: $text" }
    if (([regex]::Matches($text, '"zephyra": \{')).Count -ne 1) { Fail 'duplicate zephyra entries after two updates' }

    Info 'PASS: binaries + MCP config + skill all pushed; second update is idempotent'
}
finally {
    Remove-Item Env:ZEPHYRA_INSTALL_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:XDG_CONFIG_HOME -ErrorAction SilentlyContinue
    Remove-Item Env:ZEPHYRA_NO_PATH -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
