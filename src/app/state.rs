//! 面板的单一数据源。
//!
//! 这里不碰 gpui 的窗口/事件系统：按键由 tap 转发进来，渲染由
//! `ui::view` 读取本结构。这样"交互逻辑"和"画界面"可以分别测试。

use std::sync::Arc;

use gpui_kit::{AnyWindowHandle, Context, Entity, Global};

use crate::app::event::KeyMsg;
use crate::config::Config;
use crate::handoff;
use crate::index::service::IndexService;
use crate::index::{Hit, Snapshot};
use crate::paths;
use crate::preview::{self, PreviewDoc, vars::PreviewCtx};
use crate::search::{Query, SearchIndex, rank};
use crate::trigger::keymap;
use crate::usage::{self, Usage};

/// 一次按键处理的结果：面板自己处理不了的动作交回调度器。
pub enum Intent {
    None,
    Confirm(Hit),
    Cancel,
}

pub struct AppState {
    pub index: Arc<IndexService>,
    pub search: SearchIndex,
    pub usage: Usage,
    pub config: Config,

    pub open: bool,
    pub query: String,
    pub hits: Vec<Hit>,
    pub selected: usize,
    /// 被自己吞掉、需要在收起时回删的字符数。
    pub erase: usize,
    pub preview: Option<PreviewDoc>,

    pub status: String,
    pub tap_ok: bool,
    pub handle: Option<AnyWindowHandle>,

    /// 当前 SearchIndex 对应的快照时间戳，用来判断要不要重建。
    built_at: i64,
}

impl AppState {
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

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.index.snapshot()
    }

    /// 索引可能已被后台线程替换：按时间戳判断是否需要重建检索表。
    pub fn sync_index(&mut self) {
        let snapshot = self.index.snapshot();
        if snapshot.built_at_ms != self.built_at {
            self.search = SearchIndex::build(&snapshot);
            self.built_at = snapshot.built_at_ms;
            self.refilter(&snapshot);
        }
    }

    pub fn begin_open(&mut self, erase: usize) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.erase = erase;
        self.status.clear();
        self.sync_index();
        self.refilter_owned();
    }

    pub fn begin_close(&mut self) {
        self.open = false;
        self.query.clear();
        self.hits.clear();
        self.preview = None;
        self.erase = 0;
        self.selected = 0;
    }

    pub fn type_char(&mut self, ch: char) {
        if self.query.chars().count() >= 60 {
            return;
        }
        self.query.push(ch);
        self.selected = 0;
        self.refilter_owned();
    }

    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.selected = 0;
            self.refilter_owned();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        let len = self.hits.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        self.rebuild_preview();
    }

    pub fn selected_hit(&self) -> Option<&Hit> {
        self.hits.get(self.selected)
    }

    pub fn on_key(&mut self, key: KeyMsg) -> Intent {
        match key.code {
            keymap::ESCAPE => Intent::Cancel,
            keymap::RETURN => match self.selected_hit().cloned() {
                Some(hit) => Intent::Confirm(hit),
                None => Intent::None,
            },
            keymap::BACKSPACE => {
                self.backspace();
                Intent::None
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

    fn refilter_owned(&mut self) {
        let snapshot = self.index.snapshot();
        self.refilter(&snapshot);
    }

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
    pub fn fallback_text(&self) -> Option<String> {
        let preview = self.preview.as_ref()?;
        let text = preview.body.replace('▮', "");
        (!text.trim().is_empty()).then_some(text)
    }

    pub fn record_use(&mut self, trigger: &str) {
        self.usage.record(trigger);
        self.usage.flush_if_dirty(&paths::usage_path());
    }

    /// 一行健康状态（面板头部右侧）。
    pub fn health_line(&self) -> String {
        let snapshot = self.index.snapshot();
        if snapshot.is_empty() {
            return "未读到片段".to_string();
        }
        if snapshot.espanso_checked {
            format!("espanso 已校验 {}", snapshot.espanso_known.len())
        } else {
            "espanso 未校验".to_string()
        }
    }
}

pub struct GlobalApp(pub Entity<AppState>);
impl Global for GlobalApp {}

/// 供 UI 层取全局状态。
pub fn app(cx: &gpui_kit::App) -> Entity<AppState> {
    cx.global::<GlobalApp>().0.clone()
}

// 让 `confirm` 的调用点只有一处，避免"关窗/回删/exec"顺序被改坏。
pub fn run_confirm(hit: Hit, erase: usize, config: Config, fallback: Option<String>) {
    std::thread::Builder::new()
        .name("semisnip-handoff".into())
        .spawn(move || {
            let outcome = handoff::confirm(&hit, erase, &config, fallback.as_deref());
            match outcome {
                handoff::Outcome::Expanded => log::info!("已交给 espanso 展开：{}", hit.trigger),
                handoff::Outcome::Pasted => log::warn!("espanso 不可用，已用粘贴兜底"),
                handoff::Outcome::Failed(err) => log::error!("展开失败：{err}"),
            }
        })
        .ok();
}
