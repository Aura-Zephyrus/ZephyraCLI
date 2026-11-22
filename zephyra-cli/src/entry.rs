//! `main` 的真实实现。放在 lib 里，让两个薄壳（`zephyra` 与 `cargo-zephyra`）
//! 共用同一份 —— 之前它们都是各自指向 `src/main.rs`，Cargo 会为同一个 path
//! 编译两次并**告警**。
//!
//! 两个壳存在的原因：`cargo zephyra dev` 之所以能用，靠的是 Cargo 那个
//! 「`cargo-` + 子命令名」的查找约定 —— 它在 PATH 上找 **`cargo-zephyra`**，
//! 并把 `zephyra dev` 原样传进去。所以要有两个可执行文件名，而逻辑只能有一份。

use std::process::ExitCode as ProcessExit;

use crate::args::{self, ParseOutcome};
use crate::commands::{self, Io, Write};
use crate::ExitCode;

pub fn main_entry() -> ProcessExit {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    // `cargo zephyra dev` 会把 `zephyra` 一并传进来 —— 剥掉，两个入口就等价了。
    crate::strip_cargo_subcommand(&mut argv);

    let bin = display_name(std::env::args().next());

    // ⚠ 这里拿的是**未加锁**的 stdout/stderr 句柄，不是 `stdout.lock()` 的守卫。
    // `Stdout`/`Stderr` 的 `Write` 实现在每次 `write` 内部短暂加锁、写完即放，
    // 单线程命令下与「先锁住整个 dispatch」行为一致。而 MCP 是**长驻多线程**的：
    // 它的工作线程要直接写 stdout（`write_reply`）与 stderr，若主线程在
    // dispatch 全程握着那两把锁，工作线程会在第一次 `eprintln!`/`stdout.lock()`
    // 上永久死锁——实测：`initialize`/`ping`（主线程就地处理，重入锁成功）正常，
    // 所有 `tools/call`（投工作线程）无应答。所以锁必须在每次调用内自取自放。
    // `--json`：把所有输出收进内存，最后只吐一行 JSON 信封。agent 不必再解析
    // 人类可读的混合流；退出码与 stdout / stderr 分行返回。
    if argv.iter().any(|a| a == "--json") {
        let mut capture = Capture::default();
        let code = dispatch(&bin, argv, &mut capture);
        println!("{}", json_envelope(code, &capture));
        return ProcessExit::from(code.code());
    }

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut io = Io {
        out: &mut stdout,
        err: &mut stderr,
    };

    ProcessExit::from(dispatch(&bin, argv, &mut io).code())
}

fn json_envelope(code: ExitCode, capture: &Capture) -> String {
    serde_json::to_string(&serde_json::json!({
        "ok": code == ExitCode::Success,
        "exitCode": code.code(),
        "stdout": capture.out,
        "stderr": capture.err,
    }))
    .unwrap()
}

#[derive(Default)]
struct Capture {
    out: Vec<String>,
    err: Vec<String>,
}

impl Write for Capture {
    fn line(&mut self, s: &str) {
        self.out.push(s.to_string());
    }
    fn err_line(&mut self, s: &str) {
        self.err.push(s.to_string());
    }
}

/// 把名字列表渲染成 `--a --b`；空表给 `(none)`，免得报错里是一行空白。
fn render_names(names: Vec<&'static str>) -> String {
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names
            .iter()
            .map(|s| format!("--{s}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn dispatch(bin: &str, argv: Vec<String>, io: &mut dyn Write) -> ExitCode {
    let parsed = match args::parse(argv) {
        ParseOutcome::Help(Some(name)) => {
            if let Some(cmd) = commands::find(&name) {
                io.line(&commands::render_command_help(bin, crate::VERSION, cmd));
                return ExitCode::Success;
            }
            io.err_line(&format!("error: unknown command `{name}`"));
            io.err_line(&commands::render_help(bin, crate::VERSION));
            return ExitCode::Usage;
        }
        ParseOutcome::Help(None) => {
            io.line(&commands::render_help(bin, crate::VERSION));
            return ExitCode::Success;
        }
        ParseOutcome::Version => {
            io.line(&format!("{bin} {}", crate::VERSION));
            return ExitCode::Success;
        }
        ParseOutcome::Usage(m) => {
            io.err_line(&format!("error: {m}"));
            io.err_line(&format!("try `{bin} --help`"));
            return ExitCode::Usage;
        }
        ParseOutcome::Parsed(p) => p,
    };

    let Some(name) = parsed.command.as_deref() else {
        io.err_line(&commands::render_help(bin, crate::VERSION));
        return ExitCode::Usage;
    };

    let Some(cmd) = commands::find(name) else {
        io.err_line(&format!("error: unknown command `{name}`"));
        // 顺手给最接近的 —— 拼错命令名是这个 CLI 最常见的用法错误。
        for c in commands::COMMANDS {
            if c.name.starts_with(name) || name.starts_with(c.name) {
                io.err_line(&format!("  · did you mean `{}`?", c.name));
            }
        }
        io.err_line(&format!("try `{bin} --help`"));
        return ExitCode::Usage;
    };

    // ⚠ 未知标志**立刻**报用法错，不静默忽略 —— 见 `Command::options` 的说明。
    // `--` 之后的位置参数不算标志（它们原样透传给 cargo）。
    // `any` 而不是 `position(..).is_some()`：后者会在 clippy 1.93 上触发
    // `search_is_some`（`-D warnings` 直接红），而这里要的只是「有没有」。
    //
    // 门禁与解析器读的是**同一份**声明（`cmd.options`）：标志属不属于本命令、
    // 选项是否取值，都在命令表里声明一次。这样就不会出现「解析器把 2000 吃成
    // `--duration-ms` 的值、门禁却报 unknown option」这种自相矛盾的错。
    let after_ddash = parsed.positional.iter().any(|x| x == "--");
    if !after_ddash {
        for f in parsed.flags() {
            // `--json` 是全局输出模式，不属于任何具体命令。
            if f == "json" {
                continue;
            }

            if !cmd.accepts_flag(f) {
                io.err_line(&format!(
                    "error: unknown flag `--{f}` for `{bin} {}`",
                    cmd.name
                ));
                io.err_line(&format!(
                    "       accepted: {}",
                    render_names(cmd.accepted_flags())
                ));
                io.err_line(&format!("try `{bin} --help`"));
                return ExitCode::Usage;
            }
        }
        for (k, _) in parsed.options() {
            if !cmd.accepts_value(k) {
                io.err_line(&format!(
                    "error: unknown option `--{k}` for `{bin} {}`",
                    cmd.name
                ));
                io.err_line(&format!(
                    "       accepted: {}",
                    render_names(cmd.accepted_values())
                ));
                io.err_line(&format!("try `{bin} --help`"));
                return ExitCode::Usage;
            }
        }
    }

    match (cmd.run)(&parsed, io) {
        Ok(code) => code,
        Err(e) => {
            io.err_line(&format!("error: {}", e.message));
            e.code
        }
    }
}
/// 帮助里显示的名字。
///
/// `cargo zephyra dev` 时 `argv[0]` 是 `cargo-zephyra.exe`，而用户敲的是
/// `cargo zephyra dev` —— 原样打印会让帮助里出现一个**没人会敲**的命令
/// （照着抄会得到 `cargo-zephyra.exe dev`，而那个名字不保证在 PATH 上）。
///
/// # ⚠ 为什么**不**用 `Path::file_name()`
///
/// `Path` 用平台的分隔符：Unix 上 `\` 是普通字符。于是
/// `Path::new(r"E:\tools\zephyra.exe").file_name()` 在 Linux 上返回**整串**
/// （分隔符是 `/`），帮助里就会显示 `E:\tools\zephyra` —— 一个没人会敲、而且在
/// Linux 上根本不存在的东西。症状是「帮助里的名字对不上」，而它只在**跨平台那一侧**
/// 显形（本仓 2026-10 第一次在 Linux 上跑 `cargo test --workspace` 时被这条测试逮到）。
///
/// 而这不只是「测试数据写成了 Windows 路径」：WSL 互操作里从 bash 调 Windows 版
/// 可执行文件时，`argv[0]` **真的**就是 `E:\...`。所以两个分隔符都要认。
pub fn display_name(argv0: Option<String>) -> String {
    let raw = argv0
        .map(|s| {
            // 两个平台的分隔符都当分隔符，见函数文档。
            let tail = s.rsplit(['/', '\\']).next().unwrap_or(&s);
            if tail.is_empty() { "zephyra" } else { tail }.to_string()
        })
        .unwrap_or_else(|| "zephyra".to_string());
    let stem = raw.strip_suffix(".exe").unwrap_or(&raw);
    match stem.strip_prefix("cargo-") {
        Some(rest) => format!("cargo {rest}"),
        None => stem.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_envelope_separates_streams_and_exit_code() {
        let mut capture = Capture::default();
        capture.line("hello");
        capture.err_line("warn");
        let parsed: serde_json::Value =
            serde_json::from_str(&json_envelope(ExitCode::Failure, &capture)).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["exitCode"], 1);
        assert_eq!(parsed["stdout"][0], "hello");
        assert_eq!(parsed["stderr"][0], "warn");
    }

    #[test]
    fn global_json_flag_is_accepted_by_every_command() {
        let mut capture = Capture::default();
        let code = dispatch(
            "zephyra",
            vec!["info".into(), "--json".into()],
            &mut capture,
        );
        assert_eq!(code, ExitCode::Success);
        assert!(!capture.out.is_empty());
        assert!(capture.err.is_empty(), "{:?}", capture.err);
    }

    #[test]
    fn help_for_a_known_command_renders_scoped_help() {
        let mut capture = Capture::default();
        let code = dispatch(
            "zephyra",
            vec!["check".into(), "--help".into()],
            &mut capture,
        );
        assert_eq!(code, ExitCode::Success);
        assert!(capture.out.iter().any(|l| l.contains("check")));
        assert!(capture.err.is_empty(), "{:?}", capture.err);
    }

    #[test]
    fn help_for_an_unknown_command_is_a_usage_error() {
        let mut capture = Capture::default();
        let code = dispatch(
            "zephyra",
            vec!["totally-fake".into(), "--help".into()],
            &mut capture,
        );
        assert_eq!(code, ExitCode::Usage);
        assert!(capture.err.iter().any(|l| l.contains("unknown command")));
    }

    #[test]
    fn no_command_prints_help_and_returns_usage() {
        let mut capture = Capture::default();
        let code = dispatch("zephyra", vec![], &mut capture);
        assert_eq!(code, ExitCode::Usage);
        assert!(
            !capture.err.is_empty(),
            "bare invocation must print help to stderr"
        );
    }

    #[test]
    fn unknown_command_suggests_the_nearest_match() {
        let mut capture = Capture::default();
        // "bui" 是 "build" 的真前缀，应给出 did-you-mean。
        let code = dispatch("zephyra", vec!["bui".into()], &mut capture);
        assert_eq!(code, ExitCode::Usage);
        assert!(
            capture.err.iter().any(|l| l.contains("did you mean")),
            "must suggest a nearby command: {:?}",
            capture.err
        );
    }

    #[test]
    fn unknown_flag_for_a_command_is_rejected_not_ignored() {
        let mut capture = Capture::default();
        // --json 是全局的，但 --bogus 不属于 info。
        let code = dispatch(
            "zephyra",
            vec!["info".into(), "--bogus-flag".into()],
            &mut capture,
        );
        assert_eq!(code, ExitCode::Usage);
        assert!(
            capture.err.iter().any(|l| l.contains("unknown flag")),
            "unknown flag must be reported: {:?}",
            capture.err
        );
        assert!(
            capture.err.iter().any(|l| l.contains("accepted:")),
            "error must list accepted flags: {:?}",
            capture.err
        );
    }

    #[test]
    fn unknown_option_for_a_command_is_rejected_not_ignored() {
        let mut capture = Capture::default();
        // --format 是 check 的取值选项；build 不认识它 → unknown option。
        let code = dispatch(
            "zephyra",
            vec!["build".into(), "--format".into(), "x".into()],
            &mut capture,
        );
        assert_eq!(code, ExitCode::Usage);
        assert!(
            capture.err.iter().any(|l| l.contains("unknown option")),
            "unknown option must be reported: {:?}",
            capture.err
        );
    }

    #[test]
    fn version_prints_the_binary_name_and_version() {
        let mut capture = Capture::default();
        let code = dispatch("zephyra", vec!["--version".into()], &mut capture);
        assert_eq!(code, ExitCode::Success);
        assert!(
            capture.out.iter().any(|l| l.starts_with("zephyra ")),
            "version line: {:?}",
            capture.out
        );
    }
}
