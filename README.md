# ZephyraCLI

ZephyraUI 的命令行工具仓：`zephyra`（含 `cargo-zephyra`）、`zstylec`、`zypuic` 的可执行文件。

## 与 ZephyraUI 主仓的关系

依赖严格单向：**本仓依赖主仓，主仓不依赖本仓**。

- 编译库（`zstylec` / `zypuic` 的 lib 部分）留在主仓 `E:\ZephyraUI`，因为主仓的 `gallery/build.rs`
  仍要在构建期调用 `zypuic::compile()` 把 `.zypui` 编成二进制产物。
- 本仓的 `zephyra-cli`、`zstylec-bin`、`zypuic-bin` 通过路径依赖引用主仓的编译库。
- 因此构建本仓需要主仓 checkout 位于兄弟目录 `../ZephyraUI`。

## 成员

| 包 | 产物 |
|---|---|
| `zephyra-cli` | `zephyra` / `cargo-zephyra`：new / check / dev / run / build / info / props / fmt / mcp / bridge / perf / debug |
| `zstylec-bin` | `zstylec`：`.zstyle` 检查与排版 |
| `zypuic-bin` | `zypuic`：`.zypui` 检查、编译与排版 |

## 门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --no-deps --workspace
```
