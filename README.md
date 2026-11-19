# ZephyraCLI

ZephyraUI 的命令行工具仓：`zephyra`（含 `cargo-zephyra`）、`zstylec`、`zypuic` 的可执行文件。

## 与 ZephyraUI 主仓的关系

依赖严格单向：**本仓依赖主仓，主仓不依赖本仓**。

- 编译库（`zstyle-compile` / `zypui-compile`）留在主仓 `E:\ZephyraUI`，因为独立仓的
  gallery `build.rs` 仍要在构建期调用 `zypui_compile::compile()` 把 `.zypui` 编成二进制产物。
- 本仓的 `zephyra-cli`、`zstylec`、`zypuic` 通过路径依赖引用主仓的编译库。
- 因此构建本仓需要主仓 checkout 位于兄弟目录 `../ZephyraUI`。

## 成员

| 包 | 产物 |
|---|---|
| `zephyra-cli` | `zephyra` / `cargo-zephyra`：new / check / dev / run / build / info / props / fmt / mcp / bridge / perf / debug |
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
