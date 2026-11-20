//! 子命令表。`--help` 与执行都查这一张表，所以两者不可能不同步。
//!
//! # 命令的取舍
//!
//! 参考 Tauri CLI 的形状（`init/dev/build/add/info` 那套），但只保留对 ZephyraUI
//! 真的有意义的：
//!
//! | 命令 | 干什么 | 为什么要 |
//! |---|---|---|
//! | `new` | 生成一个可跑的空工程 | 「上手成本」是一切的前置 |
//! | `check` | 查样式表 | 唯一能在不启动 GUI 的情况下抓错的一步，必须能进 CI |
//! | `fmt` | 排版（`.zstyle` / `.zypui` / Rust） | 写法统一靠命令不靠评审；判据在编译器里，CLI 只聚合 |
//! | `run` | 跑起来 | |
//! | `build` | 构建 | |
//! | `info` | 报版本与工具链状态 | 出问题时第一句要问的就是「你装的什么版本」 |
//! | `props` | 列受支持的属性 | 与 `zstylec props` 同源，不制造第二条路径 |
//! | `mcp` | MCP server：AI 直连运行中的界面 | 「AI 会用工具」是当代的必备形态；桥协议在 `zephyra-fx::bridge` |
//!
//! 没有 `migrate` / `sign` / `updater` —— 那些对应 Tauri 的分发体系，ZephyraUI
//! 现在没有，加进来就是空壳。

use crate::args::{ArgKind, ArgSpec, Args};
use crate::ExitCode;

/// 应用连入 MCP 的端口环境变量。由 CLI 在启动应用时注入；应用侧
/// `zephyra_fx::bridge::MCP_PORT_ENV` 读同一个名字（测试钉住一致性）。
pub const MCP_PORT_ENV: &str = "ZEPHYRA_MCP_PORT";
/// dev 把资源目录交给应用的变量名。应用侧 `zephyra_fx::dev_reload` 读取；
/// release 构建里没有任何代码读它（热重载模块被 cfg 剔除）。
pub const DEV_ASSETS_ENV: &str = "ZEPHYRA_DEV_ASSETS";
/// 默认监听端口，与 `zephyra_bridge_port()` / MCP 默认端口一致。
pub const DEFAULT_MCP_PORT: u16 = 7878;

/// CLI 进程自己的 `ZEPHYRA_MCP_PORT`（没有就用默认值），转发给应用子进程。
/// MCP 也用同一个变量决定监听端口，所以一处设置两端自动对齐。
pub(crate) fn mcp_port_for_child() -> String {
    std::env::var(MCP_PORT_ENV).unwrap_or_else(|_| DEFAULT_MCP_PORT.to_string())
}

pub struct Command {
    pub name: &'static str,
    pub about: &'static str,
    /// **位置参数**说明（不含选项 —— 选项由 `options` 生成）。
    pub args: &'static str,
    /// 这个命令认的全部选项：布尔标志 + 取值选项。
    ///
    /// ⚠ 这是选项元数据的**唯一**来源。解析器查它决定 `--name` 是否取值，
    /// 门禁查它判断标志属不属于本命令，`render_help` 从它生成帮助 ——
    /// 三者读同一份声明，结构上不可能出现「改了 A 忘了改 B」的漂移。
    ///
    /// 这也顺手治掉了老毛病：`zephyra check --strictness` 以前安静通过、
    /// exit 0，看起来像「开了严格模式跑过了」，其实那个标志根本没生效。
    pub options: &'static [ArgSpec],
    pub run: fn(&Args, &mut dyn Write) -> Result<ExitCode, CliError>,
}

impl Command {
    /// 本命令是否声明了名为 `name` 的布尔标志。
    pub fn accepts_flag(&self, name: &str) -> bool {
        self.options
            .iter()
            .any(|o| o.name == name && o.kind == ArgKind::Flag)
    }

    /// 本命令是否声明了名为 `name` 的取值选项。
    pub fn accepts_value(&self, name: &str) -> bool {
        self.options
            .iter()
            .any(|o| o.name == name && o.kind == ArgKind::Value)
    }

    /// 报错时展示的布尔标志名单。
    pub fn accepted_flags(&self) -> Vec<&'static str> {
        self.options
            .iter()
            .filter(|o| o.kind == ArgKind::Flag)
            .map(|o| o.name)
            .collect()
    }

    /// 报错时展示的取值选项名单。
    pub fn accepted_values(&self) -> Vec<&'static str> {
        self.options
            .iter()
            .filter(|o| o.kind == ArgKind::Value)
            .map(|o| o.name)
            .collect()
    }
}

/// 统一的输出端。抽出来是为了**能测**：命令往 stdout/stderr 写东西时，
/// 把它接到内存里就不必起子进程去验。
pub trait Write {
    fn line(&mut self, s: &str);
    fn err_line(&mut self, s: &str);
}

pub struct Io<'a> {
    pub out: &'a mut dyn std::io::Write,
    pub err: &'a mut dyn std::io::Write,
}

impl Write for Io<'_> {
    fn line(&mut self, s: &str) {
        let _ = writeln!(self.out, "{s}");
    }
    fn err_line(&mut self, s: &str) {
        let _ = writeln!(self.err, "{s}");
    }
}

/// 命令级错误。带退出码，所以「用法错」和「环境缺」不会混成一种。
#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub code: ExitCode,
}

impl CliError {
    pub fn usage(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            code: ExitCode::Usage,
        }
    }
    pub fn missing(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            code: ExitCode::MissingEnv,
        }
    }
    pub fn failure(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            code: ExitCode::Failure,
        }
    }
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "new",
        about: "scaffold a new ZephyraUI project",
        args: "<name>",
        options: &[
            ArgSpec::value("path", "dir", "create the project in this directory"),
            ArgSpec::flag("force", "overwrite the target directory if it exists"),
        ],
        run: new::run,
    },
    Command {
        name: "check",
        about: "check one or more stylesheets",
        args: "<file.zstyle>...",
        options: &[ArgSpec::value(
            "format",
            "fmt",
            "output format: human or json",
        )],
        run: check::run,
    },
    Command {
        name: "fmt",
        about: "format project sources: .zstyle / .zypui / Rust (via cargo fmt)",
        args: "[<path>...]",
        options: &[
            ArgSpec::flag("check", "report files that would change, do not write"),
            ArgSpec::value("manifest-path", "p", "path to Cargo.toml"),
        ],
        run: fmt::run,
    },
    Command {
        name: "dev",
        about: "run in dev mode (debug only): hot-reload Rust and assets",
        args: "",
        options: &[
            ArgSpec::value("styles", "dir", "directory of stylesheets to watch"),
            ArgSpec::flag("no-watch", "do not restart when files change"),
            ArgSpec::flag("no-trace", "disable ZEPHYRA_TRACE output"),
            ArgSpec::value("manifest-path", "p", "path to Cargo.toml"),
            ArgSpec::value("log", "file", "also write child output to this file"),
        ],
        run: dev::run,
    },
    Command {
        name: "build",
        about: "build a ZephyraUI app (default: release)",
        args: "",
        options: &[
            ArgSpec::flag("debug", "build in debug mode (default is release)"),
            ArgSpec::flag("release", "build in release mode (default)"),
            ArgSpec::flag("sf", "single file: embed config and assets into the exe"),
            ArgSpec::value("manifest-path", "p", "path to Cargo.toml"),
            ArgSpec::value("log", "file", "also write child output to this file"),
        ],
        run: build::build,
    },
    Command {
        name: "info",
        about: "print versions and toolchain status",
        args: "",
        options: &[],
        run: info::run,
    },
    Command {
        name: "props",
        about: "list every supported stylesheet property",
        args: "",
        options: &[],
        run: props::run,
    },
    Command {
        name: "mcp",
        about: "MCP server bound to the agent (stdio); the app connects in",
        args: "",
        options: &[
            ArgSpec::value("port", "n", "port MCP listens on for the app to connect to"),
            ArgSpec::value("host", "addr", "address MCP listens on for app connections"),
        ],
        run: mcp::run,
    },
    Command {
        name: "bridge",
        about: "talk to a running app's debug bridge directly (one process, many commands)",
        args: "<command>... | log <file>",
        options: &[
            ArgSpec::value("port", "n", "TCP port of the debug bridge"),
            ArgSpec::value("host", "addr", "host of the debug bridge"),
            ArgSpec::flag("stdin", "read one bridge command per line from stdin"),
            ArgSpec::flag("list", "list reachable app instances"),
            ArgSpec::value("tail", "n", "with `log`, print only the last N lines"),
        ],
        run: bridge::run,
    },
    Command {
        name: "perf",
        about: "performance monitor for a running app (profile / watch)",
        args: "profile|watch",
        options: &[
            ArgSpec::value("port", "n", "TCP port of the debug bridge"),
            ArgSpec::value("host", "addr", "host of the debug bridge"),
            ArgSpec::value("seconds", "n", "sample duration in seconds (profile)"),
            ArgSpec::value("interval-ms", "n", "poll interval in milliseconds (watch)"),
            ArgSpec::value(
                "duration-ms",
                "n",
                "total observation window in milliseconds (watch)",
            ),
        ],
        run: perf::run,
    },
    Command {
        name: "debug",
        about: "debugger for a running app (watch / tree / snapshot / diff)",
        args: "watch|tree|snapshot|diff <args>...",
        options: &[
            ArgSpec::value("port", "n", "TCP port of the debug bridge"),
            ArgSpec::value("host", "addr", "host of the debug bridge"),
            ArgSpec::value("interval-ms", "n", "poll interval in milliseconds"),
            ArgSpec::value(
                "duration-ms",
                "n",
                "total observation window in milliseconds",
            ),
            ArgSpec::value("depth", "n", "maximum child depth to print"),
            ArgSpec::flag("all", "print every node"),
        ],
        run: debug::run,
    },
];

pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// 渲染帮助。**从表里生成**，所以「帮助里有的」一定「真能跑」。
///
/// 选项那几行直接来自 [`Command::options`]：名字、是否取值、值名、说明都在
/// 那里声明一次。因此「加了选项忘了写帮助」和「写了帮助忘了实现」都不再可能。
pub fn render_help(bin: &str, version: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("{bin} {version}\nZephyraUI command line interface\n\nUSAGE\n    {bin} <command> [options]\n\nCOMMANDS\n"));
    let w = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in COMMANDS {
        s.push_str(&format!("    {:w$}    {}\n", c.name, c.about, w = w));
    }
    s.push_str("\nOPTIONS\n");
    s.push_str("    --help, -h        this text\n");
    s.push_str("    --version, -V     print version\n");
    s.push('\n');

    // 先把每个选项的展示形态算**一次**：列宽、USAGE、说明行都要用它，
    // 反复调 `display()` 会为每个选项分配三次 String。
    let displays: Vec<Vec<String>> = COMMANDS
        .iter()
        .map(|c| c.options.iter().map(ArgSpec::display).collect())
        .collect();
    // 选项列宽取全局最大值：跨命令对齐，扫起来不费劲。
    let ow = displays
        .iter()
        .flatten()
        .map(String::len)
        .max()
        .unwrap_or(0);

    for (c, ds) in COMMANDS.iter().zip(displays.iter()) {
        s.push_str(&format!("USAGE\n    {bin} {}", c.name));
        if !c.args.is_empty() {
            s.push(' ');
            s.push_str(c.args);
        }
        for d in ds {
            s.push_str(" [");
            s.push_str(d);
            s.push(']');
        }
        s.push('\n');
        if !ds.is_empty() {
            s.push('\n');
            for (d, o) in ds.iter().zip(c.options.iter()) {
                s.push_str(&format!("    {:ow$}    {}\n", d, o.help, ow = ow));
            }
        }
        s.push('\n');
    }
    push_exit_codes(&mut s);
    s
}

/// 单个子命令的详细帮助（`zephyra <cmd> --help`）。
///
/// 与全局帮助同源：命令的 about / args / options 全部来自命令表，所以这里
/// 不可能出现「帮助里写了但命令不认」的选项。
pub fn render_command_help(bin: &str, version: &str, cmd: &Command) -> String {
    let mut s = String::new();
    s.push_str(&format!("{bin} {version}\n\nUSAGE\n    {bin} {}", cmd.name));
    if !cmd.args.is_empty() {
        s.push(' ');
        s.push_str(cmd.args);
    }
    for o in cmd.options {
        s.push_str(" [");
        s.push_str(&o.display());
        s.push(']');
    }
    s.push_str("\n\n");
    s.push_str(cmd.about);
    s.push_str("\n\nOPTIONS\n");

    let mut rows: Vec<(String, &str)> = cmd.options.iter().map(|o| (o.display(), o.help)).collect();
    rows.push(("--help, -h".to_string(), "this text"));
    rows.push(("--version, -V".to_string(), "print version"));
    let ow = rows.iter().map(|(d, _)| d.len()).max().unwrap_or(0);
    for (display, help) in rows {
        s.push_str(&format!("    {:ow$}    {}\n", display, help, ow = ow));
    }
    s.push('\n');
    push_exit_codes(&mut s);
    s
}

fn push_exit_codes(s: &mut String) {
    s.push_str("EXIT CODES\n");
    s.push_str("    0  success\n");
    s.push_str("    1  the action failed (check found errors, build failed, ...)\n");
    s.push_str("    2  bad usage (unknown command, missing argument)\n");
    s.push_str("    3  environment is missing (no cargo, directory already exists)\n");
}

mod bridge;
mod build;
mod check;
mod debug;
mod dev;
mod fmt;
mod info;
mod mcp;
mod new;
mod perf;
mod props;

/// 找 cargo。没有它时给**明确**的缺环境错误，而不是让调用方看到一条奇怪的 spawn 失败。
pub fn cargo_path() -> Result<std::path::PathBuf, CliError> {
    // 先看 CARGO（cargo 跑我们时设的），再看 PATH 里的 cargo。
    if let Ok(p) = std::env::var("CARGO") {
        let pb = std::path::PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }
    which_cargo().ok_or_else(|| {
        CliError::missing(
            "cargo not found. Install Rust from https://rustup.rs and make sure `cargo` is on PATH.",
        )
    })
}

/// 工程根目录：`--manifest-path` 的父目录，否则 cwd。
///
/// `dev` 的源码/资源监视与窗口配置解析共用这一份判据，避免「配置在 A 目录、
/// 监视却盯着 B 目录」这种漂移。
pub fn project_root(a: &Args) -> Result<std::path::PathBuf, CliError> {
    match a.option("manifest-path") {
        Some(manifest) => {
            let manifest = std::path::PathBuf::from(manifest);
            let manifest = if manifest.is_absolute() {
                manifest
            } else {
                std::env::current_dir()
                    .map_err(|e| CliError::failure(format!("cannot get current directory: {e}")))?
                    .join(manifest)
            };
            Ok(manifest
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or(manifest))
        }
        None => std::env::current_dir()
            .map_err(|e| CliError::failure(format!("cannot get current directory: {e}"))),
    }
}

pub fn app_config_path(a: &Args) -> Result<Option<std::path::PathBuf>, CliError> {
    let root = project_root(a)?;
    let config = root.join("ZephyraUI.json");
    if !config.is_file() {
        return Ok(None);
    }
    std::fs::canonicalize(&config)
        .map(Some)
        .map_err(|e| CliError::failure(format!("cannot resolve {}: {e}", config.display())))
}

fn which_cargo() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let c = dir.join("cargo.exe");
        if c.exists() {
            return Some(c);
        }
        let c = dir.join("cargo");
        if c.exists() {
            return Some(c);
        }
    }
    None
}

/// 起一个子进程并把它的输出**继承**出去（直接透传，不缓冲）。
///
/// `log` 非空时额外把子进程的 stdout / stderr **落一份**到那个文件。
///
/// # 为什么落盘这一档要走管道
///
/// `status()` 让子进程直接继承本进程的 stdout/stderr —— 那是零拷贝、零延迟的，
/// 但**没法**再抄一份到文件。所以落盘这一档必须改成 `piped()` + 转发。
///
/// ⚠ 转发按**字节块**做（不是按行）：cargo 的进度输出用 `\r` 结尾而不带 `\n`，
/// 按行读会把「编译中 12%」这类进度憋到整行结束才吐出来 —— 那正是本函数
/// 的注释里说的「看起来半天才出来」。
///
/// ⚠ stdout 与 stderr 写进**同一个**文件，两股流可能在块边界处交错。
/// 这是刻意的：应用与 cargo 的诊断都在 stderr 里，分成两个文件反而要人自己对。
pub fn spawn_inherit<S: AsRef<std::ffi::OsStr>>(
    program: &std::path::Path,
    args: &[S],
    config_path: Option<&std::path::Path>,
    log: Option<&std::path::Path>,
) -> Result<ExitCode, CliError> {
    let Some(log_path) = log else {
        let mut command = std::process::Command::new(program);
        command.args(args);
        if let Some(path) = config_path {
            command.env("ZEPHYRA_APP_CONFIG_PATH", path);
        }
        // 告诉应用 MCP 在哪：应用启动后主动连入，agent 只经 stdio 与 MCP 说话。
        command.env(MCP_PORT_ENV, mcp_port_for_child());
        let status = command
            .status()
            .map_err(|e| CliError::missing(format!("cannot run {}: {e}", program.display())))?;
        return Ok(if status.success() {
            ExitCode::Success
        } else {
            ExitCode::Failure
        });
    };

    let mut child = spawn_pumped(program, args, config_path, Some(log_path))?;
    let status = child
        .wait()
        .map_err(|e| CliError::failure(format!("cannot wait for {}: {e}", program.display())))?;
    Ok(if status.success() {
        ExitCode::Success
    } else {
        ExitCode::Failure
    })
}

/// 一个**输出被转发**的子进程。
///
/// 「转发」= 子进程的 stdout / stderr 走管道，另开线程写到终端（以及可选的
/// 日志文件）。为什么要有这一档：`status()` 那种「直接继承」没法抄一份到
/// 文件，而 `--log` 的全部价值就在于那份副本。
pub struct PumpedChild {
    child: std::process::Child,
    pumps: Vec<std::thread::JoinHandle<()>>,
}

impl PumpedChild {
    /// 子进程是否已经结束（不阻塞）。
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// 结束子进程并**等转发线程收尾**。
    ///
    /// ⚠ 必须等转发线程：不等的话，子进程写完的最后一段（常常正是崩溃那一行）
    /// 会在父进程退出时被丢掉 —— 而那正是要留证据的地方。
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait();
        for pump in self.pumps.drain(..) {
            let _ = pump.join();
        }
        status
    }

    /// 杀掉子进程（不join 转发线程 —— 调用方接着会 `wait`）。
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

/// 起一个子进程，stdout/stderr 走管道并被转发。
///
/// `log` 非空时转发目标多一个文件（追加写）。
pub fn spawn_pumped<S: AsRef<std::ffi::OsStr>>(
    program: &std::path::Path,
    args: &[S],
    config_path: Option<&std::path::Path>,
    log: Option<&std::path::Path>,
) -> Result<PumpedChild, CliError> {
    let file = match log {
        Some(path) => {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| {
                    CliError::failure(format!("cannot open log {}: {e}", path.display()))
                })?;
            Some(std::sync::Arc::new(std::sync::Mutex::new(f)))
        }
        None => None,
    };

    let mut command = std::process::Command::new(program);
    command.args(args);
    if let Some(path) = config_path {
        command.env("ZEPHYRA_APP_CONFIG_PATH", path);
    }
    // 告诉应用 MCP 在哪：应用启动后主动连入，agent 只经 stdio 与 MCP 说话。
    command.env(MCP_PORT_ENV, mcp_port_for_child());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| CliError::missing(format!("cannot run {}: {e}", program.display())))?;

    let mut pumps = Vec::new();
    if let Some(out) = child.stdout.take() {
        pumps.push(pump(out, std::io::stdout(), file.clone()));
    }
    if let Some(err) = child.stderr.take() {
        pumps.push(pump(err, std::io::stderr(), file));
    }
    Ok(PumpedChild { child, pumps })
}

/// 把一条流**同时**写到终端与日志文件。
fn pump<R: std::io::Read + Send + 'static>(
    reader: R,
    mut terminal: impl std::io::Write + Send + 'static,
    log: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>>,
) -> std::thread::JoinHandle<()> {
    use std::io::Read;
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::with_capacity(8 * 1024, reader);
        let mut buf = [0u8; 8 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let _ = terminal.write_all(&buf[..n]);
            let _ = terminal.flush();
            if let Some(log) = &log {
                if let Ok(mut file) = log.lock() {
                    use std::io::Write;
                    let _ = file.write_all(&buf[..n]);
                    let _ = file.flush();
                }
            }
        }
    })
}
