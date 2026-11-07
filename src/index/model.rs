//! 归一化后的片段模型：UI 只认这一层，不再接触 YAML。
//!
//! espanso 的 match 定义非常宽松——trigger 可以是字符串也可以是数组，body 可以
//! 是纯文本、markdown、html、图片路径，`form:` 还能简写成一行。解析这些东西的
//! 代码在 `index::loader`，产出这里的结构；之后所有环节（检索、预览、展开、
//! 面板渲染）都只看这些已经规整过的字段。
//!
//! 这样做的收益：YAML 的方言变化只影响 loader 一个文件；上层不需要到处
//! `match value { ... }`，也不会因为某个字段缺失而 panic。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_yaml::Value;

/// 片段在单次快照内的稳定标识。
///
/// 注意它**不是**持久 id：重建索引时会重新分配，只能用来在同一次快照里做引用。
/// 需要跨快照稳定时用 `trigger`（这也是使用统计的 key）。
pub type SnippetId = u64;

/// 片段正文的形态。espanso 支持几种互斥的写法，这里照抄成枚举。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// 普通纯文本（`text:` 或直接写在 `replace:` 下）。
    Text(String),
    /// markdown 源码。展开时依然是纯文本，只是面板预览按 markdown 渲染。
    Markdown(String),
    /// html 源码。同上，预览时按 HTML 处理。
    Html(String),
    /// 图片片段（`image_path:`）。记录下来，但预览不加载文件内容。
    Image(PathBuf),
    /// `form:` 简写，正文是带 `[[字段]]` 的 layout。
    Form(String),
}

impl Body {
    /// 取可读文本。
    ///
    /// 用于搜索折叠、预览渲染和兜底粘贴。图片没有"文本"可言，返回占位符，
    /// 免得调用方在 UI 里画出一个空字符串。
    pub fn as_text(&self) -> &str {
        match self {
            Self::Text(s) | Self::Markdown(s) | Self::Html(s) | Self::Form(s) => s,
            Self::Image(_) => "[图片]",
        }
    }

    /// 正文类型的短名，直接用于界面上的类型标签（`text` / `markdown` …）。
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

/// espanso 变量（`{{name}}`）的类型。
///
/// 只区分"预览时怎么处理"，不追求覆盖 espanso 的全部语义：未知类型统一落到
/// [`VarKind::Unknown`]，预览时保留占位符并给一条提示，而不是报错。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VarKind {
    /// 常量（`echo:`）。
    Echo,
    /// 日期时间（`date:`），带 `format` 与可选 `offset`。
    Date,
    /// 随机取一个（`random:`）。
    Random,
    /// 读剪贴板（`clipboard:`）。
    Clipboard,
    /// 展开时执行命令——预览**不执行**。
    Shell,
    /// 展开时执行脚本——预览**不执行**。
    Script,
    /// 展开时弹表单（`form:` 变量）。
    Form,
    /// 展开时让用户选一个（`choice:`）。
    Choice,
    /// 嵌套展开另一条 trigger（`match:`）。
    Match,
    /// 不认识的类型，保留原始名字用于提示。
    Unknown(String),
}

impl VarKind {
    /// 从 YAML 里的类型名映射成枚举。未知名字保留下来，方便在界面上告诉用户
    /// 到底哪一类没被支持。
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
    ///
    /// 只有 shell / script 会真执行命令。clipboard 也算"读外部状态"，但它不改变
    /// 系统、代价也只有一次系统提示，所以单独由 `PreviewCtx.show_clipboard` 控制，
    /// 不列在这里——否则用户即使打开了剪贴板预览也永远看不到内容。
    pub fn is_side_effecting(&self) -> bool {
        matches!(self, Self::Shell | Self::Script)
    }

    /// 界面上的类型标签。
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

/// 一个变量的完整定义：名字 + 类型 + 原始参数表。
///
/// `params` 保留成 `serde_yaml::Value` 而不是逐类型建结构体，是因为 espanso 的
/// 参数按类型各不相同（`format`/`offset`/`cmd`/`args`/`choices`/`values`…），
/// 穷举会随着版本升级不断加分支。取用时由 `param_str` / `param_list` 兜底。
#[derive(Clone, Debug)]
pub struct VarSpec {
    pub name: String,
    pub kind: VarKind,
    pub params: Value,
}

impl VarSpec {
    /// 取一个字符串参数（`cmd` / `echo` / `format` / `trigger` …）。
    ///
    /// 类型不符（例如写成了数字）时返回 `None`，由调用方决定用默认值还是提示。
    pub fn param_str(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(|v| v.as_str())
    }

    /// 取字符串数组参数（`choices` / `values`）。
    ///
    /// 两种写法都要认：纯字符串数组，以及 `{label: ..., value: ...}` 形式的映射
    /// （espanso 用它给选项起显示名）。映射里取 `label`，取不到就跳过这一项。
    /// 缺失或类型不符时返回空 `Vec`——调用方按"没有候选"处理即可。
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

/// `form:` 简写里的一个字段（正文中的 `[[name]]` 占位）。
#[derive(Clone, Debug, Default)]
pub struct FormField {
    pub name: String,
    /// 字段类型（`text` / `choice` / `list` …），保持原始字符串以便透传到界面。
    pub kind: String,
    /// 默认值。预览会用它填进正文；没有则显示 `<类型>` 占位。
    pub default: Option<String>,
    /// `choice` 类型的候选值。
    pub values: Vec<String>,
    /// 是否多行输入。影响预览里那段说明文字。
    pub multiline: bool,
}

/// 片段是从哪儿来的。面板详情栏与诊断信息都要展示它。
#[derive(Clone, Debug)]
pub struct SourceRef {
    pub file: PathBuf,
    /// 文件内的第几个 match（0 基）。YAML 解析拿不到行号，用序号定位。
    ///
    /// 对用户的展示习惯是 1 基（`base.yml#3`），所以 `display()` 会 +1。
    pub ordinal: usize,
    /// 来自 espanso package 时的包名（`packages/` 下的目录名）。
    pub package: Option<String>,
}

impl SourceRef {
    /// 形如 `log/base.yml#3` 的定位串，用于面板详情和 `doctor` 输出。
    ///
    /// 只取文件名不取全路径：全路径太长会把详情栏撑坏，而用户通常只需要
    /// 知道"哪个文件里的第几条"，必要时可以配合 `semisnip match list` 找全路径。
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

/// 解析完成的一条片段。
#[derive(Clone, Debug)]
pub struct Snippet {
    pub id: SnippetId,
    /// 全部 trigger。espanso 允许一条 match 挂多个 trigger，第一个是"主"的，
    /// 展开时也用它（见 [`Snippet::trigger`]）。
    pub triggers: Vec<String>,
    /// 人类可读的名字，来自 `label:`；缺失时由 loader 用正文首行补。
    pub label: String,
    pub body: Body,
    pub vars: Vec<VarSpec>,
    pub form_fields: Vec<FormField>,
    /// `search_terms:` 里声明的额外检索词（espanso 原生字段）。
    pub search_terms: Vec<String>,
    pub source: SourceRef,
    /// `word: true`——只在词边界命中。面板不模拟词边界，仅作信息展示。
    pub word: bool,
    /// espanso 是否真的认识这条（`match list --json` 交叉校验的结果）。
    ///
    /// 为假说明本地读到了但 espanso 不认（典型原因：被 app filter 挡掉）。
    /// 这类片段仍然展示，但在排序里降权——回车大概率什么都不发生。
    pub usable: bool,
}

impl Snippet {
    /// 主 trigger；空列表时返回空串（而非 panic），调用方按"无效"处理。
    pub fn trigger(&self) -> &str {
        self.triggers.first().map(String::as_str).unwrap_or("")
    }

    /// 是否包含会在展开时产生副作用的变量。
    ///
    /// 面板用它决定要不要在候选行上加个提示，避免用户以为回车只是插文本，
    /// 结果触发了一条会执行命令的片段。
    pub fn has_side_effect(&self) -> bool {
        self.vars.iter().any(|v| v.kind.is_side_effecting())
    }

    /// 用于模糊搜索的折叠文本：label + search_terms + 正文首行。
    ///
    /// 只取正文首行是性能取舍：整段正文（可能几千字）拼进检索串会让每次按键的
    /// 子序列扫描变慢，而首行已经覆盖了绝大多数"我大概记得它长什么样"的场景。
    /// 全部转小写，与 `Query::parse` 的规整方式保持一致。
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

/// 索引阶段发现的问题。不阻断加载，只汇总给 `doctor` 与日志。
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
    /// 一条人类可读的说明，直接用于 CLI 输出与面板提示。
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

/// 一次索引构建的完整产物，**不可变**。
///
/// 后台线程构建完成后整体替换（`Arc` 交换），前台只读。因此不存在"读到一半被改"
/// 的问题，也不需要锁。`built_at_ms` 既是时间戳也是版本号：前台靠它判断
/// 自己的检索表是不是过期了（见 `AppState::sync_index`）。
pub struct Snapshot {
    /// 构建时刻（毫秒）。0 表示空快照。
    pub built_at_ms: i64,
    /// 扫描的根目录（espanso 的 `match/`），用于诊断输出。
    pub config_root: PathBuf,
    pub snippets: Vec<Arc<Snippet>>,
    /// trigger → 在 `snippets` 里的下标列表。长度 > 1 即重复定义。
    pub by_trigger: HashMap<String, Vec<usize>>,
    pub diagnostics: Vec<Diagnostic>,
    /// espanso CLI 报上来的 trigger 全集；为空表示这次没校验成功。
    ///
    /// 注意它包含 espanso 认识的**全部** trigger，包括我们自己写进
    /// `semisnip.yml` 的降级触发，所以数量不能直接和候选数对比。
    pub espanso_known: HashSet<String>,
    /// 这一次是否真的跑过 `match list` 并用它做了交叉校验。
    ///
    /// 与 `espanso_known.is_empty()` 不等价：espanso 装了但一条片段都没有时，
    /// `known` 为空而 `checked` 为真。面板的健康状态行按这个字段说话。
    pub espanso_checked: bool,
}

impl Snapshot {
    /// 空快照：索引还没读完、或读取失败时的兜底。所有查询都能安全地在它上面跑。
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

    /// 片段条数。
    pub fn len(&self) -> usize {
        self.snippets.len()
    }

    /// 是否一条都没有。
    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    /// 按下标取片段。下标来自 `SearchIndex`，两者必须同源，越界说明重建过程有 bug。
    pub fn get(&self, index: usize) -> Option<&Arc<Snippet>> {
        self.snippets.get(index)
    }

    /// 某个 trigger 被定义了几次。0 表示不存在，>1 表示会弹消歧窗。
    pub fn duplicates(&self, trigger: &str) -> usize {
        self.by_trigger.get(trigger).map_or(0, |v| v.len())
    }
}

/// 检索结果：必须带上"命中的是哪个 trigger"，因为 `match exec` 要按 trigger 展开。
///
/// 一条 match 可以有多个 trigger，用户可能用第二个触发符搜到了它——这时
/// `trigger` 是用户实际搜到的那个，而不是 `snippet.trigger()`。
#[derive(Clone, Debug)]
pub struct Hit {
    pub snippet: Arc<Snippet>,
    pub trigger: String,
    pub score: i64,
}
