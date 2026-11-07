//! 面板渲染：查询行 / 候选列表 / 详情预览 / 状态条。
//!
//! 面板不接收焦点、不处理 gpui 的键盘事件——所有按键由 tap 直接改状态，
//! 这里只负责把状态画出来（因此重绘靠 `cx.notify()` 驱动）。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, IntoElement, Render, SharedString, div, px};

use crate::app::state::AppState;

const ROW_H: f32 = 30.0;
const DETAIL_W: f32 = 286.0;

impl Render for AppState {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (border, bg, fg, muted, selection, detail_bg, danger, mono) = {
            let theme = cx.theme();
            (
                theme.colors.border,
                theme.colors.popover,
                theme.colors.foreground,
                theme.colors.muted_foreground,
                theme.colors.selection,
                theme.colors.secondary,
                theme.colors.danger,
                theme.mono_font_family.clone(),
            )
        };

        let rows: Vec<AnyElement> = self
            .hits
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let selected = index == self.selected;
                let usable = hit.snippet.usable;
                div()
                    .id(("snipshot-row", index))
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
                    ),
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
                    .child(div().w(px(DETAIL_W)).child(detail)),
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
    }
}

impl AppState {
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
