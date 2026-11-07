//! 面板渲染：查询行 / 候选列表 / 详情预览 / 状态条。
//!
//! 面板不接收焦点、不处理 gpui 的键盘事件——所有按键由 tap 直接改状态，
//! 这里只负责把状态画出来（因此重绘靠 `cx.notify()` 驱动）。
//!
//! 唯一的鼠标交互是右上角的 `✕`：键盘那条路依赖 tap，tap 不在时它就是
//! 全部（详见 [`close_button`]）。别的点击一律不处理。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, IntoElement, Render, SharedString, div, px};

use crate::app::event::{self, Event};
use crate::app::state::AppState;

/// 候选行高。30 是压着"信息密度"和"点得到"之间的平衡点：
/// 再矮会让触控板手抖选错，再高则 9 条候选放不进默认的 420 高面板。
const ROW_H: f32 = 30.0;
/// 详情栏宽度。固定值而不是比例：正文用等宽字体，宽度一变折行位置就跳，
/// 用户来回切候选时会觉得版面在抖。
const DETAIL_W: f32 = 286.0;

/// 面板用到的主题色。都是 `Copy` 的小值，按值往下传最省事。
///
/// 为什么要打包成一个结构体：这些 helper 拿不到 `Context`（主题的生命周期
/// 绑在 `App` 上），只能把颜色一列列传进去。传三四个还看得懂，传到七八个就
/// 分不清谁是谁了——clippy 的 `too_many_arguments` 也是这个意思。
#[derive(Clone, Copy)]
struct Palette {
    border: gpui_kit::Hsla,
    bg: gpui_kit::Hsla,
    fg: gpui_kit::Hsla,
    muted: gpui_kit::Hsla,
    selection: gpui_kit::Hsla,
    detail_bg: gpui_kit::Hsla,
    danger: gpui_kit::Hsla,
}

/// 面板右上角的关闭按钮。
///
/// **面板不能只有一条关闭路径。** `Esc` 走的是全局键盘 tap：tap 没装上
/// （没有「辅助功能」权限、或权限在重签之后失效）时，面板收不到任何按键——
/// 而面板是 `NonactivatingPanel`，也不会因为点别处而自己消失。这时候如果
/// 连个鼠标入口都没有，用户就只能强杀 daemon。
///
/// 所以这里给一个不依赖键盘、也不依赖权限的出口：点一下等于按 `Esc`，
/// 统一投 [`Event::Cancel`] 交给 scheduler 收尾（放开吞键、删窗口、把触发词
/// 补回文档），不在这里直接关窗——几条关闭路径必须走同一套收尾逻辑。
fn close_button(p: Palette) -> AnyElement {
    div()
        .id("semisnip-close")
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .w(px(18.))
        .h(px(18.))
        .rounded_sm()
        .cursor_pointer()
        .text_xs()
        .text_color(p.muted)
        .hover(move |style| style.bg(p.selection).text_color(p.fg))
        // 用 `on_click`（按下 + 抬起都落在按钮上）而不是 `on_mouse_down`：
        // 按下就拆窗口的话，紧跟着的 mouseUp 会打到已经不存在的窗口上，
        // gpui 会在日志里刷 `window not found`。
        .on_click(|_, _, cx| {
            if let Some(tx) = event::tx(cx) {
                let _ = tx.send(Event::Cancel);
            }
        })
        .child(SharedString::from("✕"))
        .into_any_element()
}

/// 降级视图：`tap` 没装上时**整块换掉**，不画候选列表。
///
/// 这条路径以前不存在，代价是一次真实的困惑：没有「辅助功能」授权时
/// `;;` 仍然经 espanso 打开面板，列表也照常渲染，于是用户打字发现毫无反应，
/// 看起来就是「面板卡死」。摆一列点了没反应的候选等于骗人——那一刻该说的是
/// 「我需要授权」，并且要给出一次点击就能到的地方。
///
/// 两个按钮分别对应两种真实场景：
/// - 「打开辅助功能设置」：还没授权的；
/// - 「重新检查」：授权过了但列表里是旧条目（重装换了签名），或者刚勾上还没生效。
///   点它等于立刻跑一次 [`crate::trigger::tap::retry`]，几秒内面板自己会变成正常态。
fn permission_view(p: Palette) -> AnyElement {
    let step = |number: &'static str, text: &'static str| {
        div()
            .flex()
            .flex_row()
            .gap_2()
            .text_sm()
            .text_color(p.fg)
            .child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .text_color(p.muted)
                    .child(SharedString::from(number)),
            )
            .child(SharedString::from(text))
    };

    div()
        .flex()
        .flex_col()
        .size_full()
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(p.border)
        .bg(p.bg)
        .shadow_lg()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(p.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_lg()
                        .text_color(p.fg)
                        .child(SharedString::from("需要「辅助功能」授权")),
                )
                .child(close_button(p)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap_3()
                .p_4()
                .overflow_hidden()
                .child(div().text_sm().text_color(p.fg).child(SharedString::from(
                    "面板本身不接收键盘：按键全靠一个全局 tap 转发过来。",
                )))
                .child(div().text_sm().text_color(p.fg).child(SharedString::from(
                    "没有授权，这个 tap 装不上，所以在这里打字、回车、Esc 都不会有反应。",
                )))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(step("1", "系统设置 → 隐私与安全性 → 辅助功能"))
                        .child(step(
                            "2",
                            "勾选 Semisnip（重装会换签名；已在列表里的话先删掉再加回来）",
                        ))
                        .child(step("3", "回来点「重新检查」——不用重启 daemon")),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(action_button(
                            p,
                            "semisnip-open-ax",
                            "打开辅助功能设置",
                            true,
                            |_| crate::ui::open_accessibility_settings(),
                        ))
                        .child(action_button(
                            p,
                            "semisnip-recheck-tap",
                            "重新检查",
                            false,
                            |_| {
                                crate::trigger::tap::retry();
                            },
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from(
                            "授权之前「;;」仍然能打开这个面板，但里面搜不了东西。",
                        )),
                ),
        )
        .into_any_element()
}

/// 授权页上的按钮。和 `✕` 一样走 `on_click`，不需要键盘、也不需要 focus。
fn action_button(
    p: Palette,
    id: &'static str,
    label: &'static str,
    primary: bool,
    action: impl Fn(&mut gpui_kit::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(26.))
        .px_3()
        .rounded_sm()
        .border_1()
        .border_color(p.border)
        .cursor_pointer()
        .text_sm()
        .text_color(p.fg)
        .bg(if primary { p.selection } else { p.bg })
        .hover(move |style| style.bg(p.selection))
        .on_click(move |_, _, cx| action(cx))
        .child(SharedString::from(label))
        .into_any_element()
}

impl Render for AppState {
    /// 画整个面板。
    ///
    /// 三条互斥路径：
    /// 1. `tap_ok == false` → 只画授权引导页（`permission_view`），不画列表；
    /// 2. 正常态：查询行 + 列表（+ 详情栏）+ 状态条；
    /// 3. 无命中/索引为空：列表区换成一句说明，其余部分不变。
    ///
    /// 这里**不读也不写**除 `AppState` 以外的任何状态；所有重绘由 `cx.notify()`
    /// 驱动。唯一的鼠标入口是 `✕`，其余点击一律不处理——面板不是普通窗口，
    /// 任何交互都要考虑到"键盘可能根本收不到"这个前提。
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = {
            let theme = cx.theme();
            Palette {
                border: theme.colors.border,
                bg: theme.colors.popover,
                fg: theme.colors.foreground,
                muted: theme.colors.muted_foreground,
                selection: theme.colors.selection,
                detail_bg: theme.colors.secondary,
                danger: theme.colors.danger,
            }
        };
        let mono = cx.theme().mono_font_family.clone();
        // 解构成局部变量：下面几十处用到它们，逐个改成 `p.x` 只会让 diff 变吵。
        let Palette {
            border,
            bg,
            fg,
            muted,
            selection,
            detail_bg,
            danger,
        } = p;

        // 没有键盘接管就别画列表：那种情况下打字没反应，候选再漂亮也是假的。
        if !self.tap_ok {
            return permission_view(p);
        }

        let rows: Vec<AnyElement> = self
            .hits
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let selected = index == self.selected;
                let usable = hit.snippet.usable;
                div()
                    .id(("semisnip-row", index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(ROW_H))
                    .px_2()
                    .rounded_sm()
                    .bg(if selected { selection } else { bg })
                    .child(
                        div()
                            .w(px(16.))
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(format!("{}", index + 1))),
                    )
                    .child(
                        div()
                            .w(px(112.))
                            .font_family(mono.clone())
                            .text_sm()
                            .text_color(if !usable { muted } else { fg })
                            .truncate()
                            .child(SharedString::from(hit.trigger.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(muted)
                            .truncate()
                            .child(SharedString::from(hit.snippet.label.clone())),
                    )
                    .child(
                        div()
                            .w(px(104.))
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(SharedString::from(hit.snippet.source.display())),
                    )
                    .into_any_element()
            })
            .collect();

        let empty = self.hits.is_empty();

        let detail = self.build_detail(border, fg, muted, detail_bg, danger, mono.clone());

        div()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow_lg()
            .child(
                // 查询行
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(mono.clone())
                            .text_lg()
                            .text_color(fg)
                            .truncate()
                            .child(SharedString::from(if self.query.is_empty() {
                                "搜索片段…".to_string()
                            } else {
                                format!("{}▌", self.query)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(format!(
                                "{}/{} · {}",
                                self.hits.len(),
                                self.snapshot().len(),
                                self.health_line()
                            ))),
                    )
                    .child(close_button(p)),
            )
            .child(
                // 列表 + 详情
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .p_1()
                            .overflow_hidden()
                            .when(empty, |this| {
                                this.child(div().p_3().text_sm().text_color(muted).child(
                                    SharedString::from(if self.query.is_empty() {
                                        format!(
                                            "没读到片段。检查 {}",
                                            self.snapshot().config_root.display()
                                        )
                                    } else {
                                        format!("没有匹配 `{}` 的片段", self.query)
                                    }),
                                ))
                            })
                            .children(rows),
                    )
                    // 详情栏可以在设置里关掉：关掉后列表占满整个面板，
                    // 适合只想要"快速挑一条"的人。`show_detail` 只影响这一列，
                    // 预览本身仍然在后台算（`fallback_text` 要用它）。
                    .when(self.config.show_detail, |this| {
                        this.child(div().w(px(DETAIL_W)).child(detail))
                    }),
            )
            .child(
                // 状态条
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from("↑↓ 选择"))
                    .child(SharedString::from("↵ 交给 espanso 展开"))
                    .child(SharedString::from("esc 取消"))
                    .when(!self.status.is_empty(), |this| {
                        this.child(
                            div()
                                .text_color(danger)
                                .child(SharedString::from(self.status.clone())),
                        )
                    }),
            )
            // 和降级视图统一成 AnyElement：`render` 只有一个返回类型。
            .into_any_element()
    }
}

impl AppState {
    /// 画右侧详情栏。
    ///
    /// 所有文案都已经由 `PreviewDoc` 算好，这里只做排版。三个「take(N)」是硬上限：
    /// 正文 18 行、变量 6 条、警告 3 条——超出就会被 `overflow_hidden` 裁掉，
    /// 与其让 gpui 裁在半行上，不如自己截断并给一句"已截断"。
    ///
    /// 变量名在 `side_effect` 时标红：这是"回车会执行命令"的唯一视觉提示。
    fn build_detail(
        &self,
        border: gpui_kit::Hsla,
        fg: gpui_kit::Hsla,
        muted: gpui_kit::Hsla,
        detail_bg: gpui_kit::Hsla,
        danger: gpui_kit::Hsla,
        mono: gpui_kit::SharedString,
    ) -> AnyElement {
        let Some(doc) = self.preview.as_ref() else {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .border_l_1()
                .border_color(border)
                .bg(detail_bg)
                .p_3()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from("选中一条片段查看详情"))
                .into_any_element();
        };

        let mut body_lines: Vec<AnyElement> = doc
            .body
            .lines()
            .take(18)
            .map(|line| {
                div()
                    .font_family(mono.clone())
                    .text_xs()
                    .text_color(fg)
                    .child(SharedString::from(line.to_string()))
                    .into_any_element()
            })
            .collect();
        if doc.truncated {
            body_lines.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from("…（已截断）"))
                    .into_any_element(),
            );
        }

        let var_lines: Vec<AnyElement> = doc
            .vars
            .iter()
            .take(6)
            .map(|var| {
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .text_xs()
                    .child(
                        div()
                            .w(px(76.))
                            .font_family(mono.clone())
                            .text_color(if var.side_effect { danger } else { fg })
                            .truncate()
                            .child(SharedString::from(var.name.clone())),
                    )
                    .child(
                        div()
                            .text_color(muted)
                            .truncate()
                            .child(SharedString::from(format!("{} · {}", var.kind, var.detail))),
                    )
                    .into_any_element()
            })
            .collect();

        let warnings: Vec<AnyElement> = doc
            .warnings
            .iter()
            .take(3)
            .map(|warning| {
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(SharedString::from(warning.clone()))
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .size_full()
            .border_l_1()
            .border_color(border)
            .bg(detail_bg)
            .p_3()
            .gap_1()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_sm()
                            .text_color(fg)
                            .child(SharedString::from(doc.trigger.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(doc.kind.clone())),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(fg)
                    .truncate()
                    .child(SharedString::from(doc.label.clone())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(doc.source.clone())),
            )
            .children(warnings)
            .child(div().flex().flex_col().gap_0p5().children(body_lines))
            .children(var_lines)
            .into_any_element()
    }
}
