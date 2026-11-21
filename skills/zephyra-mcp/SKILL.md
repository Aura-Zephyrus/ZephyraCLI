---
name: zephyra-mcp
description: 'Operate and inspect a running ZephyraUI application through the ZephyraUI MCP server. Use when reading UI layout or node properties of a running ZephyraUI app, driving it with hover / click / key / scroll / drag, capturing screenshots, collecting frame performance profiles or resource telemetry, or troubleshooting the MCP connection.'
argument-hint: 'Describe the running app and the interaction or diagnostics you need'
---

# ZephyraUI MCP 操作

用 MCP 工具检查/操控**本机运行中的** ZephyraUI 窗口。默认用文本布局理解界面，截图只在需要像素证据时用；性能采集按需限时。结论必须有读回状态佐证。

## 选对入口

- **运行中的应用**（读布局/注入输入/截图/性能）→ MCP 工具。
- **工程级事情**（建项目、校验、排版、构建、跑 dev、查属性/版本）→ shell 跑 `zephyra ...`。MCP 里**没有** `build` / `check` / `fmt` / `dev` 工具，别找。
- 不确定参数 → `zephyra ai <command>` 或 `zephyra --help`。
- 读源码语义 → `docs/developer/`、`docs/dev-log/`，不是 MCP 的事。

CLI 输出：加全局 `--json` 得 `{"ok","exitCode","stdout":[...],"stderr":[...]}`；退出码 `0` 成功 / `1` 操作失败 / `2` 用法错 / `3` 环境缺失，`stderr` 是错误。

## 连接与多窗口

应用监听 `ZEPHYRA_DEBUG_PORT` 基址（默认 7878），每个窗口占 `基址 + window id`；MCP 进程连出去，多个 agent 不抢端口。连接/外设由安装器配置，通常无需手动设置。

- 桥**默认关闭**：设了 `ZEPHYRA_DEBUG_PORT=<端口>` 才监听，空字符串取默认 7878。端口被占时桥绑定失败但**应用照常运行**（无桥），stderr 会打印失败。
- 启动成功标志：stderr 出现 `[zephyra] 调试桥已开启 127.0.0.1:<端口>`，多窗口时每窗口一行。
- `zephyra dev` / `run` 起的子进程继承该环境变量；先设变量再 `dev` 同样能开桥。
- 应用仓库通常在 `E:\zephyra-gallery`（默认 `d3d12`）；切 D3D11 用 `--no-default-features --features d3d11`，切 Vulkan 加 `--features linux-vk`。互斥后端不要同时启用。
- `zephyra mcp [--port <基址>] [--host <addr>]`：`--port` 默认读 `ZEPHYRA_DEBUG_PORT`，再退回 `ZEPHYRA_MCP_PORT`，最后 7878；发现线程扫 `基址..基址+16`。stdout 只有 JSON-RPC，诊断全在 stderr。
- 客户端配置：OpenCode v2 用 `mcp.servers.zephyra`（`type: local`、`command: ["zephyra","mcp"]`、`environment.ZEPHYRA_DEBUG_PORT="7878"`、`codemode:false`）；Codex 用 `[mcp_servers.zephyra]` + `args=["mcp"]`；Claude Desktop 用 `mcpServers.zephyra`。多 agent 共用一个长驻服务时改用 Streamable HTTP：`zephyra mcp --http --http-port 8931`，配置 `{"type":"remote","url":"http://127.0.0.1:8931/mcp"}`。
- 握手 `initialize` / `tools/list` 不需要应用在线；工具调用才需要应用连上。协议版本声明到 2026-07-28（含 `server/discover`），向下兼容 2025-11-25 等。

- 先 `list_windows` 拿 id，再给工具传 `window_id`。只连一个窗口时可省略；多个窗口在线时**必填**，不传报错。
- `list_windows` 显示 `reconnecting` 的窗口已断连，等它重连，别再对该 id 发命令。
- `ping` 走桥，用来区分「MCP server 就绪」与「应用在线」。

## 工作流：读 → 操作 → 读

1. `get_layout` 一次拿视口（物理像素）+ 帧统计 + 布局树；从树里挑目标（路径 / 绝对矩形 / key）。
2. 最小必要动作，**一次一个**：`click` / `hover` / `press_key` / `scroll` / `pointer_drag`（一整段手势，不是多个 click）。目标矩形零宽高或不可见时不要点。
3. 操作后重新 `get_layout` / `get_props` / `get_stats` 验证。状态异步更新时隔一拍再读，不要靠快速重复点击碰运气。桥回 `clicked` 只说明事件注入了，**不等于**应用行为正确；`wait_frames` 可按渲染帧等待。

## 工具速查

| 工具 | 参数 | 用途 |
|---|---|---|
| `list_windows` | 无 | 列窗口 id / 身份 / 状态（多窗口先调） |
| `get_layout` / `get_tree` | 无 | 布局上下文 / 只要树 |
| `get_props` | `path` | 单节点全部属性 |
| `get_stats` / `get_viewport` / `get_resources` | 无 | 帧统计 / 视口 / 进程与 GPU 遥测 |
| `ping` | 无 | 应用是否在线 |
| `hover` / `click` | `x`, `y` | 移动 / 完整点击 |
| `press_key` | `key` | 逻辑键按下抬起 |
| `scroll` | `x`, `y`, `delta_x`, `delta_y` | 滚轮注入 |
| `pointer_drag` | `mode` + 参数 | 按住拖动（可带 progressToken） |
| `analyze_performance` | `duration_seconds`，可选 `include_rows` | 有界逐帧 CPU 报告 |
| `screenshot` | 可选 `path`, `inline` | 默认 PNG；`inline=true` 回 base64 图片（默认只给路径，省上下文） |
| `get_app_info` | 可选 `window_id` | 应用身份 |
| `click_by_key` / `hover_by_key` | `key` | 按元素 key 点击 / 悬停（免抄坐标） |
| `click_by_path` / `hover_by_path` | `path` | 按树路径点击 / 悬停 |
| `find_by_text` | `text` | 按可见文字找元素 |
| `type_text` | `text` | 输入一段文字（语义动作，非逐键） |
| `wait_frames` | `frames` | 等应用真渲染 N 帧再返回 |
| `hit_test` | `x`, `y` | 反查某像素上的元素 |
| `touch` | `phase`, `pointer_id`, `x`, `y` | 多点触控注入 |
| `resize_window` | `width`, `height` | 改客户区尺寸（物理像素） |

除 `list_windows` 外都接受可选 `window_id`。工具返回优先读 `structuredContent`，`content` 文本兜底。完整参数与返回样例见 `references/tools.md`、`references/output-formats.md`。

## 坐标与路径（易错）

- 所有坐标是**窗口物理像素、绝对坐标**，从树里抄一个点就能点；不要累加父级偏移、不要把截图缩放坐标直接用。`hit_test` 可用来反查「某个像素上是什么」。
- **每窗口独立一套坐标**：窗口 A 的坐标不能拿去点窗口 B。
- `get_props` 的 `rect` 行以 `(absolute)` 标注，为准（工具描述里的 "relative to parent" 是旧的）。
- 树路径首段 `0` 是根标记，其余是逐层子下标（`0.1.2`）。样式分类名只是名字，不是状态；状态看 `get_props` 属性与 `get_stats.hovered`。

## 能力边界（越界会得出错误结论）

- D3D11/D3D12/Vulkan 都可用；Vulkan 的 `get_resources` 仅 Windows 提供，Linux 下该工具报错，不是断连。
- `press_key` 白名单：`Backspace Delete Enter Escape Space Tab ArrowLeft/Right/Up/Down Home End PageUp/PageDown`；单字符会「边沿+字符」，白名单键只产生边沿、**不产生字符事件**，且**无修饰键**（Ctrl/Alt/Shift 不支持）。`press_key("Space")` 不会往文本框插入空格，也不接受裸空格字符。
- 应用是否响应输入取决于应用是否消费 `View::on_input`；如 gallery 不消费文本事件，MCP 输入文字不会生效。
- `scroll` 增量单位跟随后端；性能报告是 **CPU 生成耗时**，不是 GPU 执行时间；资源是单次采样，`warming_up` 不是真实读数。

## 排障

| 现象 | 处理 |
|---|---|
| 连不上应用 | 应用是否设了 `ZEPHYRA_DEBUG_PORT`；`--port` 基址是否一致；等应用起来重试 |
| 握手成功但工具全失败 | `ping` 分辨 server 与应用状态 |
| 点了没反应 | 重新 `get_layout` 取坐标、确认可见非零，再用 `get_props` 核验 |
| 落到错误的窗口 | `list_windows` 核对 `window_id`，不要猜第一个 |
| 截图空/失败 | 给可写 `.png`（或 `.bmp`）路径；失败改用 `get_layout`，保留原错误 |
| 性能 0 帧 | 采集期间无宿主帧（静止窗口），如实报告未完成 |

完整步骤见 `references/troubleshooting.md`。

## 汇报

说清：目标窗口、做过的动作、**读回验证的状态**、数据口径与限制。性能注明时长/帧数/单位；资源注明采样时间与不可用字段；超时/未验证的步骤明确标未完成。不要把「桥响应成功」说成「应用行为正确」。

## 参考（相对仓库根）

- `zephyra-cli/README.md`：CLI 命令表
- `zephyra-cli/src/commands/mcp.rs`：MCP 工具表与并发模型
- `zephyra/zephyra-fx/src/bridge.rs`：桥协议与 dump
- `docs/dev-log/2026-10-25-mcp-debug-bridge.md`：协议分层
- `docs/dev-log/2026-10-26-mcp-input-layout-d3d12-capture.md`：输入注入与截图读回
- `docs/dev-log/2026-10-26-mcp-clicks-resources.md`：拖动、逐帧性能、资源遥测
- `docs/dev-log/2026-10-26-mcp-reconnect-timeout.md`：重连、超时、取消
