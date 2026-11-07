//! 详情预览的文档模型：UI 只负责把 `PreviewDoc` 画出来。

pub mod vars;

use crate::index::model::Snippet;
use crate::preview::vars::{PreviewCtx, RenderedVar, render};

/// 正文截断上限：足够看清结构，又不会把详情区撑爆。
///
/// 两个上限同时生效，取先到者——行数挡不住"一行一万字"，字符数也挡不住
/// "一万行短句"。数值是按面板详情栏的实际高度量的。
const MAX_CHARS: usize = 1200;
const MAX_LINES: usize = 24;

/// 详情栏要画的一切都已在这里算好，UI 不再做字符串处理。
///
/// 这么设计是因为详情栏的排版（等宽字体、折行、截断）在 gpui 里很难做，
/// 与其在渲染代码里现算，不如先在这里产出一份"已经定稿"的文档。
#[derive(Clone, Debug)]
pub struct PreviewDoc {
    /// 展开时会用的 trigger（可能不是片段的主 trigger，见 `Hit::trigger`）。
    pub trigger: String,
    pub label: String,
    /// `文件#序号` 形式的来源定位。
    pub source: String,
    /// 正文类型标签（`text` / `form` …）。
    pub kind: String,
    /// 已经求过变量、替换过光标标记的正文，可能被截断。
    pub body: String,
    /// `body` 是否为截断结果（UI 会提示"已截断"）。
    pub truncated: bool,
    /// 逐个变量的说明。
    pub vars: Vec<RenderedVar>,
    /// 需要提醒的事项，按重要性排好（重复定义在最前）。
    pub warnings: Vec<String>,
    /// espanso 是否认识这条；为假时回车大概率没反应。
    pub usable: bool,
    /// 同一 trigger 是否有多处定义。
    pub duplicate: bool,
}

impl PreviewDoc {
    /// 是否有"值得让用户停一下"的信息。
    ///
    /// UI 用它决定详情栏顶部要不要变色：只要有一条（espanso 不认、重复定义、
    /// 有副作用/表单提醒）就该醒目，而不是藏在小字里。
    pub fn critical(&self) -> bool {
        !self.usable || self.duplicate || !self.warnings.is_empty()
    }
}

/// 组装一份预览文档。
///
/// `trigger` 由调用方传入而不是从片段里取：用户可能是用第二个触发符搜到的，
/// 详情栏应该显示他脑子里想的那个词。
/// `duplicate_count` 来自快照的 `by_trigger`，>1 时在最前面插一条消歧提醒——
/// 这条比"里面有 shell 变量"更紧急，所以 `insert(0)` 而不是 `push`。
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

/// 截断正文，并把 espanso 的 `$|$` 光标占位符换成一个可见的块（`▮`）。
///
/// 双重上限（24 行 / 1200 字符）是给详情栏的固定高度留余量：只看行数挡不住
/// "一行一万字"的紧凑正文，只看字符数又会让多行片段在字符数没超时就把栏撑爆。
///
/// 截断按字符边界做（`chars().take()`），不会切坏 UTF-8。注意 `truncated`
/// 只表示"有内容被丢了"，具体丢了多少不影响展示。
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
