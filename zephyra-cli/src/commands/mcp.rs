//! `zephyra mcp`：**Model Context Protocol** server —— AI 客户端经 stdio 上的
//! JSON-RPC 读布局、操控运行中的 ZephyraUI 应用。
//!
//! # 链路与职责
//!
//! ```text
//! AI 客户端 ──stdio·JSON-RPC── 本进程 ←──TCP·行协议── 运行中的应用
//! ```
//!
//! **方向很重要**：应用主动连入 MCP，MCP 不再拿端口去连应用。`zephyra dev` /
//! `run` 启动应用时把 MCP 的监听端口经 `ZEPHYRA_MCP_PORT` 交给子进程，应用
//! 据此连回来。于是 agent 侧只有一个通道（stdio），MCP 与 agent 绑定、与
//! 应用分离 —— 不再要求 agent 知道应用的调试端口。
//!
//! 本进程只做**翻译**：MCP 工具调用 ↔ 桥的行协议命令。它不理解布局树的内容
//! （树 dump 是桥那边生成的文本，原样转发），也不持有任何 UI 状态 —— 应用一关，
//! 工具调用返回连接错误，仅此而已。
//!
//! # 为什么应用侧是行协议而这里是 JSON
//!
//! 库层保持零依赖（见 `zephyra-fx::bridge` 的模块文档）；JSON 的解析责任收敛在
//! 本 crate——它反正要为 MCP 持有 `serde_json`。三层里只有这一层需要懂 JSON。
//!
//! # stdio 纪律
//!
//! MCP 的 stdio 传输是**换行分隔的 JSON**：stdout 上只能出现协议消息，
//! 任何日志/诊断必须走 stderr。违反这一条的代价是 AI 客户端直接解析失败——
//! 所以本文件的 `eprintln!` 一个都不能改到 stdout 上。
//!
//! # 并发模型：协议消息即时答，工具调用排队做
//!
//! stdin 线程只处理**不碰桥**的消息（`initialize` / `tools/list` / `ping` /
//! 通知）；`tools/call` 连同原始行投给唯一的工具工作线程，后者按到达顺序执行。
//! 这样做的理由：`analyze_performance` 最长 300 秒、`pointer_drag` 最长 60
//! 秒——单线程版在这些调用期间对任何消息都不应答，而 MCP host 的默认工具超时
//! 往往只有 60 秒，超时后整个 server 看起来死了。
//!
//! # 多窗口：按窗口 id 路由
//!
//! `WindowSet` 应用里每个窗口各有一条桥连入本 server，`hello` 帧的第三段是
//! 它的 `WindowKey`。工作线程用 `HashMap<window_id, BridgeClient>` 同时持有它们，
//! 不再「后连的覆盖先连的」。工具调用用可选 `window_id` 指定目标：`list_windows`
//! 用来发现 id；只连着一个窗口时可省略，连着多个却没指定则报错，绝不猜。
//!
//! 取消（`notifications/cancelled`）由跨线程共享的登记表传递：排队中的调用
//! 在开跑前查表跳过；在途的 `analyze_performance` 在每轮轮询间隙查表中止。
//! 按协议，被取消的请求**不回**。

use base64::Engine as _;
use serde_json::{json, Value};

use super::{CliError, Write};
use crate::bridge_client::{AppHello, BridgeClient, BridgeResponse};
use crate::{args::Args, ExitCode};

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 一个 MCP 工具的定义（`tools/list` 的数据源 + `tools/call` 的执行）。
struct Tool {
    name: &'static str,
    description: &'static str,
    /// JSON Schema（`inputSchema`）。存**静态文本**：`json!` 宏展开成
    /// 非常量调用，进不了 `static TOOLS`；运行时 parse 一次，成本可忽略。
    schema: &'static str,
    /// `annotations`：只读/破坏性提示。AI 客户端据此判断哪些调用会改变
    /// 界面状态——没有它，模型只能靠猜工具名里的动词。
    annotations: &'static str,
}

/// 本 server 支持的协议版本（新→旧）。
///
/// 消息形状（tools / ping / 取消 / 通知）在这些 revision 之间没有差别，所以全部
/// 声明支持：客户端要哪个就回哪个；不认识的版本回我们最新的 —— 按 MCP 规范，
/// 这就是让客户端自行决定继续还是断开的降级信号。
const PROTOCOL_VERSIONS: &[&str] = &[
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// `initialize` 的 `instructions`：OpenCode / Claude 等客户端会把它注入系统
/// 提示。有了它，模型不必从十几个工具名里猜「先读再操作再读」的工作流。
const SERVER_INSTRUCTIONS: &str = "ZephyraUI UI driver. Read first, then act, then read again: \
use get_layout for textual layout/context and get_tree for ABSOLUTE pixel coordinates; \
use get_props for one node's full properties; use screenshot only when visual pixels matter. \
Input tools (click/hover/press_key/scroll/pointer_drag) take effect on the current frame, so \
follow them with get_layout or wait_frames before concluding. Tools need the app launched by \
`zephyra dev` listening on ZEPHYRA_DEBUG_PORT, which this MCP server connects out to; \
initialize and tools/list work before that. \
When several windows are connected, call list_windows to get their ids and pass window_id to \
every tool; omitting it is only allowed when exactly one window is connected. A detailed skill \
named `zephyra-mcp` is installed in the agent (load it with the skill tool) — read it before \
complex debugging sessions for the full workflow, capability limits and troubleshooting.";

/// MCP resources：把给 AI 的技能文档通过协议暴露，不依赖客户端是否扫描
/// 本地 skills 目录。内容编译期嵌入，与工具描述同源。
const SKILL_MD: &str = include_str!("../../../skills/zephyra-mcp/SKILL.md");
const SKILL_TOOLS_MD: &str = include_str!("../../../skills/zephyra-mcp/references/tools.md");
const SKILL_OUTPUT_MD: &str =
    include_str!("../../../skills/zephyra-mcp/references/output-formats.md");
const SKILL_TROUBLESHOOTING_MD: &str =
    include_str!("../../../skills/zephyra-mcp/references/troubleshooting.md");

/// (uri, name, description, text)
const RESOURCES: &[(&str, &str, &str, &str)] = &[
    (
        "zephyra://skill",
        "ZephyraUI MCP skill",
        "Full workflow, capability limits and troubleshooting for driving ZephyraUI via MCP",
        SKILL_MD,
    ),
    (
        "zephyra://skill/tools",
        "Tool reference",
        "Parameters and semantics of every zephyra tool",
        SKILL_TOOLS_MD,
    ),
    (
        "zephyra://skill/output-formats",
        "Output formats",
        "Exact text shapes returned by each tool",
        SKILL_OUTPUT_MD,
    ),
    (
        "zephyra://skill/troubleshooting",
        "Troubleshooting",
        "Symptom → cause → fix for MCP connection and UI driving problems",
        SKILL_TROUBLESHOOTING_MD,
    ),
];

/// 按客户端请求选协议版本；见 [`PROTOCOL_VERSIONS`]。
fn negotiated_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(v) => PROTOCOL_VERSIONS
            .iter()
            .copied()
            .find(|s| *s == v)
            .unwrap_or_else(|| {
                eprintln!(
                    "[zephyra-mcp] client requested protocolVersion={v}; serving latest supported {}",
                    PROTOCOL_VERSIONS[0]
                );
                PROTOCOL_VERSIONS[0]
            }),
        None => PROTOCOL_VERSIONS[0],
    }
}

/// 读工具：不改变应用状态。
const READ_ONLY: &str = r#"{"readOnlyHint": true, "destructiveHint": false}"#;
/// 输入/采集类工具：会改变界面状态或进程内采样状态，但不销毁数据。
const MUTATES: &str = r#"{"readOnlyHint": false, "destructiveHint": false}"#;

/// `list_windows` 不碰桥，因此不接受 `window_id`；其余工具都接受（在 `tools/list`
/// 里统一注入该参数，省得每个 schema 字面量都抄一遍）。
const NO_WINDOW_TOOLS: &[&str] = &["list_windows"];

/// 这些工具的返回同时带 `structuredContent`：agent 不必从自然语言里抠字段。
/// 文本 `content` 仍然保留，兼容不看结构化结果的客户端。
const STRUCTURED_SCHEMAS: &[(&str, &str)] = &[
    (
        "list_windows",
        r#"{"type":"object","properties":{"windows":{"type":"array","items":{"type":"object","properties":{"id":{"type":"integer"},"label":{"type":"string"},"identity":{"type":"string"},"status":{"type":"string"}},"required":["id","label","identity","status"]}},"currentTargetId":{"type":["integer","null"]}},"required":["windows"]}"#,
    ),
    (
        "get_viewport",
        r#"{"type":"object","properties":{"width":{"type":"number"},"height":{"type":"number"},"unit":{"const":"physical-pixels"}},"required":["width","height","unit"]}"#,
    ),
    (
        "get_app_info",
        r#"{"type":"object","properties":{"label":{"type":"string"},"identity":{"type":"string"}},"required":["label","identity"]}"#,
    ),
    (
        "screenshot",
        r#"{"type":"object","properties":{"path":{"type":"string"},"bytes":{"type":"integer"},"format":{"type":"string"}},"required":["path","bytes","format"]}"#,
    ),
];

const TOOLS: &[Tool] = &[
    Tool {
        name: "list_windows",
        description: "List the ZephyraUI windows currently connected to this MCP server. \
Returns each window's numeric id (taken from its WindowKey), its `window#N` label, app identity \
and connection state. When a `WindowSet` app opens several windows, each one connects with its \
own id; call this first, then pass that id as `window_id` to other tools for precise targeting.",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "get_app_info",
        description: "Identity the connected app reported when it linked to this MCP server \
(`hello <app-name> <version>`). Use it to tell which app and framework version you are \
driving. Needs no bridge round-trip and works before any other tool call.",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "get_layout",
        description: "Return the current UI layout as plain text for model context: \
physical-pixel viewport, last-frame stats, and each node's child-index path, tag, key, \
absolute bounds, and style classes. Coordinates use the same space as hover/click. \
Use get_props for a node's full properties; use screenshot only when visual pixels are needed.",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "get_tree",
        description: "Dump the layout tree of the running ZephyraUI app. Each line: \
`path tag key x,y w*h [styles]` — path (child-index chain like `0.1.2`) is the key for \
`get_props`; coordinates are ABSOLUTE pixels and are the same coordinate system that \
`hover`/`click` use, so you can copy a coordinate from the tree and click it directly.",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "get_props",
        description: "Dump the properties of one node. `path` is the child-index chain \
from `get_tree` (e.g. `0.1.2`, including the leading root marker `0`). Returns tag / key / \
rect (ABSOLUTE pixels, same space as hover/click) / styles / every property \
(`name = value (styled)` lines are overridden by later local values).",
        schema: r#"{"type": "object", "properties": {"path": {"type": "string", "description": "child-index chain, e.g. 0.1.2"}}, "required": ["path"]}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "get_stats",
        description: "Stats of the last rendered frame: quad count, dirty flags, hovered node.",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "get_resources",
        description: "Read process resource telemetry: CPU utilization as a percentage of total \
machine capacity, working/private memory, and Windows GPU engine utilization plus dedicated/shared GPU memory when the driver exposes them. \
The first sample may report warming_up while interval counters collect a baseline.",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "get_viewport",
        description: "Viewport size of the app window in physical pixels (`width height`).",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "ping",
        description: "Check that the target ZephyraUI app is reachable over the debug bridge. \
Returns pong on success. Use this to tell 'MCP server is ready' apart from 'app is online' when \
other tools report connection failures.",
        schema: r#"{"type": "object", "properties": {}}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "hover",
        description: "Move the pointer to (x, y) — same as a physical mouse move; hover \
states and hit-testing see the synthetic event exactly like a real one. Takes effect \
this frame; read the tree afterwards to see what changed.",
        schema: r#"{"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "click",
        description: "Synthesize a full click (move → down → up) at (x, y). Pick the \
coordinate from `get_tree` output. Takes effect this frame.",
        schema: r#"{"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "click_by_key",
        description: "Synthesize a full click on the element with the given `key` \
(the key column of `get_tree`; the app routes events by that key). The click lands on \
the element's center, so no coordinate arithmetic is needed. Takes effect this frame.",
        schema: r#"{"type": "object", "properties": {"key": {"type": "string", "description": "Element key as shown in get_tree, e.g. \"switch:solid\""}}, "required": ["key"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "hover_by_key",
        description: "Move the pointer onto the element with the given `key` \
(the key column of `get_tree`). Same coordinate handling as `click_by_key`.",
        schema: r#"{"type": "object", "properties": {"key": {"type": "string", "description": "Element key as shown in get_tree"}}, "required": ["key"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "click_by_path",
        description: "Synthesize a full click on the element at the given layout-tree \
path (the first column of `get_tree`, e.g. \"0.1.1\"). Use this when the element has no \
key or when several elements share one key.",
        schema: r#"{"type": "object", "properties": {"path": {"type": "string", "description": "Child-index chain from get_tree, e.g. \"0.1.1\""}}, "required": ["path"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "hover_by_path",
        description: "Move the pointer onto the element at the given layout-tree path \
(the first column of `get_tree`).",
        schema: r#"{"type": "object", "properties": {"path": {"type": "string", "description": "Child-index chain from get_tree"}}, "required": ["path"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "press_key",
        description: "Synthesize one logical key press and release. The running View receives \
both key-edge events. Named keys are whitelisted: Backspace, Delete, Enter, Escape, Space, Tab, \
ArrowLeft/Right/Up/Down, Home, End, PageUp/PageDown; a single printable character also produces \
a char event. Named keys produce edges only (no char event), and no modifier keys are supported.",
        schema: r#"{"type": "object", "properties": {"key": {"type": "string", "description": "One logical key name; spaces and control characters are not allowed"}}, "required": ["key"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "scroll",
        description: "Synthesize a wheel event at (x,y) in physical pixels. delta_y > 0 \
means scroll up; delta_y < 0 means scroll down. The View receives the position and \
wheel deltas (native events preserve the platform's pixel/line units).",
        schema: r#"{"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "delta_x": {"type": "number"}, "delta_y": {"type": "number"}}, "required": ["x", "y", "delta_x", "delta_y"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "pointer_drag",
        description: "Press and hold the primary pointer button while moving, then release. \
Modes: segmented follows a polyline in `points`; linear moves from `start` to `end`; easing \
uses `start`/`end` and `easing` (linear, ease_in, ease_out, smoothstep). Sampling occurs \
across `duration_ms`; optional hold_before_ms/hold_after_ms keep the button down. \
Coordinates are physical pixels. Sampling and total duration are bounded.",
        schema: r#"{"type":"object","properties":{"mode":{"type":"string","enum":["segmented","linear","easing"]},"points":{"type":"array","minItems":2,"maxItems":128,"items":{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]}},"start":{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]},"end":{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]},"easing":{"type":"string","enum":["linear","ease_in","ease_out","smoothstep"]},"duration_ms":{"type":"integer","minimum":1,"maximum":60000},"sample_interval_ms":{"type":"integer","minimum":1,"maximum":1000,"default":16},"hold_before_ms":{"type":"integer","minimum":0,"maximum":10000,"default":0},"hold_after_ms":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"required":["mode","duration_ms"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "touch",
        description: "Inject one touch contact (multi-touch). Phases are down / move / up / \
cancel. `cancel` is what Windows sends when the system takes the gesture over (three- \
and four-finger gestures) -- it must NOT commit a tap, because the user never lifted \
their finger. Each finger has its own `pointer_id`.",
        schema: r#"{"type": "object", "properties": {"phase": {"type": "string", "enum": ["down", "move", "up", "cancel"]}, "pointer_id": {"type": "integer", "description": "Which finger (0, 1, ...) -- different ids are different fingers"}, "x": {"type": "number"}, "y": {"type": "number"}}, "required": ["phase", "pointer_id", "x", "y"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "hit_test",
        description: "Find out what is at a pixel coordinate -- the element under the \
pointer, its key, its visible text, and whether that spot is interactive. Use it when you \
have a screenshot and want to know what a particular pixel belongs to, instead of \
guessing from the tree.",
        schema: r#"{"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "resize_window",
        description: "Resize the app window's client area (logical pixels). The app re-lays out \
on the next frame. Use it to check whether a layout survives a narrow window -- reading the \
stylesheet cannot answer that, because flex, wrapping and clipping all need a real layout pass.",
        schema: r#"{"type": "object", "properties": {"width": {"type": "integer", "minimum": 1, "maximum": 16384}, "height": {"type": "integer", "minimum": 1, "maximum": 16384}}, "required": ["width", "height"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "find_by_text",
        description: "Locate elements by their visible text (case-insensitive substring match). \
Use this when you know what the interface says but not which element it is -- e.g. \"the nav \
item labelled 快速开始\". Each hit reports the text node's path plus `owner`: the nearest \
ancestor that has an element key, which is usually the clickable one.",
        schema: r#"{"type": "object", "properties": {"text": {"type": "string", "description": "Visible text to look for (substring, case-insensitive)"}}, "required": ["text"]}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "type_text",
        description: "Type a string of text into whatever currently has keyboard focus. This is \
one semantic action (the text appears in the field), not a sequence of key presses -- use it \
instead of press_key for filling text fields.",
        schema: r#"{"type": "object", "properties": {"text": {"type": "string", "description": "The text to insert"}}, "required": ["text"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "analyze_performance",
        description: "Collect a bounded CPU frame-performance profile and return it when the \
requested capture duration ends. Records per-frame update/layout, render-submit and total CPU \
generation time, with min/average/median/p95/max plus frame-by-frame rows. Sampling is opt-in; \
duration_seconds is required and limited to 1..=300 seconds, with automatic stop at 3000 frames. \
By default only the summary is returned; pass include_rows=true to also receive the per-frame rows.",
        schema: r#"{"type":"object","properties":{"duration_seconds":{"type":"integer","minimum":1,"maximum":300,"description":"Required bounded capture duration; automatically stops at expiry or 3000 frames."},"include_rows":{"type":"boolean","default":false,"description":"Also return per-frame rows (can be up to 3000 lines). Default false keeps the response summary-only."}},"required":["duration_seconds"]}"#,
        annotations: MUTATES,
    },
    Tool {
        name: "wait_frames",
        description: "Wait until the app has actually rendered N more frames. Use this \
instead of sleeping: it waits for real frames, so the next screenshot or tree read sees \
the state after those frames. A static app renders no frames — the wait then reports how \
few frames it managed to wait for.",
        schema: r#"{"type": "object", "properties": {"frames": {"type": "integer", "minimum":0, "maximum":10000, "description": "How many rendered frames to wait for"}}, "required": ["frames"]}"#,
        annotations: READ_ONLY,
    },
    Tool {
        name: "screenshot",
        description: "Render a frame and read back the pixels, saved as PNG by default (lossless). \
D3D11, D3D12 and Vulkan apps support this. Returns the absolute file path that was written; use \
get_layout for textual layout context without pixel data. Pass a .bmp path to keep the raw \
24bpp BMP for pixel-exact diagnosis. By default the capture keeps the current frame's damage \
region, so stale pixels from the previous frame (ghosting) stay visible; pass repaint=true for \
a clean full-frame image. Pass inline=true to also receive the PNG as base64 image content.",
        schema: r#"{"type": "object", "properties": {"path": {"type": "string", "description": "output path; defaults to a temp .png file. A .bmp path keeps the raw 24bpp BMP"}, "repaint": {"type": "boolean", "default": false, "description": "force a full-frame repaint instead of keeping the current damage region"}, "inline": {"type": "boolean", "default": false, "description": "also return the PNG as base64 image content (off by default to save context)"}}}"#,
        annotations: MUTATES,
    },
];

/// 启动应用端点发现线程；stdio 与 HTTP 两种传输共用。
fn spawn_connector(
    host: &str,
    port: u16,
    live: Arc<Mutex<HashSet<u64>>>,
) -> Result<mpsc::Receiver<(BridgeClient, Option<AppHello>)>, CliError> {
    let (app_tx, app_rx) = mpsc::channel::<(BridgeClient, Option<AppHello>)>();
    let connector_host = host.to_string();
    std::thread::Builder::new()
        .name("zephyra-mcp-app-connect".into())
        .spawn(move || loop {
            for offset in 0..WINDOW_PROBE_LIMIT {
                let Some(probe_port) = port.checked_add(offset) else {
                    break;
                };
                let Ok(mut addrs) = (connector_host.as_str(), probe_port).to_socket_addrs() else {
                    continue;
                };
                let Some(addr) = addrs.next() else { continue };
                let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(150))
                else {
                    continue;
                };
                let mut client = BridgeClient::from_stream(stream);
                let Some(hello) = client.identify() else {
                    continue;
                };
                let id = hello.window_id.unwrap_or(LEGACY_WINDOW_ID);
                {
                    let mut live = live.lock().unwrap();
                    if live.contains(&id) {
                        continue;
                    }
                    live.insert(id);
                }
                eprintln!(
                    "[zephyra-mcp] {} connected: {}",
                    window_label(id),
                    hello.identity
                );
                if app_tx.send((client, Some(hello))).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        })
        .map_err(|e| CliError::failure(format!("cannot start app connector: {e}")))?;
    Ok(app_rx)
}

/// 读一条 HTTP 请求（只实现 Streamable HTTP 需要的 POST + Content-Length）。
fn read_http_request(stream: &mut TcpStream) -> Option<(String, String)> {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok()?;
    Some((
        request_line.trim_end().to_string(),
        String::from_utf8_lossy(&body).into_owned(),
    ))
}

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nMcp-Session-Id: zephyra-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Streamable HTTP 传输：一个长驻 server 给多个 agent 用，避免每个目录各起进程。
fn run_http(host: &str, bridge_port: u16, http_port: u16) -> Result<ExitCode, CliError> {
    let listener = TcpListener::bind((host, http_port)).map_err(|e| {
        CliError::failure(format!(
            "cannot listen for MCP HTTP on {host}:{http_port}: {e}"
        ))
    })?;
    let live: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
    let app_rx = spawn_connector(host, bridge_port, live.clone())?;
    let session = Arc::new(Mutex::new(Session::outbound(
        host,
        bridge_port,
        Cancelled::default(),
        live,
    )));

    // 发现线程送来的窗口连接由这里挂进共享 session。
    let attach_session = session.clone();
    std::thread::Builder::new()
        .name("zephyra-mcp-http-attach".into())
        .spawn(move || loop {
            while let Ok((client, hello)) = app_rx.try_recv() {
                let id = hello
                    .as_ref()
                    .and_then(|h| h.window_id)
                    .unwrap_or(LEGACY_WINDOW_ID);
                let identity = hello.map(|h| h.identity);
                attach_session.lock().unwrap().attach(id, client, identity);
            }
            std::thread::sleep(Duration::from_millis(50));
        })
        .map_err(|e| CliError::failure(format!("cannot start HTTP attach thread: {e}")))?;

    eprintln!("[zephyra-mcp] HTTP transport ready on http://{host}:{http_port}/mcp");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let request_session = session.clone();
        std::thread::spawn(move || {
            use std::io::Write as _;
            let Some((request_line, body)) = read_http_request(&mut stream) else {
                return;
            };
            let is_post = request_line.starts_with("POST ");
            let mut guard = request_session.lock().unwrap();
            let response = if !is_post || body.trim().is_empty() {
                http_response("202 Accepted", "")
            } else {
                match handle_message(&body, &mut guard) {
                    Some(reply) => http_response("200 OK", &reply),
                    None => http_response("202 Accepted", ""),
                }
            };
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
    }
    Ok(ExitCode::Success)
}

pub fn run(args: &Args, io: &mut dyn Write) -> Result<ExitCode, CliError> {
    let port: u16 = args
        .option("port")
        .map(|p| p.parse())
        .transpose()
        .map_err(|_| CliError::usage("--port needs a number"))?
        .unwrap_or_else(default_mcp_port);
    let host = args.option("host").unwrap_or("127.0.0.1").to_string();

    if args.has("http") {
        let http_port: u16 = args
            .option("http-port")
            .map(|p| p.parse())
            .transpose()
            .map_err(|_| CliError::usage("--http-port needs a number"))?
            .unwrap_or(8931);
        return run_http(&host, port, http_port);
    }

    // CDP 式拓扑：**应用持有端点，MCP 连出去**。每个窗口监听
    // `ZEPHYRA_DEBUG_PORT 基址 + 窗口 id`，所以多个 agent 窗口各起一个 MCP
    // 进程也不会抢同一个端口 —— 这是相对「MCP 监听、应用连入」的关键改动。
    let live: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
    let app_rx = spawn_connector(&host, port, live.clone())?;

    // 取消登记表：stdin 线程写，工具工作线程读（见模块文档的并发模型）。
    let cancelled = Cancelled::default();
    let (job_tx, job_rx) = mpsc::channel::<String>();
    let worker_cancelled = cancelled.clone();
    let worker_host = host.clone();
    let worker_live = live.clone();
    std::thread::Builder::new()
        .name("zephyra-mcp-tools".into())
        .spawn(move || {
            // 工具线程持有全部窗口的桥会话——工具调用因此天然串行，AI 的
            // 「读-改-读」次序不会被并发打乱；窗口之间则按 id 分开存放。
            let mut session = Session::outbound(&worker_host, port, worker_cancelled, worker_live);
            loop {
                // 先收发现线程送来的窗口连接（非阻塞）。
                while let Ok((client, hello)) = app_rx.try_recv() {
                    match hello {
                        Some(hello) => {
                            let id = hello.window_id.unwrap_or(LEGACY_WINDOW_ID);
                            session.attach(id, client, Some(hello.identity));
                        }
                        None => session.attach(LEGACY_WINDOW_ID, client, None),
                    }
                }
                // 再等工具调用；20ms 超时保证 app_rx 不会被饿死。
                match job_rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(line) => {
                        if let Some(reply) = handle_message(&line, &mut session) {
                            write_reply(&reply);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            // 通道关闭即退出。**不**等在途调用：客户端关 stdin 意味着它已经
            // 不在了，等一个没人收的应答只会让 host 觉得 server 卡死。
        })
        .map_err(|e| CliError::failure(format!("cannot start tool worker thread: {e}")))?;

    // 主线程只处理不碰桥的消息（initialize / tools/list / ping / 通知）。
    // 握手不需要应用在线——AI 客户端往往先拉起 MCP 再起应用。
    let mut session = Session::outbound(&host, port, cancelled, live);

    io.err_line(&format!(
        "[zephyra-mcp] ready (connecting to app bridge on {host}:{port}); stdout is protocol-only"
    ));

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break }; // EOF → 正常退出
        if line.trim().is_empty() {
            continue;
        }
        if is_tools_call(&line) {
            // 投给工作线程；应答由它直接写 stdout（行级原子，见 write_reply）。
            if job_tx.send(line).is_err() {
                io.err_line("[zephyra-mcp] tool worker thread is gone; dropping tool call");
            }
        } else if let Some(reply) = handle_message(&line, &mut session) {
            write_reply(&reply);
        }
    }
    Ok(ExitCode::Success)
}

/// 一条消息是否是工具调用（决定它去工作线程还是主线程就地处理）。
fn is_tools_call(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|msg| {
            msg.get("method")
                .and_then(Value::as_str)
                .map(|m| m == "tools/call")
        })
        .unwrap_or(false)
}

/// 写一行协议消息到 stdout。两个线程都会调：`Stdout` 的内部锁保证整行
/// 原子（LineWriter 在锁内冲到末尾换行），不会出现两条应答交错成乱 JSON。
fn write_reply(reply: &str) {
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{reply}");
    let _ = lock.flush();
}

/// MCP `notifications/progress` 的正文；抽成纯函数便于测试形状。
fn progress_notification(token: &Value, progress: u64, total: u64) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {"progressToken": token, "progress": progress, "total": total},
    })
    .to_string()
}

/// 老应用 / 单窗口兼容路径不发窗口 id，用这个键（与主仓
/// `WindowKey::unspecified()` 同值）挂在表里，以免和真实窗口 id 撞键。
const LEGACY_WINDOW_ID: u64 = u64::MAX;

/// 发现阶段最多探测 `base..base+N` 这些端口（一个进程最多这么多窗口）。
/// 探测是「连接被拒就跳过」，16 个端口在应用没起来时也只是一瞬间。
const WINDOW_PROBE_LIMIT: u16 = 16;

/// 给 AI 看的窗口标签。真实 id 就是 `WindowKey::as_u64()` 的数字。
fn window_label(id: u64) -> String {
    if id == LEGACY_WINDOW_ID {
        "legacy-single".into()
    } else {
        format!("window#{id}")
    }
}

/// 一条窗口连接的身份与流。`client: None` = 曾连过但现在断了（等应用重连）。
struct WindowLink {
    client: Option<BridgeClient>,
    identity: Option<String>,
}

/// 一个会话的桥侧状态：按窗口 id 缓存的连接 + 取消登记表。
///
/// 主线程与工具线程各有一个实例（主线程的 `clients` 永远是空的——它不碰桥），
/// 但共享同一张取消表。
///
/// 生产路径用 [`Self::outbound`]：发现线程拨号应用端点，连接由 [`Self::attach`]
/// 安放；`live_windows` 与发现线程共享，连接失败时由 `drop_client` 摘掉 id，
/// 下一轮扫描才会重连。`port` 是 **基址**，窗口 id 对应的端口 = 基址 + id。
/// [`Self::new`] 保留「替身服务器」形态，只给测试用。
struct Session {
    host: String,
    /// 应用桥的监听**基址**（`ZEPHYRA_DEBUG_PORT`）。
    port: u16,
    clients: HashMap<u64, WindowLink>,
    /// 当前调用解析出的目标窗口。`ensure_client` / `drop_client` 读它，
    /// 这样多步手势（pointer_drag）中途不用反复传 id。
    current_target: u64,
    cancelled: Cancelled,
    /// 测试替身：表为空时允许默认窗口 0，由 `ensure_client` 现场拨号。
    lazy_dial: bool,
    /// 已挂上的窗口 id，供发现线程跳过、避免每轮替换活连接。
    live_windows: Arc<Mutex<HashSet<u64>>>,
    /// 当前工具调用的 `_meta.progressToken`，用于发进度通知。
    progress_token: Option<Value>,
}

impl Session {
    /// 替身服务器形态。**仅供测试**。
    #[cfg(test)]
    fn new(host: &str, port: u16, cancelled: Cancelled) -> Self {
        Self {
            host: host.to_string(),
            port,
            clients: HashMap::new(),
            current_target: 0,
            cancelled,
            lazy_dial: true,
            live_windows: Arc::new(Mutex::new(HashSet::new())),
            progress_token: None,
        }
    }

    /// 生产拓扑：MCP 进程主动连出到应用持有的端点。
    fn outbound(
        host: &str,
        port: u16,
        cancelled: Cancelled,
        live_windows: Arc<Mutex<HashSet<u64>>>,
    ) -> Self {
        Self {
            host: host.to_string(),
            port,
            clients: HashMap::new(),
            current_target: LEGACY_WINDOW_ID,
            cancelled,
            lazy_dial: false,
            live_windows,
            progress_token: None,
        }
    }

    /// 目标窗口对应的端口：基址 + id；老应用（无 id）直接用基址。
    fn port_for(&self, id: u64) -> u16 {
        if id == LEGACY_WINDOW_ID {
            self.port
        } else {
            self.port.checked_add(id as u16).unwrap_or(self.port)
        }
    }

    /// 安放发现线程送来的 client。工具线程是唯一调用者。
    /// 同一 id 再连（应用重启）只替换那一条，不挤掉其它窗口。
    fn attach(&mut self, id: u64, client: BridgeClient, identity: Option<String>) {
        match &identity {
            Some(name) => eprintln!("[zephyra-mcp] {} hello: {name}", window_label(id)),
            None => eprintln!(
                "[zephyra-mcp] {} connected (no hello; legacy app?)",
                window_label(id)
            ),
        }
        match self.clients.get_mut(&id) {
            Some(link) => {
                if let Some(previous) = link.identity.take() {
                    eprintln!(
                        "[zephyra-mcp] {} stream replaced (previous hello: {previous})",
                        window_label(id)
                    );
                }
                link.identity = identity;
                link.client = Some(client);
            }
            None => {
                self.clients.insert(
                    id,
                    WindowLink {
                        client: Some(client),
                        identity,
                    },
                );
            }
        }
        if let Ok(mut live) = self.live_windows.lock() {
            live.insert(id);
        }
    }

    /// 从工具参数解析目标窗口 id，并记到 `current_target` 供本轮调用使用。
    fn resolve_target(&mut self, a: &Value) -> Result<u64, String> {
        let id = match a.get("window_id") {
            Some(v) => {
                let id = v
                    .as_u64()
                    .ok_or_else(|| "window_id 必须是非负整数（来自 list_windows）".to_string())?;
                self.require_connected(id)?;
                id
            }
            None => self.default_window()?,
        };
        self.current_target = id;
        Ok(id)
    }

    /// 没传 `window_id` 时的默认目标：只有一个在线窗口就用它；多个则报错
    /// 让 agent 显式选择，绝不猜。测试拨号形态下表为空时允许 0（由
    /// `ensure_client` 现场拨号）。
    fn default_window(&self) -> Result<u64, String> {
        if self.lazy_dial && self.clients.is_empty() {
            return Ok(0);
        }
        let online: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, link)| link.client.is_some())
            .map(|(id, _)| *id)
            .collect();
        match online.len() {
            0 => Err(format!(
                "连不上应用调试桥 {}:{}——用 `zephyra dev` 启动应用，\
                 并确认它设置了 ZEPHYRA_DEBUG_PORT={}",
                self.host, self.port, self.port
            )),
            1 => Ok(online[0]),
            _ => {
                let list = online
                    .iter()
                    .map(|id| format!("{} (id={id})", window_label(*id)))
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(format!(
                    "多个窗口已连入（{list}）；window_id 必填：先调 list_windows，再给工具传 window_id"
                ))
            }
        }
    }

    /// 显式指定的 id 必须真的在线，避免对着已关闭的窗口发命令。
    fn require_connected(&self, id: u64) -> Result<u64, String> {
        if self
            .clients
            .get(&id)
            .map(|link| link.client.is_some())
            .unwrap_or(false)
        {
            Ok(id)
        } else {
            let online = self.online_labels();
            Err(format!(
                "窗口 {id} 未连接或已断开；当前在线：{online}——调 list_windows 刷新"
            ))
        }
    }

    fn online_labels(&self) -> String {
        let mut ids: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, link)| link.client.is_some())
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        if ids.is_empty() {
            "(none)".into()
        } else {
            ids.iter()
                .map(|id| format!("{} (id={id})", window_label(*id)))
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    /// `list_windows` 的正文：按 id 排序，带身份与连接状态。
    fn list_windows_text(&self) -> String {
        if self.clients.is_empty() {
            return "no window connected yet (start the app with `zephyra dev`)".into();
        }
        let mut ids: Vec<u64> = self.clients.keys().copied().collect();
        ids.sort_unstable();
        let mut lines = vec![format!("windows connected ({}):", ids.len())];
        for id in ids {
            let link = &self.clients[&id];
            let status = if link.client.is_some() {
                "connected"
            } else {
                "reconnecting"
            };
            let identity = link.identity.as_deref().unwrap_or("(no hello)");
            lines.push(format!(
                "  id={id}  {}  {identity}  {status}",
                window_label(id)
            ));
        }
        lines.push(format!(
            "current target: {}",
            window_label(self.current_target)
        ));
        lines.push(
            "pass `window_id` to target a tool (required when several windows are connected)"
                .into(),
        );
        lines.join("\n")
    }

    /// `list_windows` 的结构化结果：文本给人读，这个给 agent 直接取字段。
    fn windows_structured(&self) -> Vec<Value> {
        let mut ids: Vec<u64> = self.clients.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .map(|id| {
                let link = &self.clients[&id];
                json!({
                    "id": id,
                    "label": window_label(id),
                    "identity": link.identity.as_deref().unwrap_or("(no hello)"),
                    "status": if link.client.is_some() { "connected" } else { "reconnecting" },
                })
            })
            .collect()
    }

    /// 当前调用带 progressToken 时发一条进度通知；没有就静默。
    fn notify_progress(&self, progress: u64, total: u64) {
        if let Some(token) = &self.progress_token {
            write_reply(&progress_notification(token, progress, total));
        }
    }

    /// 当前调用目标的身份（给 `get_app_info` 用，不碰桥）。
    fn current_identity(&self) -> Option<&str> {
        self.clients
            .get(&self.current_target)
            .and_then(|link| link.identity.as_deref())
    }

    /// 拿到当前目标窗口的连接。没有缓存连接就**现场拨号**到 `基址 + id`——
    /// CDP 式拓扑里端口归应用所有，MCP 只是客户端，所以拨号是正常路径。
    fn ensure_client(&mut self) -> Result<&mut BridgeClient, String> {
        let id = self.current_target;
        let missing = self
            .clients
            .get(&id)
            .map(|link| link.client.is_none())
            .unwrap_or(true);
        if missing {
            let host = self.host.clone();
            let port = self.port_for(id);
            let client = BridgeClient::connect(&host, port, 3).map_err(|e| {
                format!(
                    "连不上应用调试桥 {host}:{port}（{e}）——确认应用以 ZEPHYRA_DEBUG_PORT={} 启动",
                    self.port
                )
            })?;
            self.clients
                .entry(id)
                .or_insert(WindowLink {
                    client: None,
                    identity: None,
                })
                .client = Some(client);
            if let Ok(mut live) = self.live_windows.lock() {
                live.insert(id);
            }
        }
        self.clients
            .get_mut(&id)
            .and_then(|link| link.client.as_mut())
            .ok_or_else(|| {
                format!(
                    "窗口 {} 尚未连入 MCP（应用桥 {}:{}）——调 list_windows 查看当前在线窗口",
                    window_label(id),
                    self.host,
                    self.port
                )
            })
    }

    /// 丢弃当前连接。**半读之后的流不可复用**：缓冲里可能压着半条响应，
    /// 继续用会让后续响应切分错位。同时把 id 从共享表里摘掉，让发现线程
    /// 下一轮扫描重新连上来。
    fn drop_client(&mut self) {
        let id = self.current_target;
        if let Some(link) = self.clients.get_mut(&id) {
            link.client = None;
        }
        if let Ok(mut live) = self.live_windows.lock() {
            live.remove(&id);
        }
    }

    /// 测试断言用：当前是否缓存着任意一条在线连接。
    #[cfg(test)]
    fn client(&self) -> Option<&BridgeClient> {
        self.clients.values().find_map(|link| link.client.as_ref())
    }
}

/// 已取消请求 id 的登记表（跨线程共享）。id 存 JSON 文本，数字/字符串都对得上。
#[derive(Clone, Default)]
struct Cancelled(std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>);

impl Cancelled {
    fn mark(&self, id: &Value) {
        if let Ok(mut set) = self.0.lock() {
            set.insert(id.to_string());
        }
    }

    /// 取出并清除：`true` = 这个请求已被取消，调用方应回「无应答」。
    fn take(&self, id: &str) -> bool {
        self.0.lock().map(|mut set| set.remove(id)).unwrap_or(false)
    }
}

/// 应用侧桥的默认端口。运行时刻意不依赖 `zephyra-fx`，常量独立写；
/// 一致性由下面 `default_port_matches_bridge` 的 dev-dependency 测试钉住。
fn zephyra_bridge_port() -> u16 {
    7878
}

/// MCP 要连的应用桥**基址**。优先读 `ZEPHYRA_DEBUG_PORT`（应用监听那个端口
/// 及 +id 的衍生端口）；旧的 `ZEPHYRA_MCP_PORT` 也认，兼容已有配置。
fn default_mcp_port() -> u16 {
    std::env::var(super::BRIDGE_PORT_ENV)
        .or_else(|_| std::env::var(super::MCP_PORT_ENV))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(zephyra_bridge_port)
}

/// 处理一条 JSON-RPC 消息。返回要写回 stdout 的 JSON 行；通知类与已取消的
/// 请求返回 `None`（协议规定：被取消的请求不回）。
///
/// 拆成纯函数（stdin/网络都走参数）是为了能测：MCP 消息的形状与分派逻辑
/// 不必起真进程就能钉住。
fn handle_message(line: &str, session: &mut Session) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_reply(
                Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ))
        }
    };
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let id_key = id.to_string();
    let is_notification = msg.get("id").is_none();

    match method {
        // 通知不回（回了会污染 AI 客户端的消息流）。
        "notifications/initialized" => None,
        "notifications/cancelled" => {
            // 登记后两处消费：工具线程开跑前（跳过排队中的调用）、
            // analyze_performance 的轮询间隙（中止在途采集）。
            if let Some(request_id) = msg.pointer("/params/requestId") {
                session.cancelled.mark(request_id);
            }
            None
        }
        // MCP 的 keepalive。回空 result，不碰桥——客户端用它探测 server 活性，
        // 与「应用在不在线」无关。
        "ping" => Some(json_reply(id, json!({}))),
        // OpenCode 的 `protocol: auto` 用 server/discover 探测 2026-07-28；
        // 我们支持该修订，直接回报能力即可，客户端不必退回 legacy。
        "server/discover" => Some(json_reply(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSIONS[0],
                "capabilities": {
                    "tools": {"listChanged": false},
                    "resources": {"subscribe": false, "listChanged": false}
                },
                "serverInfo": {"name": "zephyra", "version": crate::VERSION},
            }),
        )),
        "initialize" => {
            // 版本协商见 `negotiated_version`。`instructions` 会被 OpenCode /
            // Claude 等客户端注入系统提示，是「模型会用这些工具」的关键一环。
            let version = negotiated_version(
                msg.pointer("/params/protocolVersion")
                    .and_then(Value::as_str),
            );
            Some(json_reply(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {"listChanged": false},
                        "resources": {"subscribe": false, "listChanged": false}
                    },
                    "serverInfo": {"name": "zephyra", "version": crate::VERSION},
                    "instructions": SERVER_INSTRUCTIONS
                }),
            ))
        }
        "resources/list" => Some(json_reply(
            id,
            json!({
                "resources": RESOURCES.iter().map(|(uri, name, description, _)| json!({
                    "uri": uri,
                    "name": name,
                    "description": description,
                    "mimeType": "text/markdown",
                })).collect::<Vec<_>>()
            }),
        )),
        "resources/templates/list" => Some(json_reply(id, json!({"resourceTemplates": []}))),
        "resources/read" => {
            let uri = msg
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or("");
            match RESOURCES
                .iter()
                .find(|(candidate, _, _, _)| *candidate == uri)
            {
                Some((_, _, _, text)) => Some(json_reply(
                    id,
                    json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "text/markdown",
                            "text": text,
                        }]
                    }),
                )),
                None => Some(error_reply(id, -32602, &format!("unknown resource: {uri}"))),
            }
        }
        "tools/list" => {
            let tools: Vec<Value> = TOOLS
                .iter()
                .map(|t| {
                    let mut schema = serde_json::from_str::<Value>(t.schema)
                        .expect("内置工具的 schema 必须是合法 JSON");
                    // 不碰桥的工具没有目标窗口可言，其余统一接受可选 window_id。
                    if !NO_WINDOW_TOOLS.contains(&t.name) {
                        if let Some(properties) = schema
                            .get_mut("properties")
                            .and_then(Value::as_object_mut)
                        {
                            properties.insert(
                                "window_id".into(),
                                json!({
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Target window id from list_windows. Optional only when exactly one window is connected."
                                }),
                            );
                        }
                    }
                    let mut tool = json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": schema,
                        "annotations": serde_json::from_str::<Value>(t.annotations)
                            .expect("内置工具的 annotations 必须是合法 JSON"),
                    });
                    if let Some((_, output_schema)) = STRUCTURED_SCHEMAS
                        .iter()
                        .find(|(name, _)| *name == t.name)
                    {
                        tool["outputSchema"] = serde_json::from_str::<Value>(output_schema)
                            .expect("内置工具的 outputSchema 必须是合法 JSON");
                    }
                    tool
                })
                .collect();
            Some(json_reply(id, json!({ "tools": tools })))
        }
        "tools/call" => {
            if session.cancelled.take(&id_key) {
                // 排队期间被取消：不执行、不回。
                return None;
            }
            let name = msg
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if name.is_empty() {
                return Some(json_reply(
                    id,
                    json!({
                        "content": [{"type": "text", "text":
                            "tools/call 需要 params.name；用 tools/list 查看可用工具"}],
                        "isError": true
                    }),
                ));
            }
            session.progress_token = msg.pointer("/params/_meta/progressToken").cloned();
            let tool_args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            match run_tool(name, &tool_args, session, &id_key) {
                Ok(ToolOutcome::Reply {
                    text,
                    structured,
                    images,
                }) => {
                    let mut content = vec![json!({"type": "text", "text": text})];
                    content.extend(images);
                    let mut result = json!({"content": content});
                    if let Some(value) = structured {
                        result["structuredContent"] = value;
                    }
                    Some(json_reply(id, result))
                }
                Ok(ToolOutcome::Cancelled) => None,
                Err(e) => Some(json_reply(
                    id,
                    json!({"content": [{"type": "text", "text": e}], "isError": true}),
                )),
            }
        }
        other => {
            if is_notification {
                None
            } else {
                Some(error_reply(
                    id,
                    -32601,
                    &format!("unknown method `{other}`"),
                ))
            }
        }
    }
}

/// 在桥连接上执行一条命令；传输失败时**丢连接、重连、重试一次**。
///
/// 实测踩过：旧版把连接缓存在会话里、失败后却从不刷新——应用重启（dev
/// 工作流的常态）之后，本进程的每个工具调用都对着一条死连接报「桥连接已
/// 断开」，直到 MCP host 杀掉进程重配。旧注释写着「连接失败重连一次再试」，
/// 实现里没有那一句。
///
/// 重试的代价：读超时/读失败时命令可能已经被应用执行过（只是响应丢了），
/// 对输入类命令意味着注入两次。权衡后仍然重试——一个永久死掉的会话比
/// 偶发的重复点击糟得多；而且读超时要先冻结 10 秒，那时重试同样不会成功。
fn with_bridge<T>(
    session: &mut Session,
    a: &Value,
    f: impl Fn(&mut BridgeClient) -> Result<T, String>,
) -> Result<T, String> {
    session.resolve_target(a)?;
    let first = f(session.ensure_client()?);
    match first {
        Ok(v) => Ok(v),
        Err(first_err) => {
            session.drop_client();
            match session.ensure_client() {
                Ok(client) => f(client).map_err(|second_err| {
                    format!(
                        "{first_err}；重连 {}:{} 重试仍失败：{second_err}",
                        session.host, session.port
                    )
                }),
                Err(connect_err) => Err(format!("{first_err}；重连失败：{connect_err}")),
            }
        }
    }
}

/// 工具执行的结果。`Cancelled` = 请求已被取消，按协议**不回**。
///
/// `structured` 是可选的 `structuredContent`（与文本 content 同时返回）；
/// `images` 是可选的 base64 image content（目前只有 `screenshot inline=true` 用）。
enum ToolOutcome {
    Reply {
        text: String,
        structured: Option<Value>,
        images: Vec<Value>,
    },
    Cancelled,
}

fn reply(text: String) -> Result<ToolOutcome, String> {
    Ok(ToolOutcome::Reply {
        text,
        structured: None,
        images: Vec::new(),
    })
}

fn reply_structured(text: String, structured: Value) -> Result<ToolOutcome, String> {
    Ok(ToolOutcome::Reply {
        text,
        structured: Some(structured),
        images: Vec::new(),
    })
}

/// 桥响应 → 文本正文（或应用侧错误）。
fn text_of(r: BridgeResponse) -> Result<String, String> {
    match r {
        BridgeResponse::Text(lines) => Ok(if lines.is_empty() {
            "(empty)".into()
        } else {
            lines.join("\n")
        }),
        BridgeResponse::Err(e) => Err(e),
        BridgeResponse::Binary { .. } => Err("意外的二进制响应".into()),
    }
}

/// 执行一个工具调用。返回给 AI 的文本（或「取消」）。
///
/// `cancel_key` 是本次请求的 id：`analyze_performance` 的每轮轮询间隙会查
/// 取消表，让 AI 能中止一次长达 300 秒的采集。
fn run_tool(
    name: &str,
    a: &Value,
    session: &mut Session,
    cancel_key: &str,
) -> Result<ToolOutcome, String> {
    match name {
        // 发现窗口：不碰桥，只列已连入的连接。
        "list_windows" => {
            let windows = session.windows_structured();
            let current = if session.clients.values().any(|link| link.client.is_some()) {
                json!(session.current_target)
            } else {
                Value::Null
            };
            reply_structured(
                session.list_windows_text(),
                json!({"windows": windows, "currentTargetId": current}),
            )
        }
        // 身份是会话级信息，不碰桥 —— 没有应用连入也能答（告诉 AI 还没连上）。
        "get_app_info" => {
            if session.clients.is_empty() {
                reply("no app connected yet (or app did not send hello)".into())
            } else {
                let id = session.resolve_target(a)?;
                let label = window_label(id);
                let identity = session
                    .current_identity()
                    .unwrap_or("(no hello)")
                    .to_string();
                reply_structured(
                    format!("{label}: app identity: {identity}"),
                    json!({"label": label, "identity": identity}),
                )
            }
        }
        "get_tree" => reply(text_of(with_bridge(session, a, |c| c.call("tree"))?)?),
        "get_layout" => reply(text_of(with_bridge(session, a, |c| c.call("layout"))?)?),
        "get_stats" => reply(text_of(with_bridge(session, a, |c| c.call("stats"))?)?),
        "get_resources" => reply(text_of(with_bridge(session, a, |c| c.call("resources"))?)?),
        "get_viewport" => {
            let text = text_of(with_bridge(session, a, |c| c.call("viewport"))?)?;
            // 桥回的是 "<宽> <高>"（物理像素）；解析成功才带 structuredContent，
            // 解析不出来仍保留纯文本，绝不让结构化结果把工具调用搞挂。
            let mut words = text.split_whitespace();
            let width = words.next().and_then(|s| s.parse::<f64>().ok());
            let height = words.next().and_then(|s| s.parse::<f64>().ok());
            match (width, height) {
                (Some(width), Some(height)) => reply_structured(
                    text,
                    json!({"width": width, "height": height, "unit": "physical-pixels"}),
                ),
                _ => reply(text),
            }
        }
        "ping" => {
            let text = text_of(with_bridge(session, a, |c| c.call("ping"))?)?;
            reply(format!(
                "app reachable over the debug bridge ({text}) — 与「MCP server 已就绪」不同，这才说明应用在线"
            ))
        }
        "get_props" => {
            let path = a
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("get_props 需要 path 参数（来自 get_tree 的路径列）")?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("props {path}"))
            })?)?)
        }
        "hover" => {
            let (x, y) = xy(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("hover {x} {y}"))
            })?)?)
        }
        "click" => {
            let (x, y) = xy(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("click {x} {y}"))
            })?)?)
        }
        "click_by_key" => {
            let key = element_key(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("click_key {key}"))
            })?)?)
        }
        "hover_by_key" => {
            let key = element_key(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("hover_key {key}"))
            })?)?)
        }
        "click_by_path" => {
            let path = element_path(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("click_path {path}"))
            })?)?)
        }
        "hover_by_path" => {
            let path = element_path(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("hover_path {path}"))
            })?)?)
        }
        "press_key" => {
            let key = a
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or("press_key 需要 key 参数")?;
            if key.is_empty()
                || key.chars().any(char::is_whitespace)
                || key.chars().any(char::is_control)
            {
                return Err("key 必须是单个逻辑键名，不能包含空白或控制字符".into());
            }
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("press_key {key}"))
            })?)?)
        }
        "scroll" => {
            let (x, y) = xy(a)?;
            let (delta_x, delta_y) = deltas(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("scroll {x} {y} {delta_x} {delta_y}"))
            })?)?)
        }
        "pointer_drag" => {
            let plan = drag_samples(a)?;
            // 多命令手势：先定好目标，后续 drag_send 全走同一条连接。
            session.resolve_target(a)?;
            match perform_pointer_drag(
                session,
                plan.start,
                &plan.samples,
                plan.hold_before_ms,
                plan.hold_after_ms,
            ) {
                Ok(()) => reply(format!(
                    "pointer drag completed ({} movement samples)",
                    plan.samples.len()
                )),
                Err(e) => {
                    if matches!(e, DragError::Transport(_)) {
                        // 拖动是多命令手势，不能整段盲目重试（按下态可能已建立）。
                        // 丢掉半读的连接，下一次调用会重连。
                        session.drop_client();
                    }
                    Err(e.into_message())
                }
            }
        }
        "analyze_performance" => {
            let duration_seconds = profile_duration_seconds(a)?;
            let include_rows = a
                .get("include_rows")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            text_of(with_bridge(session, a, |c| {
                c.call(&format!("profile_start {duration_seconds}"))
            })?)?;
            let started = std::time::Instant::now();
            let deadline = started + std::time::Duration::from_secs(duration_seconds + 10);
            let mut last_progress = u64::MAX;
            loop {
                if session.cancelled.take(cancel_key) {
                    return Ok(ToolOutcome::Cancelled);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                // 每秒最多一条通知：客户端能看出还在跑，又不会被通知流刷屏。
                let elapsed = started.elapsed().as_secs().min(duration_seconds);
                if elapsed != last_progress {
                    last_progress = elapsed;
                    session.notify_progress(elapsed, duration_seconds);
                }
                let report = text_of(with_bridge(session, a, |c| c.call("profile_report"))?)?;
                if report.starts_with("PROFILE_STATE complete") {
                    return reply(if include_rows {
                        report
                    } else {
                        strip_profile_rows(&report)
                    });
                }
                if std::time::Instant::now() >= deadline {
                    return Err("性能采样未在声明时限内完成；可能宿主帧循环已停止".into());
                }
            }
        }
        "screenshot" => {
            let repaint = a.get("repaint").and_then(Value::as_bool).unwrap_or(false);
            let inline = a.get("inline").and_then(Value::as_bool).unwrap_or(false);
            let cmd = if repaint { "shot --repaint" } else { "shot" };
            let r = with_bridge(session, a, |c| c.call(cmd))?;
            match r {
                BridgeResponse::Binary { data, .. } => {
                    if data.is_empty() {
                        return Err("后端读回为空".into());
                    }
                    let path = resolve_shot_path(a);
                    // 扩展名 `.png` ⇒ 转 PNG（见 `crate::shot`）；其余原样 BMP。
                    let format = if std::path::Path::new(&path)
                        .extension()
                        .map(|e| e.eq_ignore_ascii_case("png"))
                        .unwrap_or(false)
                    {
                        "PNG"
                    } else {
                        "24bpp BMP"
                    };
                    crate::shot::write_shot(std::path::Path::new(&path), &data)
                        .map_err(|e| format!("写 {path} 失败: {e}"))?;
                    // 回**绝对**路径：AI 客户端不知道本进程的 cwd，相对路径
                    // 等于没说。
                    //
                    // ⚠ 报的是**落盘后**的大小：转成 PNG 之后大小会变，
                    // 报线上那个字节数会让人以为文件被截断了。
                    let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let text = format!(
                        "screenshot written to {path} ({} bytes, {}{})",
                        written,
                        format,
                        if repaint {
                            ", forced full-frame repaint"
                        } else {
                            ", damage region kept (stale pixels stay visible)"
                        }
                    );
                    let mut structured = json!({"path": path, "bytes": written, "format": format});
                    let mut images = Vec::new();
                    if inline {
                        // 内联只回 PNG：原 BMP 大且多数客户端不认；编码复用落盘那条路。
                        let png = crate::shot::bmp_to_png(&data)?;
                        let base64 = base64::engine::general_purpose::STANDARD.encode(&png);
                        images.push(json!({
                            "type": "image",
                            "data": base64,
                            "mimeType": "image/png",
                        }));
                        structured["inline"] = json!(true);
                    }
                    Ok(ToolOutcome::Reply {
                        text,
                        structured: Some(structured),
                        images,
                    })
                }
                BridgeResponse::Err(e) => Err(e),
                BridgeResponse::Text(_) => Err("桥回了文本而不是图像".into()),
            }
        }
        "wait_frames" => {
            let frames = a
                .get("frames")
                .and_then(Value::as_u64)
                .ok_or("wait_frames 需要 frames 参数（0..=10000 的整数）")?;
            if frames > 10_000 {
                return Err("frames 不能超过 10000".into());
            }
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("wait_frames {frames}"))
            })?)?)
        }
        "touch" => {
            let phase = a
                .get("phase")
                .and_then(Value::as_str)
                .ok_or("touch 需要 phase 参数（down/move/up/cancel）")?;
            let pointer_id = a
                .get("pointer_id")
                .and_then(Value::as_u64)
                .ok_or("touch 需要 pointer_id 参数（哪一根手指）")?;
            let (x, y) = xy(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("touch {phase} {pointer_id} {x} {y}"))
            })?)?)
        }
        "hit_test" => {
            let (x, y) = xy(a)?;
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("hit {x} {y}"))
            })?)?)
        }
        "resize_window" => {
            let width = a
                .get("width")
                .and_then(Value::as_u64)
                .ok_or("resize_window 需要 width 参数")?;
            let height = a
                .get("height")
                .and_then(Value::as_u64)
                .ok_or("resize_window 需要 height 参数")?;
            if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
                return Err("宽高要在 1..=16384 之间".into());
            }
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("resize {width} {height}"))
            })?)?)
        }
        "find_by_text" => {
            let text = a
                .get("text")
                .and_then(Value::as_str)
                .ok_or("find_by_text 需要 text 参数（要找的可见文字）")?;
            if text.trim().is_empty() {
                return Err("text 不能是空白".into());
            }
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("find {}", quote_argument(text)))
            })?)?)
        }
        "type_text" => {
            let text = a
                .get("text")
                .and_then(Value::as_str)
                .ok_or("type_text 需要 text 参数（要输入的文字）")?;
            if text.is_empty() {
                return Err("text 不能是空的".into());
            }
            reply(text_of(with_bridge(session, a, |c| {
                c.call(&format!("type_text {}", quote_argument(text)))
            })?)?)
        }
        other => Err(format!(
            "unknown tool `{other}`；用 tools/list 查看可用工具"
        )),
    }
}

/// 性能报告默认只回汇总块。
///
/// 逐帧行最多 3000 行 × 约 40 字节 ≈ 120KB——整段灌进模型上下文是纯负担
/// （实测 gallery 2 秒 155 帧就有 155 行）；而 min/avg/p50/p95/max 汇总才是
/// 判断依据。要逐帧数据的调用方传 `include_rows=true`。
fn strip_profile_rows(report: &str) -> String {
    const HEADER: &str = "frame_index elapsed_ms total_us update_layout_us render_submit_us";
    let Some(header_at) = report.find(HEADER) else {
        return report.to_string();
    };
    let rows_start = report[header_at..]
        .find('\n')
        .map(|i| header_at + i + 1)
        .unwrap_or(report.len());
    let row_count = report[rows_start..]
        .lines()
        .filter(|l| !l.is_empty())
        .count();
    if row_count == 0 {
        return report.to_string();
    }
    format!(
        "{}(已省略 {row_count} 行逐帧数据；传 include_rows=true 获取全量)\n",
        &report[..rows_start]
    )
}

/// `screenshot` 的落盘路径：给 `path` 就用它（转成绝对），否则临时目录。
fn resolve_shot_path(a: &Value) -> String {
    match a.get("path").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => absolute_path(p),
        _ => std::env::temp_dir()
            .join(format!("zephyra-shot-{}.png", std::process::id()))
            .display()
            .to_string(),
    }
}

/// 相对路径补上 cwd——给 AI 的回执里必须是客户端能直接打开的绝对路径。
fn absolute_path(p: &str) -> String {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return p.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path).display().to_string(),
        Err(_) => p.to_string(),
    }
}

/// 从工具参数里取一个有限、可由 f32 表示的数，返回它的文本形式。
///
/// f32 范围检查与拖动路径一致：1e300 这种数若放行，会在桥侧被解析成 inf
/// 再被拒——错误离原因更远，且白白一个来回。
fn finite_f32(v: &Value, name: &str) -> Result<String, String> {
    let n = v.as_f64().ok_or_else(|| format!("{name} 必须是数字"))?;
    if !n.is_finite() || n.abs() > f32::MAX as f64 {
        return Err(format!("{name} 必须是有限且 f32 可表示的数"));
    }
    Ok(format!("{n}"))
}

/// 从工具参数里取 (x, y)，接受 int / float。
fn xy(a: &Value) -> Result<(String, String), String> {
    let get = |k: &str| -> Result<String, String> {
        let v = a.get(k).ok_or_else(|| format!("需要 {k} 坐标"))?;
        finite_f32(v, k)
    };
    Ok((get("x")?, get("y")?))
}

fn deltas(a: &Value) -> Result<(String, String), String> {
    let get = |k: &str| -> Result<String, String> {
        let v = a.get(k).ok_or_else(|| format!("需要 {k} 滚轮增量"))?;
        finite_f32(v, k)
    };
    Ok((get("delta_x")?, get("delta_y")?))
}

/// 把一段文字包成**协议里的一个参数**（带引号 + 转义）。
///
/// ⚠ 必须转义：桥协议是**行文本**（一行一条命令），而文字里可能有换行、
/// 引号或反斜杠。一个没转义的换行会把一条命令劈成两条 —— 症状是
/// 「应用收到了莫名其妙的命令」，而没人知道为什么。
fn quote_argument(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 从工具参数里取元素 key。
///
/// ⚠ 不能含空白：桥协议是**行文本**（一行一条命令），key 里带空白会被切成两条命令。
/// 那种错会表现为「应用收到了莫名其妙的命令」，极难归因 —— 所以在这里挡住。
fn element_key(a: &Value) -> Result<String, String> {
    let key = a
        .get("key")
        .and_then(Value::as_str)
        .ok_or("需要 key 参数（来自 get_tree 的 key 列）")?;
    if key.is_empty() || key.chars().any(char::is_whitespace) {
        return Err("key 不能为空，也不能含空白".into());
    }
    Ok(key.to_string())
}

/// 从工具参数里取布局树路径（`0.1.1` 形态）。
fn element_path(a: &Value) -> Result<String, String> {
    let path = a
        .get("path")
        .and_then(Value::as_str)
        .ok_or("需要 path 参数（来自 get_tree 的路径列，形如 0.1.1）")?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("path 不能为空".into());
    }
    // 形态校验：全是非负整数、用 `.` 连接。桥那边会再解析一次，这里先挡住
    // 明显不是路径的输入（否则错误会变成应用侧的「路径不存在」，误导方向）。
    if !trimmed
        .split('.')
        .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!(
            "path `{trimmed}` 不是路径形态（应为 0 开头的子下标链，如 0.1.1）"
        ));
    }
    Ok(trimmed.to_string())
}

#[derive(Debug, Clone, Copy)]
struct DragSample {
    x: f64,
    y: f64,
    delay_ms: u64,
}

struct DragPlan {
    start: (f64, f64),
    samples: Vec<DragSample>,
    hold_before_ms: u64,
    hold_after_ms: u64,
}

fn drag_samples(a: &Value) -> Result<DragPlan, String> {
    const MAX_POINTS: usize = 128;
    const MAX_SAMPLES: usize = 512;
    const MAX_DURATION_MS: u64 = 60_000;
    let mode = a
        .get("mode")
        .and_then(Value::as_str)
        .ok_or("pointer_drag 需要 mode")?;
    let duration_ms = a
        .get("duration_ms")
        .and_then(Value::as_u64)
        .filter(|value| (1..=MAX_DURATION_MS).contains(value))
        .ok_or("duration_ms 必须是 1..=60000 的整数")?;
    let sample_interval_ms = optional_u64(a, "sample_interval_ms", 16, 1, 1000)?;
    let hold_before_ms = optional_u64(a, "hold_before_ms", 0, 0, 10_000)?;
    let hold_after_ms = optional_u64(a, "hold_after_ms", 0, 0, 10_000)?;
    if duration_ms + hold_before_ms + hold_after_ms > MAX_DURATION_MS {
        return Err("拖动（含按下/抬起前停留）的总时长不能超过 60000 毫秒".into());
    }

    let points = match mode {
        "segmented" => {
            let values = a
                .get("points")
                .and_then(Value::as_array)
                .ok_or("segmented 模式需要 points 数组")?;
            if !(2..=MAX_POINTS).contains(&values.len()) {
                return Err(format!(
                    "segmented 模式的 points 必须包含 2..={MAX_POINTS} 个坐标"
                ));
            }
            values
                .iter()
                .enumerate()
                .map(|(index, value)| read_point(value, &format!("points[{index}]")))
                .collect::<Result<Vec<_>, _>>()?
        }
        "linear" | "easing" => {
            let start = read_point(a.get("start").ok_or("此模式需要 start 坐标")?, "start")?;
            let end = read_point(a.get("end").ok_or("此模式需要 end 坐标")?, "end")?;
            vec![start, end]
        }
        _ => return Err("mode 必须是 segmented、linear 或 easing".into()),
    };
    let easing = if mode == "easing" {
        match a.get("easing").and_then(Value::as_str).unwrap_or("linear") {
            "linear" => Easing::Linear,
            "ease_in" => Easing::EaseIn,
            "ease_out" => Easing::EaseOut,
            "smoothstep" => Easing::Smoothstep,
            _ => return Err("easing 必须是 linear、ease_in、ease_out 或 smoothstep".into()),
        }
    } else {
        Easing::Linear
    };

    let segment_count = points.len() - 1;
    let mut samples = Vec::new();
    let mut duration_left = duration_ms;
    let mut segments_left = segment_count as u64;
    for pair in points.windows(2) {
        let segment_duration = duration_left.div_ceil(segments_left);
        duration_left -= segment_duration;
        segments_left -= 1;
        let steps = segment_duration.div_ceil(sample_interval_ms).max(1) as usize;
        if samples.len().saturating_add(steps) > MAX_SAMPLES {
            return Err(format!(
                "采样点超过 {MAX_SAMPLES}；请增大 sample_interval_ms 或缩短 duration_ms"
            ));
        }
        let mut elapsed = 0;
        for step in 1..=steps {
            let next_elapsed = segment_duration * step as u64 / steps as u64;
            let t = step as f64 / steps as f64;
            let t = easing.apply(t);
            samples.push(DragSample {
                x: pair[0].0 + (pair[1].0 - pair[0].0) * t,
                y: pair[0].1 + (pair[1].1 - pair[0].1) * t,
                delay_ms: next_elapsed - elapsed,
            });
            elapsed = next_elapsed;
        }
    }
    Ok(DragPlan {
        start: points[0],
        samples,
        hold_before_ms,
        hold_after_ms,
    })
}

#[derive(Clone, Copy)]
enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    Smoothstep,
}

impl Easing {
    fn apply(self, t: f64) -> f64 {
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::Smoothstep => t * t * (3.0 - 2.0 * t),
        }
    }
}

fn read_point(value: &Value, name: &str) -> Result<(f64, f64), String> {
    let x = value
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{name}.x 必须是数字"))?;
    let y = value
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{name}.y 必须是数字"))?;
    if !(x.is_finite() && y.is_finite() && x.abs() <= f32::MAX as f64 && y.abs() <= f32::MAX as f64)
    {
        return Err(format!("{name} 坐标必须是可表示为有限 f32 的数"));
    }
    Ok((x, y))
}

fn optional_u64(
    value: &Value,
    name: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, String> {
    let value = value.get(name).map_or(Ok(default), |value| {
        value.as_u64().ok_or_else(|| format!("{name} 必须是整数"))
    })?;
    if !(min..=max).contains(&value) {
        return Err(format!("{name} 必须在 {min}..={max} 范围内"));
    }
    Ok(value)
}

fn profile_duration_seconds(arguments: &Value) -> Result<u64, String> {
    arguments
        .get("duration_seconds")
        .and_then(Value::as_u64)
        .filter(|value| (1..=300).contains(value))
        .ok_or_else(|| "duration_seconds 必须是 1..=300 的整数".into())
}

/// 拖动过程中的失败。区分「连接层面」与「应用回的 ERR」：只有前者需要
/// 丢连接重连，后者说明连接完好，会话可以继续用。
enum DragError {
    Transport(String),
    App(String),
}

impl DragError {
    fn into_message(self) -> String {
        match self {
            Self::Transport(m) | Self::App(m) => m,
        }
    }
}

fn drag_send(session: &mut Session, command: &str) -> Result<(), DragError> {
    let response = session
        .ensure_client()
        .map_err(DragError::Transport)?
        .call(command)
        .map_err(DragError::Transport)?;
    match response {
        BridgeResponse::Text(_) => Ok(()),
        BridgeResponse::Err(e) => Err(DragError::App(e)),
        BridgeResponse::Binary { .. } => Err(DragError::App("拖动命令收到意外二进制响应".into())),
    }
}

fn perform_pointer_drag(
    session: &mut Session,
    start: (f64, f64),
    samples: &[DragSample],
    hold_before_ms: u64,
    hold_after_ms: u64,
) -> Result<(), DragError> {
    drag_send(session, &format!("hover {} {}", start.0, start.1))?;

    // 移动中途失败也要补抬起：按下态是**跨命令**状态，不清理的话应用
    // 会一直认为按钮按着（后续点击全变成拖动的延续）。
    let movement = (|| {
        drag_send(session, "pointer_down")?;
        if hold_before_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(hold_before_ms));
        }
        for (index, sample) in samples.iter().enumerate() {
            drag_send(session, &format!("hover {} {}", sample.x, sample.y))?;
            if sample.delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(sample.delay_ms));
            }
            session.notify_progress((index + 1) as u64, samples.len() as u64);
        }
        if hold_after_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(hold_after_ms));
        }
        Ok(())
    })();
    let release = drag_send(session, "pointer_up");
    match (movement, release) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(DragError::App(format!(
            "拖动完成但抬起注入失败：{}",
            error.into_message()
        ))),
        (Err(error), Err(release_error)) => Err(DragError::App(format!(
            "{}；清理时补发 pointer_up 也失败：{}",
            error.into_message(),
            release_error.into_message()
        ))),
    }
}

fn json_reply(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn error_reply(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起一个只服务**一条命令**的假桥（真 TCP，行协议同款应答）。
    ///
    /// 返回（端口, 已收到的命令接收端）。`handle_message` 的连接管理是真
    /// 网络路径，用纯函数假对象测不到「懒连接 / 重连」这些行为——所以这里
    /// 宁可起线程。响应固定为 `resp + OK`。
    fn mock_bridge(resp: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                s.read_exact(&mut byte).unwrap();
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            let _ = tx.send(String::from_utf8(buf).unwrap());
            s.write_all(resp.as_bytes()).unwrap();
            s.write_all(b"\nOK\n").unwrap();
        });
        (port, rx)
    }

    fn mock_bridge_with_responses(
        responses: &'static [&'static str],
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let reader = stream.try_clone().unwrap();
            let mut reader = BufReader::new(reader);
            for response in responses {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                tx.send(line.trim_end().to_string()).unwrap();
                if response.starts_with("ERR ") {
                    writeln!(stream, "{response}").unwrap();
                } else {
                    writeln!(stream, "{response}\nOK").unwrap();
                }
            }
        });
        (port, rx)
    }

    fn mock_bridge_sequence(command_count: usize) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let reader = stream.try_clone().unwrap();
            for _ in 0..command_count {
                let mut line = String::new();
                BufReader::new(&reader).read_line(&mut line).unwrap();
                tx.send(line.trim_end().to_string()).unwrap();
                stream.write_all(b"clicked\nOK\n").unwrap();
            }
        });
        (port, rx)
    }

    /// 模拟应用重启：第一条连接收到命令后**不回就断开**；第二条连接正常应答。
    fn mock_bridge_dying_once(
        second_resp: &'static str,
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let read_line = |stream: &std::net::TcpStream| {
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                line.trim_end().to_string()
            };
            let (first, _) = listener.accept().unwrap();
            let _ = tx.send(format!("first:{}", read_line(&first)));
            drop(first); // 应用死掉：不回任何响应，直接关连接
            let (mut second, _) = listener.accept().unwrap();
            let _ = tx.send(format!("second:{}", read_line(&second)));
            writeln!(second, "{second_resp}\nOK").unwrap();
        });
        (port, rx)
    }

    /// 一直回「采集中」的假 profile：应用收到 `profile_start` 后持续 running，
    /// 直到客户端断开。给「在途取消」测试用——取消发生在轮询间隙。
    fn mock_bridge_running_profile() -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // 客户端断开即收工
                    Ok(_) => {}
                }
                let cmd = line.trim_end().to_string();
                let resp = if cmd.starts_with("profile_start") {
                    "performance profiling started for 1s"
                } else {
                    "PROFILE_STATE running remaining_ms=900"
                };
                if tx.send(cmd).is_err() || writeln!(stream, "{resp}\nOK").is_err() {
                    break;
                }
            }
        });
        (port, rx)
    }

    /// 回一条 4 字节的 BIN 截图（真二进制帧：BIN 头 + 载荷 + OK 终止符）。
    fn mock_bridge_binary_shot() -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = tx.send(line.trim_end().to_string());
            stream.write_all(b"BIN 4 bmp 24bpp\n").unwrap();
            stream.write_all(&[0x42, 0x4d, 0x00, 0x00]).unwrap();
            stream.write_all(b"\nOK\n").unwrap();
        });
        (port, rx)
    }

    /// 回一条合法 1x1 24bpp BMP，供 inline 截图测试走真实 BMP→PNG 转换。
    fn mock_bridge_binary_valid_bmp() -> u16 {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let bmp = tiny_bmp();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            writeln!(stream, "BIN {} bmp 24bpp", bmp.len()).unwrap();
            stream.write_all(&bmp).unwrap();
            stream.write_all(b"\nOK\n").unwrap();
        });
        port
    }

    /// 54 字节头 + 4 字节像素（1x1，BGR + 行对齐）。
    fn tiny_bmp() -> Vec<u8> {
        let pixels: [u8; 4] = [1, 2, 3, 0];
        let mut out = Vec::new();
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&pixels);
        out
    }

    #[test]
    fn initialize_announces_tools_capability() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"protocolVersion\""), "{r}");
        assert!(r.contains("serverInfo"), "{r}");
        // 握手不碰桥——AI 客户端先配 MCP 再起应用是常态。
        assert!(session.client().is_none());
    }

    /// 版本协商按 MCP 规范：客户端要的版本我们支持就原样回；不认识就回我们
    /// 最新的（降级信号）。`instructions` 会被 OpenCode / Claude 注入系统提示。
    #[test]
    fn initialize_negotiates_version_and_returns_instructions() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"protocolVersion\":\"2025-06-18\""), "{r}");
        assert!(r.contains("\"instructions\""), "{r}");
        assert!(r.contains("get_layout"), "{r}");

        let r2 = handle_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
            &mut session,
        )
        .unwrap();
        assert!(
            r2.contains(&format!("\"protocolVersion\":\"{}\"", PROTOCOL_VERSIONS[0])),
            "{r2}"
        );
    }

    /// 生产拓扑：没有应用连入就明确报错；连入后工具走那条已建立的流。
    #[test]
    fn outbound_session_uses_app_stream_and_reports_when_absent() {
        let mut absent = Session::outbound(
            "127.0.0.1",
            1,
            Cancelled::default(),
            Arc::new(Mutex::new(HashSet::new())),
        );
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":40,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
            &mut absent,
        )
        .unwrap();
        assert!(r.contains("连不上应用调试桥"), "{r}");
        assert!(r.contains("isError"), "{r}");

        let (port, seen) = mock_bridge("pong");
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut session = Session::outbound(
            "127.0.0.1",
            port,
            Cancelled::default(),
            Arc::new(Mutex::new(HashSet::new())),
        );
        session.attach(0, BridgeClient::from_stream(stream), None);
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":41,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "ping");
        assert!(r.contains("pong"), "{r}");
    }

    /// JSON-RPC 层的 `ping`（MCP keepalive）必须立即回空 result 且不碰桥：
    /// 它探测的是「MCP server 活没活着」，与应用在不在线无关。
    #[test]
    fn jsonrpc_ping_does_not_touch_bridge() {
        // 端口 1 必然连不上：若 ping 碰桥，会得到 isError 而不是空 result。
        let mut session = Session::new("127.0.0.1", 1, Cancelled::default());
        let r =
            handle_message(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#, &mut session).unwrap();
        assert!(r.contains("\"result\":{}"), "{r}");
        assert!(!r.contains("isError"), "{r}");
        assert!(session.client().is_none());
    }

    /// 工具层的 `ping` 走桥：回执必须点明「这才说明应用在线」。
    #[test]
    fn ping_tool_reports_app_status() {
        let (port, seen) = mock_bridge("pong");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "ping");
        assert!(r.contains("app reachable over the debug bridge"), "{r}");
        assert!(r.contains("pong"), "{r}");
    }

    #[test]
    fn notifications_produce_no_output() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        assert!(handle_message(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &mut session
        )
        .is_none());
    }

    #[test]
    fn tools_list_contains_every_tool() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &mut session,
        )
        .unwrap();
        for t in [
            "get_app_info",
            "list_windows",
            "get_tree",
            "get_layout",
            "get_props",
            "get_stats",
            "get_resources",
            "get_viewport",
            "ping",
            "hover",
            "click",
            "press_key",
            "scroll",
            "pointer_drag",
            "analyze_performance",
            "screenshot",
        ] {
            assert!(r.contains(t), "tools/list 缺 {t}：{r}");
        }
        assert!(session.client().is_none(), "tools/list 也不该碰桥");
    }

    /// `auto` 协议探测要能拿到最新修订，避免客户端退回 legacy。
    #[test]
    fn server_discover_advertises_latest_protocol() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#,
            &mut session,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(
            parsed["result"]["protocolVersion"], PROTOCOL_VERSIONS[0],
            "{parsed}"
        );
    }

    /// 技能文档通过 MCP resources 暴露，客户端不必扫本地 skills 目录。
    #[test]
    fn resources_expose_skill_docs() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let listed = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
            &mut session,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&listed).unwrap();
        let resources = parsed["result"]["resources"].as_array().unwrap();
        assert!(resources.iter().any(|r| r["uri"] == "zephyra://skill"));

        let read = handle_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"zephyra://skill"}}"#,
            &mut session,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&read).unwrap();
        let text = parsed["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(text.contains("ZephyraUI MCP"), "{text}");
    }

    /// 进度通知形状固定：progressToken 原样回传，progress/total 是数字。
    #[test]
    fn progress_notification_shape() {
        let line = progress_notification(&json!("tok-1"), 3, 10);
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["method"], "notifications/progress");
        assert_eq!(parsed["params"]["progressToken"], "tok-1");
        assert_eq!(parsed["params"]["progress"], 3);
        assert_eq!(parsed["params"]["total"], 10);
    }

    /// 结构化返回要在 tools/list 里声明 outputSchema，否则客户端不知道怎么解析。
    #[test]
    fn tools_list_declares_structured_outputs() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &mut session,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&r).unwrap();
        let tools = parsed["result"]["tools"].as_array().unwrap();
        for name in ["list_windows", "get_viewport", "get_app_info", "screenshot"] {
            let tool = tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("tools/list 缺 {name}"));
            assert!(
                tool.get("outputSchema").is_some(),
                "{name} 缺 outputSchema：{tool}"
            );
        }
    }

    /// list_windows 的结构化结果必须能直接取字段，不用解析文本。
    #[test]
    fn list_windows_returns_structured_data() {
        let (port0, _) = mock_bridge("pong");
        let (port1, _) = mock_bridge("pong");
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        for (id, port, identity) in [(0, port0, "app zero"), (1, port1, "app one")] {
            session.attach(
                id,
                BridgeClient::connect("127.0.0.1", port, 3).unwrap(),
                Some(identity.to_string()),
            );
        }
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_windows","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&r).unwrap();
        let windows = parsed["result"]["structuredContent"]["windows"]
            .as_array()
            .unwrap();
        assert_eq!(windows.len(), 2, "{parsed}");
        assert_eq!(windows[0]["id"], 0);
        assert_eq!(windows[0]["identity"], "app zero");
        assert_eq!(windows[1]["identity"], "app one");
        assert!(
            parsed["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("current target"),
            "文本回执也要保留可读的当前目标"
        );
    }

    /// get_app_info 也接受 window_id，多窗口时能点名问身份。
    #[test]
    fn get_app_info_targets_a_window_by_id() {
        let (port0, _) = mock_bridge("pong");
        let (port1, _) = mock_bridge("pong");
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        session.attach(
            0,
            BridgeClient::connect("127.0.0.1", port0, 3).unwrap(),
            Some("app zero".into()),
        );
        session.attach(
            1,
            BridgeClient::connect("127.0.0.1", port1, 3).unwrap(),
            Some("app one".into()),
        );
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_app_info","arguments":{"window_id":1}}}"#,
            &mut session,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(
            parsed["result"]["structuredContent"]["identity"], "app one",
            "{parsed}"
        );
    }

    /// annotations 必须把「只读」与「会改状态」分开：AI 客户端据此决定哪些
    /// 调用可以直接发、哪些要先征求用户同意。
    #[test]
    fn annotations_declare_read_only_vs_mutating() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":16,"method":"tools/list"}"#,
            &mut session,
        )
        .unwrap();
        let list: Value = serde_json::from_str(&r).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        let find = |name: &str| {
            tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("tools/list 缺 {name}"))
        };
        assert_eq!(find("get_tree")["annotations"]["readOnlyHint"], true);
        assert_eq!(find("click")["annotations"]["readOnlyHint"], false);
        assert_eq!(find("pointer_drag")["annotations"]["readOnlyHint"], false);
        // 采集会启动进程内 profiler，属于改状态——但 destructiveHint 仍为 false。
        let profile = find("analyze_performance")["annotations"].clone();
        assert_eq!(profile["readOnlyHint"], false);
        assert_eq!(profile["destructiveHint"], false);
    }

    /// `tools/call` 漏 params.name 是客户端 bug 的信号，必须回明确错误
    /// （旧版落进 `unknown tool `` ` 的兜底，报错文本指不到真因）。
    #[test]
    fn tools_call_missing_name_is_a_clear_error() {
        let mut session = Session::new("127.0.0.1", 1, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"isError\":true"), "{r}");
        assert!(r.contains("params.name"), "报错要指出缺什么：{r}");
    }

    #[test]
    fn tool_call_forwards_to_bridge_command() {
        let (port, seen) = mock_bridge("0 Container root 0,0 800x600");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_tree","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "tree");
        assert!(r.contains("0 Container root"), "{r}");
        assert!(!r.contains("\"isError\":true"), "{r}");
        assert!(session.client().is_some(), "工具调用后连接应当被缓存复用");
    }

    /// 应用重启（dev 工作流常态）会留下一条死连接。传输失败必须**丢连接、
    /// 重连、重试一次**——旧版缓存死连接从不刷新，会话从此永久报废。
    #[test]
    fn dead_connection_is_reconnected_and_retried_once() {
        let (port, seen) = mock_bridge_dying_once("reconnected root");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":18,"method":"tools/call","params":{"name":"get_tree","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(
            seen.try_iter().collect::<Vec<_>>(),
            ["first:tree", "second:tree"],
            "第一次连接读到命令即断开，第二次连接应答"
        );
        assert!(!r.contains("\"isError\":true"), "重连重试后应成功：{r}");
        assert!(r.contains("reconnected root"), "{r}");
    }

    /// 排队期间被取消（`notifications/cancelled` 先于工具线程开跑到达）：
    /// 不执行、不回、不碰桥。
    #[test]
    fn queued_call_cancelled_before_start_is_skipped() {
        // 端口 1 连不上：若取消失效去执行了工具，会得到 isError 而不是 None。
        let mut session = Session::new("127.0.0.1", 1, Cancelled::default());
        session.cancelled.mark(&json!(99));
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"get_tree","arguments":{}}}"#,
            &mut session,
        );
        assert!(r.is_none(), "被取消的请求按协议不回：{r:?}");
        assert!(session.client().is_none(), "被取消的调用不该碰桥");
    }

    /// 在途的 `analyze_performance` 要能被取消：采集循环的每轮轮询间隙
    /// 查取消表，AI 由此能中止一次长达 300 秒的采样。
    #[test]
    fn in_flight_performance_capture_is_cancellable() {
        let (port, _seen) = mock_bridge_running_profile();
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let canceller = {
            let cancelled = session.cancelled.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                cancelled.mark(&json!(42));
            })
        };
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"analyze_performance","arguments":{"duration_seconds":1}}}"#,
            &mut session,
        );
        canceller.join().unwrap();
        assert!(r.is_none(), "采集中止后按协议不回：{r:?}");
    }

    #[test]
    fn resources_tool_forwards_to_bridge() {
        let (port, seen) = mock_bridge("cpu_process_percent=warming_up");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let response = handle_message(
            r#"{"jsonrpc":"2.0","id":30,"method":"tools/call","params":{"name":"get_resources","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "resources");
        assert!(
            response.contains("cpu_process_percent=warming_up"),
            "{response}"
        );
    }

    #[test]
    fn click_translates_coordinates() {
        let (port, seen) = mock_bridge("clicked");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let _ = handle_message(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"click","arguments":{"x":12.5,"y":48}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "click 12.5 48");
    }

    /// 坐标在 CLI 侧就做 f32 范围校验：1e300 若放行，会在桥侧被解析成 inf
    /// 再被拒——错误离原因更远，且白白一次连接。
    #[test]
    fn hover_rejects_out_of_f32_range_coordinates() {
        let mut session = Session::new("127.0.0.1", 1, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":19,"method":"tools/call","params":{"name":"hover","arguments":{"x":1e300,"y":0}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"isError\":true"), "{r}");
        assert!(r.contains("f32"), "报错要说明原因：{r}");
        assert!(session.client().is_none(), "参数错误不该建立连接");
    }

    #[test]
    fn layout_tool_requests_plain_text_layout_context() {
        let (port, seen) = mock_bridge("viewport 800x600 physical-pixels\nstats quads=2\nlayout-tree path tag key absolute-x,absolute-y widthxheight [classes]\n0 Container root 0,0 800x600");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"get_layout","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "layout");
        assert!(
            r.contains("physical-pixels") && r.contains("layout-tree"),
            "{r}"
        );
        assert!(r.contains("\"type\":\"text\""), "{r}");
    }

    #[test]
    fn key_and_scroll_tools_translate_and_reject_protocol_injection() {
        let (port, seen) = mock_bridge("pressed Enter");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        handle_message(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"press_key","arguments":{"key":"Enter"}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "press_key Enter");

        let (port, seen) = mock_bridge("scrolled");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        handle_message(
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"scroll","arguments":{"x":12,"y":30,"delta_x":0,"delta_y":-2.5}}}"#,
            &mut session,
        )
        .unwrap();
        assert_eq!(seen.recv().unwrap(), "scroll 12 30 0 -2.5");

        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"press_key","arguments":{"key":"Enter\nshot"}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"isError\":true"), "{r}");
        assert!(session.client().is_none(), "注入内容应在连接桥之前拒绝");
    }

    #[test]
    fn pointer_drag_sends_down_moves_and_release() {
        let (port, seen) = mock_bridge_sequence(4);
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let response = handle_message(
            r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"pointer_drag","arguments":{"mode":"linear","start":{"x":1,"y":2},"end":{"x":7,"y":9},"duration_ms":1,"sample_interval_ms":1000}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(response.contains("pointer drag completed"), "{response}");
        assert_eq!(
            seen.try_iter().collect::<Vec<_>>(),
            ["hover 1 2", "pointer_down", "hover 7 9", "pointer_up"]
        );

        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let response = handle_message(
            r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"pointer_drag","arguments":{"mode":"segmented","points":[{"x":1,"y":2}],"duration_ms":10}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(response.contains("\"isError\":true"), "{response}");
        assert!(session.client().is_none(), "无效路径必须在建立桥连接前拒绝");
    }

    #[test]
    fn pointer_drag_attempts_release_after_a_move_error() {
        let (port, seen) = mock_bridge_with_responses(&[
            "hovered",
            "pointer_down",
            "ERR simulated move failure",
            "pointer_up",
        ]);
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let response = handle_message(
            r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"pointer_drag","arguments":{"mode":"linear","start":{"x":1,"y":2},"end":{"x":7,"y":9},"duration_ms":1,"sample_interval_ms":1000}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(response.contains("\"isError\":true"), "{response}");
        assert_eq!(
            seen.try_iter().collect::<Vec<_>>(),
            ["hover 1 2", "pointer_down", "hover 7 9", "pointer_up"]
        );
    }

    #[test]
    fn pointer_drag_builds_segmented_and_eased_motion() {
        let segmented = drag_samples(&json!({
            "mode":"segmented",
            "points":[{"x":0,"y":0},{"x":10,"y":0},{"x":10,"y":10}],
            "duration_ms":100,
            "sample_interval_ms":100
        }))
        .unwrap()
        .samples;
        assert_eq!(segmented.len(), 2);
        assert_eq!((segmented[0].x, segmented[0].y), (10.0, 0.0));
        assert_eq!((segmented[1].x, segmented[1].y), (10.0, 10.0));

        let eased = drag_samples(&json!({
            "mode":"easing",
            "start":{"x":0,"y":0},
            "end":{"x":10,"y":0},
            "duration_ms":100,
            "sample_interval_ms":25,
            "easing":"smoothstep"
        }))
        .unwrap()
        .samples;
        assert_eq!(eased.len(), 4);
        assert!(eased[0].x < 2.5);
        assert_eq!(eased[3].x, 10.0);
    }

    /// 性能报告默认只回汇总：逐帧行是模型上下文的纯负担。逐帧数据用
    /// `include_rows=true` 取。
    #[test]
    fn performance_report_strips_rows_by_default_and_restores_them_on_request() {
        let responses: &[&str] = &[
            "performance profiling started for 1s",
            "PROFILE_STATE running remaining_ms=50",
            "PROFILE_STATE complete\nframes=2\nmin_us=100 max_us=110\nframe_index elapsed_ms total_us update_layout_us render_submit_us\n0 0 100 60 40\n1 16 110 65 45",
        ];

        let (port, seen) = mock_bridge_with_responses(responses);
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let response = handle_message(
            r#"{"jsonrpc":"2.0","id":31,"method":"tools/call","params":{"name":"analyze_performance","arguments":{"duration_seconds":1}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(response.contains("PROFILE_STATE complete"), "{response}");
        assert!(
            response.contains("min_us=100"),
            "汇总块必须保留：{response}"
        );
        assert!(
            !response.contains("1 16 110 65 45"),
            "逐帧行默认剥离：{response}"
        );
        assert!(
            response.contains("include_rows=true"),
            "回执要提示怎么取全量：{response}"
        );
        assert_eq!(
            seen.try_iter().collect::<Vec<_>>(),
            ["profile_start 1", "profile_report", "profile_report"]
        );

        let (port, _seen) = mock_bridge_with_responses(responses);
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let response = handle_message(
            r#"{"jsonrpc":"2.0","id":32,"method":"tools/call","params":{"name":"analyze_performance","arguments":{"duration_seconds":1,"include_rows":true}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(
            response.contains("1 16 110 65 45"),
            "include_rows=true 要有逐帧行：{response}"
        );
        assert!(!response.contains("已省略"), "{response}");
    }

    #[test]
    fn strip_profile_rows_only_removes_frame_rows() {
        let report = "PROFILE_STATE complete\nframes=1\nframe_index elapsed_ms total_us update_layout_us render_submit_us\n0 0 100 60 45";
        let stripped = strip_profile_rows(report);
        assert!(stripped.contains("frames=1"), "{stripped}");
        assert!(stripped.contains("已省略 1 行"), "{stripped}");
        assert!(!stripped.contains("0 0 100 60 45"), "{stripped}");
        // 没有表头（如后端旧版报告）就原样返回——剥错内容比不剥更糟。
        let legacy = "PROFILE_STATE complete\nframes=1\n0 0 100 60 45";
        assert_eq!(strip_profile_rows(legacy), legacy);
    }

    #[test]
    fn performance_capture_requires_a_bounded_duration() {
        assert_eq!(
            profile_duration_seconds(&json!({"duration_seconds": 30})).unwrap(),
            30
        );
        assert!(profile_duration_seconds(&json!({})).is_err());
        assert!(profile_duration_seconds(&json!({"duration_seconds": 301})).is_err());
    }

    /// 不传 path 时默认导出 PNG（临时目录、绝对路径）。
    #[test]
    fn screenshot_default_path_is_png() {
        let path = resolve_shot_path(&serde_json::json!({}));
        assert!(path.ends_with(".png"), "{path}");
        assert!(
            std::path::Path::new(&path).is_absolute(),
            "默认路径必须是绝对路径：{path}"
        );
    }

    /// inline=true 要同时回文本路径、结构化结果和 base64 image content。
    #[test]
    fn screenshot_inline_returns_image_content() {
        let port = mock_bridge_binary_valid_bmp();
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"screenshot","arguments":{"inline":true}}}"#,
            &mut session,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&r).unwrap();
        let content = parsed["result"]["content"].as_array().unwrap();
        assert!(content.len() >= 2, "text + image：{parsed}");
        let image = content
            .iter()
            .find(|c| c["type"] == "image")
            .expect("要含 image content");
        assert_eq!(image["mimeType"], "image/png");
        let data = image["data"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(&decoded[..8], b"\x89PNG\r\n\x1a\n", "必须是真 PNG");
        assert_eq!(
            parsed["result"]["structuredContent"]["format"], "PNG",
            "{parsed}"
        );
    }

    /// 截图回执必须是**绝对**路径：AI 客户端不知道本进程的 cwd，相对路径
    /// 等于没说。
    #[test]
    fn screenshot_reply_carries_an_absolute_path() {
        let shot =
            std::env::temp_dir().join(format!("zephyra-cli-test-{}.bmp", std::process::id()));
        let shot_str = shot.display().to_string();
        let (port, _seen) = mock_bridge_binary_shot();
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let r = handle_message(
            &format!(
                r#"{{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{{"name":"screenshot","arguments":{{"path":"{}"}}}}}}"#,
                shot_str.replace('\\', "\\\\")
            ),
            &mut session,
        )
        .unwrap();
        assert!(!r.contains("\"isError\":true"), "{r}");
        assert!(r.contains("4 bytes"), "{r}");
        // 绝对性用 std 判，不靠字符串前缀（Windows 长路径/盘符大小写都算）。
        let reported = r
            .split("screenshot written to ")
            .nth(1)
            .and_then(|rest| rest.split(" (").next())
            .expect("回执要含路径");
        assert!(
            std::path::Path::new(reported).is_absolute(),
            "回执路径必须是绝对路径：{reported}"
        );
        let _ = std::fs::remove_file(&shot);
    }

    #[test]
    fn binary_bridge_response_reads_large_payload_by_declared_length() {
        use std::io::Write;

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut payload = vec![0x5a; 2 * 1024 * 1024];
        payload[100..104].copy_from_slice(b"\nOK\n");
        let sent = payload.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            writeln!(stream, "BIN {} bmp 24bpp", sent.len()).unwrap();
            stream.write_all(&sent).unwrap();
            stream.write_all(b"\nOK\n").unwrap();
        });

        let mut client = BridgeClient::from_stream(std::net::TcpStream::connect(address).unwrap());
        match client.read_response().unwrap() {
            BridgeResponse::Binary { note, data } => {
                assert_eq!(note, "bmp 24bpp");
                assert_eq!(data, payload);
            }
            _ => panic!("large screenshot response must be parsed as binary"),
        }
        server.join().unwrap();
    }

    #[test]
    fn bridge_error_becomes_is_error_result() {
        let (port, _seen) = mock_bridge("ERR 路径 9.9 不存在");
        let mut session = Session::new("127.0.0.1", port, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_props","arguments":{"path":"9.9"}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"isError\":true"), "{r}");
        assert!(r.contains("不存在"), "{r}");
    }

    #[test]
    fn bad_json_is_a_parse_error_not_a_crash() {
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        let r = handle_message("not json", &mut session).unwrap();
        assert!(r.contains("-32700"), "{r}");
    }

    #[test]
    fn unreachable_bridge_is_a_tool_error_not_a_panic() {
        // 端口 1（几乎必然无监听）：重连重试也失败后，工具调用以 isError 结束。
        let mut session = Session::new("127.0.0.1", 1, Cancelled::default());
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_tree","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"isError\":true"), "{r}");
        assert!(r.contains("ZEPHYRA_DEBUG_PORT"), "错误要直接指出修法：{r}");
    }

    /// 协议常量契约：CLI 的默认端口必须与框架侧桥一致。
    ///
    /// CLI 刻意不依赖 `zephyra-fx`（连 dev-dependency 也不留），两侧各写
    /// 一份字面量，靠这条测试钉住契约值。改这里等于改跨仓协议，需同步框架侧。
    #[test]
    fn default_port_matches_bridge() {
        assert_eq!(zephyra_bridge_port(), 7878);
    }

    /// 连入 MCP 的环境变量名必须与框架侧一致（字面量契约）。
    #[test]
    fn mcp_port_env_matches_bridge() {
        assert_eq!(crate::commands::MCP_PORT_ENV, "ZEPHYRA_MCP_PORT");
    }

    /// CDP 式拓扑用工的环境变量名也必须与框架侧一致（字面量契约）。
    #[test]
    fn bridge_port_env_matches_bridge() {
        assert_eq!(crate::commands::BRIDGE_PORT_ENV, "ZEPHYRA_DEBUG_PORT");
    }

    /// CDP 式端口规划：基址 + id；老应用（无 id）仍用基址。
    #[test]
    fn window_id_maps_to_base_port_offset() {
        let session = Session::new("127.0.0.1", 7878, Cancelled::default());
        assert_eq!(session.port_for(0), 7878);
        assert_eq!(session.port_for(3), 7881);
        assert_eq!(session.port_for(LEGACY_WINDOW_ID), 7878);
    }

    /// 连出发现：桥对 `identify` 的应答要解析成 AppHello（含窗口 id）。
    #[test]
    fn identify_reads_window_id_from_bridge() {
        let (port, seen) = mock_bridge("hello app 1.0 3");
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut client = BridgeClient::from_stream(stream);
        let hello = client.identify().unwrap();
        assert_eq!(seen.recv().unwrap(), "identify");
        assert_eq!(hello.identity, "app 1.0");
        assert_eq!(hello.window_id, Some(3));
    }

    /// 两个窗口各连一条桥：表必须同时保留，list_windows 列得出两个 id。
    #[test]
    fn two_windows_are_both_kept_and_listable() {
        let (port0, _rx0) = mock_bridge("from-zero");
        let (port1, _rx1) = mock_bridge("from-one");
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        session.attach(
            0,
            BridgeClient::connect("127.0.0.1", port0, 3).unwrap(),
            Some("gallery 0.1.0".into()),
        );
        session.attach(
            1,
            BridgeClient::connect("127.0.0.1", port1, 3).unwrap(),
            Some("gallery 0.1.0".into()),
        );

        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_windows","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("id=0"), "{r}");
        assert!(r.contains("id=1"), "{r}");
        assert!(r.contains("window#0"), "{r}");
        assert!(r.contains("window#1"), "{r}");
    }

    /// 指定 window_id 时命令必须落到对应窗口；多个窗口又没指定时宁可报错。
    #[test]
    fn window_id_routes_to_the_right_bridge() {
        let (port0, _rx0) = mock_bridge("from-zero");
        let (port1, _rx1) = mock_bridge("from-one");
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        session.attach(
            0,
            BridgeClient::connect("127.0.0.1", port0, 3).unwrap(),
            None,
        );
        session.attach(
            1,
            BridgeClient::connect("127.0.0.1", port1, 3).unwrap(),
            None,
        );

        // 不选目标：多个窗口在线时应报错，绝不猜一个。
        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_tree","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("\"isError\":true"), "{r}");
        assert!(r.contains("list_windows"), "{r}");

        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_tree","arguments":{"window_id":1}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("from-one"), "{r}");
        assert!(!r.contains("from-zero"), "{r}");
    }

    /// 一个窗口重连只替换自己的连接，不影响另一个窗口。
    #[test]
    fn reconnect_of_one_window_keeps_the_other() {
        let (port0, _rx0) = mock_bridge("from-zero");
        let (port1, _rx1) = mock_bridge("from-one");
        let (port0b, _rx0b) = mock_bridge("from-zero-again");
        let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
        session.attach(
            0,
            BridgeClient::connect("127.0.0.1", port0, 3).unwrap(),
            Some("old".into()),
        );
        session.attach(
            1,
            BridgeClient::connect("127.0.0.1", port1, 3).unwrap(),
            Some("other".into()),
        );
        session.attach(
            0,
            BridgeClient::connect("127.0.0.1", port0b, 3).unwrap(),
            Some("new".into()),
        );

        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_windows","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("id=0") && r.contains("id=1"), "{r}");
        assert!(r.contains("new") && r.contains("other"), "{r}");

        let r = handle_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_tree","arguments":{"window_id":1}}}"#,
            &mut session,
        )
        .unwrap();
        assert!(r.contains("from-one"), "{r}");
    }
}

/// 身份行的读者必须与桥的写法对齐：`hello <name> <version> [<window-id>]` 一行，
/// 不含其它。第三段可选：老应用没有它，多窗口应用每个窗口各报自己的 id。
#[test]
fn read_hello_consumes_only_the_identity_line() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"hello gallery 0.1.0\n").unwrap();
        stream
    });
    let stream = std::net::TcpStream::connect(addr).unwrap();
    let mut client = crate::bridge_client::BridgeClient::from_stream(stream);
    let hello = client.read_hello();
    server.join().unwrap();
    let hello = hello.unwrap();
    assert_eq!(hello.identity, "gallery 0.1.0");
    assert_eq!(hello.window_id, None);
}

/// 新格式：第三段是数字窗口 id；app 名带空格也不该被拆错（从右往左解析）。
#[test]
fn read_hello_parses_optional_window_id() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"hello my gallery 0.1.0 42\n").unwrap();
        stream
    });
    let stream = std::net::TcpStream::connect(addr).unwrap();
    let mut client = crate::bridge_client::BridgeClient::from_stream(stream);
    let hello = client.read_hello().unwrap();
    server.join().unwrap();
    assert_eq!(hello.identity, "my gallery 0.1.0");
    assert_eq!(hello.window_id, Some(42));
}

/// 纯数字版本号的老应用不能被误读成带 window id 的新格式。
#[test]
fn read_hello_does_not_mistake_numeric_version_for_window_id() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"hello app 123\n").unwrap();
        stream
    });
    let stream = std::net::TcpStream::connect(addr).unwrap();
    let mut client = crate::bridge_client::BridgeClient::from_stream(stream);
    let hello = client.read_hello().unwrap();
    server.join().unwrap();
    assert_eq!(hello.identity, "app 123");
    assert_eq!(hello.window_id, None);
}

#[test]
fn get_app_info_without_app_says_so() {
    let mut session = Session::new("127.0.0.1", 7878, Cancelled::default());
    let r = handle_message(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_app_info","arguments":{}}}"#,
            &mut session,
        )
        .unwrap();
    assert!(r.contains("no app"), "{r}");
    assert!(session.client().is_none(), "get_app_info 不该碰桥");
}
