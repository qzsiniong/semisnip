//! 变量渲染（**只读**）。
//!
//! 预览与展开是两条完全不同的路径：
//! - 展开：espanso 真求值，会跑 shell、弹表单、读剪贴板；
//! - 预览：这里只做无副作用的近似，让用户看清"将要插入什么"。
//!
//! 铁律：`shell` / `script` 在预览里**绝不执行**，只显示命令本身。

use crate::index::model::{Snippet, VarKind, VarSpec};

/// 预览的上下文：那些"不由片段本身决定"的输入。
///
/// 单独拆出来是为了让 `render` 保持纯函数：同样的 (片段, ctx) 永远得到同样的
/// 结果，测试里传一个固定 `now` 就能断言日期格式。
pub struct PreviewCtx {
    /// 剪贴板内容；`None` 表示不读取（默认策略）。
    pub clipboard: Option<String>,
    /// 是否在预览里显示剪贴板内容。为假时只显示字符数，不显示正文。
    pub show_clipboard: bool,
    /// 求值 `{{date}}` 用的"当前时间"。抽成字段主要是为了测试可控。
    pub now: chrono::DateTime<chrono::Local>,
}

impl Default for PreviewCtx {
    fn default() -> Self {
        Self {
            clipboard: None,
            show_clipboard: false,
            now: chrono::Local::now(),
        }
    }
}

/// 单个变量在详情栏里的展示项。
///
/// `detail` 是右侧那句灰色说明，`side_effect` 用来给它标红/标警示。
#[derive(Clone, Debug)]
pub struct RenderedVar {
    pub name: String,
    /// 类型标签（`echo` / `date` / `form:choice` …）。
    pub kind: String,
    /// 一句话说明：常量值、日期格式、命令首行……
    pub detail: String,
    /// 展开时是否会执行外部命令。详情栏据此加提示。
    pub side_effect: bool,
}

/// 预览的完整产物。
pub struct Rendered {
    /// 替换后的正文。未求值的变量保留 `{{name}}` 原样。
    pub text: String,
    /// 逐个变量的说明，顺序与片段里声明的一致。
    pub vars: Vec<RenderedVar>,
    /// 需要提醒用户的事项（不会执行 shell、会弹表单……）。
    pub warnings: Vec<String>,
}

/// 把 `{{name}}` 逐个替换成预览值；未知变量保留原文。
///
/// 顺序上先处理 `{{变量}}` 再处理 `[[字段]]`：form 简写的 layout 里两者可以混用，
/// 而变量的值里万一含 `[[...]]` 也不该被二次替换——现在这个顺序正好满足。
///
/// 每个变量只替换一次全部出现（`String::replace` 是全局的），这与 espanso 的
/// 行为一致：同名变量在正文里出现几次都是同一个值。
pub fn render(snippet: &Snippet, ctx: &PreviewCtx) -> Rendered {
    let mut text = snippet.body.as_text().to_string();
    let mut vars = Vec::with_capacity(snippet.vars.len());
    let mut warnings = Vec::new();

    for var in &snippet.vars {
        let token = format!("{{{{{}}}}}", var.name);
        let (replacement, detail, warning) = preview_value(var, ctx);
        if let Some(warning) = warning {
            warnings.push(warning);
        }
        if text.contains(&token) {
            text = text.replace(&token, &replacement);
        }
        vars.push(RenderedVar {
            name: var.name.clone(),
            kind: var.kind.label().to_string(),
            detail,
            side_effect: var.kind.is_side_effecting(),
        });
    }

    // `[[field]]` 是 form 简写的占位符。
    // 有默认值就显示默认值，否则显示 `<类型>`——空着会让用户以为字段漏了。
    for field in &snippet.form_fields {
        let token = format!("[[{}]]", field.name);
        if text.contains(&token) {
            let shown = field
                .default
                .clone()
                .unwrap_or_else(|| format!("<{}>", field.kind));
            text = text.replace(&token, &shown);
        }
        vars.push(RenderedVar {
            name: field.name.clone(),
            kind: format!("form:{}", field.kind),
            detail: field
                .values
                .first()
                .map(|_| field.values.join(" / "))
                .unwrap_or_else(|| {
                    if field.multiline {
                        "多行文本".into()
                    } else {
                        "单行文本".into()
                    }
                }),
            side_effect: false,
        });
    }

    // 只要带字段就一定会弹表单：这条提醒不能少，否则用户以为回车就是插文本，
    // 结果被一个表单挡住，还以为卡住了。
    if !snippet.form_fields.is_empty() {
        warnings.push("该片段会弹出 espanso 表单，展开后需要填写字段".into());
    }

    Rendered {
        text,
        vars,
        warnings,
    }
}

/// 把一个变量求成"给人看的"三件套：替换文本 / 一句话说明 / 可选警告。
///
/// 三条约定：
/// - **有副作用的类型绝不执行**，替换文本保留 `{{name}}` 原样，另附一条警告；
/// - 能静态推断的就真算（date 按 `format` 求值），让用户看到真实结果；
/// - 算不出来的（choice / form / match）保留占位符，但说明里讲清展开时会发生什么。
///
/// 保留占位符而不是删掉，是为了让用户在预览里还能看出"这里有个变量"，
/// 而不是以为片段正文就是那几行字。
fn preview_value(var: &VarSpec, ctx: &PreviewCtx) -> (String, String, Option<String>) {
    match &var.kind {
        VarKind::Echo => {
            let value = var.param_str("echo").unwrap_or("").to_string();
            (value.clone(), "常量".into(), None)
        }
        VarKind::Date => {
            let format = var.param_str("format").unwrap_or("%Y-%m-%d");
            let offset = var
                .param_str("offset")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let target = ctx.now + chrono::Duration::seconds(offset);
            let value = target.format(format).to_string();
            (value, format!("按 `{format}` 求值"), None)
        }
        VarKind::Random => {
            let choices = var.param_list("choices");
            if choices.is_empty() {
                ("{{random}}".into(), "随机取值".into(), None)
            } else {
                let shown = format!("[{}]", choices.join(" 或 "));
                (shown, format!("{} 个候选", choices.len()), None)
            }
        }
        VarKind::Clipboard => {
            let text = match (&ctx.clipboard, ctx.show_clipboard) {
                (Some(text), true) if !text.is_empty() => {
                    let head: String = text.chars().take(80).collect();
                    format!("{head}{}", if text.chars().count() > 80 { "…" } else { "" })
                }
                (Some(text), _) => format!("[剪贴板 {} 字符]", text.chars().count()),
                _ => "[剪贴板内容]".into(),
            };
            (text, "展开时读取".into(), None)
        }
        VarKind::Shell => {
            let cmd = var.param_str("cmd").unwrap_or("(未提供 cmd)");
            (
                "{{".to_string() + &var.name + "}}",
                format!("展开时执行：{}", first_line(cmd, 48)),
                Some(format!("`{}` 是 shell 变量，预览不执行", var.name)),
            )
        }
        VarKind::Script => {
            let args = var.param_list("args");
            let shown = if args.is_empty() {
                "(未提供 args)".to_string()
            } else {
                args.join(" ")
            };
            (
                "{{".to_string() + &var.name + "}}",
                format!("展开时执行：{}", first_line(&shown, 48)),
                Some(format!("`{}` 是脚本变量，预览不执行", var.name)),
            )
        }
        VarKind::Choice => {
            let values = var.param_list("values");
            (
                "{{".to_string() + &var.name + "}}",
                format!("展开时选择（{} 项）", values.len()),
                None,
            )
        }
        VarKind::Form => (
            "{{".to_string() + &var.name + "}}",
            "展开时弹表单".into(),
            None,
        ),
        VarKind::Match => {
            let trigger = var.param_str("trigger").unwrap_or("");
            (
                "{{".to_string() + &var.name + "}}",
                format!("嵌套展开 `{trigger}`"),
                None,
            )
        }
        VarKind::Unknown(kind) => (
            "{{".to_string() + &var.name + "}}",
            format!("未知类型 `{kind}`"),
            Some(format!("`{}` 的类型 `{kind}` 无法预览", var.name)),
        ),
    }
}

/// 取首行并按字符数截断。
///
/// 按 `chars()` 而不是字节切：命令里可能有中文参数，按字节切会把 UTF-8 切坏。
/// 截断后补省略号，让用户知道后面还有内容。
fn first_line(text: &str, max_chars: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{Body, SourceRef};
    use serde_yaml::Value;
    use std::path::PathBuf;

    fn snippet(body: &str, vars: Vec<VarSpec>) -> Snippet {
        Snippet {
            id: 0,
            triggers: vec![";;x".into()],
            label: "x".into(),
            body: Body::Text(body.into()),
            vars,
            form_fields: vec![],
            search_terms: vec![],
            source: SourceRef {
                file: PathBuf::from("base.yml"),
                ordinal: 0,
                package: None,
            },
            word: false,
            usable: true,
        }
    }

    #[test]
    fn echo_is_rendered() {
        let s = snippet(
            "hello {{name}}",
            vec![VarSpec {
                name: "name".into(),
                kind: VarKind::Echo,
                params: serde_yaml::from_str("echo: world").unwrap(),
            }],
        );
        let out = render(&s, &PreviewCtx::default());
        assert_eq!(out.text, "hello world");
    }

    #[test]
    fn shell_is_not_executed_but_warned() {
        let s = snippet(
            "{{out}}",
            vec![VarSpec {
                name: "out".into(),
                kind: VarKind::Shell,
                params: serde_yaml::from_str("cmd: \"rm -rf /tmp/x\"").unwrap(),
            }],
        );
        let out = render(&s, &PreviewCtx::default());
        assert_eq!(out.text, "{{out}}", "占位符必须原样保留");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("不执行"));
    }

    #[test]
    fn date_uses_the_requested_format() {
        let s = snippet(
            "{{d}}",
            vec![VarSpec {
                name: "d".into(),
                kind: VarKind::Date,
                params: serde_yaml::from_str("format: \"%Y\"").unwrap(),
            }],
        );
        let out = render(&s, &PreviewCtx::default());
        assert_eq!(out.text.len(), 4, "年份应为 4 位：{}", out.text);
    }

    #[test]
    fn caret_marker_survives() {
        let s = snippet("console.log($|$)", vec![]);
        let out = render(&s, &PreviewCtx::default());
        assert!(out.text.contains("$|$"));
    }

    #[test]
    fn unknown_type_keeps_placeholder() {
        let s = snippet(
            "{{x}}",
            vec![VarSpec {
                name: "x".into(),
                kind: VarKind::Unknown("weird".into()),
                params: Value::Null,
            }],
        );
        let out = render(&s, &PreviewCtx::default());
        assert_eq!(out.text, "{{x}}");
        assert_eq!(out.warnings.len(), 1);
    }
}
