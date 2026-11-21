<div align="center">
   <h1>ZephyraCLI</h1>
   <p>一个与AI深度融合的脚手架CLI</p>
</div>



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
安装后得到 `zephyra`、`cargo-zephyra`（供 `cargo zephyra dev` 使用）以及
**`zephyra-mcp`**（MCP 专用进程名，避免被针对 `zephyra` 的清理误杀）。

**默认还会接入 agent**：把 `skills/zephyra-mcp` 装进 OpenCode 全局 skills 目录
（`~/.config/opencode/skills/zephyra-mcp/`），并注册 MCP server（端口基址 7878）。
`ZEPHYRA_NO_MCP=1` / `-NoMcp` 可跳过；已有配置不会覆盖，只会在空配置时直接写入、
或在装了 `opencode` CLI 时用 `opencode mcp add --global` 追加。

**Windows 细节**：默认自动把安装目录写入**用户 PATH**（不动系统 PATH，也不需要管理员），
并通知当前会话；`-NoPath` 可跳过。`-Uninstall` 会删除二进制、MCP 配置和 skill 并从用户 PATH 移除：

```powershell
.\install.ps1 -Uninstall          # 或： install.cmd -Uninstall
```

## 更新

`update` 脚本在覆盖二进制的同时**强制重新推送 MCP 配置和 agent skill**。MCP 会用带版本号
的进程名（如 `zephyra-mcp-0.1.0.exe`），这样旧进程还活着也不会锁住新文件；改配置路径会让
OpenCode 自动重载 MCP 连接，不会留下缓存的失败态。

```bash
curl -fsSL https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/update.sh | bash
# 或本地包：ZEPHYRA_BASE_URL=file:///tmp/pkg bash update.sh
```

```powershell
irm https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/update.ps1 | iex
# 或本地包：.\update.ps1 -BaseUrl file:///C:/pkg
```

本地开发自测（构造假包，在临时目录里跑两遍更新，验证二进制 / MCP / skill 都到位且幂等；
不碰真实安装目录、agent 配置和用户 PATH）：

```bash
bash test-update.sh          # Linux / macOS
```

```powershell
.\test-update.ps1            # Windows（默认先 cargo build）
$env:ZEPHYRA_TEST_SKIP_BUILD=1; .\test-update.ps1   # 复用现有 target/debug 二进制
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
