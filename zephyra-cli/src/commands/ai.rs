//! `ai` — 给 agent 的结构化命令清单。
//!
//! 直接从 `COMMANDS` 表生成，所以帮助文本、工具描述和实际能力不可能漂移。
//! `zephyra ai` 输出全量清单；`zephyra ai <command>` 输出单命令详情。

use serde_json::{json, Value};

use super::{CliError, Write, COMMANDS};
use crate::args::{ArgKind, Args};
use crate::ExitCode;

/// 全局退出码含义，agent 据此区分「内容有问题」与「环境缺失」。
const EXIT_CODES: &[(&str, &str)] = &[
    ("0", "success"),
    (
        "1",
        "the action failed (check found errors, build failed, ...)",
    ),
    ("2", "bad usage (unknown command, missing argument)"),
    (
        "3",
        "environment is missing (no cargo, directory already exists)",
    ),
];

fn arg_kind(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Flag => "flag",
        ArgKind::Value => "value",
    }
}

fn command_json(cmd: &super::Command) -> Value {
    json!({
        "name": cmd.name,
        "about": cmd.about,
        "args": cmd.args,
        "options": cmd.options.iter().map(|spec| json!({
            "name": spec.name,
            "short": spec.short,
            "kind": arg_kind(spec.kind),
            "valueName": spec.value_name,
            "help": spec.help,
        })).collect::<Vec<_>>(),
    })
}

fn manifest() -> Value {
    json!({
        "name": "zephyra",
        "version": crate::VERSION,
        "globalFlags": ["--json"],
        "exitCodes": EXIT_CODES.iter().map(|(code, meaning)| json!({"code": code, "meaning": meaning})).collect::<Vec<_>>(),
        "commands": COMMANDS.iter().map(command_json).collect::<Vec<_>>(),
    })
}

pub fn run(args: &Args, w: &mut dyn Write) -> Result<ExitCode, CliError> {
    let manifest = match args.positional.first().map(String::as_str) {
        Some(name) => match COMMANDS.iter().find(|cmd| cmd.name == name) {
            Some(cmd) => command_json(cmd),
            None => {
                return Err(CliError::usage(format!(
                    "unknown command `{name}`; use `zephyra ai` for the full manifest"
                )))
            }
        },
        None => manifest(),
    };
    w.line(&serde_json::to_string(&manifest).unwrap());
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::ParseOutcome;

    #[test]
    fn manifest_is_generated_from_the_command_table() {
        let text = serde_json::to_string(&manifest()).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let commands = parsed["commands"].as_array().unwrap();
        assert_eq!(commands.len(), COMMANDS.len());
        assert!(commands.iter().any(|c| c["name"] == "ai"));
        assert!(commands.iter().any(|c| c["name"] == "mcp"));
        assert!(parsed["globalFlags"]
            .as_array()
            .unwrap()
            .contains(&json!("--json")));
    }

    #[test]
    fn single_command_detail_emits_that_command_only() {
        let ParseOutcome::Parsed(args) = crate::args::parse(["ai", "check"].map(String::from))
        else {
            panic!("expected Parsed");
        };
        let mut out = Vec::new();
        let code = run(
            &args,
            &mut crate::commands::Io {
                out: &mut out,
                err: &mut Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(code, ExitCode::Success);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v["name"], "check",
            "single-command manifest is the command object"
        );
        assert!(
            v["options"].is_array(),
            "command detail carries its options"
        );
    }

    #[test]
    fn unknown_command_detail_is_a_usage_error() {
        let ParseOutcome::Parsed(args) =
            crate::args::parse(["ai", "no-such-command"].map(String::from))
        else {
            panic!("expected Parsed");
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run(
            &args,
            &mut crate::commands::Io {
                out: &mut out,
                err: &mut err,
            },
        );
        assert!(result.is_err(), "unknown command must be a usage error");
        assert!(result.unwrap_err().message.contains("unknown command"));
    }
}
