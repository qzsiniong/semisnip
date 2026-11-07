//! UI 侧调度：消费 tap 线程与 IPC 线程的事件，驱动窗口与状态。
//!
//! 轮询（16ms 忙 / 50ms 闲）而不是跨线程唤醒——gpui 的 `App` 只能在 UI
//! 线程访问，轮询是这里最稳的桥。这和 snippets / clipbeam 的做法一致。

use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use gpui_kit::{App, AsyncApp, Entity};

use crate::app::event::Event;
use crate::app::state::{self, AppState, Intent};
use crate::trigger::tap;
use crate::ui;

/// tap 的看门狗状态。见 [`watch_tap`]。
struct TapWatch {
    /// 上次对齐（读 `tap::ready()`）的时间。
    last_check: Instant,
    /// 下一次允许「重新安装」的时间。
    next_retry: Instant,
}

impl TapWatch {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            last_check: now,
            next_retry: now,
        }
    }
}

/// 启动事件循环（协程形式，跑在 gpui 的主线程上）。
///
/// 循环本身没有"等待事件"的阻塞调用：每轮先跑一次 tap 看门狗，再按面板开合
/// 决定下一次醒来的间隔（开着 16ms≈60fps，关着 50ms 省电），然后 `try_recv`
/// 把这一轮积累的事件全部处理掉。
///
/// 为什么用轮询而不是阻塞 `recv`：`std::sync::mpsc` 的接收端没法塞进 gpui 的
/// async 执行器（没有 async 版本），而 16/50ms 的空转代价在这样一个每秒只处理
/// 几次事件的进程里可以忽略。**代价必须知道**：这个循环永远不会退出，
/// 面板关闭时它仍在跑——所以里面不能有"只在打开时该做"的重活。
pub fn spawn(cx: &mut AsyncApp, state: Entity<AppState>, rx: Receiver<Event>, tx: Sender<Event>) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let bg = cx.background_executor().clone();
        let mut watch = TapWatch::new();
        loop {
            let busy = cx.update(|cx: &mut App| {
                watch_tap(cx, &state, &mut watch);
                state.read(cx).open
            });
            bg.timer(Duration::from_millis(if busy { 16 } else { 50 }))
                .await;

            while let Ok(event) = rx.try_recv() {
                cx.update(|cx: &mut App| handle(cx, &state, &tx, event));
            }
        }
    })
    .detach();
}

/// tap 看门狗：把「有没有键盘接管」这件事与面板状态对齐。
///
/// 两件事只有在这里做才不容易出错：
///
/// 1. **同步 `tap_ok`**。`AppState::new` 里读的那次 `tap::ready()` 早于
///    `tap::install`，好机器上也一定是 false；只有每次开面板和这里各对齐一次，
///    面板画的才是真实状态。
/// 2. **权限补上之后自动接上**。`install()` 只跑一次，用户后来才去系统设置
///    勾授权的话，进程这边不会自己知道——以前必须重启 daemon 才生效。
///    这里每 5 秒试一次（`retry` 内部还会先看权限，没权限时不会建线程）。
fn watch_tap(cx: &mut App, state: &Entity<AppState>, watch: &mut TapWatch) {
    const CHECK: Duration = Duration::from_secs(1);
    const RETRY: Duration = Duration::from_secs(5);

    if watch.last_check.elapsed() < CHECK {
        return;
    }
    watch.last_check = Instant::now();

    if !tap::ready() && Instant::now() >= watch.next_retry {
        watch.next_retry = Instant::now() + RETRY;
        if tap::retry() {
            log::info!("检测到辅助功能已授权，正在重装键盘 tap");
        }
    }

    let ready = tap::ready();
    if state.read(cx).tap_ok == ready {
        return;
    }
    state.update(cx, |state, cx| {
        state.tap_ok = ready;
        // 授权到位就把「降级中」的警告擦掉，否则面板底部会一直挂着一条过时消息。
        if ready {
            state.status.clear();
        }
        cx.notify();
    });
    log::info!(
        "键盘 tap {}",
        if ready {
            "已接上（面板现在能收按键）"
        } else {
            "不可用（降级为 espanso 触发）"
        }
    );
}

fn handle(cx: &mut App, state: &Entity<AppState>, tx: &Sender<Event>, event: Event) {
    match event {
        Event::Open { erase, .. } => {
            // 幂等：espanso 的 shell 唤醒与自己的 tap 可能同时到达。
            if state.read(cx).open {
                log::debug!("面板已打开，忽略重复的 open");
                return;
            }
            log::info!("打开面板：erase={erase}");
            state.update(cx, |state, cx| {
                state.begin_open(erase);
                // 开面板这一刻按 tap 的真实状态决定画什么：有授权就画列表，
                // 没有就画授权页——摆一列点了没反应的候选等于骗人。
                state.tap_ok = tap::ready();
                cx.notify();
            });
            tap::set_panel_open(true);
            if let Err(err) = ui::window::open(cx, state.clone()) {
                log::error!("打开面板失败：{err}");
                tap::set_panel_open(false);
                state.update(cx, |state, cx| {
                    state.begin_close();
                    state.status = format!("面板打开失败：{err}");
                    cx.notify();
                });
            }
        }

        Event::Key(msg) => {
            let intent = state.update(cx, |state, cx| {
                let intent = state.on_key(msg);
                cx.notify();
                intent
            });
            match intent {
                Intent::None => {}
                Intent::Cancel => cancel(cx, state),
                Intent::Confirm(hit) => confirm(cx, state, hit),
            }
        }

        Event::Cancel => cancel(cx, state),

        Event::IndexUpdated => {
            state.update(cx, |state, cx| {
                state.sync_index();
                cx.notify();
            });
        }

        // 设置窗口落盘之后投过来的：把新配置换进面板，并让触发状态机跟着换。
        // 面板没开时也要处理——trigger 是全局的，下一次 `;;` 就得按新键码走。
        Event::ConfigChanged => {
            state.update(cx, |state, cx| {
                state.reload_config();
                cx.notify();
            });
        }

        Event::Reload => {
            let index = state.read(cx).index.clone();
            index.reload_async(tx.clone());
        }

        Event::TapUnavailable(message) => {
            log::warn!("{message}");
            state.update(cx, |state, cx| {
                state.tap_ok = false;
                state.status = message;
                cx.notify();
            });
        }

        Event::Shutdown => {
            log::info!("收到退出请求");
            cx.quit();
        }
    }
}

/// 取消：把吞掉的那一个触发符回删掉，文档保持干净。
/// 只有 tap 路径会出现 `erase > 0`；espanso 唤醒的路径由 espanso 自己抹掉 trigger。
fn cancel(cx: &mut App, state: &Entity<AppState>) {
    let erase = state.read(cx).erase;
    close(cx, state);
    if erase > 0 {
        std::thread::Builder::new()
            .name("semisnip-erase".into())
            .spawn(move || crate::handoff::inject::backspace(erase))
            .ok();
    }
}

fn close(cx: &mut App, state: &Entity<AppState>) {
    // 先放开 swallow，再拆窗口：否则收尾期间用户按键会继续被吞。
    tap::set_panel_open(false);
    ui::window::close(cx, state);
    // 这里**不发 `cx.notify()`**：窗口在 `window::close` 里已经摘掉了，
    // 再通知重绘只会让 gpui 找不到窗口（日志里冒出 `window not found`）。
    // 清状态本身不需要重绘——`open = false` 之后调度器自然降回慢轮询。
    state.update(cx, |state, _| state.begin_close());
}

fn confirm(cx: &mut App, state: &Entity<AppState>, hit: crate::index::Hit) {
    let (erase, config, fallback) = {
        let state = state.read(cx);
        (state.erase, state.config.clone(), state.fallback_text())
    };

    close(cx, state);

    state.update(cx, |state, _| state.record_use(&hit.trigger));
    log::info!("展开 {}", hit.trigger);
    state::run_confirm(hit, erase, config, fallback);
}
