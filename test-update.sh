#!/usr/bin/env bash
# 本地开发用：构造一个"新版"发布包，用 file:// 喂给 update.sh，
# 在临时 INSTALL_DIR / XDG_CONFIG_HOME 里验证更新会不会重新推送 MCP + skill。
# 绝不碰真实安装目录和真实 agent 配置。
#
#   bash test-update.sh                       # 先 cargo build，再测
#   ZEPHYRA_TEST_SKIP_BUILD=1 bash test-update.sh   # 复用已有 target/debug 二进制
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
TEST_VERSION="${ZEPHYRA_TEST_VERSION:-0.0.0-dev}"
SKIP_BUILD="${ZEPHYRA_TEST_SKIP_BUILD:-0}"

info() { printf '[test-update] %s\n' "$*" >&2; }
fail() { printf '[test-update] FAIL: %s\n' "$*" >&2; exit 1; }

os=$(uname -s); arch=$(uname -m)
case "$os" in
  Linux)  [[ "$arch" == x86_64 || "$arch" == amd64 ]] || fail "arch $arch"; TARGET='x86_64-unknown-linux-gnu' ;;
  Darwin) case "$arch" in arm64|aarch64) TARGET='aarch64-apple-darwin';; x86_64) TARGET='x86_64-apple-darwin';; *) fail "arch $arch";; esac ;;
  MINGW*|MSYS*|CYGWIN*)
    printf '[test-update] Windows detected; use .\\test-update.ps1 instead\n' >&2
    exit 0
    ;;
  *) fail "unsupported OS $os" ;;
esac

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
PKG="$TMP/pkg"; STAGE="$PKG/zephyra-${TEST_VERSION}-${TARGET}"
SANDBOX_INSTALL="$TMP/install"; SANDBOX_CONFIG="$TMP/config"
mkdir -p "$STAGE" "$PKG" "$SANDBOX_INSTALL" "$SANDBOX_CONFIG"

if [[ "$SKIP_BUILD" != '1' ]]; then
  info "building binaries"
  (cd "$ROOT" && cargo build -p zephyra-cli --bins)
fi
BIN_DIR="$ROOT/target/debug"
for bin in zephyra cargo-zephyra; do
  [[ -f "$BIN_DIR/$bin" ]] || fail "missing $BIN_DIR/$bin (try without ZEPHYRA_TEST_SKIP_BUILD)"
  cp "$BIN_DIR/$bin" "$STAGE/$bin"
done
cp -R "$ROOT/skills" "$STAGE/skills"

ARCHIVE="zephyra-${TEST_VERSION}-${TARGET}.tar.gz"
(cd "$PKG" && tar -czf "$ARCHIVE" "zephyra-${TEST_VERSION}-${TARGET}")
(cd "$PKG" && sha256sum "$ARCHIVE" > "$ARCHIVE.sha256")
info "built local package: $PKG/$ARCHIVE"

run_update() {
  ZEPHYRA_UPDATE=1 \
  ZEPHYRA_VERSION="v${TEST_VERSION}" \
  ZEPHYRA_BASE_URL="file://$PKG" \
  ZEPHYRA_INSTALL_DIR="$SANDBOX_INSTALL" \
  XDG_CONFIG_HOME="$SANDBOX_CONFIG" \
    bash "$ROOT/update.sh"
}

info "first update"
run_update >"$TMP/run1.log" 2>&1 || { cat "$TMP/run1.log" >&2; fail "first update exited nonzero"; }
info "second update (idempotency)"
run_update >"$TMP/run2.log" 2>&1 || { cat "$TMP/run2.log" >&2; fail "second update exited nonzero"; }

# ---- 断言 ----
[[ -x "$SANDBOX_INSTALL/zephyra" ]] || fail "zephyra not installed"
[[ -x "$SANDBOX_INSTALL/cargo-zephyra" ]] || fail "cargo-zephyra not installed"
MCP_BIN="$SANDBOX_INSTALL/zephyra-mcp-${TEST_VERSION}"
[[ -x "$MCP_BIN" ]] || fail "versioned MCP binary not installed: $MCP_BIN"

SKILL="$SANDBOX_CONFIG/opencode/skills/zephyra-mcp"
[[ -f "$SKILL/SKILL.md" ]] || fail "skill not pushed"
[[ -f "$SKILL/references/tools.md" ]] || fail "skill references not pushed"

CFG="$SANDBOX_CONFIG/opencode/opencode.jsonc"
[[ -f "$CFG" ]] || fail "agent config not written"
grep -q 'zephyra-mcp (managed' "$CFG" || fail "managed marker missing"
JSON_PATH=$(printf '%s' "$MCP_BIN" | sed 's/\//\\\//g')
grep -q "\"command\": \[\"$JSON_PATH\", \"mcp\"\]" "$CFG" || fail "config does not point to versioned MCP binary: $(cat "$CFG")"
[[ $(grep -c '"zephyra": {' "$CFG") -eq 1 ]] || fail "duplicate zephyra entries after two updates"

info "PASS: binaries + MCP config + skill all pushed; second update is idempotent"
info "sandbox cleaned on exit"
