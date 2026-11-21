# 安装 ZephyraCLI（zephyra.exe + cargo-zephyra.exe）到用户目录，不需要管理员权限。
# 默认还会：
#   - 装一份 zephyra-mcp.exe（进程名与 zephyra.exe 不同，避免被针对后者的清理误杀）
#   - 在 OpenCode 注册 MCP server（指向 zephyra-mcp.exe mcp）
#   - 把 skills/zephyra-mcp 拷进 OpenCode 的全局 skills 目录，让 agent 学会用法
#
#   irm https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.ps1 | iex
#   .\install.ps1 -Version v0.1.0
#   .\install.ps1 -NoPath              # 不自动改 PATH
#   .\install.ps1 -NoMcp               # 只装二进制，不接 agent
#   .\install.ps1 -Update              # 更新：强制刷新 MCP 配置和 skill，MCP 用带版本号的文件名
#   .\install.ps1 -BaseUrl file:///C:/pkg   # 从本地包安装（开发测试用）
#   .\install.ps1 -Uninstall           # 删除二进制、MCP 配置、skill 并从用户 PATH 移除
#   $env:ZEPHYRA_INSTALL_DIR = 'C:\tools\zephyra'; .\install.ps1
param(
    [string]$Version = '',
    [string]$BaseUrl = '',
    [switch]$NoPath,
    [switch]$NoMcp,
    [switch]$Update,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'

# 环境变量兜底：update.ps1 等包装脚本用它转发参数，避免 PowerShell 数组 splat 的坑。
if (-not $Version) { $Version = $env:ZEPHYRA_VERSION }
if (-not $BaseUrl) { $BaseUrl = $env:ZEPHYRA_BASE_URL }
if ($env:ZEPHYRA_NO_PATH -eq '1') { $NoPath = $true }
if ($env:ZEPHYRA_NO_MCP -eq '1') { $NoMcp = $true }

$Repo = 'Aura-Zephyrus/ZephyraCLI'
$InstallDir = if ($env:ZEPHYRA_INSTALL_DIR) { $env:ZEPHYRA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Zephyra\bin' }
$Bins = @('zephyra.exe', 'cargo-zephyra.exe')
$SkillId = 'zephyra-mcp'
$McpPort = '7878'

function Info($msg) { Write-Host "[zephyra] $msg" }

# 不依赖 Get-FileHash（某些裁剪过的 PowerShell 环境里没有这个 cmdlet）。
function Get-Sha256Hex([string]$path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($path)
        try {
            return -join ($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') })
        } finally { $stream.Dispose() }
    } finally { $sha.Dispose() }
}

# 支持 http(s)、file:// 和本地目录路径（本地目录用于开发测试）。
function Get-PackageFile([string]$source, [string]$out) {
    if ($source -match '^[a-zA-Z]:[\\/]' -or $source.StartsWith('\\')) {
        Copy-Item -Force $source $out
    } elseif ($source -like 'file://*') {
        Copy-Item -Force ([uri]$source).LocalPath $out
    } else {
        Invoke-WebRequest -Uri $source -OutFile $out
    }
}

# ---- agent（OpenCode）目录 ----
# OpenCode V2 约定：全局配置 ~/.config/opencode，skill 在 ~/.config/opencode/skills/<id>/SKILL.md。
function Get-AgentConfigDir {
    if ($env:XDG_CONFIG_HOME) { return Join-Path $env:XDG_CONFIG_HOME 'opencode' }
    return Join-Path $env:USERPROFILE '.config\opencode'
}

function Get-AgentConfigPath {
    $dir = Get-AgentConfigDir
    foreach ($name in @('opencode.jsonc', 'opencode.json')) {
        $candidate = Join-Path $dir $name
        if (Test-Path $candidate) { return $candidate }
    }
    return (Join-Path $dir 'opencode.jsonc')
}

# 我们写的配置块带标记；卸载/更新时按标记整块替换，绝不碰用户其它配置。
$ManagedBegin = '    // zephyra-mcp (managed by ZephyraCLI installer)'
function Get-ManagedBlock([string]$mcpPath) {
    $path = $mcpPath -replace '\\','/'
    return @"
$ManagedBegin
    "zephyra": {
      "type": "local",
      "command": ["$path", "mcp"],
      "environment": { "ZEPHYRA_DEBUG_PORT": "$McpPort" },
      "codemode": false
    }
"@
}

function Install-AgentSkill([string]$skillSource) {
    if (-not (Test-Path $skillSource)) {
        Info "skill not found in package ($skillSource); skipping"
        return
    }
    $dest = Join-Path (Get-AgentConfigDir) "skills\$SkillId"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item -Force (Join-Path $skillSource 'SKILL.md') (Join-Path $dest 'SKILL.md')
    $refs = Join-Path $skillSource 'references'
    if (Test-Path $refs) {
        $destRefs = Join-Path $dest 'references'
        New-Item -ItemType Directory -Force -Path $destRefs | Out-Null
        Copy-Item -Recurse -Force (Join-Path $refs '*') $destRefs
    }
    Info "installed agent skill: $dest"
}

# 从 `"zephyra"` 的起始位置往后做括号配平，返回对象结束后的下标。
function Get-JsonObjectEnd([string]$text, [int]$start) {
    $depth = 0; $end = -1; $inString = $false; $escaped = $false
    for ($i = $text.IndexOf('{', $start); $i -lt $text.Length; $i++) {
        $ch = $text[$i]
        if ($escaped) { $escaped = $false; continue }
        if ($ch -eq '\') { $escaped = $true; continue }
        if ($ch -eq '"') { $inString = -not $inString; continue }
        if ($inString) { continue }
        if ($ch -eq '{') { $depth++ }
        elseif ($ch -eq '}') { $depth--; if ($depth -eq 0) { $end = $i + 1; break } }
    }
    if ($end -lt $text.Length -and $text[$end] -eq ',') { $end++ }
    return $end
}

function Register-AgentMcp([string]$mcpPath) {
    $configPath = Get-AgentConfigPath
    $configDir = Split-Path $configPath -Parent
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null

    $existing = if (Test-Path $configPath) { Get-Content $configPath -Raw } else { '' }
    $at = $existing.IndexOf($ManagedBegin)

    if ($at -ge 0) {
        if (-not $Update) {
            Info "MCP server already managed in $configPath; skipping (use -Update to refresh)"
            return
        }
        $start = $existing.IndexOf('"zephyra"', $at)
        $end = Get-JsonObjectEnd $existing $start
        if ($end -le $start) { throw "could not locate managed MCP entry in $configPath" }
        $newText = $existing.Substring(0, $at) + (Get-ManagedBlock $mcpPath) + $existing.Substring($end)
        Set-Content -Path $configPath -Value $newText -Encoding UTF8
        Info "refreshed MCP server entry in $configPath -> $mcpPath"
        return
    }

    if ($existing.Contains('"zephyra"')) {
        Info "MCP server 'zephyra' exists but is not managed by this script; leaving it alone"
        return
    }

    if ([string]::IsNullOrWhiteSpace($existing)) {
        # 空/新配置：直接写完整配置，带上 codemode:false（工具用裸名，和 skill 文档一致）。
        $json = @"
{
  "`$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
$(Get-ManagedBlock $mcpPath)
    }
  }
}
"@
        Set-Content -Path $configPath -Value $json -Encoding UTF8
        Info "registered MCP server in $configPath"
        return
    }

    # 已有配置不手改（JSONC 可能有注释）；交给 opencode CLI，它只动 mcp.servers。
    if (Get-Command opencode -ErrorAction SilentlyContinue) {
        & opencode mcp add zephyra --global --env "ZEPHYRA_DEBUG_PORT=$McpPort" -- $mcpPath mcp
        Info "registered MCP server via 'opencode mcp add' (global)"
    } else {
        Write-Host ''
        Write-Host "[zephyra] opencode CLI not found; add this to $configPath manually:"
        Write-Host "  mcp.servers.zephyra = { type: 'local', command: ['$mcpPath', 'mcp'], environment: { ZEPHYRA_DEBUG_PORT: '$McpPort' } }"
    }
}

function Uninstall-AgentIntegration {
    $configPath = Get-AgentConfigPath
    if (Test-Path $configPath) {
        $text = Get-Content $configPath -Raw
        $at = $text.IndexOf($ManagedBegin)
        if ($at -ge 0) {
            $start = $text.IndexOf('"zephyra"', $at)
            $end = Get-JsonObjectEnd $text $start
            if ($end -gt $start) {
                $newText = $text.Substring(0, $at) + $text.Substring($end)
                Set-Content -Path $configPath -Value $newText -Encoding UTF8
                Info "removed MCP server entry from $configPath"
            }
        } else {
            Info "MCP config not managed by this installer; remove 'zephyra' from $configPath yourself"
        }
    }
    $skillDir = Join-Path (Get-AgentConfigDir) "skills\$SkillId"
    if (Test-Path $skillDir) {
        Remove-Item -Recurse -Force $skillDir
        Info "removed agent skill: $skillDir"
    }
}

# ---- PATH 工具 ----
# 用注册表里的**原始**用户 PATH（含未展开的 %VAR%），不要用 $env:PATH，
# 否则把系统 PATH 里所有变量展开后写回用户 PATH 会造成污染。
function Get-UserPath {
    $raw = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $raw) { return '' }
    return $raw
}

function Add-ToUserPath([string]$dir) {
    $raw = Get-UserPath
    $parts = @($raw -split ';' | Where-Object { $_ })
    if ($parts | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') }) {
        Info "PATH already contains $dir"
        return $false
    }
    $new = if ($raw.TrimEnd(';')) { $raw.TrimEnd(';') + ';' + $dir } else { $dir }
    [Environment]::SetEnvironmentVariable('Path', $new, 'User')
    if (($env:PATH -split ';') -notcontains $dir) { $env:PATH = "$env:PATH;$dir" }
    return $true
}

function Remove-FromUserPath([string]$dir) {
    $raw = Get-UserPath
    if (-not $raw) { return }
    $parts = @($raw -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ine $dir.TrimEnd('\') })
    $new = $parts -join ';'
    if ($new -ne $raw) {
        [Environment]::SetEnvironmentVariable('Path', $new, 'User')
        Info "removed $dir from user PATH"
    }
}

# 通知其它程序重新读取环境变量（新开的终端总会拿到）。
function Broadcast-EnvironmentChange {
    try {
        Add-Type -Namespace ZephyraWin32 -Name NativeMethods -MemberDefinition @'
[DllImport("user32.dll", SetLastError=true, CharSet=CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, IntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out IntPtr lpdwResult);
'@ -ErrorAction Stop
        $result = [IntPtr]::Zero
        [void][ZephyraWin32.NativeMethods]::SendMessageTimeout(
            [IntPtr]0xffff, 0x1A, [IntPtr]::Zero, 'Environment', 2, 2000, [ref]$result)
    } catch {
        # 广播失败不算安装失败。
    }
}

# ---- 卸载 ----
if ($Uninstall) {
    Info "uninstalling from $InstallDir"
    if (-not $NoMcp) { Uninstall-AgentIntegration }
    $allBins = @($Bins) + @('zephyra-mcp.exe')
    if (Test-Path $InstallDir) {
        $allBins += @(Get-ChildItem $InstallDir -Filter 'zephyra-mcp-*.exe' -File | Select-Object -ExpandProperty Name)
    }
    foreach ($bin in ($allBins | Select-Object -Unique)) {
        $path = Join-Path $InstallDir $bin
        if (Test-Path $path) {
            try { Remove-Item -Force $path -ErrorAction Stop; Info "removed $path" }
            catch { Info "could not remove $path (still running?)" }
        }
    }
    Remove-FromUserPath $InstallDir
    Broadcast-EnvironmentChange
    if ((Test-Path $InstallDir) -and -not (Get-ChildItem $InstallDir -Force)) {
        Remove-Item $InstallDir
    }
    Info 'done'
    return
}

# ---- 平台 → release 产物里的 target triple（与 release.yml 的 matrix 一致） ----
switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { $Target = 'x86_64-pc-windows-msvc' }
    default { throw "unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}
Info "target: $Target"

# ---- 版本：参数 / 环境变量 / 最新 release ----
$versionInput = $Version
if (-not $versionInput) { $versionInput = $env:ZEPHYRA_VERSION }
if (-not $versionInput) {
    $versionInput = (Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest").tag_name
}
if (-not $versionInput) { throw 'could not determine the latest version' }
Info "version: $versionInput"

$plain = $versionInput.TrimStart('v')
$name = "zephyra-$plain-$Target"
$archive = "$name.tar.gz"
$base = if ($BaseUrl) { $BaseUrl.TrimEnd('/') } else { "https://github.com/$Repo/releases/download/$versionInput" }

# ---- 下载到临时目录并校验 ----
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("zephyra-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    Info "downloading $archive"
    Get-PackageFile "$base/$archive" (Join-Path $tmp $archive)
    Get-PackageFile "$base/$archive.sha256" (Join-Path $tmp "$archive.sha256")

    $expected = ((Get-Content (Join-Path $tmp "$archive.sha256") -TotalCount 1) -split '\s+')[0].ToLowerInvariant()
    $actual = Get-Sha256Hex (Join-Path $tmp $archive)
    if ($expected -ne $actual) {
        throw "checksum mismatch: expected $expected, got $actual"
    }
    Info 'checksum OK'

    if (-not (Get-Command tar -ErrorAction SilentlyContinue)) {
        throw 'tar is required to extract the archive (Windows 10 1803+ includes it)'
    }
    & tar -xzf (Join-Path $tmp $archive) -C $tmp
    if ($LASTEXITCODE -ne 0) { throw "failed to extract $archive" }

    $stage = Join-Path $tmp $name
    if (-not (Test-Path $stage)) { throw "archive layout is wrong: missing $name/" }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    foreach ($bin in $Bins) {
        $src = Join-Path $stage $bin
        if (-not (Test-Path $src)) { throw "archive is missing $bin" }
        Copy-Item -Force $src (Join-Path $InstallDir $bin)
        Info "installed $(Join-Path $InstallDir $bin)"
    }

    if (-not $NoMcp) {
        # MCP 用独立名字；更新时再带版本号：旧进程锁着旧文件也不影响新文件，
        # 改配置路径还能让 OpenCode 自动重载连接（不留缓存失败态）。
        $mcpName = if ($Update) { "zephyra-mcp-$plain.exe" } else { 'zephyra-mcp.exe' }
        $mcpPath = Join-Path $InstallDir $mcpName
        try {
            Copy-Item -Force (Join-Path $InstallDir $Bins[0]) $mcpPath
            Info "installed $mcpPath (dedicated MCP process name)"
        } catch {
            Info "could not create $mcpPath ($_); using $($Bins[0]) for MCP instead"
            $mcpPath = Join-Path $InstallDir $Bins[0]
        }
        Install-AgentSkill (Join-Path $stage 'skills\zephyra-mcp')
        Register-AgentMcp $mcpPath
    }
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# ---- PATH ----
if ($NoPath) {
    Write-Host ''
    Write-Host "[zephyra] PATH was not changed (-NoPath). To add it manually:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$InstallDir;`$env:PATH`", 'User')"
} elseif (Add-ToUserPath $InstallDir) {
    Broadcast-EnvironmentChange
    Info "added $InstallDir to user PATH (new terminals will see it)"
}

Info 'done. try: zephyra --version'
