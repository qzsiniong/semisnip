//! 面板窗口的生命周期。
//!
//! `WindowKind::PopUp` 在 macOS 上是 NSPanel + NonactivatingPanel：
//! 浮在最前、跟随所有 Space，但**不激活 App** —— 目标 App 的光标位置
//! 全程不变，这是回传能"零折返"的前提。

use gpui_kit::{
    App, Entity, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, px, size,
};

use crate::app::state::AppState;

pub fn open(cx: &mut App, state: Entity<AppState>) -> anyhow::Result<()> {
    let (width, height) = {
        let state = state.read(cx);
        (
            state.config.panel_width.max(360.0),
            state.config.panel_height.max(200.0),
        )
    };

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(width), px(height)), cx)),
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: false,
        show: true,
        is_resizable: false,
        is_movable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };

    let view = state.clone();
    match gpui_kit::open_window(options, cx, move |_window, _cx| view.clone()) {
        Ok((handle, _)) => {
            state.update(cx, |state, _| state.handle = Some(handle));
            // 菜单第一项是「打开 / 关闭」二选一，开合之后必须重建。
            crate::tray::refresh(cx);
            log::info!("面板已打开 {width}x{height}");
            Ok(())
        }
        Err(err) => anyhow::bail!("{err:?}"),
    }
}

pub fn close(cx: &mut App, state: &Entity<AppState>) {
    let handle = state.update(cx, |state, _| state.handle.take());
    match handle {
        Some(handle) => match handle.update(cx, |_, window, _| window.remove_window()) {
            Ok(()) => log::info!("面板已收起"),
            // 句柄失效说明窗口已经不在了（用户关掉了 / 之前收过一次）。
            // 这里吞掉错误会让面板永远留在屏幕上，所以至少要留一行警告。
            Err(err) => log::warn!("收起面板失败：{err}"),
        },
        // 正常路径不会走到：`state.open` 为真时一定先 open 过窗口。
        None => log::debug!("收面板时没有窗口句柄，忽略"),
    }
    crate::tray::refresh(cx);
}
