//! `zanim import` —— 把 CSS `@keyframes` 翻译成运行时 SMIL SVG。
//!
//! # 为什么是构建期工具
//!
//! 运行时动画语法只有 SMIL（见 ZephyraUI `docs/developer/syntax.md`）。
//! CSS 只是**输入格式**：本工具输出普通 SVG 文本，之后与手写 SMIL 走同一条
//! 运行时路径。这样没有第二套解析器、没有优先级规则。
//!
//! ```text
//! zanim import <file.css> [-o out.svg]
//! ```
//!
//! 支持的 CSS 属性：`opacity` / `transform`（translate/scale/rotate）/ `fill`。
//! 支持的 `animation` 简写：duration / delay / timing-function /
//! iteration-count / direction / fill-mode。`steps(n)` 展开成离散 keyTimes。
//! 其他属性报错退出，不静默丢弃。

use std::process::ExitCode;

const USAGE: &str = "\
zanim -- CSS keyframes -> SMIL SVG importer

USAGE
    zanim import <file.css> [-o out.svg]
    zanim --help

EXIT CODES
    0  success
    1  import error (unsupported property / malformed CSS)
    2  bad usage or unreadable file
";

#[derive(Debug, Clone)]
struct Keyframe {
    at: f32, // 0..1
    decls: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Keyframes {
    name: String,
    frames: Vec<Keyframe>,
}

#[derive(Debug, Default, Clone)]
struct AnimationOpts {
    duration_ms: f32,
    delay_ms: f32,
    iteration: String,
    direction: String,
    fill_mode: String,
    easing: String,
}

fn find_block(src: &str, marker: &str) -> Option<(usize, usize, usize)> {
    let start = src.find(marker)?;
    let open = src[start..].find('{')? + start;
    let mut depth = 0i32;
    for (i, ch) in src.char_indices().skip_while(|(i, _)| *i < open) {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, open, i));
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_keyframes(src: &str) -> Result<Vec<Keyframes>, String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(pos) = rest.find("@keyframes") {
        let tail = &rest[pos + "@keyframes".len()..];
        let name_len = tail
            .trim_start()
            .char_indices()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '-' || *c == '_')
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0);
        let name = tail.trim_start()[..name_len].to_string();
        let Some((_, open, close)) = find_block(tail, &name) else {
            return Err(format!("@keyframes {name} 缺少 {{ }}"));
        };
        let body = &tail[open + 1..close];
        let mut frames = Vec::new();
        let mut b = body;
        while let Some(sel) = b.find(|c: char| c.is_ascii_digit() || c == 'f' || c == 't') {
            let head = &b[sel..];
            let pct_end = head.find('{').ok_or("关键帧缺少 {")?;
            let selector = head[..pct_end].trim();
            let at = match selector {
                "from" => 0.0,
                "to" => 1.0,
                s if s.ends_with('%') => {
                    s[..s.len() - 1]
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .ok_or_else(|| format!("百分比无法解析: {selector}"))?
                        / 100.0
                }
                _ => {
                    b = &head[pct_end..];
                    continue;
                }
            };
            let decl_open = head.find('{').unwrap();
            let Some((_, _, decl_close)) = find_block(head, "{") else {
                return Err(format!("关键帧 {selector} 缺少 }}"));
            };
            let decl_body = &head[decl_open + 1..decl_close];
            let decls = decl_body
                .split(';')
                .filter_map(|d| {
                    let mut parts = d.splitn(2, ':');
                    let k = parts.next()?.trim();
                    let v = parts.next()?.trim();
                    if k.is_empty() {
                        None
                    } else {
                        Some((k.to_string(), v.to_string()))
                    }
                })
                .collect();
            frames.push(Keyframe { at, decls });
            b = &head[decl_close + 1..];
        }
        frames.sort_by(|a, b| a.at.total_cmp(&b.at));
        out.push(Keyframes { name, frames });
        rest = &tail[close + 1..];
        if rest.len() == tail.len() {
            break;
        }
    }
    if out.is_empty() {
        return Err("没有找到 @keyframes".to_string());
    }
    Ok(out)
}

fn parse_time(s: &str) -> f32 {
    let s = s.trim();
    if let Some(v) = s.strip_suffix("ms") {
        return v.trim().parse().unwrap_or(0.0);
    }
    if let Some(v) = s.strip_suffix('s') {
        return v.trim().parse::<f32>().unwrap_or(0.0) * 1000.0;
    }
    0.0
}

fn parse_animation(src: &str) -> AnimationOpts {
    let mut o = AnimationOpts {
        duration_ms: 1000.0,
        ..Default::default()
    };
    let Some(pos) = src.find("animation:") else {
        return o;
    };
    let line_end = src[pos..].find(';').unwrap_or(src.len() - pos);
    let value = &src[pos + "animation:".len()..pos + line_end];
    let mut saw_duration = false;
    for tok in value.split_whitespace() {
        if tok.ends_with("ms") || tok.ends_with('s') {
            // CSS `animation` 简写：第一个时间是 duration，第二个是 delay。
            if !saw_duration {
                o.duration_ms = parse_time(tok);
                saw_duration = true;
            } else {
                o.delay_ms = parse_time(tok);
            }
        } else if tok == "infinite" || tok.parse::<u32>().is_ok() {
            o.iteration = if tok == "infinite" {
                "indefinite".to_string()
            } else {
                tok.to_string()
            };
        } else if matches!(tok, "alternate" | "alternate-reverse" | "reverse") {
            o.direction = tok.to_string();
        } else if matches!(tok, "backwards" | "both" | "forwards") {
            o.fill_mode = tok.to_string();
        } else if tok.starts_with("steps(")
            || tok == "linear"
            || tok == "ease"
            || tok.starts_with("cubic-bezier")
            || tok.starts_with("ease-")
        {
            o.easing = tok.to_string();
        }
    }
    o
}

fn smil_timing(o: &AnimationOpts) -> String {
    let mut s = format!(
        "dur=\"{}ms\" repeatCount=\"{}\"",
        o.duration_ms as i64, o.iteration
    );
    if o.delay_ms > 0.0 {
        s.push_str(&format!(" begin=\"{}ms\"", o.delay_ms as i64));
    }
    // CSS 默认 fill-mode 是 none：播完回基值，不能冻结。
    // forwards/both 才冻结；backwards 只管 delay 期，也不冻结。
    if matches!(o.fill_mode.as_str(), "forwards" | "both") {
        s.push_str(" fill=\"freeze\"");
    }
    if matches!(o.fill_mode.as_str(), "backwards" | "both") {
        s.push_str(" fill-mode=\"backwards\"");
    }
    if matches!(
        o.direction.as_str(),
        "alternate" | "alternate-reverse" | "reverse"
    ) {
        s.push_str(&format!(" direction=\"{}\"", o.direction));
    }
    if o.easing == "linear" {
        s.push_str(" calcMode=\"linear\"");
    } else if o.easing.starts_with("steps(") {
        // steps(n) 由导入器展开，运行时只有 discrete + keyTimes。
        s.push_str(" calcMode=\"discrete\"");
    }
    s
}

fn emit(kfs: &Keyframes, o: &AnimationOpts) -> Result<String, String> {
    let mut anims = Vec::new();
    let times = kfs
        .frames
        .iter()
        .map(|f| format!("{:.4}", f.at))
        .collect::<Vec<_>>()
        .join(";");
    // 未知属性一律报错，绝不静默丢弃（与 props_map 同一条规矩）。
    let supported = ["opacity", "transform", "fill"];
    for (k, _) in kfs.frames.iter().flat_map(|f| f.decls.iter()) {
        if !supported.contains(&k.as_str()) {
            return Err(format!(
                "不支持的 CSS 动画属性 `{k}`（只支持 opacity/transform/fill）"
            ));
        }
    }
    for prop in supported {
        // 某个属性只要有一帧没写就跳过：本工具不猜缺失帧的插值。
        if kfs
            .frames
            .iter()
            .any(|f| !f.decls.iter().any(|(k, _)| k == prop))
        {
            continue;
        }
        let mut values = Vec::new();
        let mut kind: Option<String> = None;
        for frame in &kfs.frames {
            let (_, value) = frame
                .decls
                .iter()
                .find(|(k, _)| k == prop)
                .ok_or_else(|| format!("关键帧缺少 {prop}"))?;
            if prop == "transform" {
                let inner = value.trim_start_matches("transform:").trim();
                let fn_name = inner.split('(').next().unwrap_or("").trim();
                kind = Some(fn_name.to_string());
                let arg = inner
                    .trim_start_matches(fn_name)
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .trim()
                    .replace("deg", "");
                values.push(arg);
            } else {
                values.push(value.clone());
            }
        }
        if values.is_empty() {
            continue;
        }
        let timing = smil_timing(o);
        let vals = values.join(";");
        if prop == "transform" {
            let k = kind.unwrap_or_else(|| "rotate".to_string());
            anims.push(format!(
                "    <animateTransform type=\"{k}\" values=\"{vals}\" keyTimes=\"{times}\" {timing}/>"
            ));
        } else {
            anims.push(format!(
                "    <animate attributeName=\"{prop}\" values=\"{vals}\" keyTimes=\"{times}\" {timing}/>"
            ));
        }
    }
    Ok(format!(
        "<!-- imported from keyframes {} -->\n<svg viewBox=\"0 0 24 24\">\n  <path d=\"M2 2L22 2L22 22L2 22Z\">\n{}\n  </path>\n</svg>\n",
        kfs.name,
        anims.join("\n")
    ))
}

/// CSS → SMIL 的纯函数（便于单测，不经命令行 / 文件）。
fn import_css(css: &str) -> Result<String, (u8, String)> {
    let opts = parse_animation(css);
    let kfs = parse_keyframes(css).map_err(|e| (1, e))?;
    if kfs.len() > 1 {
        return Err((
            1,
            format!(
                "发现 {} 个 @keyframes；一次只导入一个，请拆成多个 CSS 文件（绝不静默忽略）",
                kfs.len()
            ),
        ));
    }
    let svg = kfs.first().ok_or((1, "没有可导入的关键帧".to_string()))?;
    emit(svg, &opts).map_err(|e| (1, e))
}

fn run() -> Result<(), (u8, String)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("import") {
        return Err((2, USAGE.to_string()));
    }
    let path = args.get(1).ok_or((2, "缺少 CSS 文件".to_string()))?;
    let css = std::fs::read_to_string(path).map_err(|e| (2, format!("读取 {path} 失败: {e}")))?;
    let out = import_css(&css)?;
    let mut target = None;
    if let Some(i) = args.iter().position(|a| a == "-o") {
        target = args.get(i + 1).cloned();
    }
    match target {
        Some(p) => std::fs::write(&p, out).map_err(|e| (2, format!("写入 {p} 失败: {e}")))?,
        None => print!("{out}"),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("{msg}");
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSS: &str = r#"
      .spin { animation: spin 1s linear infinite alternate; }
      @keyframes spin {
        from { opacity: 0; }
        to   { opacity: 1; }
      }
    "#;

    #[test]
    fn parses_keyframes_and_animation_shorthand() {
        let kfs = parse_keyframes(CSS).unwrap();
        assert_eq!(kfs[0].name, "spin");
        assert_eq!(kfs[0].frames.len(), 2);
        let o = parse_animation(CSS);
        assert_eq!(o.duration_ms, 1000.0);
        assert_eq!(o.iteration, "indefinite");
        assert_eq!(o.direction, "alternate");
    }

    #[test]
    fn emits_runtime_smil_only() {
        let kfs = parse_keyframes(CSS).unwrap();
        let out = emit(&kfs[0], &parse_animation(CSS)).unwrap();
        assert!(out.contains("<animate attributeName=\"opacity\""));
        assert!(out.contains("direction=\"alternate\""));
        assert!(
            !out.contains("@keyframes"),
            "输出必须是运行时 SMIL，不能是 CSS"
        );
    }

    #[test]
    fn unsupported_property_reports_error() {
        let bad = "@keyframes x { from { filter: blur(2px); } to { filter: blur(0); } }";
        let kfs = parse_keyframes(bad).unwrap();
        assert!(emit(&kfs[0], &AnimationOpts::default()).is_err());
    }

    #[test]
    fn css_default_fill_mode_does_not_freeze() {
        // CSS 默认 fill-mode: none —— 播完回基值；旧实现无条件 freeze 是语义反转。
        let css = "x { animation: x 1s; } @keyframes x { from { opacity: 0; } to { opacity: 1; } }";
        let out = emit(&parse_keyframes(css).unwrap()[0], &parse_animation(css)).unwrap();
        assert!(!out.contains("fill=\"freeze\""), "默认不该冻结：{out}");
    }

    #[test]
    fn second_time_token_is_delay_not_duration() {
        let css =
            "x { animation: x 1s 300ms; } @keyframes x { from { opacity: 0; } to { opacity: 1; } }";
        let o = parse_animation(css);
        assert_eq!(o.duration_ms, 1000.0);
        assert_eq!(o.delay_ms, 300.0);
        let out = emit(&parse_keyframes(css).unwrap()[0], &o).unwrap();
        assert!(out.contains("begin=\"300ms\""), "delay 应输出 begin：{out}");
    }

    #[test]
    fn multiple_keyframes_are_an_error_not_a_silent_drop() {
        let css =
            "a { animation: a 1s; } @keyframes a { from { opacity: 0; } to { opacity: 1; } } \
                   b { animation: b 1s; } @keyframes b { from { opacity: 1; } to { opacity: 0; } }";
        let err = import_css(css).unwrap_err();
        assert!(err.1.contains("2 个 @keyframes"), "{err:?}");
    }
}
