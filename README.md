# ZephyraCLI

ZephyraUI 的命令行工具仓：`zephyra`（含 `cargo-zephyra`）、`zstylec`、`zypuic` 的可执行文件。

## 安装

从 GitHub Releases 安装预编译产物（脚本会校验 SHA256，默认装到用户目录，不需要 sudo）：

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.sh | bash
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.ps1 | iex
```

```cmd
:: Windows：执行策略受限时用 .cmd 启动器（自动绕过，参数透传）
curl -fsSL https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.cmd -o install.cmd && install.cmd
```

可选项：`ZEPHYRA_VERSION=v0.1.0` 指定版本，`ZEPHYRA_INSTALL_DIR=<dir>` 指定安装目录。
安装后得到 `zephyra` 与 `cargo-zephyra` 两个可执行文件（后者供 `cargo zephyra dev` 使用）。

**Windows 细节**：默认自动把安装目录写入**用户 PATH**（不动系统 PATH，也不需要管理员），
并通知当前会话；`-NoPath` 可跳过。`-Uninstall` 会删除两个二进制并从用户 PATH 移除：

```powershell
.\install.ps1 -Uninstall          # 或： install.cmd -Uninstall
```

从源码构建需要主仓 checkout 位于兄弟目录 `../ZephyraUI`（见下）。CI / Release 的
GitHub Actions 已自动并排 checkout 两个仓；若主仓设为私有，需要给 Action 配一个
有读权限的 token。

## 与 ZephyraUI 主仓的关系

依赖严格单向：**本仓依赖主仓，主仓不依赖本仓**。

- 编译库（`zstyle-compile` / `zypui-compile`）留在主仓 `E:\ZephyraUI`，因为独立仓的
  gallery `build.rs` 仍要在构建期调用 `zypui_compile::compile()` 把 `.zypui` 编成二进制产物。
- 本仓的 `zephyra-cli`、`zstylec`、`zypuic` 通过路径依赖引用主仓的编译库。
- 因此构建本仓需要主仓 checkout 位于兄弟目录 `../ZephyraUI`。

## 成员

| 包 | 产物 |
|---|---|
| `zephyra-cli` | `zephyra` / `cargo-zephyra`：new / check / dev / build / info / props / fmt / mcp / bridge / perf / debug |
| `zstylec` | `zstylec`：`.zstyle` 检查与排版（lib 转发主仓 `zstyle-compile`） |
| `zypuic` | `zypuic`：`.zypui` 检查、编译与排版（lib 转发主仓 `zypui-compile`） |

## 随仓携带的技能

`skills/zephyra-mcp/` 是配给 AI 客户端的 ZephyraUI MCP 操作技能（主文档 `SKILL.md`
+ `references/` 三份）。它操作的是本仓 `zephyra mcp` 启动的 server，因此随本仓维护。

## 门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --no-deps --workspace
```
