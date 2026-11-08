//! semisnip —— espanso 的片段检索层。
//!
//! 分工：**semisnip 只负责"选"，espanso 负责"展"**。
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
pub mod mark;
pub mod paths;
pub mod platform;
pub mod preview;
pub mod search;
pub mod tray;
pub mod trigger;
pub mod ui;
pub mod usage;

/// 端到端模拟测试：进程内跑通「打字 → 触发 → 面板 → 确认 → 回传 → 展开」，
/// 不依赖 espanso、不需要辅助功能授权。只在测试构建里编译，见模块文档。
#[cfg(test)]
mod sim_tests;

use gpui_kit::AppContext as _;

use crate::app::event::Event;
use crate::app::state::AppState;
use crate::index::service::IndexService;

/// daemon 入口：唯一会初始化 gpui 的路径。
pub fn run_daemon() {
    cli::init_logging();

    let (tx, rx) = std::sync::mpsc::channel::<Event>();

    // 单实例：socket 抢不到就说明已有 daemon 在跑，直接退出。
    // tap 状态由 daemon 自己回答（`doctor` 会问），这里只注入取值方式。
    let tap_probe: ipc::server::TapProbe = std::sync::Arc::new(crate::trigger::tap::ready);
    match ipc::server::serve(tx.clone(), tap_probe) {
        Ok(path) => log::info!("IPC 就绪：{}", path.display()),
        // 关键：这种情况必须退出码 0。launchd 的 KeepAlive 只在「非正常退出」时重启，
        // 退出码 1 会让它每 10 秒拉起一个新实例、再撞一次 socket、再写一行 ERROR。
        Err(ipc::server::ServeError::AlreadyRunning) => {
            log::info!("已有 daemon 在运行，本实例退出");
            std::process::exit(0);
        }
        Err(ipc::server::ServeError::Io(err)) => {
            log::error!("IPC 启动失败：{err}");
            eprintln!("semisnip: {err}");
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

            // 常驻工具不该占着 Dock 和 Cmd-Tab。Info.plist 里的 LSUIElement 只管
            // 启动瞬间，gpui 会按自己的默认（Regular）覆盖回去，所以这里必须再
            // 明确设一次 Accessory：不进 Dock，只在菜单栏留一个图标。
            cx.set_activation_policy(gpui_kit::ActivationPolicy::Accessory);

            // 窗口（设置 / 引导）要能向 UI 线程投事件，但拿不到 scheduler 那条
            // `Sender`；放进全局，任何 `&mut App` 都能取一份。
            cx.set_global(app::event::GlobalTx(tx.clone()));

            let state = cx.new(|cx| AppState::new(cx, index.clone()));
            cx.set_global(app::state::GlobalApp(state.clone()));

            // tap 必须在后台线程装：创建时可能触发系统授权弹窗，不能卡首屏。
            let config = config::Config::load();
            let trigger_config =
                trigger::TriggerConfig::from_text(&config.trigger, config.trigger_timeout_ms)
                    .unwrap_or_default();
            trigger::tap::install(tx.clone(), trigger_config);

            app::scheduler::spawn(&mut cx.to_async(), state, rx, tx.clone());

            // 菜单栏图标最后装：它要的 action handler 已经注册好了，
            // 且失败了也不该拖垮 daemon（面板靠 `;;` 仍然可用）。
            if let Err(err) = tray::install(cx, tx.clone()) {
                log::error!("菜单栏图标创建失败：{err}");
            }

            // 首次运行：`onboarded` 还是 false（配置文件可能都还不存在）就弹引导。
            // 放在最后，是因为引导窗里的「体检」要读索引快照——索引是异步建的，
            // 这里读到的可能就是空快照；空也没关系，那一步会提示"没读到片段"。
            if !config.onboarded {
                if let Err(err) = ui::onboarding::open(cx) {
                    log::warn!("首次引导打开失败：{err}");
                }
            }
        });
}
