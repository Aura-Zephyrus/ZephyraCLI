#!/usr/bin/env bash
# 更新 ZephyraCLI：重置安装流程并强制重新推送 MCP 配置与 agent skill。
# 除了 install.sh 的环境变量，还支持 ZEPHYRA_BASE_URL 从本地包更新（开发测试用）。
#
#   curl -fsSL https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/update.sh | bash
#   ZEPHYRA_VERSION=v0.1.0 bash update.sh
#   ZEPHYRA_BASE_URL=file:///tmp/pkg bash update.sh
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
if [[ -f "$SCRIPT_DIR/install.sh" ]]; then
  INSTALLER="$SCRIPT_DIR/install.sh"
else
  TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
  INSTALLER="$TMP/install.sh"
  curl -fsSL https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.sh -o "$INSTALLER"
fi

export ZEPHYRA_UPDATE=1
exec bash "$INSTALLER" "$@"
