#![deny(rustdoc::broken_intra_doc_links)]
//! 参数解析。手写，但**有测试**，而且「有哪些选项」是**声明式**的。
//!
//! # 为什么选项元数据只有一份
//!
//! 以前取值型标志写在一张手写的 `VALUE_FLAGS` 里，布尔标志写在
//! `Command::flags` 里，帮助文本又是命令表里的一段字符串 —— 三份名单要手工
//! 保持同步。实测踩过：`--styles` 加进了「允许白名单」却漏进 `VALUE_FLAGS`，
//! 于是目录名漏成位置参数，报的还是「unknown flag `--styles`」——用户完全
//! 看不到真正的原因。
//!
//! 现在每个命令用 [`ArgSpec`] 声明自己的选项（名字、种类、短别名、说明）。
//! 解析器查它决定 `--name` 要不要吃下一个参数，门禁查它决定某个标志属不属于
//! 这个命令，帮助从它生成 —— 三者读的是**同一份**声明，结构上没法漂移。
//!
//! # 为什么仍然手写
//!
//! 子命令十几个、标志十来个，全是「位置参数 + 布尔开关 + 取值选项」，没有嵌套
//! 子命令、没有变参类型推导。clap 那套 derive 的价值在这里用不上，而它带来的
//! 是一棵依赖树和一层编译期宏。手写解析器换来的是：**错误文案完全可控**
//! （这项目对报错质量要求很高，见 `zstylec/src/errors.rs` 写死的规格），以及零依赖。
//!
//! # 查询性能：索引一次，O(1) 命中
//!
//! 借鉴 tauri-plugin-cli 的两处数据结构（见其 `parser.rs`）：
//!
//! 1. 它的 `Config` 把参数预先组织成 `HashMap`，而不是每次解析都遍历配置。
//!    这里用 [`OnceLock`] 把命令表里的声明建成一次性索引 —— 每个 `--name`
//!    直接哈希命中，不再为十几个命令 × 若干选项反复做字符串比较。
//! 2. 它的 `Matches { args: HashMap<String, ArgData> }` 把「值」和「出现次数」
//!    放在同一条记录里。这里用 [`Args`] 的同名结构替代过去三份平行 Vec
//!    （`flags` / `options` / `counts`）—— `has` / `option` / `count` 都是一次
//!    查找，而不是各自线性扫一遍。另存一个 `order` 仅用于按 argv 顺序遍历，
//!    使报错输出可复现（HashMap 自身的迭代顺序是不确定的）。
//!
//! # 约定
//!
//! - `--flag` 与 `--flag=value` 都收；`-f` 形式只给少数短标志。
//! - `--` 之后的一切当位置参数，不再解析标志（`zephyra new -- --weird-name`）。
//! - 未知标志**立刻**报用法错（退出码 2），不静默忽略 —— 静默忽略会让
//!   `zephyra check --strictness` 看起来跑成功了，其实什么都没开。
//! - `-h` / `-V` 是全局内建别名（help / version），与长名等价；保留给它们，
//!   命令声明的短别名不应使用这两个字母。

use std::collections::{hash_map::Entry, HashMap};
use std::sync::OnceLock;

use crate::commands::COMMANDS;

/// 选项的种类。决定 `--name` 后面要不要跟一个值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// 布尔标志：`--force`。
    Flag,
    /// 取值选项：`--path dir` 或 `--path=dir`。
    Value,
}

/// 一个选项的**全部**元数据。声明一次，解析 / 校验 / 帮助三处共用。
///
/// 用 `const` 构造，所以可以直接写在 [`crate::commands::Command`] 的
/// `options` 表里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    /// 长名，**不带** `--`。
    pub name: &'static str,
    /// 短别名，如 `Some('v')`；`None` 表示没有。
    pub short: Option<char>,
    pub kind: ArgKind,
    /// 取值选项的值在帮助里的名字，如 `dir` / `n`；布尔标志为空串。
    pub value_name: &'static str,
    /// 帮助里显示的一句话说明。
    pub help: &'static str,
}

impl ArgSpec {
    /// 声明一个布尔标志。
    pub const fn flag(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            short: None,
            kind: ArgKind::Flag,
            value_name: "",
            help,
        }
    }

    /// 声明一个取值选项。
    pub const fn value(name: &'static str, value_name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            short: None,
            kind: ArgKind::Value,
            value_name,
            help,
        }
    }

    /// 追加一个短别名（可链式调用）。
    pub const fn with_short(mut self, c: char) -> Self {
        self.short = Some(c);
        self
    }

    /// 帮助里显示的形态，如 `--path <dir>`（有短别名时 `-p, --path <dir>`）。
    pub fn display(&self) -> String {
        let mut s = String::new();
        if let Some(c) = self.short {
            s.push('-');
            s.push(c);
            s.push_str(", ");
        }
        s.push_str("--");
        s.push_str(self.name);
        if self.kind == ArgKind::Value && !self.value_name.is_empty() {
            s.push_str(" <");
            s.push_str(self.value_name);
            s.push('>');
        }
        s
    }
}

/// 声明表的一次性索引。
///
/// tauri-plugin-cli 把 `Config` 里的参数预先组织成 `HashMap`，而不是每来一个
/// token 就重新遍历配置。这里同理：**建一次**，之后每个 `--name` 都是 O(1) 命中，
/// 不再为 12 个命令 × 若干选项反复做字符串比较。
struct ArgIndex {
    by_name: HashMap<&'static str, ArgKind>,
    by_short: HashMap<char, &'static ArgSpec>,
}

static ARG_INDEX: OnceLock<ArgIndex> = OnceLock::new();

fn arg_index() -> &'static ArgIndex {
    ARG_INDEX.get_or_init(|| {
        let mut by_name = HashMap::new();
        let mut by_short = HashMap::new();
        for cmd in COMMANDS {
            for spec in cmd.options {
                // 同名声明（release / port / … 出现在多个命令）种类一致；即便
                // 将来不一致，解析阶段只关心「是否取值」，取先声明的即可。
                by_name.entry(spec.name).or_insert(spec.kind);
                if let Some(c) = spec.short {
                    by_short.entry(c).or_insert(spec);
                }
            }
        }
        ArgIndex { by_name, by_short }
    })
}

/// `--name` 要不要吃掉下一个参数。**唯一判据**就是命令表里的声明。
pub fn is_value_flag(name: &str) -> bool {
    arg_index().by_name.get(name) == Some(&ArgKind::Value)
}

/// 按短别名找声明（`-v` → 名字为 `verbose` 的声明）。
///
/// 只认单字母：`-foo` 不该被当成短别名 `f` 加上两个多余字符。
fn short_spec(short: &str) -> Option<&'static ArgSpec> {
    let c = short.chars().next()?;
    if short.len() != c.len_utf8() {
        return None;
    }
    arg_index().by_short.get(&c).copied()
}

/// 单个参数的解析结果。
///
/// 对应 tauri-plugin-cli 的 `ArgData`：值与出现次数放在同一条记录里，
/// 一次哈希查找就拿到全部信息，不必再扫三份平行的 Vec。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgData {
    /// 取值选项的值；布尔标志为 `None`。
    pub value: Option<String>,
    /// 出现次数（`-v -v` 记 2；取值选项重复给也记 2）。
    pub occurrences: usize,
}

/// 解析后的参数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// 子命令名（`check` / `new` / …）。`None` 表示没给。
    pub command: Option<String>,
    /// 位置参数，按出现顺序。保持 `Vec`：要按下标取、切片、原样透传。
    pub positional: Vec<String>,
    /// 名字 → 结果。O(1) 查询。
    data: HashMap<String, ArgData>,
    /// 首次出现顺序：报错与遍历仍按 argv 顺序，输出可复现。
    order: Vec<String>,
}

/// 解析结果：要么是参数，要么是一条**用法错误文案**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Parsed(Args),
    /// 需要打印帮助。`Some(name)` = 只打印该子命令的帮助（如 `zephyra dev --help`）；
    /// `None` = 全局帮助（`zephyra --help` / `-h`）。
    Help(Option<String>),
    Version,
    Usage(String),
}

/// 解析到 `--help` 时，第一个位置参数就是帮助的目标命令。
fn help_target(out: &Args) -> Option<String> {
    out.positional.first().cloned()
}

pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> ParseOutcome {
    let mut out = Args::default();
    let mut it = argv.into_iter().peekable();
    let mut literal = false;

    while let Some(a) = it.next() {
        if literal {
            push_positional(&mut out, a);
            continue;
        }
        if a == "--" {
            literal = true;
            continue;
        }
        if let Some(rest) = a.strip_prefix("--") {
            if rest.is_empty() {
                return ParseOutcome::Help(help_target(&out));
            }
            match rest.split_once('=') {
                Some((k, v)) => {
                    if k.is_empty() {
                        return ParseOutcome::Usage(format!("bad flag `--{rest}`"));
                    }
                    out.record_option(k.to_string(), v.to_string());
                }
                None => {
                    if rest == "help" {
                        return ParseOutcome::Help(help_target(&out));
                    }
                    if rest == "version" {
                        return ParseOutcome::Version;
                    }
                    if is_value_flag(rest) {
                        // `--name value`：吃掉下一个参数。
                        match it.next() {
                            Some(v) => out.record_option(rest.to_string(), v),
                            None => {
                                return ParseOutcome::Usage(format!("`--{rest}` needs an argument"))
                            }
                        }
                        continue;
                    }
                    out.record_flag(rest.to_string());
                }
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            // 短标志只认单字母，不做聚簇（`check -vq`）。够用且没有歧义。
            let k = a.trim_start_matches('-');
            if k.is_empty() {
                return ParseOutcome::Usage(format!("bad flag `{a}`"));
            }
            // 全局内建别名。与长名走同一条返回路径，修掉了「帮助里写了
            // `-h`，实际却被当成未知标志 `--h`」的文档/行为漂移。
            if k == "h" {
                return ParseOutcome::Help(help_target(&out));
            }
            if k == "V" {
                return ParseOutcome::Version;
            }
            // 命令声明的短别名：解析成**长名**，后续门禁只认长名。
            if let Some(spec) = short_spec(k) {
                match spec.kind {
                    ArgKind::Flag => out.record_flag(spec.name.to_string()),
                    ArgKind::Value => match it.next() {
                        Some(v) => out.record_option(spec.name.to_string(), v),
                        None => return ParseOutcome::Usage(format!("`-{k}` needs an argument")),
                    },
                }
                continue;
            }
            out.record_flag(k.to_string());
            continue;
        }
        push_positional(&mut out, a);
    }

    // 第一个位置参数当子命令。
    if !out.positional.is_empty() {
        out.command = Some(out.positional.remove(0));
    }

    ParseOutcome::Parsed(out)
}

fn push_positional(out: &mut Args, a: String) {
    // 子命令位置之前的第一个位置参数已经被当成 command 了；这里只负责后续的。
    out.positional.push(a);
}

impl Args {
    /// 是否给过布尔标志 `--flag`。
    pub fn has(&self, flag: &str) -> bool {
        self.data
            .get(flag)
            .is_some_and(|d| d.value.is_none() && d.occurrences > 0)
    }

    /// 出现次数。
    pub fn count(&self, flag: &str) -> usize {
        self.data.get(flag).map_or(0, |d| d.occurrences)
    }

    /// 取值选项的值；重复给时返回**第一次**的值（与旧行为一致）。
    pub fn option(&self, name: &str) -> Option<&str> {
        self.data.get(name).and_then(|d| d.value.as_deref())
    }

    pub fn positional_at(&self, i: usize) -> Option<&str> {
        self.positional.get(i).map(|s| s.as_str())
    }

    /// 按 argv 顺序遍历出现过的布尔标志。
    pub fn flags(&self) -> impl Iterator<Item = &str> {
        self.order.iter().filter_map(|name| {
            self.data
                .get(name)
                .is_some_and(|d| d.value.is_none())
                .then_some(name.as_str())
        })
    }

    /// 按 argv 顺序遍历出现过的取值选项。
    pub fn options(&self) -> impl Iterator<Item = (&str, &str)> {
        self.order.iter().filter_map(|name| {
            self.data
                .get(name)
                .and_then(|d| d.value.as_deref())
                .map(|value| (name.as_str(), value))
        })
    }

    fn record_flag(&mut self, name: String) {
        match self.data.entry(name.clone()) {
            Entry::Occupied(mut e) => e.get_mut().occurrences += 1,
            Entry::Vacant(e) => {
                e.insert(ArgData {
                    value: None,
                    occurrences: 1,
                });
                self.order.push(name);
            }
        }
    }

    fn record_option(&mut self, name: String, value: String) {
        match self.data.entry(name.clone()) {
            Entry::Occupied(mut e) => {
                // 值保留第一次给的；出现次数照样累加。
                e.get_mut().occurrences += 1;
            }
            Entry::Vacant(e) => {
                e.insert(ArgData {
                    value: Some(value),
                    occurrences: 1,
                });
                self.order.push(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_of_a_value_option_shows_its_value_placeholder() {
        let spec = ArgSpec::value("path", "dir", "target dir");
        assert_eq!(spec.display(), "--path <dir>");
    }

    #[test]
    fn display_of_a_flag_has_no_value_placeholder() {
        let spec = ArgSpec::flag("force", "overwrite");
        assert_eq!(spec.display(), "--force");
    }

    #[test]
    fn display_with_a_short_alias_puts_it_first() {
        let spec = ArgSpec::flag("verbose", "more output").with_short('v');
        assert_eq!(spec.display(), "-v, --verbose");
    }

    #[test]
    fn value_option_with_an_empty_placeholder_omits_the_brackets() {
        let spec = ArgSpec::value("name", "", "name");
        assert_eq!(spec.display(), "--name");
    }

    #[test]
    fn is_value_flag_uses_the_command_table() {
        assert!(is_value_flag("path"), "new --path takes a value");
        assert!(!is_value_flag("force"), "new --force is a flag");
        assert!(!is_value_flag("no-such-flag-at-all"));
    }

    #[test]
    fn short_spec_ignores_multi_character_clusters() {
        // `-foo` 不能被当成短别名 `f` + "oo"。当前没有声明短别名，
        // 两种情况都返回 None；这条锁住「多字符不解析成单字符别名」。
        assert!(short_spec("foo").is_none());
        assert!(short_spec("f").is_none());
    }
}
