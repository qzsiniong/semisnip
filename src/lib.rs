//! snipshot —— espanso 的片段检索层。
//!
//! 分工：**snipshot 只负责"选"，espanso 负责"展"**。
//! 任意输入框里打 `;;` → 非激活面板 → 过滤 → 详情预览 → 回车交给
//! `espanso match exec -t <trigger>` 求值并注入。
//!
//! 模块依赖方向（单向无环）：
//! ```text
//! index / search / preview  ←  app / ui / handoff
//! espanso / trigger / ipc   ←  app
//! ```
//! `search`、`preview`、`trigger::machine` 是纯逻辑，不依赖 gpui，可单测。

pub mod app;
pub mod cli;
pub mod config;
pub mod espanso;
pub mod handoff;
pub mod index;
pub mod ipc;
pub mod preview;
pub mod search;
pub mod trigger;
pub mod ui;
pub mod usage;

use gpui_kit::AppContext as _;

use crate::app::event::Event;
use crate::app::state::AppState;
use crate::index::service::IndexService;

/// daemon 入口：唯一会初始化 gpui 的路径。
pub fn run_daemon() {
    cli::init_logging();

    let (tx, rx) = std::sync::mpsc::channel::<Event>();

    // 单实例：socket 抢不到就说明已有 daemon 在跑，直接退出。
    match ipc::server::serve(tx.clone()) {
        Ok(path) => log::info!("IPC 就绪：{}", path.display()),
        Err(err) => {
            log::error!("IPC 启动失败：{err}");
            eprintln!("snipshot: {err}");
            std::process::exit(1);
        }
    }

    let index = IndexService::new();
    index.spawn_watcher(tx.clone());
    index.reload_async(tx.clone());

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let state = cx.new(|cx| AppState::new(cx, index.clone()));
            cx.set_global(app::state::GlobalApp(state.clone()));

            // tap 必须在后台线程装：创建时可能触发系统授权弹窗，不能卡首屏。
            let config = config::Config::load();
            let trigger_config =
                trigger::TriggerConfig::from_text(&config.trigger, config.trigger_timeout_ms)
                    .unwrap_or_default();
            trigger::tap::install(tx.clone(), trigger_config);

            app::scheduler::spawn(&mut cx.to_async(), state, rx, tx.clone());
        });
}
