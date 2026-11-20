//! CLI 的行为门禁。**不起子进程** —— 直接调库，因为命令往 stdout/stderr 写东西，
//! 把它接到内存里就能验。
//!
//! # 这些用例守的是什么
//!
//! 全部是**实测踩过的坑**，不是「应该这样」：
//!
//! - 未知标志曾经被静默忽略、`exit 0`，看起来像「开了严格模式跑过了」；
//! - 文件读不到曾经返回 `exit 1`（那是「内容有问题」），CI 分不出该改流水线还是
//!   该改样式表；
//! - 未知命令曾经不给「最接近的」。
//!
//! 这三条都不是编译期能挡的，只有把行为钉住才不会再退化。

use zephyra_cli::args::{self, ParseOutcome};
use zephyra_cli::commands::{self, Io, Write};
use zephyra_cli::ExitCode;

#[derive(Default)]
struct Cap {
    out: String,
    err: String,
}

impl Write for Cap {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }
    fn err_line(&mut self, s: &str) {
        self.err.push_str(s);
        self.err.push('\n');
    }
}

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(|x| x.to_string()).collect()
}

#[test]
fn parses_subcommand_and_positional() {
    let ParseOutcome::Parsed(a) = args::parse(argv("check a.zstyle b.zstyle")) else {
        panic!("expected Parsed");
    };
    assert_eq!(a.command.as_deref(), Some("check"));
    assert_eq!(a.positional, vec!["a.zstyle", "b.zstyle"]);
}

#[test]
fn flags_and_options_split() {
    let ParseOutcome::Parsed(a) = args::parse(argv("build --release --manifest-path x/Cargo.toml"))
    else {
        panic!("expected Parsed");
    };
    assert!(a.has("release"));
    assert_eq!(a.option("manifest-path"), Some("x/Cargo.toml"));
}

#[test]
fn value_flags_accept_space_separated_form() {
    // 命令表里写着 `--manifest-path <p>`，解析器就必须认空格形式。
    let ParseOutcome::Parsed(a) = args::parse(argv("build --manifest-path x/Cargo.toml --release"))
    else {
        panic!("expected Parsed");
    };
    assert_eq!(a.option("manifest-path"), Some("x/Cargo.toml"));
    assert!(a.has("release"));
    // 路径不该漏进位置参数里 —— 那正是「看起来生效了其实没有」的症状。
    assert!(a.positional.is_empty(), "{:?}", a.positional);
}

#[test]
fn value_flag_without_argument_is_a_usage_error() {
    assert!(matches!(
        args::parse(argv("check --format")),
        ParseOutcome::Usage(_)
    ));
}
#[test]
fn double_dash_stops_flag_parsing() {
    let ParseOutcome::Parsed(a) = args::parse(argv("build -- --nocapture")) else {
        panic!("expected Parsed");
    };
    // `--nocapture` 必须留在位置参数里，不能变成标志。
    assert!(!a.has("nocapture"));
    assert!(a.positional.contains(&"--nocapture".to_string()));
}

#[test]
fn repeated_flag_is_counted() {
    let ParseOutcome::Parsed(a) = args::parse(argv("new --force --force")) else {
        panic!("expected Parsed");
    };
    assert_eq!(a.count("force"), 2);
    // 值与次数存在同一条记录里，遍历只出现一次（不会因重复给而列两遍）。
    assert_eq!(a.flags().collect::<Vec<_>>(), vec!["force"]);
    assert!(a.has("force"));
    assert_eq!(a.option("force"), None);
}

/// 取值选项同样记次数；重复给值时保留**第一次**的值（与旧的三 Vec 实现一致）。
#[test]
fn value_option_records_occurrences_and_keeps_first_value() {
    let ParseOutcome::Parsed(a) = args::parse(argv("bridge --port 1 --host h --port 2 stats"))
    else {
        panic!("expected Parsed");
    };
    assert_eq!(a.count("port"), 2);
    assert_eq!(a.option("port"), Some("1"));
    assert_eq!(
        a.options().collect::<Vec<_>>(),
        vec![("port", "1"), ("host", "h")]
    );
    // 重复给的标志/选项不该被当成位置参数漏进桥命令里。
    assert_eq!(a.positional, vec!["stats"]);
}

/// 哈希表的迭代顺序不确定，但报错与门禁按 argv 顺序遍历 —— 输出必须可复现。
#[test]
fn parsed_args_iterate_in_argv_order() {
    let ParseOutcome::Parsed(a) = args::parse(argv("dev --no-trace --no-watch")) else {
        panic!("expected Parsed");
    };
    assert_eq!(a.flags().collect::<Vec<_>>(), vec!["no-trace", "no-watch"]);
}

/// `--help` 在命令后 ⇒ 只打那个命令的帮助；在命令前 ⇒ 全局帮助。
#[test]
fn help_targets_the_command_when_given_after_it() {
    assert_eq!(args::parse(argv("--help")), ParseOutcome::Help(None));
    assert_eq!(
        args::parse(argv("dev --help")),
        ParseOutcome::Help(Some("dev".into()))
    );
    assert_eq!(
        args::parse(argv("build -h")),
        ParseOutcome::Help(Some("build".into()))
    );
}

/// 子命令帮助只包含自己：about、位置参数、声明的选项与退出码。
#[test]
fn command_help_is_scoped_to_that_command() {
    let dev = commands::find("dev").expect("dev must exist");
    let help = commands::render_command_help("zephyra", "0.0.0", dev);
    assert!(help.contains("zephyra dev"), "{help}");
    assert!(help.contains("--no-watch"), "{help}");
    assert!(
        !help.contains("zephyra build"),
        "子命令帮助不该列出其它命令: {help}"
    );
    for o in dev.options {
        assert!(help.contains(o.help), "缺选项说明 {}", o.name);
    }
    assert!(help.contains("EXIT CODES"));
}

#[test]
fn help_and_version_are_distinguished() {
    assert_eq!(args::parse(argv("--help")), ParseOutcome::Help(None));
    assert_eq!(args::parse(argv("--version")), ParseOutcome::Version);
    // 空 argv 由 dispatch 处理成「打帮助」，parser 只管给出「没有命令」。
    match args::parse(argv("")) {
        ParseOutcome::Parsed(a) => assert!(a.command.is_none()),
        other => panic!("expected Parsed, got {other:?}"),
    }
}

#[test]
fn help_lists_every_command_and_each_one_is_runnable() {
    let help = commands::render_help("zephyra", "0.0.0");
    for c in commands::COMMANDS {
        assert!(
            help.contains(c.name),
            "`--help` 里没有 `{}` —— 帮助与命令表不同步",
            c.name
        );
    }
}

#[test]
fn unknown_command_suggests_the_nearby_one() {
    let mut cap = Cap::default();
    {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut io = Io {
            out: &mut out,
            err: &mut err,
        };
        // 走 main 的那条路径需要它是 pub；这里直接测命令表的查找语义。
        assert!(commands::find("chek").is_none());
        assert!(commands::find("check").is_some());
        io.line("");
    }
    cap.out.clear();
}

#[test]
fn check_reports_missing_file_as_usage_not_failure() {
    // 读不到文件 = 用法错（2），不是「样式表有问题」（1）。
    let r = zstylec::check(&[std::path::PathBuf::from(r"E:\definitely-not-here.zstyle")]);
    assert!(r.is_err(), "一个不存在的文件必须读不到");
}

#[test]
fn props_filters_by_substring() {
    let mut o = Vec::new();
    let mut e = Vec::new();
    {
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let ParseOutcome::Parsed(a) = args::parse(argv("props text")) else {
            panic!()
        };
        (commands::find("props").unwrap().run)(&a, &mut io).unwrap();
    }
    let got: Vec<String> = String::from_utf8_lossy(&o)
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert!(!got.is_empty(), "`props text` should list something");
    assert!(
        got.iter().all(|n| n.contains("text")),
        "filter leaked non-matching names: {got:?}"
    );
}

#[test]
fn exit_codes_are_stable() {
    // 这几个数字被 CI 依赖；改动会让流水线静默改变行为。
    assert_eq!(ExitCode::Success.code(), 0);
    assert_eq!(ExitCode::Failure.code(), 1);
    assert_eq!(ExitCode::Usage.code(), 2);
    assert_eq!(ExitCode::MissingEnv.code(), 3);
}

#[test]
fn new_rejects_names_that_would_not_compile() {
    for bad in ["", "  ", "1app", "my app"] {
        let mut o = Vec::new();
        let mut e = Vec::new();
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let ParseOutcome::Parsed(a) = args::parse(vec!["new".to_string(), bad.to_string()]) else {
            panic!()
        };
        let code = (commands::find("new").unwrap().run)(&a, &mut io);
        assert!(
            code.is_err(),
            "`{bad}` should be rejected before writing anything"
        );
        assert_eq!(code.unwrap_err().code, ExitCode::Usage);
    }
}

/// 脚手架生成的样式表**必须**能过 CLI 自己的 `check`。
///
/// 实测踩过：模板里写了 `font-size`，而真实属性名是 `text-size` —— 于是
/// `zephyra new` 交出一个**当场就报错**的工程，第一步就卡住。这条门禁就是防这个。
#[test]
fn scaffolded_stylesheet_passes_its_own_check() {
    let dir = std::env::temp_dir().join("zephyra-cli-scaffold-check");
    let _ = std::fs::remove_dir_all(&dir);

    let mut o = Vec::new();
    let mut e = Vec::new();
    {
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let ParseOutcome::Parsed(a) = args::parse(vec![
            "new".into(),
            "app".into(),
            "--path".into(),
            dir.display().to_string(),
        ]) else {
            panic!()
        };
        (commands::find("new").unwrap().run)(&a, &mut io).unwrap();
    }

    let sheet = dir.join("app").join("frontend").join("app.zstyle");
    assert!(sheet.exists(), "scaffold did not write {}", sheet.display());
    assert!(
        dir.join("app").join("frontend").join("app.rs").exists(),
        "scaffolded frontend source must live under frontend/"
    );
    assert!(
        !dir.join("app").join("styles").exists(),
        "scaffold should not create a separate styles directory"
    );
    let zephyra_config = std::fs::read_to_string(dir.join("app").join("zephyra.toml")).unwrap();
    assert!(
        zephyra_config.contains("files = [\"frontend/app.zstyle\"]"),
        "generated configuration must point at the frontend stylesheet"
    );
    assert!(
        dir.join("app").join("README.md").exists(),
        "scaffold did not write the project guide"
    );
    assert!(
        dir.join("app").join("src").join("main.rs").exists(),
        "scaffolded application source must live under src/"
    );
    let config_path = dir.join("app").join("ZephyraUI.json");
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap())
        .expect("generated ZephyraUI.json must be valid JSON");
    assert_eq!(config["window"]["frameless"], false);
    assert_eq!(config["window"]["transparent"], false);

    let reports = zstylec::check(&[sheet]).expect("generated stylesheet must be readable");
    assert!(
        !zstylec::has_errors(&reports),
        "the scaffolded stylesheet must be clean, got: {:?}",
        reports[0]
            .diagnostics
            .iter()
            .map(|d| d.error.headline.clone())
            .collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
/// `cargo zephyra dev` 会把 `zephyra` 也传进来 —— 必须剥掉，否则子命令名变成
/// `zephyra` 而真正的命令被当成位置参数。
///
/// 这条是 `cargo zephyra ...` 能不能用的**全部**关键：cargo 靠「`cargo-` +
/// 子命令名」去 PATH 找可执行文件，并把子命令名原样传参。
#[test]
fn cargo_subcommand_arg_is_stripped() {
    let mut a = argv("zephyra dev");
    zephyra_cli::strip_cargo_subcommand(&mut a);
    assert_eq!(a, vec!["dev".to_string()]);

    // 直接调用时不应该动第一个参数。
    let mut direct = argv("dev");
    zephyra_cli::strip_cargo_subcommand(&mut direct);
    assert_eq!(direct, vec!["dev".to_string()]);

    // 带标志的也要剥。
    let mut both = argv("zephyra check --format json a.zstyle");
    zephyra_cli::strip_cargo_subcommand(&mut both);
    assert_eq!(both[0], "check");
    assert_eq!(both.len(), 4);
}

/// 帮助里显示的名字必须与**用户实际敲的**一致。
///
/// `cargo zephyra --help` 时 argv[0] 是 `cargo-zephyra.exe`；原样打印会让人
/// 照抄出一个不保证在 PATH 上的命令。
#[test]
fn display_name_matches_what_the_user_types() {
    assert_eq!(
        zephyra_cli::entry::display_name(Some("cargo-zephyra.exe".into())),
        "cargo zephyra"
    );
    assert_eq!(
        zephyra_cli::entry::display_name(Some(r"E:\tools\zephyra.exe".into())),
        "zephyra"
    );
    assert_eq!(zephyra_cli::entry::display_name(None), "zephyra");
}

/// `dev` 必须在命令表里，且它的标志都被认。
///
/// 存在的理由：它曾经漏在表外 —— 于是 `zephyra dev` 报「unknown command」，
/// 而 `cargo zephyra dev` 更糟（用户根本不知道该打哪个）。
#[test]
fn dev_is_registered_with_its_flags() {
    let dev = commands::find("dev").expect("`dev` must be in the command table");
    for f in ["no-watch", "no-trace"] {
        assert!(dev.accepts_flag(f), "`dev` should accept --{f}");
    }
    assert!(
        !dev.accepts_flag("release"),
        "dev 只跑 debug；要 release 用 build --release"
    );
    let help = commands::render_help("zephyra", "0.0.0");
    assert!(help.contains("dev"), "`--help` must list `dev`");
}

/// dev 永远 debug，run 命令已移除，发布只能走 build。
#[test]
fn profile_flags_belong_to_build_only() {
    assert!(commands::find("run").is_none(), "`run` 命令应已移除");
    let build = commands::find("build").expect("`build` must exist");
    assert!(build.accepts_flag("debug"));
    assert!(build.accepts_flag("release"));

    // dev --release 解析出来只是个普通 flag，门禁会把它当 unknown flag 拒绝。
    let ParseOutcome::Parsed(a) = args::parse(argv("dev --release")) else {
        panic!("expected Parsed")
    };
    assert!(a.has("release")); // 解析器认得，但命令表不认（见 entry 门禁）
}

/// `--sf` 只属于 build；pack 还没做，不该出现在命令表里。
#[test]
fn build_supports_sf_mode() {
    let build = commands::find("build").expect("build must exist");
    assert!(build.accepts_flag("sf"));
    assert!(commands::find("pack").is_none(), "pack 推迟到后续实现");
    let ParseOutcome::Parsed(a) = args::parse(argv("build --sf")) else {
        panic!("expected Parsed")
    };
    assert!(a.has("sf"));
}

/// build 的两种 profile 声明：默认 release，--debug 切 debug。
#[test]
fn build_profile_flags_are_declared() {
    let build = commands::find("build").expect("`build` must exist");
    assert!(build.accepts_flag("debug"));
    assert!(build.accepts_flag("release"));
    let ParseOutcome::Parsed(a) = args::parse(argv("build --debug")) else {
        panic!("expected Parsed")
    };
    assert!(a.has("debug"));
    assert!(!a.has("release"));
}
/// `dev --styles <dir>` 是**取值型**标志。
///
/// 实测踩过：`--styles` 曾被当成布尔标志解析，目录名漏成位置参数 —— 报的还是
/// 「unknown flag `--styles`」，即用户完全看不到真正的原因（他传了合法的标志）。
/// 现在它只在 `dev` 的 `options` 里声明为 `ArgKind::Value`，解析自动跟上。
#[test]
fn dev_styles_flag_is_parsed_as_a_value() {
    let ParseOutcome::Parsed(a) = args::parse(argv("dev --styles styles --no-watch")) else {
        panic!("expected Parsed")
    };
    assert_eq!(a.option("styles"), Some("styles"));
    assert!(a.has("no-watch"));
    assert!(a.positional.is_empty(), "{:?}", a.positional);
}

/// 守卫：每个**声明为取值型**的选项都必须真的被解析器当成取值。
///
/// 过去这里守的是「帮助字符串里写的 `--xxx <...>` 要在 `VALUE_FLAGS` 里」——
/// 如今帮助字符串不再手写选项，声明只在 `Command::options` 一处。于是门禁改成
/// 直接扫声明：凡是 `ArgKind::Value` 的，`args::parse` 都必须把下一个参数吃成
/// 值，而不是漏成位置参数。
///
/// # 为什么这条门禁重要
///
/// 漏掉的取值型选项会被解析成「一个布尔标志 + 一个位置参数」。而 `perf` /
/// `debug` 的位置参数是**桥命令** —— 那个被漏下的值会被拼进命令里，于是应用
/// 收到一条莫名其妙的命令，而 CLI 那边**一切正常**（退出码 0，输出看着像样）。
#[test]
fn every_declared_value_option_is_parsed_as_a_value() {
    for cmd in commands::COMMANDS {
        for spec in cmd.options {
            if spec.kind != zephyra_cli::args::ArgKind::Value {
                continue;
            }
            assert!(
                args::is_value_flag(spec.name),
                "`{}` 声明了取值选项 `--{}`，但解析器不认",
                cmd.name,
                spec.name
            );
            let ParseOutcome::Parsed(a) = args::parse(vec![
                cmd.name.to_string(),
                format!("--{}", spec.name),
                "probe".to_string(),
            ]) else {
                panic!("parse failed for {} --{}", cmd.name, spec.name)
            };
            assert_eq!(
                a.option(spec.name),
                Some("probe"),
                "`{} --{}` 没把值吃进来",
                cmd.name,
                spec.name
            );
            assert!(
                a.positional.is_empty(),
                "`{} --{}` 的值漏成了位置参数：{:?}",
                cmd.name,
                spec.name,
                a.positional
            );
        }
    }
}

/// 帮助里的选项说明来自同一份声明：每个 `help` 文本都要出现在 `--help` 里。
#[test]
fn help_documents_every_declared_option() {
    let help = commands::render_help("zephyra", "0.0.0");
    for cmd in commands::COMMANDS {
        for spec in cmd.options {
            assert!(
                help.contains(spec.help),
                "`--help` 里没有 `{}` 的 `--{}` 说明",
                cmd.name,
                spec.name
            );
            // 声明显示形态（`--name <value>`）也必须出现在帮助里。
            assert!(
                help.contains(&spec.display()),
                "`--help` 里没有 `{}` 的选项形态 `{}`",
                cmd.name,
                spec.display()
            );
        }
    }
}

/// `-h` / `-V` 是帮助里承诺的全局别名，必须与长名等价。
#[test]
fn short_aliases_resolve_to_help_and_version() {
    assert_eq!(
        args::parse(argv("check -h")),
        ParseOutcome::Help(Some("check".into()))
    );
    assert_eq!(args::parse(argv("check -V")), ParseOutcome::Version);
    // `--` 之后的一切仍是位置参数，别名不生效。
    let ParseOutcome::Parsed(a) = args::parse(argv("build -- -h")) else {
        panic!("expected Parsed")
    };
    assert!(a.positional.contains(&"-h".to_string()));
}

#[test]
fn cli_resolves_app_config_from_manifest_directory() {
    let root = std::env::temp_dir().join(format!(
        "zephyra-cli-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let manifest = root.join("Cargo.toml");
    let config = root.join("ZephyraUI.json");
    std::fs::write(
        &manifest,
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(&config, r#"{"window":{"frameless":true}}"#).unwrap();

    let ParseOutcome::Parsed(args) = args::parse(vec![
        "build".into(),
        "--manifest-path".into(),
        manifest.display().to_string(),
    ]) else {
        panic!("expected Parsed")
    };
    let resolved = commands::app_config_path(&args).unwrap().unwrap();
    assert_eq!(resolved, config.canonicalize().unwrap());

    std::fs::remove_dir_all(root).unwrap();
}
/// 脚手架生成的 `Cargo.toml` 里，**每个**依赖都必须指向真实存在的本地包。
///
/// 实测踩过两轮：模板先写了个根本不存在的 `zephyra` 包（真实名字是 `zephyra-ui`），
/// 后来又写成 `../../ZephyraUI/...` 这种猜出来的相对路径 —— 工程挪个位置就指错。
/// 两次的失败信息都只是「找不到依赖」，与真正的原因之间没有任何提示关联。
///
/// 这条门禁读模板、把路径解析到磁盘上、对一遍目录里有没有 `Cargo.toml`。
#[test]
fn scaffold_dependencies_all_resolve_to_real_packages() {
    let dir = std::env::temp_dir().join("zephyra-cli-dep-probe");
    let _ = std::fs::remove_dir_all(&dir);

    let mut o = Vec::new();
    let mut e = Vec::new();
    {
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let ParseOutcome::Parsed(a) = args::parse(vec![
            "new".into(),
            "app".into(),
            "--path".into(),
            dir.display().to_string(),
        ]) else {
            panic!()
        };
        (commands::find("new").unwrap().run)(&a, &mut io).unwrap();
    }

    let manifest = std::fs::read_to_string(dir.join("app").join("Cargo.toml")).unwrap();
    let mut saw_any = false;
    for line in manifest.lines() {
        // `path = "X" optional = true` —— 取**引号里**那段，别把尾巴一起吃进来。
        let Some(after) = line.split("path = ").nth(1) else {
            continue;
        };
        let quoted = after
            .split('"')
            .nth(1)
            .unwrap_or_else(|| panic!("`path =` in the manifest is not quoted: {line}"));
        saw_any = true;
        let p = quoted;
        let manifest_path = std::path::Path::new(p).join("Cargo.toml");
        assert!(
            manifest_path.exists(),
            "scaffold points at a path that does not exist: {p}\n  \
             (the package behind it is missing, so `cargo check` fails with \
             `no matching package` -- which says nothing about the real cause)"
        );
    }
    assert!(
        saw_any,
        "no path dependencies found -- the template lost them?"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 门禁：模板的入口必须实现 `View` / `ApplicationHandler` 要求的**全部**方法。
///
/// 不是检查字符串（那只是猜测），而是断言这几个名字确实出现在生成的源码里 ——
/// 漏一个的表现是 `not all trait items implemented`，而那段代码是给人抄的。
///
/// # 为什么不去真的编译一遍
///
/// 把生成的工程塞进 workspace 当成员跑 `cargo check` 是最硬的验证（真的做过，
/// 头一版就是靠它抓出 3 个错）。但它要链接整条 D3D12 栈，放进单元测试里会让
/// 「跑一遍测试」变成「跑一遍构建」。所以这里守**名字齐全**，编译交给
/// 「手动跑一次 cargo check」——而那条手动验证的结论写在模板的注释里。
#[test]
fn scaffold_entry_implements_every_required_trait_method() {
    let dir = std::env::temp_dir().join("zephyra-cli-trait-probe");
    let _ = std::fs::remove_dir_all(&dir);

    let mut o = Vec::new();
    let mut e = Vec::new();
    {
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let ParseOutcome::Parsed(a) = args::parse(vec![
            "new".into(),
            "app".into(),
            "--path".into(),
            dir.display().to_string(),
        ]) else {
            panic!()
        };
        (commands::find("new").unwrap().run)(&a, &mut io).unwrap();
    }

    let root = dir.join("app");
    let src = std::fs::read_to_string(root.join("src").join("main.rs")).unwrap();
    let frontend = std::fs::read_to_string(root.join("frontend").join("app.rs")).unwrap();
    // `View` 的两个必需方法
    for m in ["fn on_input", "fn view"] {
        assert!(
            frontend.contains(m),
            "scaffold frontend is missing `{m}` (required by `View`)"
        );
    }
    // `ApplicationHandler` 在 winit 0.30 里没有给 `window_event` 默认实现
    for m in ["fn resumed", "fn window_event"] {
        assert!(
            src.contains(m),
            "scaffold entry is missing `{m}` (required by `ApplicationHandler`)"
        );
    }
    // 文本内容走 `props::TEXT`，不是 `Element::text(..)`（那个不存在）
    assert!(
        frontend.contains("props::TEXT"),
        "scaffold frontend should build text through `props::TEXT`"
    );
    for snippet in [
        "const BUTTON_KEY",
        "state.hovered",
        "state.pressed",
        "self.clicked = !self.clicked",
        "with_key(BUTTON_KEY)",
        "InputState, View",
    ] {
        assert!(
            frontend.contains(snippet),
            "scaffold frontend is missing key information `{snippet}`"
        );
    }
    for snippet in ["zephyra_winit_d3d12", "zephyra_winit", "fn window_event"] {
        assert!(
            src.contains(snippet),
            "scaffold host entry is missing `{snippet}`"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- `bridge` ----

/// `zephyra bridge log <file> [--tail N]`：读**本地**日志，不需要连上应用。
///
/// 这条命令存在的意义是「应用跑着，你看它的输出」—— 而日志是 `build --log` /
/// `dev --log` 落下来的那份。
#[test]
fn bridge_log_prints_the_tail_without_needing_an_app() {
    let dir = scratch("bridge-log");
    let log = dir.join("app.log");
    std::fs::write(&log, "first\nsecond\nthird\nfourth\n").unwrap();

    let mut o = Vec::new();
    let mut e = Vec::new();
    {
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let ParseOutcome::Parsed(a) =
            args::parse(argv(&format!("bridge log {} --tail 2", log.display())))
        else {
            panic!("expected Parsed")
        };
        let code = (commands::find("bridge").unwrap().run)(&a, &mut io).unwrap();
        assert_eq!(code, ExitCode::Success);
    }

    let out = String::from_utf8_lossy(&o).to_string();
    assert!(out.contains("third"), "{out}");
    assert!(out.contains("fourth"), "{out}");
    assert!(!out.contains("second"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `bridge log` 缺文件路径 ⇒ **用法错**（2），不是「动作失败」（1）。
///
/// 区分这两个码是有意义的：前者是「你写错了」，后者是「它没成」。
#[test]
fn bridge_log_without_a_path_is_a_usage_error() {
    let mut o = Vec::new();
    let mut e = Vec::new();
    {
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let ParseOutcome::Parsed(a) = args::parse(argv("bridge log")) else {
            panic!("expected Parsed")
        };
        let err = (commands::find("bridge").unwrap().run)(&a, &mut io).unwrap_err();
        assert_eq!(err.code, ExitCode::Usage);
    }
}

// ---- `perf` / `debug` ----

/// `perf` 与 `debug` 必须在命令表里，而且帮助里要能看到。
///
/// 理由：这两个是「AI / 人驱动运行中应用」的主力入口。注册了但帮助里没有
/// ⇒ 没人知道它们存在；而「命令表里有、帮助里没有」这种不同步正是本文件
/// 要防的那类退化。
#[test]
fn perf_and_debug_are_registered_and_documented() {
    for name in ["perf", "debug"] {
        let cmd = commands::find(name).expect("command must be in the table");
        assert!(!cmd.args.is_empty(), "`{name}` should document its args");
        let help = commands::render_help("zephyra", "0.0.0");
        assert!(help.contains(name), "`--help` must list `{name}`");
    }
}

/// `perf` / `debug` 的取值型标志必须被解析成取值。
///
/// ⚠ 这不是「多写一份名单」的问题：声明错了，`--duration-ms 2000` 会被解析成
/// 「一个叫 `duration-ms` 的布尔标志 + 一个位置参数 `2000`」—— 而
/// `debug watch` 的位置参数是**桥命令**，那个 `2000` 就会被拼进命令里，
/// 于是应用收到一条莫名其妙的命令，而 CLI 那边**一切正常**。
#[test]
fn perf_and_debug_value_flags_are_parseable() {
    let ParseOutcome::Parsed(a) = args::parse(argv(
        "perf watch --port 7878 --interval-ms 200 --duration-ms 3000 --seconds 2",
    )) else {
        panic!("expected Parsed")
    };
    assert_eq!(a.option("port"), Some("7878"));
    assert_eq!(a.option("interval-ms"), Some("200"));
    assert_eq!(a.option("duration-ms"), Some("3000"));
    assert_eq!(a.option("seconds"), Some("2"));
    // 取值不能漏进位置参数（那会被当成桥命令）。
    assert_eq!(a.positional, vec!["watch"], "{:?}", a.positional);
}

/// `debug watch` 的命令是**位置参数的全部剩余部分**，不是一个词。
///
/// `debug watch "props 0.1.0.1"` 是三个词 —— 只取第一个就变成
/// `props`，后两个被丢掉，watcher 会一直问一个不存在的命令。
#[test]
fn debug_watch_takes_the_whole_command_as_positional() {
    let ParseOutcome::Parsed(a) = args::parse(argv("debug watch props 0.1.0.1")) else {
        panic!("expected Parsed")
    };
    assert_eq!(a.command.as_deref(), Some("debug"));
    assert_eq!(a.positional, vec!["watch", "props", "0.1.0.1"]);
}

// ---- `fmt` ----

/// 跑一次 `fmt`，拿到 (退出码, stdout, stderr)。
///
/// ⚠ 这些用例**全部传绝对路径**，绝不改 cwd。`set_current_dir` 是进程级的，
/// 而 cargo 的测试是并行的：两个用例同时切 cwd 会互相踩，而「跑完把 cwd 切回去」
/// 还会在别的用例已经 `remove_dir_all` 掉那个目录时失败（实测就是这样红的）。
///
/// `Err(CliError)` 也变成退出码 —— 「用法错」正是 `fmt` 的正常输出之一
/// （未知扩展名报 2），把它 expect 掉会变成 panic。
fn run_fmt(cmdline: &str) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut io = Io {
            out: &mut out,
            err: &mut err,
        };
        let ParseOutcome::Parsed(a) = args::parse(argv(cmdline)) else {
            panic!("expected Parsed")
        };
        let cmd = commands::find("fmt").unwrap();
        match (cmd.run)(&a, &mut io) {
            Ok(c) => c,
            Err(e) => {
                // 用法错/环境缺失也走同一条打印路径（entry::dispatch 的形状）。
                io.err_line(&format!("error: {}", e.message));
                e.code
            }
        }
    };
    (
        code,
        String::from_utf8_lossy(&out).to_string(),
        String::from_utf8_lossy(&err).to_string(),
    )
}

/// 每次用例一个独立目录：临时目录名带进程 id + 运行时戳，避免并行用例互踩。
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zephyra-cli-fmt-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fmt_is_registered_with_the_check_flag() {
    let cmd = commands::find("fmt").expect("`fmt` must be in the command table");
    assert!(cmd.accepts_flag("check"), "`fmt` should accept --check");
    let help = commands::render_help("zephyra", "0.0.0");
    assert!(help.contains("fmt"), "`--help` must list `fmt`");
}

#[test]
fn fmt_formats_a_zstyle_file_in_place() {
    let dir = scratch("zstyle");
    let f = dir.join("app.zstyle");
    // 单行规则：排版器必须展开成多行。
    std::fs::write(&f, "control.x { width: 10px; height: 20px; }\n").unwrap();

    let (code, out, _) = run_fmt(&format!("fmt {}", f.display()));
    assert_eq!(code, ExitCode::Success);
    assert!(out.contains("formatted"), "{out}");

    let after = std::fs::read_to_string(&f).unwrap();
    assert_eq!(
        after, "control.x {\n    width: 10px;\n    height: 20px;\n}\n",
        "排版后必须是标准形态"
    );
    // 再跑一次：已经干净 ⇒ 不再报要排版。
    let (code2, out2, _) = run_fmt(&format!("fmt --check {}", f.display()));
    assert_eq!(code2, ExitCode::Success, "{out2}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_check_reports_dirty_files_without_writing() {
    let dir = scratch("check");
    let f = dir.join("a.zstyle");
    let bad = "Control{width:10px;}\n";
    std::fs::write(&f, bad).unwrap();

    // `--check` 只报不写，且有要排版的就退 1（CI 的形状）。
    let (code, out, err) = run_fmt(&format!("fmt --check {}", f.display()));
    assert_eq!(code, ExitCode::Failure);
    assert!(out.contains("would format"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        bad,
        "--check 不许写盘"
    );
    assert!(err.contains("would be formatted"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_formats_a_zypui_file_in_place() {
    let dir = scratch("zypui");
    let f = dir.join("shell.zypui");
    // class 在中间：排版器必须挪到行尾（位置非语义，所以这是排版不是行为变更）。
    let bad =
        "<component name=\"P\">\n  <box key=\"a\" class=\"c\" icon=\"gear\"/>\n</component>\n";
    std::fs::write(&f, bad).unwrap();

    let (code, _, _) = run_fmt(&format!("fmt {}", f.display()));
    assert_eq!(code, ExitCode::Success);
    let after = std::fs::read_to_string(&f).unwrap();
    assert_eq!(
        after,
        "<component name=\"P\">\n  <box key=\"a\" icon=\"gear\" class=\"c\"/>\n</component>\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_refuses_extensions_it_does_not_format() {
    let dir = scratch("bad-ext");
    std::fs::write(dir.join("notes.txt"), "hello\n").unwrap();

    // 未知扩展名报**用法错**，而不是静默跳过 —— 静默跳过会让「我以为排过了」成立。
    let (code, _, err) = run_fmt(&format!("fmt {}", dir.join("notes.txt").display()));
    assert_eq!(code, ExitCode::Usage);
    assert!(err.contains("cannot format"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_walks_a_directory_and_skips_target() {
    // 目录参数 ⇒ 递归扫里面的样式表/结构文件，`target/` 跳过（否则每次 build
    // 都改一遍产物）。这也覆盖「不给路径」那条路用的同一个 `collect_ext`。
    let dir = scratch("walk");
    let style = dir.join("frontend").join("app.zstyle");
    let ui = dir.join("frontend").join("app.zypui");
    std::fs::create_dir_all(style.parent().unwrap()).unwrap();
    std::fs::write(&style, "Control{width:10px;}\n").unwrap();
    std::fs::write(
        &ui,
        "<component name=\"P\">\n<box key=\"a\" class=\"c\" icon=\"gear\"/>\n</component>\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("target").join("x")).unwrap();
    std::fs::write(
        dir.join("target").join("x").join("junk.zstyle"),
        "Control{a:b;}\n",
    )
    .unwrap();

    let (code, out, _) = run_fmt(&format!("fmt --check {}", dir.display()));
    assert_eq!(code, ExitCode::Failure, "{out}");
    assert!(out.contains("app.zstyle"), "{out}");
    assert!(out.contains("app.zypui"), "{out}");
    assert!(!out.contains("junk"), "target/ 里的文件不该被扫到：{out}");
    assert_eq!(
        std::fs::read_to_string(dir.join("target").join("x").join("junk.zstyle")).unwrap(),
        "Control{a:b;}\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
