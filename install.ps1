# 安装 ZephyraCLI（zephyra.exe + cargo-zephyra.exe）到用户目录，不需要管理员权限。
#
#   irm https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.ps1 | iex
#   .\install.ps1 -Version v0.1.0
#   .\install.ps1 -NoPath              # 不自动改 PATH
#   .\install.ps1 -Uninstall           # 删除二进制并从用户 PATH 移除
#   $env:ZEPHYRA_INSTALL_DIR = 'C:\tools\zephyra'; .\install.ps1
param(
    [string]$Version = '',
    [switch]$NoPath,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'

$Repo = 'Aura-Zephyrus/ZephyraCLI'
$InstallDir = if ($env:ZEPHYRA_INSTALL_DIR) { $env:ZEPHYRA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Zephyra\bin' }
$Bins = @('zephyra.exe', 'cargo-zephyra.exe')

function Info($msg) { Write-Host "[zephyra] $msg" }

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
    foreach ($bin in $Bins) {
        $path = Join-Path $InstallDir $bin
        if (Test-Path $path) {
            Remove-Item -Force $path
            Info "removed $path"
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
$base = "https://github.com/$Repo/releases/download/$versionInput"

# ---- 下载到临时目录并校验 ----
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("zephyra-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    Info "downloading $archive"
    Invoke-WebRequest -Uri "$base/$archive" -OutFile (Join-Path $tmp $archive)
    Invoke-WebRequest -Uri "$base/$archive.sha256" -OutFile (Join-Path $tmp "$archive.sha256")

    $expected = ((Get-Content (Join-Path $tmp "$archive.sha256") -TotalCount 1) -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash (Join-Path $tmp $archive) -Algorithm SHA256).Hash.ToLowerInvariant()
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
