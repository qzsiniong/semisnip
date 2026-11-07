//! 面板窗口的生命周期。
//!
//! `WindowKind::PopUp` 在 macOS 上是 NSPanel + NonactivatingPanel：
//! 浮在最前、跟随所有 Space，但**不激活 App** —— 目标 App 的光标位置
//! 全程不变，这是回传能"零折返"的前提。

use gpui_kit::{App, Entity, WindowBackgroundAppearance, WindowKind, WindowOptions, px, size};

use crate::app::state::AppState;
use crate::ui::place;

/// 打开面板窗口。已经开着时由调用方（scheduler）保证不会重复调用。
///
/// 几个参数都是刻意选的：
/// - `focus: false`：面板不能抢焦点，否则目标 App 失焦、光标位置丢失，
///   回传时还得把焦点还回去；
/// - `is_movable: false` / `is_resizable: false`：位置由 [`ui::place`] 按插入点
///   算出来，每次打开都重算。可拖动会让"下次打开"的位置不可预测，而这个窗口
///   只活几秒；
/// - `display_id` + `window_bounds`：必须是**同一块屏**的一对。gpui 把
///   `WindowBounds::Windowed` 的坐标解释为"相对于 `display_id` 那块屏"的本地
///   坐标，只给其中一个就会跑到主屏上去；
/// - `window_background: Transparent`：圆角与阴影由 `ui::view` 自己画，
///   窗口本身不铺底色，否则四角会出现白边。
///
/// 尺寸每次重新读 `config`：设置窗口改完尺寸后，下一次打开就该生效——
/// 这也是 `AppState::reload_config` 不需要处理尺寸的原因。
pub fn open(cx: &mut App, state: Entity<AppState>) -> anyhow::Result<()> {
    // 配置读一份快照：定位要同时看尺寸和 `position`，分两次读中间可能被设置
    // 窗口改掉，算出来的位置就会自相矛盾（比如按新宽度算、按旧策略选屏）。
    let (config, width, height) = {
        let state = state.read(cx);
        (
            state.config.clone(),
            state.config.panel_width.max(360.0),
            state.config.panel_height.max(200.0),
        )
    };

    let placement = place::compute(cx, &config, size(px(width), px(height)));

    let options = WindowOptions {
        window_bounds: Some(placement.window_bounds),
        display_id: placement.display_id,
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
            // 把窗口矩形交给鼠标监听：面板开着时，tap 靠它区分"点面板里"与
            // "点面板外"。量不到就传 0，那一轮退化成"点哪都不关"（见
            // `trigger::tap::PanelRect`）。位置写进日志，出问题时能直接对照。
            //
            // `window.bounds()` 给的是**屏幕本地**坐标，而鼠标事件是全局坐标；
            // 面板可能出现在副屏上，所以必须加上那块屏的全局原点，否则多屏时
            // "点外面"会判错块屏。
            match handle.update(cx, |_, window, _| window.bounds()) {
                Ok(bounds) => {
                    let (ox, oy) = placement.screen_origin;
                    let x = bounds.origin.x.as_f32() + ox;
                    let y = bounds.origin.y.as_f32() + oy;
                    let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
                    crate::trigger::tap::set_panel_bounds(
                        f64::from(x),
                        f64::from(y),
                        f64::from(w),
                        f64::from(h),
                    );
                    log::info!(
                        "面板已打开 {w}x{h} @ ({x:.0},{y:.0})（全局）· {}",
                        placement.note
                    );
                }
                Err(err) => log::warn!("量不到面板位置，点外部关闭本轮不可用：{err}"),
            }
            // 菜单第一项是「打开 / 关闭」二选一，开合之后必须重建。
            crate::tray::refresh(cx);
            Ok(())
        }
        Err(err) => anyhow::bail!("{err:?}"),
    }
}

/// 摘除面板窗口。
///
/// 先 `take()` 句柄再删窗：顺序反过来的话，若 `remove_window` 触发了回调
/// （它确实会触发 accessibility 相关回调），回调里读到的还是"窗口存在"，
/// 菜单第一项就会显示成"关闭片段面板"。
///
/// 句柄失效只记 warn 不 panic：窗口可能已被系统关掉（比如用户按了 Esc 之外的
/// 路径），这种"已经不在"对调用方来说等价于成功。
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
