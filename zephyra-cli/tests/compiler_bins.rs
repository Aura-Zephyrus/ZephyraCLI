//! zstylec / zypuic 两个独立二进制的端到端门禁。
//!
//! 这两个 bin 的逻辑（check/fmt/props/compile）全在 main.rs 里，无法直接调库函数测，
//! 所以这里用子进程跑真实二进制，验证退出码与输出形状。CI 依赖这些退出码，
//! 变异测试若改动了 main.rs 里的分支，这些用例会红。

use std::path::PathBuf;
use std::process::{Command, Output};

fn bin(name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(format!("CARGO_BIN_EXE_{name}")) {
        return Some(PathBuf::from(p));
    }
    // zstylec / zypuic 是 workspace 里别的包的 bin，Cargo 不会给本包设置
    // CARGO_BIN_EXE_*。从 CARGO_BIN_EXE_zephyra 的同级目录找 —— 那个 bin
    // 属于本包，一定会被构建（cargo-mutants 的临时 target 里也一样）。
    if let Some(anchor) = std::env::var_os("CARGO_BIN_EXE_zephyra").map(PathBuf::from) {
        let exe = anchor
            .parent()
            .unwrap()
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if exe.exists() {
            return Some(exe);
        }
    }
    // fallback：常规 target 目录（本地手动 cargo test 时）
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = workspace.parent().unwrap();
    for profile in ["debug", "release"] {
        let exe = workspace
            .join("target")
            .join(profile)
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if exe.exists() {
            return Some(exe);
        }
    }
    None
}

fn run(name: &str, args: &[&str]) -> Output {
    let path = match bin(name) {
        Some(p) => p,
        None => panic!("binary `{name}` not built; run `cargo build --workspace` first"),
    };
    Command::new(path)
        .args(args)
        .output()
        .expect("failed to spawn compiler binary")
}

/// 变异测试（cargo-mutants）在临时 target 里只构建被变异的包，不会构建
/// zstylec/zypuic 这两个外部包的 bin。这些测试测的是外部二进制的行为，
/// 与 zephyra-cli 源码的变异无关，缺失 bin 时跳过即可。
macro_rules! require_bins {
    ($($name:expr),+) => {
        $(if bin($name).is_none() {
            eprintln!("skip: `{}` binary not built in this target", $name);
            return;
        })+
    };
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn code(o: &Output) -> i32 {
    o.status.code().expect("process was killed")
}

fn scratch(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zephyra-bins-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for (name, body) in files {
        std::fs::write(d.join(name), body).unwrap();
    }
    d
}

// =============== zstylec ===============

#[test]
fn zstylec_help_and_version_exit_zero() {
    require_bins!("zstylec");
    let h = run("zstylec", &["--help"]);
    assert_eq!(code(&h), 0);
    assert!(stdout(&h).contains("zstylec"));
    let v = run("zstylec", &["--version"]);
    assert_eq!(code(&v), 0);
    assert!(stdout(&v).contains("zstylec"));
}

#[test]
fn zstylec_props_lists_sorted_full_table() {
    require_bins!("zstylec");
    let o = run("zstylec", &["props"]);
    assert_eq!(code(&o), 0);
    let out = stdout(&o);
    let names: Vec<&str> = out.lines().collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "props must be sorted");
    assert!(
        names.len() >= 80,
        "full property table expected, got {}",
        names.len()
    );
    assert!(names.contains(&"text-color"), "known prop must be present");
}

#[test]
fn zstylec_check_clean_exits_zero() {
    require_bins!("zstylec");
    let d = scratch(
        "zstylec-clean",
        &[("app.zstyle", "text {\n    text-color: #202020;\n}\n")],
    );
    let o = run(
        "zstylec",
        &["check", d.join("app.zstyle").to_str().unwrap()],
    );
    assert_eq!(code(&o), 0, "clean: {}", stderr(&o));
    assert!(stdout(&o).contains("ok"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn zstylec_check_error_exits_one_with_diagnostic() {
    require_bins!("zstylec");
    let d = scratch(
        "zstylec-err",
        &[("bad.zstyle", ".x {\n    fake-prop: 1;\n}\n")],
    );
    let o = run(
        "zstylec",
        &["check", d.join("bad.zstyle").to_str().unwrap()],
    );
    assert_eq!(code(&o), 1, "style error must exit 1");
    assert!(stderr(&o).contains("error") || stderr(&o).contains("unknown"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn zstylec_check_missing_file_exits_two() {
    require_bins!("zstylec");
    let o = run(
        "zstylec",
        &["check", "E:/zstylec-definitely-missing.zstyle"],
    );
    assert_eq!(code(&o), 2, "unreadable file must exit 2");
    assert!(stderr(&o).contains("cannot read"));
}

#[test]
fn zstylec_check_no_files_exits_two() {
    require_bins!("zstylec");
    let o = run("zstylec", &["check"]);
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("no input files"));
}

#[test]
fn zstylec_check_bad_format_exits_two() {
    require_bins!("zstylec");
    let o = run("zstylec", &["check", "--format", "yaml", "x.zstyle"]);
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("unknown --format"));
}

#[test]
fn zstylec_check_json_is_valid_and_reports_levels() {
    require_bins!("zstylec");
    let d = scratch(
        "zstylec-json",
        &[("bad.zstyle", ".x {\n    fake-prop: 1;\n}\n")],
    );
    let o = run(
        "zstylec",
        &[
            "check",
            "--format",
            "json",
            d.join("bad.zstyle").to_str().unwrap(),
        ],
    );
    assert_eq!(code(&o), 1);
    let v: serde_json::Value =
        serde_json::from_str(stdout(&o).trim()).expect("zstylec --format json must be valid JSON");
    let diags = v["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert!(!diags.is_empty());
    assert_eq!(diags[0]["level"], "error");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn zstylec_fmt_check_exits_one_when_dirty_and_zero_when_clean() {
    require_bins!("zstylec");
    // 合法规则但排版脏（单行、没换行）→ fmt 要改；注意属性必须合法。
    let d = scratch("zstylec-fmt", &[("a.zstyle", ".x { text-color: red; }")]);
    let f = d.join("a.zstyle");
    let check = run("zstylec", &["fmt", "--check", f.to_str().unwrap()]);
    assert_eq!(code(&check), 1, "dirty file -> exit 1");

    let apply = run("zstylec", &["fmt", f.to_str().unwrap()]);
    assert_eq!(code(&apply), 0, "formatting -> exit 0");
    let clean = run("zstylec", &["fmt", "--check", f.to_str().unwrap()]);
    assert_eq!(code(&clean), 0, "now clean -> exit 0");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn zstylec_check_prefix_is_optional_legacy_usage() {
    require_bins!("zstylec");
    // `zstylec foo.zstyle` == `zstylec check foo.zstyle`
    let d = scratch(
        "zstylec-legacy",
        &[("app.zstyle", "text {\n    text-color: #202020;\n}\n")],
    );
    let f = d.join("app.zstyle");
    let o = run("zstylec", &[f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "legacy usage must still work");
    let _ = std::fs::remove_dir_all(d);
}

// =============== zypuic ===============

const GOOD_ZYPUI: &str =
    "<page name=\"test\">\n    <box key=\"root\" class=\"toggle\"/>\n</page>\n";

#[test]
fn zypuic_help_and_version_exit_zero() {
    require_bins!("zypuic");
    let h = run("zypuic", &["--help"]);
    assert_eq!(code(&h), 0);
    assert!(stdout(&h).contains("zypuic"));
    let v = run("zypuic", &["--version"]);
    assert_eq!(code(&v), 0);
    assert!(stdout(&v).contains("zypuic"));
}

#[test]
fn zypuic_check_clean_exits_zero() {
    require_bins!("zypuic");
    let d = scratch("zypuic-clean", &[("test.zypui", GOOD_ZYPUI)]);
    let o = run("zypuic", &["check", d.join("test.zypui").to_str().unwrap()]);
    assert_eq!(code(&o), 0, "clean: {}", stderr(&o));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn zypuic_check_error_exits_one() {
    require_bins!("zypuic");
    let d = scratch("zypuic-err", &[("bad.zypui", "<page>\n</page>\n")]); // 缺 name
    let o = run("zypuic", &["check", d.join("bad.zypui").to_str().unwrap()]);
    assert_eq!(code(&o), 1, "bad ui file must exit 1");
    assert!(stderr(&o).contains("name") || stderr(&o).contains("error"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn zypuic_compile_produces_artifact_with_magic() {
    require_bins!("zypuic");
    let d = scratch("zypuic-compile", &[("test.zypui", GOOD_ZYPUI)]);
    let src = d.join("test.zypui");
    let out = d.join("out.bin");
    let o = run(
        "zypuic",
        &["compile", out.to_str().unwrap(), src.to_str().unwrap()],
    );
    assert_eq!(code(&o), 0, "compile: {}", stderr(&o));
    let bytes = std::fs::read(&out).expect("compile must write the artifact");
    assert!(bytes.len() > 8, "artifact too small: {} bytes", bytes.len());
    assert_eq!(
        &bytes[..4],
        b"ZYP1",
        "artifact must start with the magic: {:?}",
        &bytes[..4.min(bytes.len())]
    );
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn zypuic_fmt_check_exits_one_when_dirty_and_zero_when_clean() {
    require_bins!("zypuic");
    let d = scratch(
        "zypuic-fmt",
        &[("a.zypui", "<page name=\"x\"><box key=\"r\"/></page>")],
    );
    let f = d.join("a.zypui");
    let check = run("zypuic", &["fmt", "--check", f.to_str().unwrap()]);
    assert_eq!(code(&check), 1, "dirty ui file -> exit 1");
    let apply = run("zypuic", &["fmt", f.to_str().unwrap()]);
    assert_eq!(code(&apply), 0, "formatting -> exit 0");
    let clean = run("zypuic", &["fmt", "--check", f.to_str().unwrap()]);
    assert_eq!(code(&clean), 0, "now clean -> exit 0");
    let _ = std::fs::remove_dir_all(d);
}
