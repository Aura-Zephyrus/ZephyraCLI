#!/usr/bin/env bash
# 安装 ZephyraCLI（zephyra + cargo-zephyra）到用户目录，不需要 sudo。
#
#   curl -fsSL https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.sh | bash
#   ZEPHYRA_VERSION=v0.1.0 bash install.sh
#   ZEPHYRA_INSTALL_DIR=/opt/zephyra bash install.sh
set -euo pipefail

REPO="Aura-Zephyrus/ZephyraCLI"
VERSION_INPUT="${1:-${ZEPHYRA_VERSION:-}}"
INSTALL_DIR="${ZEPHYRA_INSTALL_DIR:-$HOME/.local/bin}"

info() { printf '[zephyra] %s\n' "$*" >&2; }
die()  { printf '[zephyra] error: %s\n' "$*" >&2; exit 1; }

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
BASE="https://github.com/$REPO/releases/download/${VERSION_INPUT}"

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

# ---- PATH 提示 ----
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\n[zephyra] %s is not on PATH. Add this to your shell profile:\n' "$INSTALL_DIR"
    printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    ;;
esac
info "done. try: zephyra --version"
