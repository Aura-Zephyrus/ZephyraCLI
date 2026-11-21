//! `init` — add project-level MCP config, skill and .gitignore to an existing directory.
//!
//! `new` reuses `write_agent_files` so the two paths never diverge.

use super::{CliError, Write};
use crate::args::Args;
use crate::ExitCode;

const SKILL_MD: &str = include_str!("../../../skills/zephyra-mcp/SKILL.md");
const SKILL_TOOLS: &str = include_str!("../../../skills/zephyra-mcp/references/tools.md");
const SKILL_OUTPUT: &str = include_str!("../../../skills/zephyra-mcp/references/output-formats.md");
const SKILL_TROUBLESHOOTING: &str =
    include_str!("../../../skills/zephyra-mcp/references/troubleshooting.md");
const GITIGNORE: &str = include_str!("../../templates/app/.gitignore");
const OPENCODE_CONFIG: &str = include_str!("../../templates/app/.opencode/opencode.jsonc");

fn write_file(path: &std::path::Path, text: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::failure(format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(path, text)
        .map_err(|e| CliError::failure(format!("cannot write {}: {e}", path.display())))
}

/// Project-level MCP config + skill. Existing config is never overwritten.
pub(crate) fn write_agent_files(root: &std::path::Path, w: &mut dyn Write) -> Result<(), CliError> {
    let config = root.join(".opencode/opencode.jsonc");
    if config.exists() {
        let text = std::fs::read_to_string(&config)
            .map_err(|e| CliError::failure(format!("cannot read {}: {e}", config.display())))?;
        if text.contains("\"zephyra\"") {
            w.line(&format!("kept existing {}", config.display()));
        } else {
            w.line(&format!(
                "{} exists without a zephyra server; add mcp.servers.zephyra yourself",
                config.display()
            ));
        }
    } else {
        write_file(&config, OPENCODE_CONFIG)?;
        w.line(&format!("wrote {}", config.display()));
    }

    let skill = root.join(".opencode/skills/zephyra-mcp");
    write_file(&skill.join("SKILL.md"), SKILL_MD)?;
    write_file(&skill.join("references/tools.md"), SKILL_TOOLS)?;
    write_file(&skill.join("references/output-formats.md"), SKILL_OUTPUT)?;
    write_file(
        &skill.join("references/troubleshooting.md"),
        SKILL_TROUBLESHOOTING,
    )?;
    w.line(&format!("installed agent skill in {}", skill.display()));
    Ok(())
}

pub fn run(args: &Args, w: &mut dyn Write) -> Result<ExitCode, CliError> {
    let root = match args.positional.first() {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()
            .map_err(|e| CliError::failure(format!("cannot read cwd: {e}")))?,
    };
    initialize(&root, args.has("no-agent"), args.has("git"), w)?;
    Ok(ExitCode::Success)
}

pub(crate) fn initialize(
    root: &std::path::Path,
    no_agent: bool,
    git: bool,
    w: &mut dyn Write,
) -> Result<(), CliError> {
    if !root.exists() {
        return Err(CliError::missing(format!(
            "{} does not exist",
            root.display()
        )));
    }

    if !no_agent {
        write_agent_files(root, w)?;
    }

    let ignore = root.join(".gitignore");
    if !ignore.exists() {
        write_file(&ignore, GITIGNORE)?;
        w.line(&format!("wrote {}", ignore.display()));
    }

    if git && !root.join(".git").exists() {
        let status = std::process::Command::new("git")
            .arg("init")
            .current_dir(root)
            .status()
            .map_err(|e| CliError::failure(format!("cannot run git init: {e}")))?;
        if !status.success() {
            return Err(CliError::failure("git init failed"));
        }
        w.line(&format!("initialized git in {}", root.display()));
    }

    Ok(())
}
