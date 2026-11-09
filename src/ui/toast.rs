//! 「下次可直接输入 ,,xxx」轻提示窗。
//!
//! 它和面板一样是 `PopUp` + `focus: false`：提示出现在光标附近，但**不抢焦点**
//! ——用户刚才按了回车，目标 App 的光标必须保持在原位，否则这条提示自己就会
//! 把展开结果打断。1.5 秒后由 scheduler 投 `Event::HintClose` 收窗。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Context, Entity, Global, IntoElement, Render, SharedString, Window,
    WindowBackgroundAppearance, WindowKind, WindowOptions, div, px, size,
};

use crate::ui::place;

const WIDTH: f32 = 520.0;
const HEIGHT: f32 = 44.0;

/// 提示窗自己的状态。只有一个字符串与窗口句柄。
pub struct ToastState {
    pub text: String,
    pub handle: Option<AnyWindowHandle>,
}

impl Global for ToastState {}

/// 全局句柄。窗口与状态一起存，关闭时按句柄摘窗。
pub struct GlobalToast(pub Entity<ToastState>);
impl Global for GlobalToast {}

/// 打开（或替换）提示窗。
pub fn open(cx: &mut App, text: String) {
    close(cx);

    // 定位沿用面板的 `place::compute`：拿不到插入点时会自动退化成居中，
    // 正好是计划里"失败则居中屏幕上方"想要的降级行为。
    // 提示永远贴光标：用户刚在光标处展开，提示跑屏幕正中会像另一个弹窗。
    let mut config = crate::app::state::app(cx).read(cx).config.clone();
    config.position = crate::config::Position::Auto;
    let placement = place::compute(cx, &config, size(px(WIDTH), px(HEIGHT)));

    let state = cx.new(|_| ToastState { text, handle: None });
    cx.set_global(GlobalToast(state.clone()));

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
        }
        Err(err) => {
            log::warn!("提示窗打开失败：{err:?}");
            cx.remove_global::<GlobalToast>();
        }
    }
}

/// 收起提示窗。没有提示窗时是空操作，可以重复调用。
pub fn close(cx: &mut App) {
    let Some(state) = cx
        .try_global::<GlobalToast>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    cx.remove_global::<GlobalToast>();
    let handle = state.update(cx, |state, _| state.handle.take());
    if let Some(handle) = handle {
        if let Err(err) = handle.update(cx, |_, window, _| window.remove_window()) {
            log::debug!("提示窗已经不在了：{err}");
        }
    }
}

impl Render for ToastState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .size_full()
            .px_4()
            .rounded_md()
            .border_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.popover)
            .shadow_lg()
            .child(
                div()
                    .flex_none()
                    .rounded_sm()
                    .bg(theme.colors.selection)
                    .px_1()
                    .text_xs()
                    .text_color(theme.colors.foreground)
                    .child(SharedString::from(",")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme.colors.foreground)
                    .truncate()
                    .child(SharedString::from(self.text.clone())),
            )
            .into_any_element()
    }
}
