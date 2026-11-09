//! 面板内模式的渲染：关键字视图 / 命令菜单 / 速查 / 片段管理 / 迁移预览。
//!
//! 这些视图都替换“列表 + 详情”那一块，但共用同一个外框：标题、右上角 `✕`、
//! 底部提示条。抽成 [`frame`] 是为了让五种模式的边距、圆角、关闭入口完全一致
//! ——面板不是普通窗口，任何模式都必须保留“不依赖键盘的关闭方式”。

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Entity, SharedString, div, px};

use crate::app::state::{AppState, KeywordView, MANAGE_VISIBLE_ROWS, PanelMode};
use crate::ui::view::{Palette, ROW_H, close_button};

/// 统一外框：标题行（标题 + 右侧统计 + 关闭按钮）+ 内容 + 底部提示。
fn frame(
    p: Palette,
    title: &str,
    stat: Option<String>,
    body: AnyElement,
    footer: AnyElement,
) -> AnyElement {
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
                        .truncate()
                        .child(SharedString::from(title.to_string())),
                )
                .children(stat.map(|text| {
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from(text))
                        .into_any_element()
                }))
                .child(close_button(p)),
        )
        .child(body)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_1()
                .border_t_1()
                .border_color(p.border)
                .text_xs()
                .text_color(p.muted)
                .child(footer),
        )
        .into_any_element()
}

/// 管理面板里的迷你动作按钮（导出 / 迁移）。
fn action_chip(
    id: &'static str,
    label: &'static str,
    p: Palette,
    on_click: impl Fn(&mut gpui_kit::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .h(px(24.))
        .px_2()
        .rounded_sm()
        .border_1()
        .border_color(p.border)
        .cursor_pointer()
        .text_xs()
        .text_color(p.fg)
        .hover(move |style| style.bg(p.selection))
        .on_click(move |_, _, cx| on_click(cx))
        .child(SharedString::from(label))
        .into_any_element()
}

/// 一行底部提示：几段文字之间用 `·` 分隔。
fn hints(texts: &[&str]) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_2()
        .children(
            texts
                .iter()
                .map(|text| SharedString::from(text.to_string()).into_any_element()),
        )
        .into_any_element()
}

/// 关键字视图。只读：展示触发词、别名、来源、用量与迁移建议。
pub(crate) fn render_keyword(
    p: Palette,
    mono: SharedString,
    view: &KeywordView,
    trigger: &str,
    duplicates: usize,
) -> AnyElement {
    let mut trigger_rows: Vec<AnyElement> = view
        .triggers
        .iter()
        .map(|item| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(ROW_H))
                .px_2()
                .child(
                    div()
                        .w(px(140.))
                        .font_family(mono.clone())
                        .text_sm()
                        .text_color(p.fg)
                        .truncate()
                        .child(SharedString::from(item.text.clone())),
                )
                .when(item.is_direct, |this| {
                    this.child(
                        div()
                            .rounded_sm()
                            .bg(p.selection)
                            .px_1()
                            .text_xs()
                            .text_color(p.fg)
                            .child(SharedString::from("直达")),
                    )
                })
                .when(item.count > 1, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(p.danger)
                            .child(SharedString::from(format!("{} 处定义", item.count))),
                    )
                })
                .child(div().flex_1())
                .into_any_element()
        })
        .collect();

    if trigger_rows.is_empty() {
        trigger_rows.push(
            div()
                .px_2()
                .text_sm()
                .text_color(p.muted)
                .child(SharedString::from("这条片段没有触发词"))
                .into_any_element(),
        );
    }

    let aliases: Vec<AnyElement> = view
        .aliases
        .iter()
        .map(|alias| {
            div()
                .text_xs()
                .text_color(p.fg)
                .child(SharedString::from(alias.clone()))
                .into_any_element()
        })
        .collect();

    let mut sections: Vec<AnyElement> = Vec::new();
    sections.push(
        div()
            .font_family(mono.clone())
            .text_lg()
            .text_color(p.fg)
            .child(SharedString::from(trigger.to_string()))
            .into_any_element(),
    );
    sections.extend(trigger_rows);
    if !aliases.is_empty() {
        sections.push(
            div()
                .mt_1()
                .text_xs()
                .text_color(p.muted)
                .child(SharedString::from("别名"))
                .into_any_element(),
        );
        sections.push(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_1()
                .children(aliases)
                .into_any_element(),
        );
    }
    sections.push(
        div()
            .mt_1()
            .text_xs()
            .text_color(p.muted)
            .child(SharedString::from(view.source.clone()))
            .into_any_element(),
    );
    sections.push(
        div()
            .text_xs()
            .text_color(p.muted)
            .child(SharedString::from(format!(
                "累计 {} 次 · 本周 面板 {} / 直达 {}（估算）",
                view.usage_count, view.week_panel, view.week_direct
            )))
            .into_any_element(),
    );
    if duplicates > 0 {
        sections.push(
            div()
                .text_xs()
                .text_color(p.danger)
                .child(SharedString::from(format!(
                    "{duplicates} 条片段共用这个触发词，espanso 会让你消歧"
                )))
                .into_any_element(),
        );
    }
    if let Some(suggestion) = &view.suggestion {
        sections.push(
            div()
                .text_xs()
                .text_color(p.muted)
                .child(SharedString::from(suggestion.clone()))
                .into_any_element(),
        );
    }

    let body = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_1()
        .p_3()
        .overflow_hidden()
        .children(sections)
        .into_any_element();

    frame(
        p,
        "关键字",
        None,
        body,
        hints(&["↑↓ 切换片段", "↵ 展开", ", 再按一次进直达", "esc 返回"]),
    )
}

/// `/` 命令菜单：固定五项，上下移动光标。
pub(crate) fn render_command(
    p: Palette,
    mono: SharedString,
    cursor: usize,
    commands: &[&str],
) -> AnyElement {
    let rows: Vec<AnyElement> = commands
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let selected = index == cursor;
            let color = if selected { p.fg } else { p.muted };
            div()
                .id(("semisnip-command", index))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(ROW_H))
                .px_2()
                .rounded_sm()
                .bg(if selected { p.selection } else { p.bg })
                .child(
                    div()
                        .w(px(16.))
                        .text_xs()
                        .text_color(color)
                        .child(SharedString::from(
                            if selected { "›" } else { " " }.to_string(),
                        )),
                )
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_sm()
                        .text_color(color)
                        .child(SharedString::from(label.to_string())),
                )
                .into_any_element()
        })
        .collect();

    let body = div()
        .flex()
        .flex_col()
        .flex_1()
        .p_1()
        .overflow_hidden()
        .children(rows)
        .into_any_element();

    frame(
        p,
        "命令",
        None,
        body,
        hints(&["↑↓ 选择", "↵ 执行", "esc 返回"]),
    )
}

/// 速查覆盖层：叠在当前视图上，`esc` / `?` 关闭。
pub(crate) fn render_help(p: Palette, mono: SharedString) -> AnyElement {
    const LINES: &[(&str, &str)] = &[
        (",", "关键字视图；再按一次进 ,, 直达"),
        (";", "片段管理"),
        ("/", "命令菜单"),
        ("?", "速查覆盖层（再按一次或 esc 关闭）"),
        ("'", "全文模式：正文也参与匹配（最低档）"),
        (":", "标签过滤；冒号后弹已有标签候选（↑↓ 选 / ↵ 确认）"),
        ("↑↓", "在列表 / 管理里移动"),
        ("↵", "展开选中项 / 执行命令"),
        ("esc", "返回上一层；在最外层关闭面板"),
        ("space", "管理里切换多选"),
        ("a d f e m", "全选 / 启停 / 来源过滤 / 导出 / 迁移"),
        ("t", "管理里给选中项批量加标签"),
    ];

    let rows: Vec<AnyElement> = LINES
        .iter()
        .map(|(keys, text)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .py_0p5()
                .child(
                    div()
                        .w(px(64.))
                        .flex_none()
                        .font_family(mono.clone())
                        .text_sm()
                        .text_color(p.fg)
                        .child(SharedString::from(keys.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(p.muted)
                        .truncate()
                        .child(SharedString::from(text.to_string())),
                )
                .into_any_element()
        })
        .collect();

    let body = div()
        .flex()
        .flex_col()
        .flex_1()
        .gap_1()
        .p_3()
        .overflow_hidden()
        .children(rows)
        .into_any_element();

    frame(
        p,
        "快捷键速查",
        None,
        body,
        hints(&["esc / ? 关闭速查", "覆盖层不会丢掉当前视图"]),
    )
}

/// 本周毕业率。没有任何记录时显示 `—`，别用 0% 误导用户。
fn graduation(state: &AppState) -> String {
    let (panel, direct) = state.usage.week_stats();
    let total = panel + direct;
    if total == 0 {
        return "—".to_string();
    }
    format!(
        "{}%（直达 {} / 面板 {}）",
        (direct as u64 * 100 / total as u64),
        direct,
        panel
    )
}

/// 片段管理：多选、启停、来源过滤、导出、迁移入口。
pub(crate) fn render_manage(
    state: &AppState,
    p: Palette,
    mono: SharedString,
    entity: Entity<AppState>,
) -> AnyElement {
    let manage = match &state.mode {
        PanelMode::Manage(manage) => manage,
        _ => return div().into_any_element(),
    };
    let rows = state.manage_rows();
    let total = rows.len();
    let selected = manage.selected.len();
    let filter = manage
        .filter
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "全部来源".to_string());

    // 来源过滤 chip：鼠标点击等于按 `f`。
    let entity_filter = entity.clone();
    let filter_chip = div()
        .id("semisnip-manage-filter")
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(px(24.))
        .px_2()
        .rounded_sm()
        .border_1()
        .border_color(p.border)
        .cursor_pointer()
        .text_xs()
        .text_color(p.fg)
        .bg(p.selection)
        .hover(move |style| style.bg(p.selection))
        .on_click(move |_, _, cx| {
            entity_filter.update(cx, |state, cx| {
                state.manage_click_filter();
                cx.notify();
            });
        })
        .child(SharedString::from(format!("来源：{filter}")))
        .child(div().text_color(p.muted).child(SharedString::from("▾")))
        .into_any_element();

    // 导出 / 迁移是“对选中项”的两个动作，放在标题栏下一行而不是塞进底部提示：
    // 提示条只讲键位，可点按钮给只用鼠标的人留出口。
    let entity_export = entity.clone();
    let entity_migrate = entity.clone();
    let entity_tag = entity.clone();
    let export_button = action_chip("semisnip-manage-export", "导出", p, move |cx| {
        entity_export.update(cx, |state, cx| {
            state.manage_click_export();
            cx.notify();
        });
    });
    let migrate_button = action_chip("semisnip-manage-migrate", "迁移加 ,,", p, move |cx| {
        entity_migrate.update(cx, |state, cx| {
            state.manage_click_migrate();
            cx.notify();
        });
    });
    let tag_button = action_chip("semisnip-manage-tag", "加标签", p, move |cx| {
        entity_tag.update(cx, |state, cx| {
            state.manage_click_tag();
            cx.notify();
        });
    });

    let window_end = (manage.scroll + MANAGE_VISIBLE_ROWS).min(rows.len());
    let visible: Vec<AnyElement> = rows
        .iter()
        .enumerate()
        .skip(manage.scroll)
        .take(window_end.saturating_sub(manage.scroll))
        .map(|(index, snippet)| {
            let cursor = index == manage.cursor;
            let picked = manage
                .selected
                .contains(&crate::library::snippet_key(snippet));
            let disabled = state.library.is_disabled(snippet.trigger());
            let usable = snippet.usable;

            // 逐条毕业进度：本周内"直接从面板外打出 ,,xxx"的比例。
            // 和整体毕业率用同一个定义（direct / (panel + direct)），只是粒度到这条片段。
            let usage = state.usage.get(snippet.trigger());
            let total = usage.week_panel + usage.week_direct;
            let ratio = if total > 0 {
                usage.week_direct as f32 / total as f32
            } else {
                0.0
            };
            let graduation = div()
                .flex_none()
                .w(px(62.))
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div().h(px(3.)).w(px(58.)).rounded_sm().bg(p.border).child(
                        div()
                            .h(px(3.))
                            .w(px(58. * ratio))
                            .rounded_sm()
                            .bg(if total > 0 { p.fg } else { p.border }),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from(if total > 0 {
                            format!("{}/{}", usage.week_direct, total)
                        } else {
                            "—".to_string()
                        })),
                );

            let entity_row = entity.clone();
            let entity_switch = entity.clone();
            div()
                .id(("semisnip-manage-row", index))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(ROW_H))
                .px_2()
                .rounded_sm()
                .bg(if cursor { p.selection } else { p.bg })
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    entity_row.update(cx, |state, cx| {
                        state.manage_click_select(index);
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .w(px(18.))
                        .text_xs()
                        .text_color(if picked { p.fg } else { p.muted })
                        .child(SharedString::from(
                            if picked { "☑" } else { "☐" }.to_string(),
                        )),
                )
                .child(
                    div()
                        .w(px(120.))
                        .font_family(mono.clone())
                        .text_sm()
                        .text_color(if disabled || !usable { p.muted } else { p.fg })
                        .truncate()
                        .child(SharedString::from(snippet.trigger().to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(p.muted)
                        .truncate()
                        .child(SharedString::from(snippet.label.clone())),
                )
                .child(graduation)
                .when(disabled, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(p.danger)
                            .child(SharedString::from("停用")),
                    )
                })
                .child(
                    div()
                        .id(("semisnip-manage-switch", index))
                        .flex_none()
                        .rounded_sm()
                        .border_1()
                        .border_color(p.border)
                        .px_1()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(p.muted)
                        .hover(move |style| style.bg(p.selection))
                        .on_click(move |_, _, cx| {
                            entity_switch.update(cx, |state, cx| {
                                state.manage_click_toggle_enabled(index);
                                cx.notify();
                            });
                        })
                        .child(SharedString::from(if disabled {
                            "启用"
                        } else {
                            "停用"
                        })),
                )
                .child(
                    div()
                        .w(px(108.))
                        .text_xs()
                        .text_color(p.muted)
                        .truncate()
                        .child(SharedString::from(snippet.source.display())),
                )
                .into_any_element()
        })
        .collect();

    let empty = visible.is_empty();
    let body = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .child(filter_chip)
                .child(export_button)
                .child(migrate_button)
                .child(tag_button)
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from(format!("{selected} 已选 / {total} 条"))),
                )
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from(format!(
                            "本周毕业率 {}",
                            graduation(state)
                        ))),
                ),
        )
        // 标签输入态：一行迷你输入框，回车才落盘，esc 取消。
        // 放在标题栏下方而不是弹窗——管理面板本身就是一个键盘驱动的全屏视图。
        .children(manage.tag_input.as_ref().map(|buffer| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mx_3()
                .mb_1()
                .rounded_sm()
                .border_1()
                .border_color(p.fg)
                .bg(p.detail_bg)
                .px_2()
                .py_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from("标签")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono.clone())
                        .text_sm()
                        .text_color(p.fg)
                        .truncate()
                        .child(SharedString::from(format!("{buffer}▌"))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from("空格分隔多个 · ↵ 应用 · esc 取消")),
                )
                .into_any_element()
        }))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .p_1()
                .overflow_hidden()
                .when(empty, |this| {
                    this.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(p.muted)
                            .child(SharedString::from("这个来源下没有片段")),
                    )
                })
                .children(visible),
        )
        .into_any_element();

    frame(
        p,
        "片段管理",
        None,
        body,
        hints(&[
            "space 多选",
            "a 全选",
            "d 启停",
            "f 来源",
            "e 导出",
            "t 标签",
            "m 迁移",
            "↵ 关键字",
            "esc 返回",
        ]),
    )
}

/// 迁移预览：按来源文件分组，冲突项不可勾选。
pub(crate) fn render_migrate(state: &AppState, p: Palette, mono: SharedString) -> AnyElement {
    let migrate = match &state.mode {
        PanelMode::Migrate(migrate) => migrate,
        _ => return div().into_any_element(),
    };

    let conflicts = migrate
        .plans
        .iter()
        .filter(|plan| !plan.selectable())
        .count();
    let selectable = migrate
        .plans
        .iter()
        .filter(|plan| plan.selectable())
        .count();
    let selected = migrate
        .selected
        .iter()
        .filter_map(|idx| migrate.plans.get(*idx))
        .filter(|plan| plan.selectable())
        .count();

    let window_end = (migrate.scroll + MANAGE_VISIBLE_ROWS).min(migrate.plans.len());
    let visible: Vec<AnyElement> = migrate
        .plans
        .iter()
        .enumerate()
        .skip(migrate.scroll)
        .take(window_end.saturating_sub(migrate.scroll))
        .map(|(index, plan)| {
            let cursor = index == migrate.cursor;
            let picked = plan.selectable() && migrate.selected.contains(&index);
            let conflict = plan.conflict.clone();
            div()
                .id(("semisnip-migrate-row", index))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(ROW_H))
                .px_2()
                .rounded_sm()
                .bg(if cursor { p.selection } else { p.bg })
                .child(
                    div()
                        .w(px(18.))
                        .text_xs()
                        .text_color(if picked { p.fg } else { p.muted })
                        .child(SharedString::from(
                            if picked { "☑" } else { "☐" }.to_string(),
                        )),
                )
                .child(
                    div()
                        .w(px(120.))
                        .font_family(mono.clone())
                        .text_sm()
                        .text_color(if conflict.is_some() { p.muted } else { p.fg })
                        .truncate()
                        .child(SharedString::from(plan.new_trigger.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(p.muted)
                        .truncate()
                        .child(SharedString::from(plan.old_triggers.join(", "))),
                )
                .when(conflict.is_some(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(p.danger)
                            .truncate()
                            .child(SharedString::from(conflict.clone().unwrap_or_default())),
                    )
                })
                .child(
                    div()
                        .w(px(108.))
                        .text_xs()
                        .text_color(p.muted)
                        .truncate()
                        .child(SharedString::from(
                            plan.key
                                .0
                                .file_name()
                                .map(|name| name.to_string_lossy().to_string())
                                .unwrap_or_default(),
                        )),
                )
                .into_any_element()
        })
        .collect();

    let empty = visible.is_empty();
    let body = div()
        .flex()
        .flex_col()
        .flex_1()
        .overflow_hidden()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(p.muted)
                .child(SharedString::from(format!(
                    "可迁移 {selectable} 条 · 已选 {selected} · 冲突 {conflicts}"
                )))
                .child(
                    div()
                        .flex_1()
                        .text_color(p.danger)
                        .child(SharedString::from("写入会丢失 YAML 注释，已自动备份")),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .p_1()
                .overflow_hidden()
                .when(empty, |this| {
                    this.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(p.muted)
                            .child(SharedString::from("没有可迁移的片段（已有直达或全部冲突）")),
                    )
                })
                .children(visible),
        )
        .into_any_element();

    frame(
        p,
        "迁移预览",
        None,
        body,
        hints(&["space 勾选", "a 全选可迁移项", "↵ 应用", "esc 取消"]),
    )
}
