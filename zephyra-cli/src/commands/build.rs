//! `build` —— 转交 `cargo build`。
//!
//! # profile 规则
//!
//! - 无参数 ⇒ **release**（发布是 build 的主用途）
//! - `--debug` ⇒ debug
//! - `--release` ⇒ release（显式写法，与无参数等价）
//! - 两者同时给 ⇒ 用法错
//!
//! # `--sf`（single-file）
//!
//! 默认普通模式：不内嵌配置，也不传 `sf` feature —— app 运行时从磁盘读
//! `ZephyraUI.json` 与外部资源，动态库放在 exe 同目录。
//! `--sf`：内嵌配置并给 app 传 `--features sf`，产出自包含单文件。

use super::{app_config_path, cargo_path, spawn_inherit, CliError, Write};
use crate::args::Args;
use crate::ExitCode;

pub fn build(a: &Args, w: &mut dyn Write) -> Result<ExitCode, CliError> {
    let debug = a.has("debug");
    let release = a.has("release");
    if debug && release {
        return Err(CliError::usage(
            "--debug 与 --release 不能同时给：build 只选一种 profile",
        ));
    }
    let cargo = cargo_path()?;
    let mut args: Vec<String> = vec!["build".into()];
    if !debug {
        args.push("--release".into());
    }
    if let Some(p) = a.option("manifest-path") {
        args.push("--manifest-path".into());
        args.push(p.to_string());
    }
    // 单文件模式：让 build.rs 内嵌配置，并打开 app 的 `sf` feature（资源内嵌）。
    // 普通模式两样都不做 —— exe 里不塞配置和外部资源。
    let single_file = a.has("sf");
    if single_file {
        args.push("--features".into());
        args.push("sf".into());
    }

    // 链接模式交给环境变量：CLI 不猜 app 的依赖图，依赖 zephyra-video 的 app
    // 在 build.rs 里读它决定静态/动态库。普通模式动态，--sf 静态。
    // SAFETY: 命令刚启动、尚未创建工作线程；cargo 与其 build script 会继承。
    unsafe {
        std::env::set_var(
            "ZEPHYRA_LINK_MODE",
            if single_file { "static" } else { "dynamic" },
        );
    }

    w.err_line(&format!("$ cargo {}", args.join(" ")));
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let config = if single_file {
        app_config_path(a)?
    } else {
        None
    };
    // 输出落盘：终端照旧全量显示，另存一份供事后回查。
    let log = a.option("log").map(std::path::PathBuf::from);
    if let Some(path) = &log {
        w.err_line(&format!("[log] {}", path.display()));
    }
    spawn_inherit(&cargo, &refs, config.as_deref(), log.as_deref())
}
