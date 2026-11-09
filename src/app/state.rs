//! 面板的单一数据源与交互状态机。
//!
//! 边界很清楚：**这里不碰 gpui 的窗口与事件系统**。
//! - 按键由 `trigger::tap` 解析成 [`KeyMsg`] 后调 [`AppState::on_key`]；
//! - 需要开窗/关窗/展开的动作以 [`Intent`] 返回，由 `app::scheduler` 执行；
//! - 渲染由 `ui::view` 只读访问本结构的字段。
//!
//! 这样拆分的目的：交互逻辑（过滤、选中、预览、回删计数）可以脱离窗口单元测试，
//! 窗口层也不必知道"按了回车之后该先关窗还是先展开"这种顺序约束。
//!
//! 线程模型：`AppState` 是 gpui 的 `Entity`，只在主线程被修改；索引则由
//! `IndexService` 在后台线程重建，通过 `Snapshot` 的时间戳交接（见 `sync_index`）。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{AnyWindowHandle, Context, Entity, Global};

use crate::app::event::KeyMsg;
use crate::config::Config;
use crate::handoff;
use crate::index::model::split_label;
use crate::index::service::IndexService;
use crate::index::{Hit, Snapshot};
use crate::ipc::proto::Origin;
use crate::library::migrate::{self, MigrationPlan};
use crate::library::{Library, SnippetKey, snippet_key};
use crate::paths;
use crate::preview::{self, PreviewDoc, vars::PreviewCtx};
use crate::search::{Query, SearchIndex, rank_direct, rank_filtered};
use crate::trigger::TriggerConfig;
use crate::trigger::keymap;
use crate::usage::{self, Usage, UsageKind};

/// 一次按键处理的结果。
///
/// 面板自己**不执行**副作用动作：它只判断"用户想干什么"，把决定权交回调度器。
/// 这么做的原因是顺序敏感——确认时必须先关窗（放开吞键）再让 espanso 展开，
/// 否则 espanso 注入的字符会被自己的 tap 再吃一遍。把顺序集中在一处，
/// 比在 UI 回调里散落着写要安全得多。
pub enum Intent {
    /// 继续留在面板里（打字、移动选中、退格都属于这一类）。
    None,
    /// 用户按了回车：展开这条片段。
    Confirm(Hit),
    /// 用户按了 Esc / 点了关闭按钮：收起面板。
    Cancel,
    /// 重新读一遍 espanso 的 match 目录（管理面板的应用迁移之后）。
    Reload,
    /// 打开偏好设置窗口（`/` 命令菜单）。
    OpenSettings,
}

/// 关键字视图返回到哪里。从 Query 进来就回 Query；从管理面板进来要回管理，
/// 否则 Esc 会把用户的多选上下文弄丢。
#[derive(Clone, Debug)]
pub enum KeywordReturn {
    Query,
    Manage(Box<ManageState>),
}

/// 管理面板的可变状态。列表本身不缓存，每次按键/渲染时按快照现算，
/// 这样后台索引重建后不会留下过期行。
#[derive(Clone, Debug, Default)]
pub struct ManageState {
    pub cursor: usize,
    pub selected: BTreeSet<SnippetKey>,
    pub filter: Option<PathBuf>,
    pub scroll: usize,
    /// 标签输入态：`Some(buf)` 表示正在给选中项批量加标签。
    pub tag_input: Option<String>,
}

/// 迁移预览状态。`manage` 是返回目标，Esc 时原样还回去。
#[derive(Clone, Debug)]
pub struct MigrateState {
    pub plans: Vec<MigrationPlan>,
    pub selected: BTreeSet<usize>,
    pub cursor: usize,
    pub scroll: usize,
    pub manage: Box<ManageState>,
}

/// 面板的模式。Query/Direct 共用 `hits` 列表；其余模式接管渲染与按键。
#[derive(Clone, Debug)]
pub enum PanelMode {
    Query,
    Keyword { hit: Hit, return_to: KeywordReturn },
    Direct { query: String },
    Command { cursor: usize },
    Manage(ManageState),
    Migrate(MigrateState),
}

/// `/` 命令菜单的固定项。顺序即界面顺序。
pub const COMMANDS: [&str; 5] = [
    "重新加载索引",
    "显示 / 隐藏已停用",
    "打开片段管理",
    "打开偏好设置",
    "快捷键速查",
];

/// 管理面板可见行数。滚动窗口与光标夹取共用这个值。
pub const MANAGE_VISIBLE_ROWS: usize = 8;

/// 关键字视图里的一行触发词。
#[derive(Clone, Debug)]
pub struct TriggerView {
    pub text: String,
    /// 是否带直达前缀（带前缀的才能在面板外直接打出）。
    pub is_direct: bool,
    /// 共用这个触发词的文件条数（>1 时 espanso 会弹消歧窗）。
    pub count: usize,
}

/// 关键字视图（按 `,`）的全部数据。
#[derive(Clone, Debug)]
pub struct KeywordView {
    pub triggers: Vec<TriggerView>,
    pub aliases: Vec<String>,
    pub source: String,
    pub duplicates: usize,
    pub usage_count: u32,
    pub week_panel: u32,
    pub week_direct: u32,
    /// 没有直达触发词时的迁移建议。
    pub suggestion: Option<String>,
}

/// 把滚动窗口夹到光标附近。管理面板与迁移预览共用。
fn adjust_scroll(scroll: &mut usize, cursor: usize, rows: usize) {
    if rows == 0 {
        *scroll = 0;
        return;
    }
    if cursor < *scroll {
        *scroll = cursor;
    }
    if cursor >= *scroll + rows {
        *scroll = cursor + 1 - rows;
    }
}

/// 面板的全部可变状态。
///
/// 字段大多 `pub`：渲染层要逐项读取，包一层 getter 只会增加噪声。
/// 但**写**只应发生在本文件的方法里——外部直接改字段会让 `hits`/`preview`/
/// `selected` 三者失去同步。
pub struct AppState {
    /// 索引服务：持有后台线程与当前 `Snapshot` 的句柄。
    pub index: Arc<IndexService>,
    /// 预折叠的检索表，下标与 `snapshot().snippets` 一一对应。
    pub search: SearchIndex,
    /// 使用频率统计（排序加权用，丢了不影响正确性）。
    pub usage: Usage,
    /// 当前配置。设置窗口改盘之后由 `reload_config` 换成新值。
    pub config: Config,
    /// 片段本地库：停用状态与别名。不写回用户 YAML。
    pub library: Library,
    /// 面板内当前模式。`Query` 是默认态，关闭面板后也回到这里。
    pub mode: PanelMode,
    /// `/` 命令里“显示已停用”的会话开关。不落盘。
    pub show_disabled: bool,
    /// 全文模式：`'` 切换。打开后正文参与匹配（最低档），默认关闭。
    pub full_text: bool,
    /// 快捷键速查覆盖层。叠在当前视图上，不替换模式。
    pub help_open: bool,
    /// 标签建议弹窗：用户输入 `:` 时弹出，Esc / 空格 / 选中后关闭。
    pub tag_suggest: bool,
    /// 标签建议列表里的选中下标。
    pub tag_cursor: usize,

    /// 面板是否处于打开状态（逻辑状态，不代表窗口一定存在）。
    pub open: bool,
    /// 过滤串：用户键入的原始字符，不做任何规整。
    pub query: String,
    /// 当前过滤结果，已按相关度排序并截断到 `config.max_results`。
    pub hits: Vec<Hit>,
    /// 选中项在 `hits` 里的下标；`hits` 为空时无意义。
    pub selected: usize,
    /// 落进目标文档的触发符字符数。
    ///
    /// 触发串**一个键都不吞**（见 `trigger::machine`），所以 tap 路径它等于
    /// 触发串全长：确认时回删这么多（给 espanso 一个干净的光标位置），取消时
    /// 什么都不用做——字符本来就在文档里。espanso 降级路径是 **0**：那份 match
    /// 只让 espanso 擦掉触发串、不注回（见 `cli::render_match_file`），面板出现
    /// 时文档已经干净；菜单栏 / 手动唤起同样是 0。
    pub erase: usize,
    /// 面板是被谁唤起的。
    ///
    /// 主要用于日志与 `erase` 的解释：三种来源的文档状态不同——tap 路径触
    /// 发串完整落在文档里；espanso 降级路径的触发串被 espanso 擦掉又原样注回
    /// （见 `cli::render_match_file`），效果相同；菜单栏 / 手动唤起则根本没有
    /// 触发词。
    pub origin: Origin,
    /// 选中项的预览文档；无命中时为 `None`。
    pub preview: Option<PreviewDoc>,

    /// 一行临时状态文字（错误提示等），显示在面板底部。
    pub status: String,
    /// 键盘 tap 此刻是否可用。
    ///
    /// 为假时面板收不到任何按键，`ui::view` 会整块换成授权引导页。
    /// 注意它每次打开面板都要重读（`Event::Open` 里做），因为 `AppState::new`
    /// 早于 `tap::install`，那时恒为假。
    pub tap_ok: bool,
    /// 面板窗口句柄。窗口由 `ui::window` 创建后写入，关闭时摘除；
    /// 菜单栏靠它判断第一项该显示"打开"还是"关闭"。
    pub handle: Option<AnyWindowHandle>,

    /// 当前 SearchIndex 对应的快照时间戳，用来判断要不要重建检索表。
    built_at: i64,
    /// 上一次在空查询下按 `,` 的时间；用于区分“单击开关键字视图”和“连按进直达”。
    last_comma_at: Option<Instant>,
}

impl AppState {
    /// 构造状态。此时索引可能还没读完（`IndexService::new` 会先给一份空快照），
    /// 所以这里同步构建一次检索表，即使为空也要让 `hits` 处于自洽状态。
    pub fn new(cx: &mut Context<Self>, index: Arc<IndexService>) -> Self {
        let config = Config::load();
        let usage = Usage::load(&paths::usage_path());
        let library = Library::load(&paths::library_path());
        let snapshot = index.snapshot();
        let search = SearchIndex::build_with_aliases(&snapshot, &library);
        let built_at = snapshot.built_at_ms;
        let mut state = Self {
            index,
            search,
            usage,
            config,
            library,
            mode: PanelMode::Query,
            show_disabled: false,
            full_text: false,
            help_open: false,
            tag_suggest: false,
            tag_cursor: 0,
            open: false,
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            erase: 0,
            origin: Origin::Manual,
            preview: None,
            status: String::new(),
            tap_ok: crate::trigger::tap::ready(),
            handle: None,
            built_at,
            last_comma_at: None,
        };
        state.refilter(&snapshot);
        state.refresh_direct_triggers();
        let _ = cx;
        state
    }

    /// 取当前快照（引用计数，廉价）。
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.index.snapshot()
    }

    /// 索引可能已被后台线程替换：按时间戳判断是否需要重建检索表。
    ///
    /// 后台重建索引时不会锁住状态，所以读到的可能是新快照但旧的 `search`。
    /// 用 `built_at_ms` 做版本号，比逐条比较要便宜得多。时间为 0 表示空快照，
    /// 与"还没构建过"同义，不触发重建。
    pub fn sync_index(&mut self) {
        let snapshot = self.index.snapshot();
        if snapshot.built_at_ms != self.built_at {
            self.search = SearchIndex::build_with_aliases(&snapshot, &self.library);
            self.built_at = snapshot.built_at_ms;
            self.refresh_direct_triggers();
            // 管理/迁移面板的行是现算的，不在这里重排光标；只把越界的光标拉回来。
            // 先把 mode 取出来再调 `self` 的方法：`clamp_manage` 需要不可变借用
            // 整个 `self`，直接 `&mut self.mode` 会冲突。
            let mut mode = std::mem::replace(&mut self.mode, PanelMode::Query);
            match &mut mode {
                PanelMode::Manage(state) => self.clamp_manage(state),
                PanelMode::Migrate(state) => {
                    state.cursor = state.cursor.min(state.plans.len().saturating_sub(1));
                    adjust_scroll(&mut state.scroll, state.cursor, MANAGE_VISIBLE_ROWS);
                }
                _ => {}
            }
            self.mode = mode;
            self.refilter(&snapshot);
        }
    }

    /// 配置被设置窗口改写了：重新读盘，并把"已经活着"的部分换掉。
    ///
    /// 面板尺寸、显示详情这些**不用在这里处理**——面板窗口每次打开都是新建的，
    /// `ui::window::open` 会重新读 `config.panel_width/height`。这里只需管
    /// 那些状态已经存在的：触发状态机、候选条数（影响当前列表）、以及
    /// 「启用触发」这个开关（它直接决定 tap 认不认触发串）。
    ///
    /// 触发串映射失败时**保留旧状态机**：设置窗口的输入框是逐字符落盘的，
    /// 用户打到一半的 `,` 不该让 `;;` 当场失灵。
    pub fn reload_config(&mut self) {
        let config = Config::load();
        match TriggerConfig::from_text(&config.trigger, config.trigger_timeout_ms) {
            Ok(trigger) => crate::trigger::tap::set_trigger(trigger),
            Err(err) => log::warn!("触发串 `{}` 未生效：{err}", config.trigger),
        }
        // 暂停/恢复与触发串分开设：即使触发串此刻不合法，开关也该照样生效。
        crate::trigger::tap::set_enabled(config.enabled);
        self.config = config;
        self.refresh_direct_triggers();
        self.refilter_owned();
    }

    /// 进入打开状态：清空上一轮残留，按最新索引重算候选。
    ///
    /// `origin` 与 `erase` 一起来自触发来源，两个都要记住：
    /// - tap 路径：触发键一个不吞，文档里躺着完整的触发串（`erase` = 全长）；
    /// - espanso 降级：调度器按降级触发串长度算 `erase`（espanso 擦掉又原样
    ///   注回，效果同上）；
    /// - 菜单栏 / 手动：`erase` 是 0，文档里没有触发词。
    pub fn begin_open(&mut self, origin: Origin, erase: usize) {
        self.open = true;
        self.mode = PanelMode::Query;
        self.last_comma_at = None;
        self.help_open = false;
        self.tag_suggest = false;
        self.tag_cursor = 0;
        self.query.clear();
        self.selected = 0;
        self.erase = erase;
        self.origin = origin;
        self.status.clear();
        self.sync_index();
        self.refilter_owned();
    }

    /// 进入关闭状态：把与"打开"相关的字段全部复位。
    ///
    /// 保留 `usage`/`config`/`search`——它们与面板是否可见无关。
    /// `erase`/`origin` 必须一起清掉：它们是"这一轮打开的现场"，留着会让下一次
    /// 与面板无关的关闭（比如进程退出前的收尾）误读成"要补回 `;;`"。
    pub fn begin_close(&mut self) {
        self.open = false;
        self.mode = PanelMode::Query;
        self.last_comma_at = None;
        self.help_open = false;
        self.tag_suggest = false;
        self.tag_cursor = 0;
        self.query.clear();
        self.hits.clear();
        self.preview = None;
        self.erase = 0;
        self.origin = Origin::Manual;
        self.selected = 0;
        self.library.flush_if_dirty(&paths::library_path());
    }

    /// 追加一个过滤字符。
    ///
    /// 上限 60 字符纯粹是防呆：再长也不会命中更多东西，反而每次按键都要多扫一遍。
    /// 追加后选中项回到第一条——用户继续打字时预期是"重新开始挑"，而不是
    /// 停留在原来的序号上。
    pub fn type_char(&mut self, ch: char) {
        if self.query.chars().count() >= 60 {
            return;
        }
        self.query.push(ch);
        self.selected = 0;
        self.refilter_owned();
    }

    /// 退格一个字符；空串时什么都不做（返回 false 也无意义，调用方不需要知道）。
    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.selected = 0;
            self.refilter_owned();
            // 当前词不再以 `:` 开头时，关闭标签建议。
            self.tag_suggest = self.current_tag_prefix().is_some();
        }
    }

    /// 当前正在输入的标签词（冒号后面的部分）。
    ///
    /// 取查询串最后一个空白分隔的词；只有它以 `:` 开头时才算"正在打标签"。
    /// 这是判断标签建议该不该开的依据。
    pub fn current_tag_prefix(&self) -> Option<String> {
        let word = self.query.split_whitespace().last()?;
        word.strip_prefix(':').map(|s| s.to_string())
    }

    /// 过滤后的标签候选列表：从索引里收集全部标签，按当前前缀过滤，取前 8。
    pub fn tag_candidates(&self) -> Vec<String> {
        let prefix = self.current_tag_prefix().unwrap_or_default();
        let snapshot = self.index.snapshot();
        let mut tags: BTreeSet<String> = BTreeSet::new();
        for snippet in &snapshot.snippets {
            let (_, snippet_tags) = split_label(&snippet.label);
            for tag in snippet_tags {
                tags.insert(tag.to_ascii_lowercase());
            }
        }
        tags.into_iter()
            .filter(|tag| tag.starts_with(&prefix))
            .take(8)
            .collect()
    }

    /// 选中当前高亮的标签候选，替换查询里正在输入的那个词。
    pub fn accept_tag_suggestion(&mut self) {
        if !self.tag_suggest {
            return;
        }
        let candidates = self.tag_candidates();
        if candidates.is_empty() {
            self.tag_suggest = false;
            return;
        }
        let tag = &candidates[self.tag_cursor.min(candidates.len() - 1)];
        // 找到当前词开头的那个冒号，把它连同后面的字符替换成 `:tag` + 空格。
        if let Some(colon) = self.query.rfind(':') {
            self.query.truncate(colon);
            self.query.push(':');
            self.query.push_str(tag);
            self.query.push(' ');
        }
        self.tag_suggest = false;
        self.selected = 0;
        self.refilter_owned();
    }

    /// 在标签建议列表里上下移动。
    pub fn move_tag_suggestion(&mut self, delta: isize) {
        let len = self.tag_candidates().len() as isize;
        if len == 0 {
            return;
        }
        self.tag_cursor = (self.tag_cursor as isize + delta).rem_euclid(len) as usize;
    }

    /// 上下移动选中项，**循环**：第一条再往上跳到末尾。
    ///
    /// 用 `rem_euclid` 而不是取模，避免 `-1 % n` 在 Rust 里得到负数。
    /// 空列表直接返回，防止 `rem_euclid(0)` 除零 panic。
    pub fn move_selection(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        let len = self.hits.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        self.rebuild_preview();
    }

    /// 当前选中的那条；无命中时为 `None`。
    pub fn selected_hit(&self) -> Option<&Hit> {
        self.hits.get(self.selected)
    }

    /// 按键的单一入口。返回的 [`Intent`] 决定面板之后发生什么。
    ///
    /// 这里只处理"面板内部"的语义；带修饰键的组合（Cmd/Ctrl + 任意键）在
    /// `trigger::tap` 那一层就被放行了，不会走到这里——那类按键属于系统快捷键，
    /// 面板不该截胡。控制字符也在这里被过滤掉（`char_for_keycode` 可能返回
    /// 方向键残留的码位）。
    pub fn on_key(&mut self, key: KeyMsg) -> Intent {
        self.on_key_at(key, Instant::now())
    }

    /// 可注入时间的版本，供单测固定 `,` 的连按窗口。
    ///
    /// 先把 mode 取出来再分派：各分支会重新写 `self.mode`，这样"停留在当前模式"
    /// 与"切到别的模式"都在同一个地方表达，不需要额外的标志位。
    pub fn on_key_at(&mut self, key: KeyMsg, now: Instant) -> Intent {
        // 速查是覆盖层：打开后它先吃掉所有按键，只认 Esc / `?` 两种关闭方式。
        // 这样关掉之后原视图（查询、管理、迁移……）原样还在，不用重算。
        if self.help_open {
            if key.code == keymap::ESCAPE
                || keymap::char_for_keycode(key.code, key.shift) == Some('?')
            {
                self.help_open = false;
            }
            return Intent::None;
        }

        let mode = std::mem::replace(&mut self.mode, PanelMode::Query);
        match mode {
            PanelMode::Query => self.query_key(key, now),
            PanelMode::Keyword { hit, return_to } => self.keyword_key(key, now, hit, return_to),
            PanelMode::Direct { query } => self.direct_key(key, query),
            PanelMode::Command { cursor } => self.command_key(key, cursor),
            PanelMode::Manage(mut state) => {
                // 先把模式放回去，这样"只改了光标/选择"的按键不会被误判成切换。
                self.mode = PanelMode::Manage(state.clone());
                let intent = self.manage_key(key, &mut state);
                if matches!(self.mode, PanelMode::Manage(_)) {
                    self.mode = PanelMode::Manage(state);
                }
                intent
            }
            PanelMode::Migrate(mut state) => {
                self.mode = PanelMode::Migrate(state.clone());
                let intent = self.migrate_key(key, &mut state);
                if matches!(self.mode, PanelMode::Migrate(_)) {
                    self.mode = PanelMode::Migrate(state);
                }
                intent
            }
        }
    }

    fn query_key(&mut self, key: KeyMsg, now: Instant) -> Intent {
        // 标签建议打开时，方向键 / 回车 / Esc 都先服务于它。
        // 退格与普通字符继续走正常路径：退格会根据残留词自动关弹窗，
        // 普通字符则边打边过滤候选。
        if self.tag_suggest {
            match key.code {
                keymap::ESCAPE => {
                    self.tag_suggest = false;
                    return Intent::None;
                }
                keymap::RETURN => {
                    self.accept_tag_suggestion();
                    return Intent::None;
                }
                keymap::UP => {
                    self.move_tag_suggestion(-1);
                    return Intent::None;
                }
                keymap::DOWN => {
                    self.move_tag_suggestion(1);
                    return Intent::None;
                }
                _ => {
                    // 空格意味着这个标签词输入完毕，关掉建议。
                    if keymap::char_for_keycode(key.code, key.shift) == Some(' ') {
                        self.tag_suggest = false;
                    }
                }
            }
        }

        // Query 在 on_key_at 里已经复位成 Query；这里只在切模式时改写。
        match key.code {
            keymap::ESCAPE => Intent::Cancel,
            keymap::RETURN => match self.selected_hit().cloned() {
                Some(hit) => Intent::Confirm(hit),
                None => Intent::None,
            },
            keymap::BACKSPACE => {
                // 过滤串为空还按退格：用户是在"一路往回退"，退到没有可退的就该
                // 退出面板——否则这个键在空串上是个哑键，用户只能改用 Esc/鼠标。
                // 退格删的是过滤词，不是触发词：触发词由 `cancel` 补回文档。
                if self.query.is_empty() {
                    Intent::Cancel
                } else {
                    self.backspace();
                    Intent::None
                }
            }
            keymap::UP => {
                self.move_selection(-1);
                Intent::None
            }
            keymap::DOWN => {
                self.move_selection(1);
                Intent::None
            }
            _ => {
                let Some(ch) = keymap::char_for_keycode(key.code, key.shift) else {
                    return Intent::None;
                };
                if ch.is_control() {
                    return Intent::None;
                }
                match ch {
                    // 空查询的 `,` 是“看关键字”；连按第二下才切直达。
                    ',' if self.query.is_empty() => {
                        if let Some(hit) = self.selected_hit().cloned() {
                            self.last_comma_at = Some(now);
                            self.mode = PanelMode::Keyword {
                                hit,
                                return_to: KeywordReturn::Query,
                            };
                        } else {
                            self.status = "没有可查看的片段".into();
                        }
                    }
                    // `;` 在任何查询下都进管理：用户想带着当前上下文去管片段。
                    ';' => self.enter_manage(),
                    '/' if self.query.is_empty() => {
                        self.mode = PanelMode::Command { cursor: 0 };
                    }
                    '?' if self.query.is_empty() => self.help_open = true,
                    // `'` 是全文模式开关：把正文纳入匹配（最低档）。
                    // 它只在查询视图里生效，且不进入过滤串——否则用户会以为
                    // 自己打出了一个撇号。
                    '\'' => {
                        self.full_text = !self.full_text;
                        self.status = if self.full_text {
                            "全文模式：正文也参与匹配".into()
                        } else {
                            "已关闭全文模式".into()
                        };
                        self.refilter_owned();
                    }
                    // `:` 开始一个标签词：弹出已有标签的候选列表。
                    // 只有当冒号落在词首（查询开头或空格之后）才算标签词——
                    // `log:` 里的冒号只是普通字符。
                    ':' => {
                        self.type_char(':');
                        if self.current_tag_prefix().is_some() {
                            self.tag_suggest = true;
                            self.tag_cursor = 0;
                        }
                    }
                    _ => self.type_char(ch),
                }
                Intent::None
            }
        }
    }

    fn keyword_key(
        &mut self,
        key: KeyMsg,
        now: Instant,
        hit: Hit,
        return_to: KeywordReturn,
    ) -> Intent {
        match key.code {
            keymap::ESCAPE | keymap::BACKSPACE => {
                self.mode = match return_to {
                    KeywordReturn::Query => PanelMode::Query,
                    KeywordReturn::Manage(state) => PanelMode::Manage(*state),
                };
                Intent::None
            }
            keymap::RETURN => {
                // 关键字视图里的片段可能不是查询列表的选中项（从管理进来、
                // 或上下切过），兜底粘贴用的正文必须跟着它走。
                self.set_preview_for(&hit);
                Intent::Confirm(hit)
            }
            keymap::UP | keymap::DOWN => {
                let delta = if key.code == keymap::UP { -1 } else { 1 };
                match return_to {
                    KeywordReturn::Query => {
                        self.move_selection(delta);
                        if let Some(next) = self.selected_hit().cloned() {
                            self.mode = PanelMode::Keyword {
                                hit: next,
                                return_to: KeywordReturn::Query,
                            };
                        } else {
                            self.mode = PanelMode::Query;
                        }
                    }
                    KeywordReturn::Manage(mut state) => {
                        self.manage_move_in(&mut state, delta as i64);
                        if let Some(snippet) = self.manage_snippet(&state) {
                            let trigger = snippet.trigger().to_string();
                            self.mode = PanelMode::Keyword {
                                hit: Hit {
                                    snippet,
                                    trigger,
                                    score: 0,
                                    matched: Default::default(),
                                },
                                return_to: KeywordReturn::Manage(state),
                            };
                        } else {
                            self.mode = PanelMode::Manage(*state);
                        }
                    }
                }
                Intent::None
            }
            _ => {
                if let Some(',') = keymap::char_for_keycode(key.code, key.shift) {
                    if self.comma_is_double(now) {
                        self.enter_direct();
                        self.last_comma_at = None;
                    } else {
                        self.last_comma_at = Some(now);
                        self.status = "再按一次 , 进入直达模式".into();
                        self.mode = PanelMode::Keyword { hit, return_to };
                    }
                } else if keymap::char_for_keycode(key.code, key.shift) == Some('?') {
                    self.help_open = true;
                    self.mode = PanelMode::Keyword { hit, return_to };
                } else {
                    self.mode = PanelMode::Keyword { hit, return_to };
                }
                Intent::None
            }
        }
    }

    fn direct_key(&mut self, key: KeyMsg, mut query: String) -> Intent {
        match key.code {
            keymap::ESCAPE => {
                self.mode = PanelMode::Query;
                self.last_comma_at = None;
                self.refilter_owned();
                return Intent::None;
            }
            keymap::RETURN => {
                return match self.selected_hit().cloned() {
                    Some(hit) => Intent::Confirm(hit),
                    None => Intent::None,
                };
            }
            keymap::BACKSPACE => {
                if query.pop().is_some() {
                    self.selected = 0;
                }
            }
            keymap::UP => self.move_selection(-1),
            keymap::DOWN => self.move_selection(1),
            _ => {
                if let Some(ch) = keymap::char_for_keycode(key.code, key.shift) {
                    if !ch.is_control() && query.chars().count() < 60 {
                        query.push(ch);
                        self.selected = 0;
                    }
                }
            }
        }
        self.mode = PanelMode::Direct { query };
        self.refilter_owned();
        Intent::None
    }

    fn command_key(&mut self, key: KeyMsg, cursor: usize) -> Intent {
        match key.code {
            keymap::ESCAPE => {
                self.mode = PanelMode::Query;
                Intent::None
            }
            keymap::UP => {
                self.mode = PanelMode::Command {
                    cursor: cursor.saturating_sub(1),
                };
                Intent::None
            }
            keymap::DOWN => {
                self.mode = PanelMode::Command {
                    cursor: (cursor + 1).min(COMMANDS.len() - 1),
                };
                Intent::None
            }
            keymap::RETURN => self.run_command(cursor),
            _ => {
                if keymap::char_for_keycode(key.code, key.shift) == Some('?') {
                    self.help_open = true;
                }
                self.mode = PanelMode::Command { cursor };
                Intent::None
            }
        }
    }

    fn run_command(&mut self, cursor: usize) -> Intent {
        match cursor {
            0 => {
                self.mode = PanelMode::Query;
                Intent::Reload
            }
            1 => {
                self.show_disabled = !self.show_disabled;
                self.status = if self.show_disabled {
                    "已显示停用片段".into()
                } else {
                    "已隐藏停用片段".into()
                };
                self.mode = PanelMode::Query;
                self.refilter_owned();
                Intent::None
            }
            2 => {
                self.enter_manage();
                Intent::None
            }
            3 => {
                self.mode = PanelMode::Query;
                Intent::OpenSettings
            }
            _ => {
                self.mode = PanelMode::Query;
                self.help_open = true;
                Intent::None
            }
        }
    }

    fn manage_key(&mut self, key: KeyMsg, state: &mut ManageState) -> Intent {
        // 标签输入态独占按键：这时 `esc` 是取消输入，而不是退出管理面板。
        if let Some(mut buf) = state.tag_input.take() {
            match key.code {
                keymap::ESCAPE => {
                    self.status = "已取消标签输入".into();
                }
                keymap::RETURN => {
                    if self.apply_tag_input(state, &buf) {
                        return Intent::Reload;
                    }
                }
                keymap::BACKSPACE => {
                    buf.pop();
                    state.tag_input = Some(buf);
                }
                _ => {
                    if let Some(ch) = keymap::char_for_keycode(key.code, key.shift) {
                        let allowed = ch.is_ascii_alphanumeric()
                            || ch == '-'
                            || ch == '_'
                            || ch == ':'
                            || ch == ' ';
                        if allowed {
                            buf.push(ch);
                        }
                    }
                    state.tag_input = Some(buf);
                }
            }
            return Intent::None;
        }

        match key.code {
            keymap::ESCAPE => {
                self.library.flush_if_dirty(&paths::library_path());
                self.mode = PanelMode::Query;
                self.refilter_owned();
                Intent::None
            }
            keymap::UP => {
                self.manage_move_in(state, -1);
                Intent::None
            }
            keymap::DOWN => {
                self.manage_move_in(state, 1);
                Intent::None
            }
            keymap::SPACE => {
                self.manage_toggle_selected_in(state, None);
                Intent::None
            }
            keymap::RETURN => {
                if let Some(snippet) = self.manage_snippet(state) {
                    let trigger = snippet.trigger().to_string();
                    self.mode = PanelMode::Keyword {
                        hit: Hit {
                            snippet,
                            trigger,
                            score: 0,
                            matched: Default::default(),
                        },
                        return_to: KeywordReturn::Manage(Box::new(state.clone())),
                    };
                }
                Intent::None
            }
            _ => {
                if let Some(ch) = keymap::char_for_keycode(key.code, key.shift) {
                    match ch.to_ascii_lowercase() {
                        'a' => self.manage_select_all_in(state),
                        'd' => self.manage_toggle_enabled_in(state),
                        'f' => self.manage_cycle_filter_in(state),
                        'e' => self.manage_export_in(state),
                        'm' => {
                            self.manage_begin_migrate_in(state);
                            return Intent::None;
                        }
                        't' => {
                            self.manage_begin_tag_input_in(state);
                            return Intent::None;
                        }
                        '?' => self.help_open = true,
                        _ => {}
                    }
                }
                Intent::None
            }
        }
    }

    fn migrate_key(&mut self, key: KeyMsg, state: &mut MigrateState) -> Intent {
        match key.code {
            keymap::ESCAPE => {
                self.mode = PanelMode::Manage(state.manage.as_ref().clone());
                Intent::None
            }
            keymap::UP => {
                self.migrate_move(state, -1);
                Intent::None
            }
            keymap::DOWN => {
                self.migrate_move(state, 1);
                Intent::None
            }
            keymap::SPACE => {
                self.migrate_toggle(state, None);
                Intent::None
            }
            keymap::RETURN => self.apply_migrate(state),
            _ => {
                if let Some(ch) = keymap::char_for_keycode(key.code, key.shift) {
                    if ch.eq_ignore_ascii_case(&'a') {
                        for idx in 0..state.plans.len() {
                            if state.plans[idx].selectable() {
                                state.selected.insert(idx);
                            }
                        }
                    } else if ch == '?' {
                        self.help_open = true;
                    }
                }
                Intent::None
            }
        }
    }

    /// 从索引服务现取快照再重算。
    ///
    /// 单独抽出来是因为"重算"发生在好几处（打字、退格、配置变更），
    /// 每处都写一遍 `let snapshot = ...; self.refilter(&snapshot)` 容易漏。
    fn refilter_owned(&mut self) {
        let snapshot = self.index.snapshot();
        self.refilter(&snapshot);
    }

    /// 按当前 `query` 重新计算候选，并修正选中下标。
    ///
    /// 传快照而不是自己取，是为了让调用方能在同一份快照上做多次查询（例如
    /// `sync_index` 里先建索引再过滤），避免中途被后台线程换掉。
    ///
    /// 选中下标只做"夹到有效范围"而不重置：用户上下选完之后如果再触发一次
    /// 重算（例如配置变更），保住位置比跳回第一条更不突兀。真正的重置发生在
    /// `type_char`（用户重新开始输入）和 `begin_open`。
    pub fn refilter(&mut self, snapshot: &Snapshot) {
        // 管理/迁移/关键字视图的列表不是“候选”，不要被一次后台重建冲掉。
        if matches!(
            self.mode,
            PanelMode::Manage(_) | PanelMode::Migrate(_) | PanelMode::Keyword { .. }
        ) {
            return;
        }
        let limit = self.config.max_results.max(1);
        let now = usage::now_ms();
        let direct_query = match &self.mode {
            PanelMode::Direct { query } => Some(query.clone()),
            _ => None,
        };
        self.hits = match direct_query {
            Some(query) => rank_direct(
                snapshot,
                &query,
                &self.config.direct_prefix,
                &self.library,
                &self.usage,
                limit,
                now,
            ),
            None => rank_filtered(
                &self.search,
                snapshot,
                &Query::parse(&self.query),
                &self.usage,
                &self.library,
                self.show_disabled,
                self.full_text,
                limit,
                now,
            ),
        };
        if self.selected >= self.hits.len() {
            self.selected = self.hits.len().saturating_sub(1);
        }
        self.rebuild_preview();
    }

    /// 两个 `,` 是否落在同一个连按窗口里。
    fn comma_is_double(&self, now: Instant) -> bool {
        let window = Duration::from_millis(self.config.direct_window_ms.max(200));
        self.last_comma_at
            .map(|at| now.saturating_duration_since(at) <= window)
            .unwrap_or(false)
    }

    /// 进入直达模式：只匹配带 `,,` 前缀的触发词。
    fn enter_direct(&mut self) {
        self.mode = PanelMode::Direct {
            query: String::new(),
        };
        self.selected = 0;
        self.refilter_owned();
    }

    /// 带着当前选中项进入管理面板。
    pub fn enter_manage(&mut self) {
        let key = self.selected_hit().map(|hit| snippet_key(&hit.snippet));
        let mut state = ManageState::default();
        if let Some(key) = key {
            let rows = self.manage_rows_for(&state);
            state.cursor = rows.iter().position(|s| snippet_key(s) == key).unwrap_or(0);
            state.selected.insert(key);
        }
        self.mode = PanelMode::Manage(state.clone());
        self.clamp_manage(&mut state);
        self.mode = PanelMode::Manage(state);
    }

    /// 当前管理过滤条件下可见的片段（含停用项——管理面板就是要能看见它们）。
    pub fn manage_rows(&self) -> Vec<Arc<crate::index::Snippet>> {
        match &self.mode {
            PanelMode::Manage(state) => self.manage_rows_for(state),
            _ => Vec::new(),
        }
    }

    fn manage_rows_for(&self, state: &ManageState) -> Vec<Arc<crate::index::Snippet>> {
        let snapshot = self.index.snapshot();
        snapshot
            .snippets
            .iter()
            .filter(|snippet| {
                state
                    .filter
                    .as_ref()
                    .map(|file| &snippet.source.file == file)
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    /// 管理面板里的所有来源文件，用于 `f` 循环过滤。
    pub fn manage_source_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self
            .index
            .snapshot()
            .snippets
            .iter()
            .map(|s| s.source.file.clone())
            .collect();
        files.sort();
        files.dedup();
        files
    }

    fn manage_snippet(&self, state: &ManageState) -> Option<Arc<crate::index::Snippet>> {
        self.manage_rows_for(state).get(state.cursor).cloned()
    }

    fn snippet_by_key(&self, key: &SnippetKey) -> Option<Arc<crate::index::Snippet>> {
        self.index
            .snapshot()
            .snippets
            .iter()
            .find(|s| snippet_key(s) == *key)
            .cloned()
    }

    fn clamp_manage(&self, state: &mut ManageState) {
        let len = self.manage_rows_for(state).len();
        state.cursor = state.cursor.min(len.saturating_sub(1));
        adjust_scroll(&mut state.scroll, state.cursor, MANAGE_VISIBLE_ROWS);
    }

    fn manage_move_in(&self, state: &mut ManageState, delta: i64) {
        let len = self.manage_rows_for(state).len();
        if len == 0 {
            state.cursor = 0;
            return;
        }
        state.cursor = (state.cursor as i64 + delta).clamp(0, len as i64 - 1) as usize;
        adjust_scroll(&mut state.scroll, state.cursor, MANAGE_VISIBLE_ROWS);
    }

    fn manage_toggle_selected_in(&mut self, state: &mut ManageState, index: Option<usize>) {
        let rows = self.manage_rows_for(state);
        let cursor = index.unwrap_or(state.cursor);
        state.cursor = cursor.min(rows.len().saturating_sub(1));
        adjust_scroll(&mut state.scroll, state.cursor, MANAGE_VISIBLE_ROWS);
        let Some(snippet) = rows.get(cursor) else {
            return;
        };
        let key = snippet_key(snippet);
        if !state.selected.remove(&key) {
            state.selected.insert(key);
        }
    }

    fn manage_select_all_in(&mut self, state: &mut ManageState) {
        for snippet in self.manage_rows_for(state) {
            state.selected.insert(snippet_key(&snippet));
        }
    }

    fn manage_toggle_enabled_in(&mut self, state: &mut ManageState) {
        let targets: Vec<SnippetKey> = if state.selected.is_empty() {
            self.manage_snippet(state)
                .map(|s| snippet_key(&s))
                .into_iter()
                .collect()
        } else {
            state.selected.iter().cloned().collect()
        };
        if targets.is_empty() {
            self.status = "没有可操作的片段".into();
            return;
        }
        let Some(first) = targets.iter().find_map(|key| self.snippet_by_key(key)) else {
            return;
        };
        let disable = !self.library.is_disabled(first.trigger());
        let mut changed = 0;
        for key in &targets {
            if let Some(snippet) = self.snippet_by_key(key) {
                if !self.library.is_disabled(snippet.trigger()) {
                    changed += 1;
                }
                self.library.set_disabled(snippet.trigger(), disable);
            }
        }
        if disable {
            for key in &targets {
                state.selected.remove(key);
            }
        }
        self.library.flush_if_dirty(&paths::library_path());
        self.refresh_direct_triggers();
        self.status = format!(
            "已{} {} 条",
            if disable { "停用" } else { "启用" },
            targets.len().max(changed)
        );
    }

    fn manage_cycle_filter_in(&mut self, state: &mut ManageState) {
        let files = self.manage_source_files();
        state.filter = match &state.filter {
            None => files.first().cloned(),
            Some(current) => {
                let pos = files.iter().position(|f| f == current);
                match pos {
                    Some(idx) if idx + 1 < files.len() => Some(files[idx + 1].clone()),
                    _ => None,
                }
            }
        };
        state.cursor = 0;
        state.scroll = 0;
        self.status = match &state.filter {
            Some(file) => format!("来源：{}", file.display()),
            None => "来源：全部".into(),
        };
    }

    fn manage_export_in(&mut self, state: &mut ManageState) {
        let keys: Vec<SnippetKey> = if state.selected.is_empty() {
            self.manage_snippet(state)
                .map(|s| snippet_key(&s))
                .into_iter()
                .collect()
        } else {
            state.selected.iter().cloned().collect()
        };
        if keys.is_empty() {
            self.status = "没有可导出的片段".into();
            return;
        }
        let snapshot = self.index.snapshot();
        match crate::library::export::export(&snapshot, &keys, &paths::exports_dir()) {
            Ok(report) => {
                self.status = match report.file {
                    Some(path) => format!("已导出 {} 条到 {}", report.entries, path.display()),
                    None => "已导出".into(),
                };
            }
            Err(err) => self.status = format!("导出失败：{err}"),
        }
    }

    fn manage_begin_tag_input_in(&mut self, state: &mut ManageState) {
        // 有勾选就作用于全部勾选项；没有勾选就只作用于光标这一行。
        if state.selected.is_empty() {
            let Some(snippet) = self.manage_snippet(state) else {
                self.status = "没有可加标签的片段".into();
                return;
            };
            state.selected.insert(snippet_key(&snippet));
        }
        state.tag_input = Some(String::new());
        self.status = "输入标签（空格分隔多个），↵ 应用，esc 取消".into();
    }

    /// 应用标签输入。成功返回 true（调用方据此重新加载索引）。
    fn apply_tag_input(&mut self, state: &ManageState, buffer: &str) -> bool {
        let mut tags: Vec<String> = Vec::new();
        for word in buffer.split_whitespace() {
            if let Some(tag) = crate::index::model::normalize_tag(word) {
                if !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                    tags.push(tag);
                }
            }
        }
        if tags.is_empty() {
            self.status = "标签只能用字母、数字、- 与 _".into();
            return false;
        }

        let snapshot = self.index.snapshot();
        let targets: Vec<crate::library::tags::TagTarget> = state
            .selected
            .iter()
            .filter(|key| snapshot.snippets.iter().any(|s| snippet_key(s) == **key))
            .map(|key| crate::library::tags::TagTarget {
                key: key.clone(),
                tags: tags.clone(),
            })
            .collect();
        if targets.is_empty() {
            self.status = "选中项已经不在索引里，先重新加载".into();
            return false;
        }

        let report =
            crate::library::tags::append(&targets, &snapshot.config_root, &paths::backups_dir());
        let mut message = format!("已给 {} 条追加标签：{}", report.entries, tags.join(" "));
        if report.skipped > 0 {
            message.push_str(&format!("；{} 条已存在或跳过", report.skipped));
        }
        if let Some(dir) = &report.backup_dir {
            message.push_str(&format!("；备份在 {}", dir.display()));
        }
        if !report.errors.is_empty() {
            message.push_str(&format!("；{} 处失败", report.errors.len()));
            for err in report.errors.iter().take(2) {
                log::warn!("标签写回失败：{err}");
            }
        }
        self.status = message;
        report.entries > 0
    }

    fn manage_begin_migrate_in(&mut self, state: &mut ManageState) {
        let snapshot = self.index.snapshot();
        let plans = migrate::plan(&snapshot, &self.config.direct_prefix);
        let selected: BTreeSet<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.selectable())
            .map(|(idx, _)| idx)
            .collect();
        self.mode = PanelMode::Migrate(MigrateState {
            plans,
            selected,
            cursor: 0,
            scroll: 0,
            manage: Box::new(state.clone()),
        });
    }

    fn migrate_move(&mut self, state: &mut MigrateState, delta: i64) {
        if state.plans.is_empty() {
            state.cursor = 0;
            return;
        }
        state.cursor =
            (state.cursor as i64 + delta).clamp(0, state.plans.len() as i64 - 1) as usize;
        adjust_scroll(&mut state.scroll, state.cursor, MANAGE_VISIBLE_ROWS);
    }

    fn migrate_toggle(&mut self, state: &mut MigrateState, index: Option<usize>) {
        let idx = index.unwrap_or(state.cursor);
        let Some(plan) = state.plans.get(idx) else {
            return;
        };
        if !plan.selectable() {
            self.status = plan
                .conflict
                .clone()
                .unwrap_or_else(|| "冲突项不可选".into());
            return;
        }
        if !state.selected.remove(&idx) {
            state.selected.insert(idx);
        }
    }

    fn apply_migrate(&mut self, state: &mut MigrateState) -> Intent {
        let selected: Vec<MigrationPlan> = state
            .selected
            .iter()
            .filter_map(|idx| state.plans.get(*idx).cloned())
            .filter(MigrationPlan::selectable)
            .collect();
        if selected.is_empty() {
            self.status = "没有勾选可迁移的片段".into();
            self.mode = PanelMode::Migrate(state.clone());
            return Intent::None;
        }
        let snapshot = self.index.snapshot();
        let report = migrate::apply(&selected, &snapshot.config_root, &paths::backups_dir());
        let mut message = format!("已写入 {} 个文件 / {} 条", report.files, report.entries);
        if let Some(dir) = &report.backup_dir {
            message.push_str(&format!("；备份在 {}", dir.display()));
        }
        if !report.errors.is_empty() {
            message.push_str(&format!("；{} 个文件失败", report.errors.len()));
        }
        self.status = message;
        self.mode = PanelMode::Manage(state.manage.as_ref().clone());
        Intent::Reload
    }

    /// 直达触发词列表：启用中的、带 `direct_prefix` 的触发词，供 tap 统计。
    pub fn refresh_direct_triggers(&self) {
        let prefix = &self.config.direct_prefix;
        let snapshot = self.index.snapshot();
        let mut triggers: Vec<String> = snapshot
            .snippets
            .iter()
            .filter(|snippet| !self.library.is_disabled(snippet.trigger()))
            .flat_map(|snippet| snippet.triggers.iter())
            .filter(|trigger| trigger.starts_with(prefix))
            .cloned()
            .collect();
        triggers.sort();
        triggers.dedup();
        crate::trigger::tap::set_direct_triggers(triggers);
    }

    /// 从面板确认一条非直达触发词时，提示用户下次可以走捷径。
    pub fn direct_hint(&self, hit: &Hit) -> Option<String> {
        let prefix = &self.config.direct_prefix;
        if prefix.is_empty() || hit.trigger.starts_with(prefix) {
            return None;
        }
        let direct = hit.snippet.direct_trigger(prefix)?;
        Some(format!("下次可直接输入 {direct}"))
    }

    /// 关键字视图的数据：触发词、别名、来源与本周用量。
    pub fn keyword_view(&self, hit: &Hit) -> KeywordView {
        let snapshot = self.index.snapshot();
        let prefix = &self.config.direct_prefix;
        let triggers = hit
            .snippet
            .triggers
            .iter()
            .map(|trigger| TriggerView {
                text: trigger.clone(),
                is_direct: trigger.starts_with(prefix),
                count: snapshot.by_trigger.get(trigger).map(Vec::len).unwrap_or(0),
            })
            .collect();
        let aliases = self.library.aliases_for(hit.snippet.trigger()).to_vec();
        let usage = self.usage.get(&hit.trigger);
        let suggestion = hit.snippet.direct_trigger(prefix).is_none().then(|| {
            let core = crate::search::query::core_of(hit.snippet.trigger());
            if core.is_empty() {
                "暂无直达触发词".to_string()
            } else {
                format!("可在管理面板迁移为 {prefix}{core}")
            }
        });
        KeywordView {
            triggers,
            aliases,
            source: format!(
                "{} · 第 {} 条",
                hit.snippet.source.file.display(),
                hit.snippet.source.ordinal + 1
            ),
            duplicates: snapshot.duplicates(&hit.trigger),
            usage_count: usage.count,
            week_panel: usage.week_panel,
            week_direct: usage.week_direct,
            suggestion,
        }
    }

    /// 管理面板里的鼠标点击入口（UI 直接调，不模拟按键）。
    pub fn manage_click_select(&mut self, index: usize) {
        let mut state = match &self.mode {
            PanelMode::Manage(state) => state.clone(),
            _ => return,
        };
        self.manage_toggle_selected_in(&mut state, Some(index));
        self.mode = PanelMode::Manage(state);
    }

    pub fn manage_click_toggle_enabled(&mut self, index: usize) {
        let mut state = match &self.mode {
            PanelMode::Manage(state) => state.clone(),
            _ => return,
        };
        state.cursor = index;
        self.manage_toggle_enabled_in(&mut state);
        self.mode = PanelMode::Manage(state);
    }

    pub fn manage_click_filter(&mut self) {
        let mut state = match &self.mode {
            PanelMode::Manage(state) => state.clone(),
            _ => return,
        };
        self.manage_cycle_filter_in(&mut state);
        self.mode = PanelMode::Manage(state);
    }

    pub fn manage_click_export(&mut self) {
        let mut state = match &self.mode {
            PanelMode::Manage(state) => state.clone(),
            _ => return,
        };
        self.manage_export_in(&mut state);
        self.mode = PanelMode::Manage(state);
    }

    pub fn manage_click_migrate(&mut self) {
        let mut state = match &self.mode {
            PanelMode::Manage(state) => state.clone(),
            _ => return,
        };
        self.manage_begin_migrate_in(&mut state);
    }

    /// 管理面板的「加标签」按钮：等同于按 `t`。
    pub fn manage_click_tag(&mut self) {
        let mut state = match &self.mode {
            PanelMode::Manage(state) => state.clone(),
            _ => return,
        };
        self.manage_begin_tag_input_in(&mut state);
        self.mode = PanelMode::Manage(state);
    }

    pub fn manage_click_keyword(&mut self, index: usize) {
        let mut state = match &self.mode {
            PanelMode::Manage(state) => state.clone(),
            _ => return,
        };
        state.cursor = index;
        if let Some(snippet) = self.manage_snippet(&state) {
            let trigger = snippet.trigger().to_string();
            self.mode = PanelMode::Keyword {
                hit: Hit {
                    snippet,
                    trigger,
                    score: 0,
                    matched: Default::default(),
                },
                return_to: KeywordReturn::Manage(Box::new(state)),
            };
        }
    }

    /// 为当前选中项生成预览。
    ///
    /// 三个信息来源：片段本身、重复 trigger 数（同名字段会让 espanso 弹消歧窗）、
    /// 以及 `PreviewCtx` 里的剪贴板与当前时间。剪贴板默认**不读**：
    /// 读剪贴板在 macOS 上会触发系统提示，只在用户显式打开 `preview_clipboard` 时才读。
    fn rebuild_preview(&mut self) {
        match self.hits.get(self.selected).cloned() {
            Some(hit) => self.set_preview_for(&hit),
            None => self.preview = None,
        }
    }

    /// 把预览换成指定片段。关键字视图 / 管理面板的片段不一定在 `hits` 里，
    /// 但确认时的粘贴兜底读的是 `preview`，所以确认前必须同步过来。
    fn set_preview_for(&mut self, hit: &Hit) {
        let snapshot = self.index.snapshot();
        let duplicate = snapshot.duplicates(&hit.trigger);
        let ctx = PreviewCtx {
            clipboard: None,
            show_clipboard: self.config.preview_clipboard,
            now: chrono::Local::now(),
        };
        self.preview = Some(preview::build(&hit.snippet, &hit.trigger, duplicate, &ctx));
    }

    /// 回传时用的"安全渲染文本"：只用于 espanso 不可用时的粘贴兜底。
    ///
    /// 取的是预览正文而不是原始 body：预览已经把 `{{date}}` 之类求过值、把
    /// `[[字段]]` 填成默认值，粘出来的东西才像样。`▮` 是给界面画光标用的标记，
    /// 必须去掉；纯空白直接返回 `None`，免得粘贴一堆空格。
    pub fn fallback_text(&self) -> Option<String> {
        let preview = self.preview.as_ref()?;
        let text = preview.body.replace('▮', "");
        (!text.trim().is_empty()).then_some(text)
    }

    /// 记一次使用（影响排序加权），并立刻落盘。
    ///
    /// 落盘放在这里而不是退出时：daemon 可能被 `kill`，攒着会丢。
    /// 写入失败只记日志——统计数据丢了不影响功能。
    pub fn record_use(&mut self, trigger: &str) {
        self.usage.record_kind(trigger, UsageKind::Panel);
        self.usage.flush_if_dirty(&paths::usage_path());
    }

    /// 记一次面板外的直达打出（由 tap 观察输入得到，属于估算）。
    pub fn record_direct_use(&mut self, trigger: &str) {
        self.usage.record_direct(trigger);
        self.usage.flush_if_dirty(&paths::usage_path());
    }

    /// 一行健康状态（面板头部右侧）。
    ///
    /// 只说"校验过没有"，不报 trigger 总数：`espanso_known` 里有 espanso 认识的
    /// **全部** trigger，包括我们自己写进 `semisnip.yml` 的那条降级触发，
    /// 报出来会比左边候选数多一个（"4/4 · espanso 已校验 5"），看着像 bug。
    pub fn health_line(&self) -> String {
        let snapshot = self.index.snapshot();
        if snapshot.is_empty() {
            return "未读到片段".to_string();
        }
        if snapshot.espanso_checked {
            "espanso 已校验".to_string()
        } else {
            "espanso 未校验".to_string()
        }
    }
}

/// 把 `AppState` 挂成 gpui 全局，让任意组件都能取到（`ui::view`、菜单栏回调）。
///
/// 用 `Entity` 而不是裸结构：gpui 需要在状态变化时知道通知谁重绘。
pub struct GlobalApp(pub Entity<AppState>);
impl Global for GlobalApp {}

/// 供 UI 层取全局状态。
///
/// 会 panic（`cx.global` 的语义）——全局在整个 App 生命周期里只会设置一次，
/// 拿不到说明装配顺序错了，属于编程错误而不是运行时状况。
pub fn app(cx: &gpui_kit::App) -> Entity<AppState> {
    cx.global::<GlobalApp>().0.clone()
}

/// 执行"确认"：把片段交给 espanso 展开（或降级粘贴）。
///
/// 放到独立线程的原因：`espanso match exec` 要起进程，可能在几十毫秒到几百毫秒，
/// 在主线程做会让面板关窗的那一帧卡住。展开结果只写日志，不回传状态。
///
/// 让 `confirm` 的调用点只有一处，避免"关窗/回删/exec"顺序被改坏。
pub fn run_confirm(hit: Hit, erase: usize, config: Config, fallback: Option<String>) {
    std::thread::Builder::new()
        .name("semisnip-handoff".into())
        .spawn(move || {
            let outcome = handoff::confirm(&hit, erase, &config, fallback.as_deref());
            match outcome {
                handoff::Outcome::Expanded => log::info!("已交给 espanso 展开：{}", hit.trigger),
                handoff::Outcome::Delegated => {
                    // 这一刻触发串已经打进文档，剩下的全看 espanso 的被动匹配：
                    // 它没在跑、或片段被 app-specific 配置禁用，文档里就会留着触发串。
                    log::info!("已打出触发串，等 espanso 自己命中：{}", hit.trigger);
                }
                handoff::Outcome::Pasted => log::warn!("espanso 不可用，已用粘贴兜底"),
                handoff::Outcome::Failed(err) => log::error!("展开失败：{err}"),
            }
        })
        .ok();
}
