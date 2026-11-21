#!/usr/bin/env bash
# MCP 端到端冒烟：不启应用，验证 stdio 握手、工具/资源清单与本地脚手架。
# 用法：scripts/smoke-mcp.sh <zephyra-binary>
set -euo pipefail

BIN="${1:-./target/release/zephyra}"
# 用当前目录下的临时目录，避免 Windows 上 msys 的 /tmp 与 std::env::temp_dir()
# 指向不同位置，导致「生成成功但检查路径不存在」的假失败。
WORK="../.zephyra-smoke-$$"
mkdir -p "$WORK"
OUT="$WORK/replies.jsonl"
trap 'rm -rf "$WORK"' EXIT

fail() { printf '[smoke-mcp] FAIL: %s\n' "$*" >&2; exit 1; }

[ -x "$BIN" ] || fail "binary not found or not executable: $BIN"

# 发四条协议消息后 stdin 关闭，server 应各自回一行、然后正常退出。
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"ci-smoke","version":"1"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"resources/list","params":{}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":4,"method":"ping","params":{}}'
} | timeout 30 "$BIN" mcp >"$OUT" 2>/dev/null || fail "mcp server exited non-zero"

for id in 1 2 3 4; do
  grep -q "\"id\":$id" "$OUT" || fail "missing reply to id=$id: $(cat "$OUT")"
done
grep -q 'list_windows' "$OUT" || fail 'tools/list is missing list_windows'
grep -q 'outputSchema' "$OUT" || fail 'tools/list is missing outputSchema'
grep -q 'zephyra://skill' "$OUT" || fail 'resources/list is missing the skill resource'

# 生成工程 + 校验：模板、脚本和 CLI 共用的那条路径。
DIR="$WORK/project"
mkdir -p "$DIR"
# --check 验证生成工程能编译；--no-default-features 跳过渲染后端（后端归主仓，
# 其编译状态由主仓负责），只验证 core/style/compiler 依赖链。
# cargo 由 CLI 自身通过 cargo_path() 定位，绕开 Git Bash PATH 缺 cargo 的问题。
"$BIN" new smoke --path "$DIR" --no-agent --check --no-default-features >/dev/null
[ -f "$DIR/smoke/Cargo.toml" ] || fail 'scaffold did not write Cargo.toml'
[ -f "$DIR/smoke/frontend/app.zstyle" ] || fail 'scaffold did not write the stylesheet'

MANIFEST=$("$BIN" ai)
printf '%s' "$MANIFEST" | grep -q '"commands"' || fail 'ai manifest has no commands'
printf '%s' "$MANIFEST" | grep -q '"mcp"' || fail 'ai manifest is missing mcp'

echo '[smoke-mcp] PASS'
