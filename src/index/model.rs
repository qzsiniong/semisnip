//! 归一化后的片段模型：UI 只认这一层，不再接触 YAML。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_yaml::Value;

pub type SnippetId = u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Text(String),
    Markdown(String),
    Html(String),
    Image(PathBuf),
    /// `form:` 简写，正文是带 `[[字段]]` 的 layout。
    Form(String),
}

impl Body {
    pub fn as_text(&self) -> &str {
        match self {
            Self::Text(s) | Self::Markdown(s) | Self::Html(s) | Self::Form(s) => s,
            Self::Image(_) => "[图片]",
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Markdown(_) => "markdown",
            Self::Html(_) => "html",
            Self::Image(_) => "image",
            Self::Form(_) => "form",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VarKind {
    Echo,
    Date,
    Random,
    Clipboard,
    /// 展开时执行命令——预览**不执行**。
    Shell,
    Script,
    Form,
    Choice,
    Match,
    Unknown(String),
}

impl VarKind {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "echo" => Self::Echo,
            "date" => Self::Date,
            "random" => Self::Random,
            "clipboard" => Self::Clipboard,
            "shell" => Self::Shell,
            "script" => Self::Script,
            "form" => Self::Form,
            "choice" => Self::Choice,
            "match" => Self::Match,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// 是否带副作用（预览时必须避开）。
    pub fn is_side_effecting(&self) -> bool {
        matches!(self, Self::Shell | Self::Script)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Echo => "echo",
            Self::Date => "date",
            Self::Random => "random",
            Self::Clipboard => "clipboard",
            Self::Shell => "shell",
            Self::Script => "script",
            Self::Form => "form",
            Self::Choice => "choice",
            Self::Match => "match",
            Self::Unknown(raw) => raw,
        }
    }
}

#[derive(Clone, Debug)]
pub struct VarSpec {
    pub name: String,
    pub kind: VarKind,
    pub params: Value,
}

impl VarSpec {
    /// 取一个字符串参数（`cmd` / `echo` / `format` / `trigger` …）。
    pub fn param_str(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(|v| v.as_str())
    }

    /// 取字符串数组参数（`choices` / `values`）。
    pub fn param_list(&self, key: &str) -> Vec<String> {
        self.params
            .get(key)
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|item| match item {
                        Value::String(s) => Some(s.clone()),
                        Value::Mapping(map) => map
                            .get(Value::String("label".into()))
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct FormField {
    pub name: String,
    pub kind: String,
    pub default: Option<String>,
    pub values: Vec<String>,
    pub multiline: bool,
}

#[derive(Clone, Debug)]
pub struct SourceRef {
    pub file: PathBuf,
    /// 文件内的第几个 match（0 基）。YAML 解析拿不到行号，用序号定位。
    pub ordinal: usize,
    pub package: Option<String>,
}

impl SourceRef {
    pub fn display(&self) -> String {
        let name = self
            .file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.file.display().to_string());
        match &self.package {
            Some(pkg) => format!("{pkg}/{name}#{}", self.ordinal + 1),
            None => format!("{name}#{}", self.ordinal + 1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snippet {
    pub id: SnippetId,
    pub triggers: Vec<String>,
    pub label: String,
    pub body: Body,
    pub vars: Vec<VarSpec>,
    pub form_fields: Vec<FormField>,
    pub search_terms: Vec<String>,
    pub source: SourceRef,
    pub word: bool,
    /// espanso 是否真的认识这条（`match list --json` 交叉校验的结果）。
    pub usable: bool,
}

impl Snippet {
    pub fn trigger(&self) -> &str {
        self.triggers.first().map(String::as_str).unwrap_or("")
    }

    pub fn has_side_effect(&self) -> bool {
        self.vars.iter().any(|v| v.kind.is_side_effecting())
    }

    /// 用于模糊搜索的折叠文本：label + search_terms + 正文首行。
    pub fn search_blob(&self) -> String {
        let mut blob = self.label.to_lowercase();
        for term in &self.search_terms {
            blob.push(' ');
            blob.push_str(&term.to_lowercase());
        }
        let first_line = self.body.as_text().lines().next().unwrap_or("");
        blob.push(' ');
        blob.push_str(&first_line.to_lowercase());
        blob
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Diagnostic {
    /// 文件整体解析失败（被跳过）。
    ParseFailed { file: String, error: String },
    /// 同一 trigger 出现在多个文件里：espanso 会弹消歧窗。
    DuplicateTrigger { trigger: String, count: usize },
    /// 本地索引里有、但 espanso 不认（多半被 app filter 挡了）。
    UnknownToEspanso { trigger: String },
}

impl Diagnostic {
    pub fn message(&self) -> String {
        match self {
            Self::ParseFailed { file, error } => format!("{file} 解析失败：{error}"),
            Self::DuplicateTrigger { trigger, count } => {
                format!("`{trigger}` 有 {count} 处定义，展开时 espanso 会要求消歧")
            }
            Self::UnknownToEspanso { trigger } => {
                format!("`{trigger}` 未被 espanso 识别（可能被 app 过滤挡掉）")
            }
        }
    }
}

pub struct Snapshot {
    pub built_at_ms: i64,
    pub config_root: PathBuf,
    pub snippets: Vec<Arc<Snippet>>,
    pub by_trigger: HashMap<String, Vec<usize>>,
    pub diagnostics: Vec<Diagnostic>,
    /// espanso CLI 报上来的 trigger 全集；为空表示这次没校验成功。
    pub espanso_known: HashSet<String>,
    pub espanso_checked: bool,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self {
            built_at_ms: 0,
            config_root: PathBuf::new(),
            snippets: Vec::new(),
            by_trigger: HashMap::new(),
            diagnostics: Vec::new(),
            espanso_known: HashSet::new(),
            espanso_checked: false,
        }
    }

    pub fn len(&self) -> usize {
        self.snippets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Arc<Snippet>> {
        self.snippets.get(index)
    }

    pub fn duplicates(&self, trigger: &str) -> usize {
        self.by_trigger.get(trigger).map_or(0, |v| v.len())
    }
}

/// 检索结果：必须带上"命中的是哪个 trigger"，因为 `match exec` 要按 trigger 展开。
#[derive(Clone, Debug)]
pub struct Hit {
    pub snippet: Arc<Snippet>,
    pub trigger: String,
    pub score: i64,
}
