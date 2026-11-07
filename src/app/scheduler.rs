//! UI 侧调度：消费 tap 线程与 IPC 线程的事件，驱动窗口与状态。
//!
//! 轮询（16ms 忙 / 50ms 闲）而不是跨线程唤醒——gpui 的 `App` 只能在 UI
//! 线程访问，轮询是这里最稳的桥。这和 snippets / clipbeam 的做法一致。

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use gpui_kit::{App, AsyncApp, Entity};

use crate::app::event::Event;
use crate::app::state::{self, AppState, Intent};
use crate::trigger::tap;
use crate::ui;

pub fn spawn(cx: &mut AsyncApp, state: Entity<AppState>, rx: Receiver<Event>, tx: Sender<Event>) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let bg = cx.background_executor().clone();
        loop {
            let busy = cx.update(|cx: &mut App| {
                let state = state.read(cx);
                state.open
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
            .name("snipshot-erase".into())
            .spawn(move || crate::handoff::inject::backspace(erase))
            .ok();
    }
}

fn close(cx: &mut App, state: &Entity<AppState>) {
    // 先放开 swallow，再拆窗口：否则收尾期间用户按键会继续被吞。
    tap::set_panel_open(false);
    ui::window::close(cx, state);
    state.update(cx, |state, cx| {
        state.begin_close();
        cx.notify();
    });
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
