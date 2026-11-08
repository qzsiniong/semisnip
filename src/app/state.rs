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

use std::sync::Arc;

use gpui_kit::{AnyWindowHandle, Context, Entity, Global};

use crate::app::event::KeyMsg;
use crate::config::Config;
use crate::handoff;
use crate::index::service::IndexService;
use crate::index::{Hit, Snapshot};
use crate::ipc::proto::Origin;
use crate::paths;
use crate::preview::{self, PreviewDoc, vars::PreviewCtx};
use crate::search::{Query, SearchIndex, rank};
use crate::trigger::TriggerConfig;
use crate::trigger::keymap;
use crate::usage::{self, Usage};

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
}

impl AppState {
    /// 构造状态。此时索引可能还没读完（`IndexService::new` 会先给一份空快照），
    /// 所以这里同步构建一次检索表，即使为空也要让 `hits` 处于自洽状态。
    pub fn new(cx: &mut Context<Self>, index: Arc<IndexService>) -> Self {
        let config = Config::load();
        let usage = Usage::load(&paths::usage_path());
        let snapshot = index.snapshot();
        let search = SearchIndex::build(&snapshot);
        let built_at = snapshot.built_at_ms;
        let mut state = Self {
            index,
            search,
            usage,
            config,
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
        };
        state.refilter(&snapshot);
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
            self.search = SearchIndex::build(&snapshot);
            self.built_at = snapshot.built_at_ms;
            self.refilter(&snapshot);
        }
    }

    /// 配置被设置窗口改写了：重新读盘，并把"已经活着"的部分换掉。
    ///
    /// 面板尺寸、显示详情这些**不用在这里处理**——面板窗口每次打开都是新建的，
    /// `ui::window::open` 会重新读 `config.panel_width/height`。这里只需管
    /// 那些状态已经存在的：触发状态机、候选条数（影响当前列表）。
    ///
    /// 触发串映射失败时**保留旧状态机**：设置窗口的输入框是逐字符落盘的，
    /// 用户打到一半的 `,` 不该让 `;;` 当场失灵。
    pub fn reload_config(&mut self) {
        let config = Config::load();
        match TriggerConfig::from_text(&config.trigger, config.trigger_timeout_ms) {
            Ok(trigger) => crate::trigger::tap::set_trigger(trigger),
            Err(err) => log::warn!("触发串 `{}` 未生效：{err}", config.trigger),
        }
        self.config = config;
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
        self.query.clear();
        self.hits.clear();
        self.preview = None;
        self.erase = 0;
        self.origin = Origin::Manual;
        self.selected = 0;
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
        }
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
                if let Some(ch) = keymap::char_for_keycode(key.code, key.shift) {
                    // 过滤串里不该出现控制字符
                    if !ch.is_control() {
                        self.type_char(ch);
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
        let query = Query::parse(&self.query);
        self.hits = rank(
            &self.search,
            snapshot,
            &query,
            &self.usage,
            self.config.max_results.max(1),
            usage::now_ms(),
        );
        if self.selected >= self.hits.len() {
            self.selected = self.hits.len().saturating_sub(1);
        }
        self.rebuild_preview();
    }

    /// 为当前选中项生成预览。
    ///
    /// 三个信息来源：片段本身、重复 trigger 数（同名字段会让 espanso 弹消歧窗）、
    /// 以及 `PreviewCtx` 里的剪贴板与当前时间。剪贴板默认**不读**：
    /// 读剪贴板在 macOS 上会触发系统提示，只在用户显式打开 `preview_clipboard` 时才读。
    fn rebuild_preview(&mut self) {
        let Some(hit) = self.hits.get(self.selected) else {
            self.preview = None;
            return;
        };
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
        self.usage.record(trigger);
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
