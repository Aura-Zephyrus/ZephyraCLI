//! CLI ↔ 编译器库的契约门禁。
//!
//! `check` / `fmt` / `props` / `info` 都是「CLI 薄壳 + 库判据」的结构：
//! CLI 负责找文件、选输出格式、定退出码；判据来自 ZephyraCore 的编译器。
//! 这里的测试把 CLI 那层薄壳的行为钉住 —— 变异测试若改动了退出码映射、
//! JSON 转义、计数聚合、筛选/排序，这些用例会变红。
//!
//! 与 `tests/cli.rs` 的分工：那边测参数解析、脚手架、命令表；这边测
//! 「库函数结果 → CLI 输出/退出码」这一段。

use zephyra_cli::args::{self, ParseOutcome};
use zephyra_cli::commands::{self, Io};
use zephyra_cli::ExitCode;

/// 把一段参数解析成 Parsed，失败就 panic（测试里参数都是合法的）。
fn parsed(s: &str) -> args::Args {
    let av: Vec<String> = s.split_whitespace().map(|x| x.to_string()).collect();
    let ParseOutcome::Parsed(a) = args::parse(av) else {
        panic!("expected Parsed for `{s}`");
    };
    a
}

struct Cap {
    out: Vec<u8>,
    err: Vec<u8>,
}

impl Cap {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            err: Vec::new(),
        }
    }
    fn io(&mut self) -> Io<'_> {
        Io {
            out: &mut self.out,
            err: &mut self.err,
        }
    }
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.err).into_owned()
    }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("zephyra-cli-contract-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------- check: 退出码契约 ----------------

#[test]
fn check_clean_style_exits_success_and_reports_rules() {
    let d = temp_dir("check-clean");
    let f = d.join("app.zstyle");
    std::fs::write(&f, "text {\n    text-color: #202020;\n}\n").unwrap();

    let mut cap = Cap::new();
    let a = parsed(&format!("check {}", f.display()));
    let code = (commands::find("check").unwrap().run)(&a, &mut cap.io()).unwrap();

    assert_eq!(code, ExitCode::Success, "clean stylesheet must exit 0");
    assert!(
        cap.stdout().contains("ok"),
        "human output must mark clean file: {:?}",
        cap.stdout()
    );
    assert!(
        cap.stdout().contains("1 rules"),
        "rule count must be reported: {:?}",
        cap.stdout()
    );
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn check_style_with_error_exits_failure_and_renders_human_diagnostic() {
    let d = temp_dir("check-error");
    let f = d.join("bad.zstyle");
    // 未知属性 -> 库报 error -> CLI 退出码 1
    std::fs::write(&f, ".x {\n    totally-fake-prop: 1;\n}\n").unwrap();

    let mut cap = Cap::new();
    let a = parsed(&format!("check {}", f.display()));
    let code = (commands::find("check").unwrap().run)(&a, &mut cap.io()).unwrap();

    assert_eq!(code, ExitCode::Failure, "stylesheet with error must exit 1");
    let err = cap.stderr();
    assert!(
        err.contains("error") || err.contains("unknown"),
        "human diagnostic must name the problem: {err:?}"
    );
    assert!(
        err.contains("file(s):") && err.contains("error(s)"),
        "summary line must be present: {err:?}"
    );
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn check_missing_file_is_usage_error_not_style_failure() {
    let mut cap = Cap::new();
    let a = parsed("check E:/definitely-not-here-zephyra-cli.zstyle");
    let result = (commands::find("check").unwrap().run)(&a, &mut cap.io());

    // 读不到文件是用法错：Result 层是 Err(CliError::usage)
    assert!(result.is_err(), "missing file must be a usage error");
    let msg = result.unwrap_err().message;
    assert!(
        msg.contains("cannot read"),
        "error must say the file could not be read: {msg}"
    );
}

#[test]
fn check_bad_format_is_usage_error() {
    let mut cap = Cap::new();
    let a = parsed("check --format yaml app.zstyle");
    let result = (commands::find("check").unwrap().run)(&a, &mut cap.io());
    assert!(result.is_err());
    let msg = result.unwrap_err().message;
    assert!(msg.contains("unknown --format"), "got: {msg}");
}

#[test]
fn check_no_positional_is_usage_error() {
    let mut cap = Cap::new();
    let a = parsed("check");
    let result = (commands::find("check").unwrap().run)(&a, &mut cap.io());
    assert!(result.is_err());
    let msg = result.unwrap_err().message;
    assert!(msg.contains("at least one file"), "got: {msg}");
}

// ---------------- check: JSON 输出 ----------------

#[test]
fn check_json_output_is_well_formed_and_escapes_strings() {
    let d = temp_dir("check-json");
    let f = d.join("app.zstyle");
    std::fs::write(&f, "text {\n    text-color: #202020;\n}\n").unwrap();

    let mut cap = Cap::new();
    let a = parsed(&format!("check --format json {}", f.display()));
    let code = (commands::find("check").unwrap().run)(&a, &mut cap.io()).unwrap();
    assert_eq!(code, ExitCode::Success);

    let json = cap.stdout();
    assert!(
        json.trim_start().starts_with("{\"files\":["),
        "got: {json:?}"
    );
    let parsed_json: serde_json::Value = serde_json::from_str(json.trim()).expect("valid JSON");
    let files = parsed_json["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1, "one file reported");
    let serde_json::Value::String(name) = &files[0]["file"] else {
        panic!("file must be a JSON string");
    };
    assert!(name.ends_with("app.zstyle"), "file name preserved: {name}");

    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn check_json_error_file_reports_diagnostics_with_level() {
    let d = temp_dir("check-json-err");
    let f = d.join("bad.zstyle");
    std::fs::write(&f, ".x {\n    totally-fake-prop: 1;\n}\n").unwrap();

    let mut cap = Cap::new();
    let a = parsed(&format!("check --format json {}", f.display()));
    let code = (commands::find("check").unwrap().run)(&a, &mut cap.io()).unwrap();
    assert_eq!(code, ExitCode::Failure);

    let v: serde_json::Value = serde_json::from_str(cap.stdout().trim()).expect("valid JSON");
    let diags = v["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    assert!(!diags.is_empty(), "must list the error");
    assert_eq!(diags[0]["level"], "error", "level field present");
    assert!(
        diags[0]["message"].as_str().unwrap().contains("fake"),
        "message carries the diagnostic: {diags:?}"
    );
    let _ = std::fs::remove_dir_all(d);
}

// ---------------- props / info: 与库的属性表契约 ----------------

#[test]
fn props_lists_the_full_library_table_when_unfiltered() {
    let mut cap = Cap::new();
    let a = parsed("props");
    (commands::find("props").unwrap().run)(&a, &mut cap.io()).unwrap();

    let out = cap.stdout();
    let names: Vec<&str> = out
        .lines()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let expected = zstylec::supported_props();
    assert_eq!(
        names.len(),
        expected.len(),
        "props must print exactly the library table ({}): got {}",
        expected.len(),
        names.len()
    );
    for name in &expected {
        assert!(
            names.contains(name),
            "library prop `{name}` missing from CLI output"
        );
    }
}

#[test]
fn props_filter_uses_substring_and_exits_success_on_no_match() {
    let mut cap = Cap::new();
    let a = parsed("props definitely_no_such_prop_xyz");
    let code = (commands::find("props").unwrap().run)(&a, &mut cap.io()).unwrap();
    assert_eq!(code, ExitCode::Success, "no match is still success");
    assert!(
        cap.stdout().trim().is_empty(),
        "no match prints nothing: {:?}",
        cap.stdout()
    );
}

#[test]
fn info_reports_the_library_prop_count() {
    let mut cap = Cap::new();
    let a = parsed("info");
    (commands::find("info").unwrap().run)(&a, &mut cap.io()).unwrap();
    let out = cap.stdout();
    let expected = zstylec::supported_props().len();
    assert!(
        out.contains(&format!("{expected}")),
        "info must report the library's prop count ({expected}): {out:?}"
    );
    assert!(
        out.contains("zephyra-cli"),
        "info must show the CLI version: {out:?}"
    );
}
