//! `new` — scaffold a runnable ZephyraUI project.
//!
//! Templates live in `templates/app/`, not inside `format!` strings in this file.
//! `--force` only removes files from the `TEMPLATES` list, never arbitrary user files.

use super::{CliError, Write};
use crate::args::Args;
use crate::ExitCode;

const TEMPLATES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../../templates/app/Cargo.toml")),
    (
        "src/main.rs",
        include_str!("../../templates/app/src/main.rs"),
    ),
    (
        "frontend/app.rs",
        include_str!("../../templates/app/frontend/app.rs"),
    ),
    (
        "frontend/app.zstyle",
        include_str!("../../templates/app/frontend/app.zstyle"),
    ),
    (
        "zephyra.toml",
        include_str!("../../templates/app/zephyra.toml"),
    ),
    (
        "ZephyraUI.json",
        include_str!("../../templates/app/ZephyraUI.json"),
    ),
    ("README.md", include_str!("../../templates/app/README.md")),
    (".gitignore", include_str!("../../templates/app/.gitignore")),
];

/// 程序化入口的参数（MCP 工具直接构造，不必伪造 `Args`）。
pub(crate) struct CreateOptions {
    pub path: Option<std::path::PathBuf>,
    pub force: bool,
    pub no_agent: bool,
    pub git: bool,
    pub check: bool,
}

pub fn run(a: &Args, w: &mut dyn Write) -> Result<ExitCode, CliError> {
    let Some(name) = a.positional_at(0) else {
        return Err(CliError::usage(
            "new needs a project name. Try `zephyra new myapp`.",
        ));
    };
    create(
        name,
        &CreateOptions {
            path: a.option("path").map(std::path::PathBuf::from),
            force: a.has("force"),
            no_agent: a.has("no-agent"),
            git: a.has("git"),
            check: a.has("check"),
        },
        w,
    )?;
    Ok(ExitCode::Success)
}

pub(crate) fn create(
    name: &str,
    opts: &CreateOptions,
    w: &mut dyn Write,
) -> Result<std::path::PathBuf, CliError> {
    if name.trim().is_empty() {
        return Err(CliError::usage("project name is empty"));
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return Err(CliError::usage(format!(
            "`{name}` is not a valid crate name: it must start with a letter or `_`"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CliError::usage(format!(
            "`{name}` is not a valid crate name: only letters, digits, `_` and `-`"
        )));
    }

    let root = match &opts.path {
        Some(p) => p.join(name),
        None => std::path::PathBuf::from(name),
    };
    let force = opts.force;
    let no_agent = opts.no_agent;
    let git = opts.git;
    let check = opts.check;

    // Fail before generating anything if the sibling checkout is missing.
    let deps = DependencyPaths::resolve()?;
    let default_feature = if cfg!(windows) { "d3d12" } else { "vulkan" };
    let vars: Vec<(&str, String)> = vec![
        ("NAME", name.to_string()),
        ("DEFAULT_FEATURE", default_feature.to_string()),
        ("DEP_CORE", deps.core.clone()),
        ("DEP_FX", deps.fx.clone()),
        ("DEP_STYLE", deps.style.clone()),
        ("DEP_ZSTYLE", deps.zstyle_compile.clone()),
        ("DEP_D3D12", deps.d3d12.clone()),
        ("DEP_D3D11", deps.winit.clone()),
        ("DEP_VULKAN", deps.vulkan.clone()),
    ];

    if root.exists() {
        if !force {
            return Err(CliError::missing(format!(
                "{} already exists. Pass --force to overwrite the files `zephyra new` created.",
                root.display()
            )));
        }
        let mut files: Vec<String> = TEMPLATES.iter().map(|(p, _)| (*p).to_string()).collect();
        files.push(".opencode/opencode.jsonc".into());
        for rel in files {
            let p = root.join(&rel);
            if p.exists() {
                std::fs::remove_file(&p).map_err(|e| {
                    CliError::failure(format!("cannot remove {}: {e}", p.display()))
                })?;
            }
        }
    }

    std::fs::create_dir_all(root.join("src")).map_err(|e| {
        CliError::failure(format!("cannot create {}: {e}", root.join("src").display()))
    })?;
    std::fs::create_dir_all(root.join("frontend")).map_err(|e| {
        CliError::failure(format!(
            "cannot create {}: {e}",
            root.join("frontend").display()
        ))
    })?;

    for (rel, template) in TEMPLATES {
        write_file(&root.join(rel), &render(template, &vars))?;
    }

    if !no_agent {
        super::init::write_agent_files(&root, w)?;
    }
    if git && !root.join(".git").exists() {
        let status = std::process::Command::new("git")
            .arg("init")
            .current_dir(&root)
            .status()
            .map_err(|e| CliError::failure(format!("cannot run git init: {e}")))?;
        if !status.success() {
            return Err(CliError::failure("git init failed"));
        }
    }

    w.line(&format!("created {}", root.display()));
    w.line("next:");
    w.line(&format!("  cd {}", root.display()));
    w.line("  zephyra dev");

    if check {
        w.line("running cargo check ...");
        let cargo = super::cargo_path()?;
        let status = std::process::Command::new(cargo)
            .arg("check")
            .arg("--manifest-path")
            .arg(root.join("Cargo.toml"))
            .status()
            .map_err(|e| CliError::failure(format!("cannot run cargo check: {e}")))?;
        if !status.success() {
            return Err(CliError::failure(
                "cargo check failed for the generated project",
            ));
        }
        w.line("cargo check passed");
    }
    Ok(root)
}

fn render(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    out
}

fn write_file(p: &std::path::Path, contents: &str) -> Result<(), CliError> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::failure(format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(p, contents)
        .map_err(|e| CliError::failure(format!("cannot write {}: {e}", p.display())))
}

struct DependencyPaths {
    core: String,
    fx: String,
    style: String,
    zstyle_compile: String,
    d3d12: String,
    winit: String,
    vulkan: String,
}

impl DependencyPaths {
    fn resolve() -> Result<Self, CliError> {
        let paths = Self {
            core: crate_dep("zephyra/zephyra-core"),
            fx: crate_dep("zephyra/zephyra-fx"),
            style: crate_dep("zephyra/zephyra-style"),
            zstyle_compile: crate_dep("zstyle-compile"),
            d3d12: crate_dep("zephyra/zephyra-base/zephyra-winit-d3d12"),
            winit: crate_dep("zephyra/zephyra-base/zephyra-winit"),
            vulkan: crate_dep("zephyra/zephyra-base/zephyra-vulkan"),
        };
        for p in [
            &paths.core,
            &paths.fx,
            &paths.style,
            &paths.zstyle_compile,
            &paths.d3d12,
            &paths.winit,
            &paths.vulkan,
        ] {
            if !std::path::Path::new(p).join("Cargo.toml").exists() {
                return Err(CliError::missing(format!(
                    "ZephyraUI checkout not found at {p} — clone it next to ZephyraCLI first"
                )));
            }
        }
        Ok(paths)
    }
}

/// Absolute path into the sibling ZephyraUI checkout. Relative paths break when the
/// generated project is moved, so they are deliberately not used.
fn crate_dep(rel: &str) -> String {
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let upstream = here
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(here)
        .join("ZephyraUI");
    upstream.join(rel).to_string_lossy().replace('\\', "/")
}
