# {{NAME}}

由 `zephyra new` 生成的 ZephyraUI 示例工程。

## 运行

```text
zephyra dev
# 或直接 cargo run（默认后端按平台选：Windows → d3d12，其它 → vulkan）
```

切换后端：

```text
cargo run --no-default-features --features d3d11      # Windows only
cargo run --no-default-features --features vulkan     # Windows / Linux
```

## 项目文件

- `src/main.rs`：窗口宿主入口和 winit 事件循环接线（三后端共用外壳）。
- `frontend/app.rs`：可交互示例 View，展示应用状态、稳定节点 key 和元素结构。
- `frontend/app.zstyle`：外观和布局样式；源码描述元素与状态，不硬编码颜色或字号。
- `zephyra.toml`：应用名、初始物理像素尺寸和样式表入口。
- `ZephyraUI.json`：窗口选项。`dev` / `build` 会把该配置嵌入应用构建。
- `Cargo.toml`：后端 feature 与本机 ZephyraUI 源码依赖路径；迁移工程时需修改路径。
- `.opencode/`：项目级 MCP 配置与 agent skill，`zephyra dev` 起的应用会被 agent 自动发现。

## 常用开发命令

```text
zephyra dev --styles frontend
zephyra check frontend/app.zstyle
zephyra props text
```

## 接入 AI Agent

项目级 `.opencode/opencode.jsonc` 已注册 Zephyra MCP server（stdio，端口基址 7878），
并把 `zephyra-mcp` skill 放进 `.opencode/skills/`。用 OpenCode 打开本目录即可直接操控窗口。
不需要时可用 `zephyra new {{NAME}} --no-agent` 或 `zephyra init --no-agent` 跳过。

## 扩展示例

1. 在 `frontend/app.rs` 的 `App::on_input` 读取 `InputState`，把输入转换为应用自己的状态。
2. 在 `App::view` 根据状态构造 `Element` 树；交互节点使用固定 `.with_key(...)`。
3. 在 `frontend/app.zstyle` 为节点声明样式；属性名可用 `zephyra props` 查询。
4. 窗口只能在 winit `ApplicationHandler::resumed` 中创建；`window_event` 必须转发给宿主。
5. 布局和鼠标坐标使用物理像素。有效像素断点需通过 `InputState.scale` 换算。
