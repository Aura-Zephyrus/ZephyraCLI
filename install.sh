#!/usr/bin/env bash
# 安装 ZephyraCLI（zephyra + cargo-zephyra）到用户目录，不需要 sudo。
# 默认还会：
#   - 装一份 zephyra-mcp（进程名与 zephyra 不同，避免被针对后者的清理误杀）
#   - 在 OpenCode 注册 MCP server（指向 zephyra-mcp mcp）
#   - 把 skills/zephyra-mcp 拷进 OpenCode 的全局 skills 目录，让 agent 学会用法
#
#   curl -fsSL https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.sh | bash
#   ZEPHYRA_VERSION=v0.1.0 bash install.sh
#   ZEPHYRA_INSTALL_DIR=/opt/zephyra bash install.sh
#   ZEPHYRA_NO_MCP=1 bash install.sh        # 只装二进制，不接 agent
#   ZEPHYRA_UPDATE=1 bash install.sh        # 更新模式：强制刷新 MCP 配置和 skill
#   ZEPHYRA_BASE_URL=file:///tmp/pkg bash install.sh   # 从本地包安装（开发测试用）
set -euo pipefail

REPO="Aura-Zephyrus/ZephyraCLI"
VERSION_INPUT="${1:-${ZEPHYRA_VERSION:-}}"
INSTALL_DIR="${ZEPHYRA_INSTALL_DIR:-$HOME/.local/bin}"
NO_MCP="${ZEPHYRA_NO_MCP:-0}"
UPDATE="${ZEPHYRA_UPDATE:-0}"
BASE_URL_OVERRIDE="${ZEPHYRA_BASE_URL:-}"
MCP_PORT='7878'
SKILL_ID='zephyra-mcp'
MANAGED_MARKER='    // zephyra-mcp (managed by ZephyraCLI installer)'

info() { printf '[zephyra] %s\n' "$*" >&2; }
die()  { printf '[zephyra] error: %s\n' "$*" >&2; exit 1; }

# ---- agent（OpenCode）目录 ----
agent_config_dir() {
  printf '%s/opencode' "${XDG_CONFIG_HOME:-$HOME/.config}"
}
agent_config_path() {
  local dir; dir=$(agent_config_dir)
  if [[ -f "$dir/opencode.jsonc" ]]; then printf '%s\n' "$dir/opencode.jsonc"
  elif [[ -f "$dir/opencode.json" ]]; then printf '%s\n' "$dir/opencode.json"
  else printf '%s\n' "$dir/opencode.jsonc"; fi
}

install_agent_skill() {
  local source="$1" dest
  if [[ ! -f "$source/SKILL.md" ]]; then
    info "skill not found in package ($source); skipping"
    return
  fi
  dest="$(agent_config_dir)/skills/$SKILL_ID"
  mkdir -p "$dest/references"
  cp -f "$source/SKILL.md" "$dest/SKILL.md"
  if [[ -d "$source/references" ]]; then
    cp -Rf "$source/references/." "$dest/references/"
  fi
  info "installed agent skill: $dest"
}

# 我们管理的配置块固定 7 行（marker + 6 行对象）。更新时按行号替换，
# 不动用户的其它配置；老版本 installer 写的也是这个形状。
managed_block() {
  local mcp_path="$1"
  printf '%s\n' "$MANAGED_MARKER"
  printf '    "zephyra": {\n'
  printf '      "type": "local",\n'
  printf '      "command": ["%s", "mcp"],\n' "$mcp_path"
  printf '      "environment": { "ZEPHYRA_DEBUG_PORT": "%s" },\n' "$MCP_PORT"
  printf '      "codemode": false\n'
  printf '    }\n'
}

write_fresh_config() {
  local mcp_path="$1" config="$2" tmp
  tmp=$(mktemp)
  {
    printf '{\n'
    printf '  "$schema": "https://opencode.ai/config.json",\n'
    printf '  "mcp": {\n'
    printf '    "servers": {\n'
    managed_block "$mcp_path"
    printf '    }\n'
    printf '  }\n'
    printf '}\n'
  } >"$tmp"
  mv "$tmp" "$config"
}

register_agent_mcp() {
  local mcp_path="$1" config config_dir existing marker_line tmp
  config=$(agent_config_path)
  config_dir=$(dirname "$config")
  mkdir -p "$config_dir"
  existing=''
  [[ -f "$config" ]] && existing=$(cat "$config")

  marker_line=$(grep -n -m1 -F "$MANAGED_MARKER" "$config" 2>/dev/null | cut -d: -f1 || true)
  if [[ -n "$marker_line" ]]; then
    if [[ "$UPDATE" != '1' ]]; then
      info "MCP server already managed in $config; skipping (use update to refresh)"
      return
    fi
    tmp=$(mktemp)
    {
      head -n $((marker_line - 1)) "$config"
      managed_block "$mcp_path"
      tail -n +$((marker_line + 7)) "$config"
    } >"$tmp"
    mv "$tmp" "$config"
    info "refreshed MCP server entry in $config -> $mcp_path"
    return
  fi

  if [[ "$existing" == *'"zephyra"'* ]]; then
    info "MCP server 'zephyra' exists but is not managed by this script; leaving it alone"
    return
  fi

  if [[ -z "${existing//[[:space:]]/}" ]]; then
    write_fresh_config "$mcp_path" "$config"
    info "registered MCP server in $config"
    return
  fi

  # 已有配置不乱改（JSONC 可能有注释）；交给 opencode CLI，它只动 mcp.servers。
  if command -v opencode >/dev/null 2>&1; then
    opencode mcp add zephyra --global --env "ZEPHYRA_DEBUG_PORT=$MCP_PORT" -- "$mcp_path" mcp
    info "registered MCP server via 'opencode mcp add' (global)"
  else
    printf '\n[zephyra] opencode CLI not found; add this to %s manually:\n' "$config"
    printf "  mcp.servers.zephyra = { type: 'local', command: ['%s', 'mcp'], environment: { ZEPHYRA_DEBUG_PORT: '%s' } }\n" "$mcp_path" "$MCP_PORT"
  fi
}

# ---- 平台 → release 产物里的 target triple（与 release.yml 的 matrix 一致） ----
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Linux)
    case "$arch" in
      x86_64|amd64) TARGET="x86_64-unknown-linux-gnu" ;;
      *) die "unsupported Linux architecture: $arch" ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64)        TARGET="x86_64-apple-darwin" ;;
      *) die "unsupported macOS architecture: $arch" ;;
    esac
    ;;
  *) die "unsupported OS: $os (use install.ps1 on Windows)" ;;
esac
info "target: $TARGET"

# ---- 版本：参数 / 环境变量 / 最新 release ----
if [[ -z "$VERSION_INPUT" ]]; then
  command -v curl >/dev/null || die "curl is required to resolve the latest version"
  VERSION_INPUT=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' \
    | cut -d'"' -f4)
  [[ -n "$VERSION_INPUT" ]] || die "could not determine the latest version"
fi
info "version: $VERSION_INPUT"

# tag 带前导 v，资产名里用的是纯版本号。
VERSION="${VERSION_INPUT#v}"
NAME="zephyra-${VERSION}-${TARGET}"
ARCHIVE="${NAME}.tar.gz"
BASE="${BASE_URL_OVERRIDE:-https://github.com/$REPO/releases/download/${VERSION_INPUT}}"
BASE="${BASE%/}"

# ---- 下载到临时目录并校验 ----
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
command -v curl >/dev/null || die "curl is required"

info "downloading $ARCHIVE"
curl -fSL --retry 3 -o "$TMP/$ARCHIVE"  "$BASE/$ARCHIVE"
curl -fSL --retry 3 -o "$TMP/$ARCHIVE.sha256" "$BASE/$ARCHIVE.sha256"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$TMP" && sha256sum -c "$ARCHIVE.sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "$TMP" && shasum -a 256 -c "$ARCHIVE.sha256")
else
  die "need sha256sum or shasum to verify the download"
fi
info "checksum OK"

# ---- 解压安装 ----
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
[[ -d "$TMP/$NAME" ]] || die "archive layout is wrong: missing $NAME/"

mkdir -p "$INSTALL_DIR"
for bin in zephyra cargo-zephyra; do
  [[ -f "$TMP/$NAME/$bin" ]] || die "archive is missing $bin"
  install -m 0755 "$TMP/$NAME/$bin" "$INSTALL_DIR/$bin"
  info "installed $INSTALL_DIR/$bin"
done

if [[ "$NO_MCP" != '1' ]]; then
  # 更新时用带版本号的文件名：Running 的旧 MCP 进程不会锁住新文件，
  # 改配置里的路径还能让 OpenCode 自动重载连接（不产生缓存的失败态）。
  MCP_NAME="zephyra-mcp"
  [[ "$UPDATE" == '1' ]] && MCP_NAME="zephyra-mcp-${VERSION}"
  MCP_BIN="$INSTALL_DIR/$MCP_NAME"
  install -m 0755 "$INSTALL_DIR/zephyra" "$MCP_BIN"
  info "installed $MCP_BIN (dedicated MCP process name)"
  install_agent_skill "$TMP/$NAME/skills/zephyra-mcp"
  register_agent_mcp "$MCP_BIN"
  if [[ "$UPDATE" == '1' ]]; then
    # 清掉旧版 MCP 二进制（正在运行的会删不掉，忽略即可，下次再清）。
    find "$INSTALL_DIR" -maxdepth 1 -name 'zephyra-mcp' -type f -delete 2>/dev/null || true
  fi
fi

# ---- PATH 提示 ----
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\n[zephyra] %s is not on PATH. Add this to your shell profile:\n' "$INSTALL_DIR"
    printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    ;;
esac
info "done. try: zephyra --version"
