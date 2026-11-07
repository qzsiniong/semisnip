//! 详情预览的文档模型：UI 只负责把 `PreviewDoc` 画出来。

pub mod vars;

use crate::index::model::Snippet;
use crate::preview::vars::{PreviewCtx, RenderedVar, render};

/// 正文截断上限：足够看清结构，又不会把详情区撑爆。
const MAX_CHARS: usize = 1200;
const MAX_LINES: usize = 24;

#[derive(Clone, Debug)]
pub struct PreviewDoc {
    pub trigger: String,
    pub label: String,
    pub source: String,
    pub kind: String,
    pub body: String,
    pub truncated: bool,
    pub vars: Vec<RenderedVar>,
    pub warnings: Vec<String>,
    pub usable: bool,
    pub duplicate: bool,
}

impl PreviewDoc {
    pub fn critical(&self) -> bool {
        !self.usable || self.duplicate || !self.warnings.is_empty()
    }
}

pub fn build(
    snippet: &Snippet,
    trigger: &str,
    duplicate_count: usize,
    ctx: &PreviewCtx,
) -> PreviewDoc {
    let rendered = render(snippet, ctx);
    let (body, truncated) = clip(&rendered.text);

    let mut warnings = rendered.warnings;
    if duplicate_count > 1 {
        warnings.insert(
            0,
            format!("同一 trigger 有 {duplicate_count} 处定义，回车后 espanso 会要求消歧"),
        );
    }

    PreviewDoc {
        trigger: trigger.to_string(),
        label: snippet.label.clone(),
        source: snippet.source.display(),
        kind: snippet.body.kind().to_string(),
        body,
        truncated,
        vars: rendered.vars,
        warnings,
        usable: snippet.usable,
        duplicate: duplicate_count > 1,
    }
}

/// 展示用：把 espanso 的 `$|$` 光标占位符换成一个可见的块。
fn clip(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len().min(MAX_CHARS * 4));
    let mut truncated = false;

    for (line_no, line) in text.lines().enumerate() {
        if line_no >= MAX_LINES {
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        if out.chars().count() > MAX_CHARS {
            out = out.chars().take(MAX_CHARS).collect();
            truncated = true;
            break;
        }
    }

    let replaced = out.replace("$|$", "▮");
    (replaced, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{Body, SourceRef};
    use std::path::PathBuf;

    fn snippet(body: &str) -> Snippet {
        Snippet {
            id: 1,
            triggers: vec![";;a".into()],
            label: "A".into(),
            body: Body::Text(body.into()),
            vars: vec![],
            form_fields: vec![],
            search_terms: vec![],
            source: SourceRef {
                file: PathBuf::from("/tmp/base.yml"),
                ordinal: 0,
                package: None,
            },
            word: false,
            usable: true,
        }
    }

    #[test]
    fn caret_is_visualised() {
        let doc = build(
            &snippet("console.log($|$)"),
            ";;a",
            1,
            &PreviewCtx::default(),
        );
        assert!(doc.body.contains('▮'));
    }

    #[test]
    fn long_body_is_truncated() {
        let long: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let doc = build(&snippet(&long), ";;a", 1, &PreviewCtx::default());
        assert!(doc.truncated);
        assert!(doc.body.lines().count() <= MAX_LINES);
    }

    #[test]
    fn duplicate_trigger_warns() {
        let doc = build(&snippet("x"), ";;a", 2, &PreviewCtx::default());
        assert!(doc.duplicate);
        assert!(!doc.warnings.is_empty());
    }
}
