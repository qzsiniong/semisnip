//! 变量渲染（**只读**）。
//!
//! 预览与展开是两条完全不同的路径：
//! - 展开：espanso 真求值，会跑 shell、弹表单、读剪贴板；
//! - 预览：这里只做无副作用的近似，让用户看清"将要插入什么"。
//!
//! 铁律：`shell` / `script` 在预览里**绝不执行**，只显示命令本身。

use crate::index::model::{Snippet, VarKind, VarSpec};

pub struct PreviewCtx {
    /// 剪贴板内容；`None` 表示不读取（默认策略）。
    pub clipboard: Option<String>,
    pub show_clipboard: bool,
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

#[derive(Clone, Debug)]
pub struct RenderedVar {
    pub name: String,
    pub kind: String,
    pub detail: String,
    pub side_effect: bool,
}

pub struct Rendered {
    pub text: String,
    pub vars: Vec<RenderedVar>,
    pub warnings: Vec<String>,
}

/// 把 `{{name}}` 逐个替换成预览值；未知变量保留原文。
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

    // `[[field]]` 是 form 简写的占位符
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

    if !snippet.form_fields.is_empty() {
        warnings.push("该片段会弹出 espanso 表单，展开后需要填写字段".into());
    }

    Rendered {
        text,
        vars,
        warnings,
    }
}

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
