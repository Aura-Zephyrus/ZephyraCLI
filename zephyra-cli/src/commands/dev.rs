//! `dev` —— 开发模式：带 trace 跑起来，并在源码/资源变化时热编译重启。
//!
//! # 和 `run` 的区别
//!
//! `run` 是「跑一次」。`dev` 多做两件事：
//!
//! 1. **打开 trace**（`ZEPHYRA_TRACE=1`）。不打开的话开发期最需要的那些行
//!    （视口变化、每帧四边形数、重建了没有）全都不打，而它们恰恰是排查
//!    「窗口变了但内容没跟上」的唯一判据。
//! 2. **Rust 热编译**：`.rs` / `Cargo.toml` 变化 ⇒ 杀掉旧进程、重新跑
//!    `cargo run`（增量编译就是 Rust 侧的「热编译」）。
//! 3. **资源热重载交给应用**：`.zstyle` / `.zypui` 变化**不重启进程** —— dev
//!    把资源目录经 `ZEPHYRA_DEV_ASSETS` 交给应用，应用自己在进程内替换样式表
//!    与 UI 实例（仅 debug 构建存在，release 里热重载代码编译期剔除）。窗口
//!    配置 `ZephyraUI.json` 变化仍重启。
//!
//! # 为什么现在也盯 `.rs`
//!
//! 之前的注释说「那是 cargo 自己的事，两个 watcher 会互相触发」—— 这个前提
//! 是错的：`cargo run` **不会**监视源码，它编译一次就退出把控制权交给应用。
//! 所以「改了 Rust 代码要手动 Ctrl-C 再跑一次」一直是真实痛点。现在按 tauri
//! dev 的形状自己做监视：杀进程 → 重跑 cargo（增量编译）→ 继续盯。
//!
//! 目标目录取 [`super::project_root`]，监视时跳过 `target/` / `.git/` /
//! `node_modules/` 等构建产物与依赖目录（与 tauri 的 `.taurignore` 默认项一致），
//! 并忽略 `Cargo.lock`（cargo 会自己改它，否则首次构建就会误触发一次重启）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::{
    app_config_path, bridge_port_for_child, cargo_path, project_root, spawn_pumped, CliError,
    PumpedChild, Write, BRIDGE_PORT_ENV, DEV_ASSETS_ENV, MCP_PORT_ENV,
};
use crate::args::Args;
use crate::ExitCode;

pub fn run(a: &Args, w: &mut dyn Write) -> Result<ExitCode, CliError> {
    let cargo = cargo_path()?;

    // trace 默认**开**。要关就显式 `--no-trace` —— 反过来（默认关、要显式开）
    // 会让人在没 trace 的情况下排查半天「为什么日志里什么都没有」。
    if !a.has("no-trace") {
        // SAFETY：这是本进程启动时、任何其它线程都还没起来的时候。
        //     unsafe 是因为 std::env::set_var 在多线程下不安全；这里两个条件都满足。
        unsafe {
            std::env::set_var("ZEPHYRA_TRACE", "1");
        }
        w.err_line("[dev] ZEPHYRA_TRACE=1");
    }

    let style_dir = a
        .option("styles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("styles"));

    let root = project_root(a)?;
    let config_path = app_config_path(a)?;
    let watch = !a.has("no-watch");

    // 资源监视交给应用自己做（进程内热重载）；dev 这里只盯 Rust 源码与
    // manifest，后者变了才整进程重启（热编译）。`--no-watch` 时**不注入**，
    // 否则 dev 自己不看了、应用却还在热更新，与标志语义冲突。
    if watch {
        let assets = std::env::join_paths([&root, &style_dir])
            .map_err(|e| CliError::failure(format!("cannot build dev asset paths: {e}")))?;
        // SAFETY: dev 进程启动初期，尚未创建工作线程。
        unsafe {
            std::env::set_var(DEV_ASSETS_ENV, assets);
        }
    }

    // dev 永远编 debug：要 release 用 `zephyra build --release`。
    let mut cmd: Vec<String> = vec!["run".into()];
    if let Some(p) = a.option("manifest-path") {
        cmd.push("--manifest-path".into());
        cmd.push(p.to_string());
    }

    w.err_line(&format!("[dev] cargo {} (debug)", cmd.join(" ")));
    if watch {
        // 明确分工，避免用户改了 .zstyle 后盯着「为什么没重启」。
        w.err_line("[dev] restart on change: .rs / Cargo.toml / zephyra.toml / ZephyraUI.json");
        w.err_line(&format!(
            "[dev] hot reload in app: .zstyle / .zypui (given {} and {}); Ctrl-C to stop",
            root.display(),
            style_dir.display()
        ));
    }

    // 缓存式监视器：稳态下只 stat 已知文件，不重建路径表（见 WatchState）。
    let mut watch = watch.then(|| WatchState::new(&root, &style_dir, config_path.as_deref()));

    // `--log <path>`：把子进程的输出**另外**落一份（终端照旧全量显示）。
    // 落盘那份是为了「滚过去了 / 崩了 / 当时没看屏幕」之后还能回查。
    let log = a.option("log").map(PathBuf::from);
    if let Some(path) = &log {
        w.err_line(&format!("[log] {}", path.display()));
    }

    let mut child = match spawn_cargo(&cargo, &cmd, config_path.as_deref(), log.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliError::missing(e.message));
        }
    };

    let mut last_check = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(if status.success() {
                    ExitCode::Success
                } else {
                    ExitCode::Failure
                });
            }
            Ok(None) => {}
            Err(e) => {
                return Err(CliError::failure(format!("cannot wait for child: {e}")));
            }
        }

        // 轮询而不是用通知 API：改动**很**稀疏，而轮询的代价已经被 WatchState
        // 压到「每 700ms 对已知文件 stat 一遍」（无分配、无排序）；用通知 API
        // 反而要处理漏事件、去抖、重连。tauri dev 用的是 1s 去抖，这里同量级。
        if let Some(state) = watch.as_mut() {
            if last_check.elapsed() >= POLL_INTERVAL {
                last_check = Instant::now();
                if state.poll() {
                    w.err_line("[dev] source changed, rebuilding and restarting");
                    child.stop();
                    child = match spawn_cargo(&cargo, &cmd, config_path.as_deref(), log.as_deref())
                    {
                        Ok(c) => c,
                        Err(e) => {
                            return Err(CliError::missing(format!(
                                "cannot restart: {}",
                                e.message
                            )));
                        }
                    };
                    // ⚠ 重启后**继续**盯，而不是退出循环 —— 上一版在这里 return，
                    // 结果「改了样式就重启一次、然后命令就结束了」，第二次改动再也不管。
                    // 那一版的症状极像「热重载只生效一次」。
                }
            }
        }
        // 睡到下次轮询截止（上限 200ms 保证子进程退出还能及时被发现），
        // 而不是固定 50ms 空转 —— 空闲时唤醒次数从 20/s 降到约 4/s。
        let sleep = if watch.is_some() {
            POLL_INTERVAL
                .saturating_sub(last_check.elapsed())
                .min(Duration::from_millis(200))
                .max(Duration::from_millis(10))
        } else {
            Duration::from_millis(50)
        };
        std::thread::sleep(sleep);
    }
}

/// dev 起的子进程。
///
/// 两种形态：**没有** `--log` 时直接继承输出（零线程、零延迟）；**有** `--log`
/// 时走管道转发，顺带落一份盘。
enum DevChild {
    Plain(std::process::Child),
    Pumped(PumpedChild),
}

impl DevChild {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            DevChild::Plain(c) => c.try_wait(),
            DevChild::Pumped(c) => c.try_wait(),
        }
    }

    /// 杀掉子进程并**收尾**。
    ///
    /// ⚠ 转发线程也要 join（`Pumped` 那一支）：不等的话，子进程写完的最后
    /// 一段（常常正是崩溃那一行）会丢 —— 而那正是要留证据的地方。
    fn stop(&mut self) {
        match self {
            DevChild::Plain(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            DevChild::Pumped(c) => {
                c.kill();
                let _ = c.wait();
            }
        }
    }
}

/// 起 cargo 子进程。`log` 非空 ⇒ 输出走管道并落盘；否则直接继承。
fn spawn_cargo(
    cargo: &Path,
    args: &[String],
    config_path: Option<&Path>,
    log: Option<&Path>,
) -> Result<DevChild, CliError> {
    if log.is_some() {
        let child = spawn_pumped(cargo, args, config_path, log)?;
        return Ok(DevChild::Pumped(child));
    }
    let mut command = std::process::Command::new(cargo);
    command.args(args);
    if let Some(path) = config_path {
        command.env("ZEPHYRA_APP_CONFIG_PATH", path);
    }
    // 应用监听这个基址（每窗口 +id）；MCP 进程会连出来。
    command.env(BRIDGE_PORT_ENV, bridge_port_for_child());
    command.env_remove(MCP_PORT_ENV);
    let child = command
        .spawn()
        .map_err(|e| CliError::missing(format!("cannot run {}: {e}", cargo.display())))?;
    Ok(DevChild::Plain(child))
}

/// 轮询间隔。tauri dev 用 1s 去抖，这里同量级。
const POLL_INTERVAL: Duration = Duration::from_millis(700);

/// 缓存式监视状态。
///
/// 之前的做法每 700ms 重建整张表：`read_dir` 递归 + 每个文件一个 `PathBuf`
/// 分配 + 排序 + 去重 + 两份 Vec 比较。改动其实很稀疏，绝大多数开销是白费的。
///
/// 现在分两层：
///
/// 1. **结构变化**（文件增删/改名/新目录）⇒ 目录 mtime 会变，才重扫建表；
/// 2. **内容变化**（原地写入）⇒ 目录 mtime 不变，稳态只 `stat` 缓存里的已知
///    路径，零分配、零排序。
///
/// 判据仍是 mtime 而非内容：编辑器分两遍保存不该触发两次重启，按内容看则会。
struct WatchState {
    /// 扫描根（工程根 + 工程外的 `--styles`）。
    roots: Vec<PathBuf>,
    config_path: Option<PathBuf>,
    /// 被监视目录 → 上次 mtime。`None` = 读不到（目录消失等）。
    dirs: HashMap<PathBuf, Option<SystemTime>>,
    /// 被监视文件 → 上次 mtime。稳态只对这里面的路径做 `stat`。
    files: HashMap<PathBuf, SystemTime>,
    config_mtime: Option<SystemTime>,
}

impl WatchState {
    fn new(root: &Path, style_dir: &Path, config_path: Option<&Path>) -> Self {
        let roots = if style_dir == root {
            vec![root.to_path_buf()]
        } else {
            vec![root.to_path_buf(), style_dir.to_path_buf()]
        };
        let mut state = Self {
            roots,
            config_path: config_path.map(Path::to_path_buf),
            dirs: HashMap::new(),
            files: HashMap::new(),
            config_mtime: config_path.and_then(mtime),
        };
        state.rescan();
        state
    }

    /// 完全重扫（仅在目录结构变化时调用，不在稳态路径上）。
    fn rescan(&mut self) {
        self.dirs.clear();
        self.files.clear();
        // 先克隆根，避免写 self.dirs/files 时与借用冲突。
        let roots = self.roots.clone();
        for r in &roots {
            collect_watch_dir(r, &mut self.dirs, &mut self.files, 0);
        }
    }

    /// 一次轮询：检测到变化返回 `true`，并更新内部快照。
    fn poll(&mut self) -> bool {
        // 1. 结构变化：任何目录 mtime 变了就重扫，本轮直接报变化。
        let mut structure_changed = false;
        for (path, stamp) in &mut self.dirs {
            let now = mtime(path);
            if now != *stamp {
                *stamp = now;
                structure_changed = true;
            }
        }
        if structure_changed {
            self.rescan();
            return true;
        }

        // 2. 内容变化：稳态路径，只 stat 已知文件（不分配 PathBuf）。
        let mut changed = false;
        let mut missing = Vec::new();
        for (path, stamp) in &mut self.files {
            match mtime(path) {
                Some(now) if now != *stamp => {
                    *stamp = now;
                    changed = true;
                }
                Some(_) => {}
                None => missing.push(path.clone()),
            }
        }
        for path in missing {
            self.files.remove(&path);
            changed = true;
        }

        // 3. 窗口配置可能在工程目录之外，单独盯。
        if let Some(path) = &self.config_path {
            let now = mtime(path);
            if now != self.config_mtime {
                self.config_mtime = now;
                changed = true;
            }
        }
        changed
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// 一次性快照，给测试用（断言监视集合）。生产路径用 [`WatchState`] 的增量轮询。
#[cfg(test)]
fn snapshot(
    root: &Path,
    style_dir: &Path,
    config_path: Option<&Path>,
) -> Vec<(PathBuf, SystemTime)> {
    let state = WatchState::new(root, style_dir, config_path);
    let mut out: Vec<(PathBuf, SystemTime)> =
        state.files.iter().map(|(p, t)| (p.clone(), *t)).collect();
    if let (Some(path), Some(t)) = (&state.config_path, state.config_mtime) {
        out.push((path.clone(), t));
    }
    out.sort();
    out.dedup();
    out
}

/// 对应 tauri 的 `tauri-dev-watcher.gitignore` 默认忽略项，外加编辑器目录。
const IGNORED_DIRS: &[&str] = &["target", ".git", "node_modules", "gen", ".idea", ".vscode"];
/// 递归深度上限，异常目录结构不该把轮询拖死。
const MAX_WATCH_DEPTH: usize = 16;

fn collect_watch_dir(
    dir: &Path,
    dirs: &mut HashMap<PathBuf, Option<SystemTime>>,
    files: &mut HashMap<PathBuf, SystemTime>,
    depth: usize,
) {
    if depth > MAX_WATCH_DEPTH {
        return;
    }
    dirs.entry(dir.to_path_buf()).or_insert_with(|| mtime(dir));
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if ft.is_dir() {
            if e.file_name()
                .to_str()
                .is_some_and(|n| IGNORED_DIRS.contains(&n))
            {
                continue;
            }
            collect_watch_dir(&p, dirs, files, depth + 1);
            continue;
        }
        // 软链等不跟随，避免环；只认普通文件。
        if !ft.is_file() {
            continue;
        }
        let watched = matches!(
            p.extension().and_then(|s| s.to_str()),
            // .zstyle / .zypui 由应用侧在进程内热重载，不再触发重启。
            Some("rs")
        ) || p
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| matches!(n, "Cargo.toml" | "zephyra.toml"));
        if watched {
            if let Some(t) = mtime(&p) {
                files.insert(p, t);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "zephyra-dev-watch-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn snapshot_watches_sources_and_ignores_build_output() {
        let root = scratch("sources");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("frontend")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(root.join("Cargo.lock"), "lock\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("frontend/app.zstyle"), "a{}\n").unwrap();
        std::fs::write(root.join("frontend/app.zypui"), "<component/>\n").unwrap();
        std::fs::write(root.join("target/debug/app.rs"), "junk\n").unwrap();
        std::fs::write(root.join("notes.txt"), "not watched\n").unwrap();

        // 用 PathBuf 比较，别比字符串：Windows 上 `to_string_lossy()` 给的是 `src\main.rs`。
        let paths: Vec<PathBuf> = snapshot(&root, &root, None)
            .iter()
            .map(|(p, _)| p.strip_prefix(&root).unwrap().to_path_buf())
            .collect();
        for expected in ["src/main.rs", "Cargo.toml"] {
            assert!(
                paths.contains(&PathBuf::from(expected)),
                "缺 {expected}: {paths:?}"
            );
        }
        // .zstyle / .zypui 由应用进程内热重载，dev 不该因它们重启。
        for unwanted in [
            "Cargo.lock",
            "notes.txt",
            "frontend/app.zstyle",
            "frontend/app.zypui",
        ] {
            assert!(
                !paths.contains(&PathBuf::from(unwanted)),
                "不该盯 {unwanted}: {paths:?}"
            );
        }
        assert!(
            !paths.iter().any(|p| p.starts_with("target")),
            "构建产物不该触发重启: {paths:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshot_detects_added_and_removed_source_files() {
        let root = scratch("delta");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let before = snapshot(&root, &root, None);

        std::fs::write(root.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        assert_ne!(
            snapshot(&root, &root, None),
            before,
            "新增 .rs 必须被监视到"
        );

        std::fs::remove_file(root.join("src/lib.rs")).unwrap();
        assert_eq!(snapshot(&root, &root, None), before, "删掉后应回到原状");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshot_includes_external_rust_dir_and_config() {
        let root = scratch("external-root");
        let extra = scratch("external-rust");
        let config = scratch("config").join("ZephyraUI.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(extra.join("extra.rs"), "fn x() {}\n").unwrap();
        std::fs::write(&config, "{}").unwrap();

        let snap = snapshot(&root, &extra, Some(&config));
        assert!(snap.iter().any(|(p, _)| p == &extra.join("extra.rs")));
        assert!(snap.iter().any(|(p, _)| p == &config));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&extra);
        let _ = std::fs::remove_dir_all(config.parent().unwrap());
    }

    /// 稳态轮询必须安静；原地写文件内容要能被检出，且只报一次。
    #[test]
    fn watch_state_is_quiet_when_idle_and_detects_content_edits() {
        let root = scratch("idle");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let src = root.join("src/main.rs");
        std::fs::write(&src, "fn main() {}\n").unwrap();
        let mut state = WatchState::new(&root, &root, None);

        assert!(!state.poll(), "没有变化时不该报变化");
        assert!(!state.poll(), "连续轮询都应该安静");

        // 显式改 mtime，绕开文件系统的时间粒度。
        let later = SystemTime::now() + Duration::from_secs(60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(state.poll(), "原地写入必须被检出");
        assert!(!state.poll(), "报过一次后应回到安静");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 文件增删（结构变化）与工程外配置变化都要被检出。
    #[test]
    fn watch_state_detects_add_remove_and_config_changes() {
        let root = scratch("structure");
        let config = scratch("cfg").join("ZephyraUI.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "{}").unwrap();
        let mut state = WatchState::new(&root, &root, Some(&config));

        let added = root.join("new.rs");
        std::fs::write(&added, "fn x() {}\n").unwrap();
        assert!(state.poll(), "新增文件必须被检出（目录 mtime 变化）");
        assert!(!state.poll());
        assert!(state.files.contains_key(&added), "新文件应进入缓存");

        std::fs::remove_file(&added).unwrap();
        assert!(state.poll(), "删除文件必须被检出");
        assert!(!state.files.contains_key(&added));

        let later = SystemTime::now() + Duration::from_secs(60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&config)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(state.poll(), "配置变化必须被检出");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(config.parent().unwrap());
    }
}
