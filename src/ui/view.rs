//! 面板渲染：查询行 / 候选列表 / 详情预览 / 状态条。
//!
//! 面板不接收焦点、不处理 gpui 的键盘事件——所有按键由 tap 直接改状态，
//! 这里只负责把状态画出来（因此重绘靠 `cx.notify()` 驱动）。
//!
//! 鼠标交互只有两处：右上角的 `✕`，以及授权页里那列可点候选。键盘那条路
//! 依赖 tap，tap 不在时它们就是全部出路（详见 [`close_button`] 与
//! [`permission_view`]）。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, IntoElement, Render, SharedString, div, px};

use crate::app::event::{self, Event};
use crate::app::state::{AppState, COMMANDS, PanelMode};
use crate::search::query::{Query, TokenKind};
use crate::ui::modes;

/// 候选行高。30 是压着"信息密度"和"点得到"之间的平衡点：
/// 再矮会让触控板手抖选错，再高则 9 条候选放不进默认的 420 高面板。
pub(crate) const ROW_H: f32 = 30.0;
/// 详情栏宽度。固定值而不是比例：正文用等宽字体，宽度一变折行位置就跳，
/// 用户来回切候选时会觉得版面在抖。
const DETAIL_W: f32 = 286.0;

/// 面板用到的主题色。都是 `Copy` 的小值，按值往下传最省事。
///
/// 为什么要打包成一个结构体：这些 helper 拿不到 `Context`（主题的生命周期
/// 绑在 `App` 上），只能把颜色一列列传进去。传三四个还看得懂，传到七八个就
/// 分不清谁是谁了——clippy 的 `too_many_arguments` 也是这个意思。
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub border: gpui_kit::Hsla,
    pub bg: gpui_kit::Hsla,
    pub fg: gpui_kit::Hsla,
    pub muted: gpui_kit::Hsla,
    pub selection: gpui_kit::Hsla,
    pub detail_bg: gpui_kit::Hsla,
    pub danger: gpui_kit::Hsla,
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
pub(crate) fn close_button(p: Palette) -> AnyElement {
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

/// 降级视图：`tap` 没装上时替换掉搜索列表。
///
/// 这一页要回答两件事：**为什么打字没反应**、**现在还能做什么**。
///
/// - 原因：面板自己不处理键盘，按键全靠全局 tap 转发；没有授权，tap 装不上。
/// - 还能做什么：保留一份**鼠标可点**的候选——点一下直接走 espanso 展开。
///   过去这一页把列表整块换掉，面板虽然有窗口却完全不可用；没有键盘的人
///   连一条片段都选不了。现在列表里每一行都是通往 espanso 的真实路径。
///
/// 三个按钮分别对应三种真实场景：
/// - 「打开辅助功能设置」：还没授权的。会先调用带 prompt 的 AX API，把本进程
///   登记进 TCC 列表并弹一次系统提示，再打开设置面板。
/// - 「重新检查」：刚勾上还没生效、或列表里是重装前的旧条目。点它立刻跑一次
///   [`crate::trigger::tap::retry`]，结果直接写在页面上，不再静默。
/// - 「稍后」：没有 tap 时"点面板外关闭"也失效（鼠标判定同样装在 tap 上），
///   所以必须有一条不需要键盘、也不需要点外面的退出路径。
fn permission_view(p: Palette, mono: SharedString, state: &AppState) -> AnyElement {
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

    // 候选只放前 6 条：这一页主体是授权说明，列表只是"现在还能做什么"的兜底。
    let rows: Vec<AnyElement> = state
        .hits
        .iter()
        .take(6)
        .enumerate()
        .map(|(index, hit)| {
            let trigger = hit.trigger.clone();
            let label = hit.snippet.label.clone();
            div()
                .id(("semisnip-permission-row", index))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(24.))
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .hover(move |style| style.bg(p.selection))
                .on_click(move |_, _, cx| {
                    if let Some(tx) = event::tx(cx) {
                        let _ = tx.send(Event::ConfirmIndex(index));
                    }
                })
                .child(
                    div()
                        .w(px(112.))
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(p.fg)
                        .truncate()
                        .child(SharedString::from(trigger)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(p.muted)
                        .truncate()
                        .child(SharedString::from(label)),
                )
                .into_any_element()
        })
        .collect();

    let reason = (!state.permission_reason.is_empty()).then(|| {
        div()
            .text_xs()
            .text_color(p.danger)
            .child(SharedString::from(state.permission_reason.clone()))
            .into_any_element()
    });

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
                .min_h_0()
                .gap_2()
                .p_4()
                .overflow_hidden()
                .child(div().text_sm().text_color(p.fg).child(SharedString::from(
                    "面板自己不处理键盘：按键全靠一个全局 tap 转发过来。没有授权，tap 装不上，所以在这里打字、回车、Esc 都不会有反应。",
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
                        .child(step("3", "回来点「重新检查」——几秒内自动接管，不用重启 daemon")),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
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
                            |cx| {
                                let outcome = crate::trigger::tap::retry();
                                set_permission_reason(cx, recheck_message(outcome));
                            },
                        ))
                        .child(action_button(p, "semisnip-permission-later", "稍后", false, |cx| {
                            if let Some(tx) = event::tx(cx) {
                                let _ = tx.send(Event::Cancel);
                            }
                        })),
                )
                .children(reason)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .gap_1()
                        .border_t_1()
                        .border_color(p.border)
                        .pt_2()
                        .child(
                            div().text_xs().text_color(p.muted).child(SharedString::from(
                                "没有键盘也能用鼠标直接展开（走 espanso 求值）：",
                            )),
                        )
                        .when(rows.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(p.muted)
                                    .child(SharedString::from("还没读到片段，检查 espanso 的 match 目录。")),
                            )
                        })
                        .children(rows),
                ),
        )
        .into_any_element()
}

/// 「重新检查」的结果文案。每种失败都要说清下一步，不再点完毫无变化。
fn recheck_message(outcome: crate::trigger::tap::Retry) -> &'static str {
    use crate::trigger::tap::Retry;
    match outcome {
        Retry::AlreadyReady => "键盘接管已就绪，正在切回搜索列表…",
        Retry::Disabled => "调试开关 SEMISNIP_NO_TAP 正在强制降级，取消它后重启 daemon",
        Retry::NotInstalled => "键盘接管尚未初始化，请重启 semisnip 再试",
        Retry::NotTrusted => "系统里还没勾选 Semisnip：先按上面三步授权",
        Retry::Spawned => "已提交接管请求，成功后本页会自动切换…",
        Retry::SpawnFailed => "无法启动键盘线程，请查看日志后重启 daemon",
    }
}

/// 把检测结果写回面板状态。授权页没有状态条，结果只能走 `permission_reason`。
fn set_permission_reason(cx: &mut gpui_kit::App, message: &str) {
    if let Some(global) = cx.try_global::<crate::app::state::GlobalApp>() {
        let entity = global.0.clone();
        entity.update(cx, |state, cx| {
            state.permission_reason = message.to_string();
            cx.notify();
        });
    }
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

/// 速查覆盖层：叠在当前视图上，四周留一点边，让人看得出"底下那层还在"。
///
/// 关掉之后原模式原样恢复——这也是它不写成 `PanelMode::Help` 的原因：
/// 模式一换，返回目标（管理多选、迁移勾选）就得额外保存一份。
fn with_help(base: AnyElement, p: Palette, mono: SharedString, open: bool) -> AnyElement {
    if !open {
        return base;
    }
    div()
        .relative()
        .size_full()
        .child(base)
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .p_3()
                .child(modes::render_help(p, mono)),
        )
        .into_any_element()
}

/// 查询解析条：实时告诉用户每个词被当成了什么。
///
/// 关键词、标签、被丢弃的字符分三色。没有这几行，用户按了 `:` 却不知道
/// 它变成了标签过滤，或者粘贴了一段带引号的文本发现没搜到，只会觉得"面板坏了"。
fn parse_bar(p: Palette, mono: SharedString, raw: &str) -> Option<AnyElement> {
    let query = Query::parse(raw);
    if query.terms.is_empty() {
        return None;
    }
    let chips: Vec<AnyElement> = query
        .terms
        .iter()
        .map(|term| {
            let (color, bg) = match term.kind {
                TokenKind::Keyword => (p.fg, p.bg),
                TokenKind::Tag => (p.fg, p.selection),
                TokenKind::Ignored => (p.danger, p.bg),
            };
            div()
                .flex_none()
                .rounded_sm()
                .bg(bg)
                .px_1()
                .font_family(mono.clone())
                .text_xs()
                .text_color(color)
                .child(SharedString::from(term.text.clone()))
                .into_any_element()
        })
        .collect();

    Some(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_3()
            .pb_1()
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted)
                    .child(SharedString::from("解析")),
            )
            .children(chips)
            .into_any_element(),
    )
}

/// 标签建议弹窗：用户输入 `:` 时浮在列表上方，列出已有标签供挑选。
///
/// 与速查覆盖层不同，它只占顶部一小块（锚在查询行下方），不盖住整个面板——
/// 用户一边看候选一边还能看到列表的变化。没有匹配候选时返回 `None`，
/// 调用方就不画这层，省一个空白浮窗。
fn render_tag_suggestions(state: &AppState, p: Palette, mono: SharedString) -> Option<AnyElement> {
    if !state.tag_suggest {
        return None;
    }
    let candidates = state.tag_candidates();
    if candidates.is_empty() {
        return None;
    }
    let cursor = state.tag_cursor.min(candidates.len() - 1);
    let rows: Vec<AnyElement> = candidates
        .iter()
        .enumerate()
        .map(|(idx, tag)| {
            let selected = idx == cursor;
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .bg(if selected { p.selection } else { p.bg })
                .child(
                    div()
                        .flex_none()
                        .w(px(36.))
                        .text_xs()
                        .text_color(p.muted)
                        .child(SharedString::from(if selected { "↵" } else { "" })),
                )
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_sm()
                        .text_color(if selected { p.fg } else { p.muted })
                        .child(SharedString::from(format!(":{tag}"))),
                )
                .into_any_element()
        })
        .collect();

    Some(
        div()
            .absolute()
            // 锚在查询行（约 44px）下方，左右留与查询行一致的内边距。
            .left_0()
            .right_0()
            .top(px(46.))
            .mx_3()
            .rounded_sm()
            .border_1()
            .border_color(p.border)
            .bg(p.bg)
            .shadow_lg()
            .overflow_hidden()
            .children(rows)
            .into_any_element(),
    )
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
            return permission_view(p, mono, self);
        }

        // 模式视图整块替换“列表 + 详情”，各自都有同一个 `✕` 出口。
        let entity = cx.entity();
        match &self.mode {
            PanelMode::Keyword { hit, .. } => {
                let view = self.keyword_view(hit);
                let base =
                    modes::render_keyword(p, mono.clone(), &view, &hit.trigger, view.duplicates);
                return with_help(base, p, mono.clone(), self.help_open);
            }
            PanelMode::Command { cursor } => {
                return with_help(
                    modes::render_command(p, mono.clone(), *cursor, &COMMANDS),
                    p,
                    mono.clone(),
                    self.help_open,
                );
            }
            PanelMode::Manage(_) => {
                return with_help(
                    modes::render_manage(self, p, mono.clone(), entity),
                    p,
                    mono.clone(),
                    self.help_open,
                );
            }
            PanelMode::Migrate(_) => {
                return with_help(
                    modes::render_migrate(self, p, mono.clone()),
                    p,
                    mono.clone(),
                    self.help_open,
                );
            }
            _ => {}
        }

        let show_badges = self.config.show_match_badges;
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
                    // 分值与命中字段徽标：调试"为什么这条排前面"时最有用。
                    // 用固定宽度而不是自适应，避免分数长度变化让整列左右跳。
                    .children(show_badges.then(|| {
                        let badges = hit.matched.labels().join("·");
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(38.))
                                    .text_xs()
                                    .text_color(muted)
                                    .child(SharedString::from(hit.score.to_string())),
                            )
                            .child(
                                div()
                                    .w(px(88.))
                                    .text_xs()
                                    .text_color(if hit.matched.is_empty() { muted } else { fg })
                                    .truncate()
                                    .child(SharedString::from(if badges.is_empty() {
                                        "—".to_string()
                                    } else {
                                        badges
                                    })),
                            )
                            .into_any_element()
                    }))
                    // 直达触发词是“下次可以不开面板”的线索，放在触发词这一侧。
                    .children(hit.snippet.direct_trigger(&self.config.direct_prefix).map(
                        |direct| {
                            div()
                                .w(px(76.))
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(SharedString::from(direct.to_string()))
                                .into_any_element()
                        },
                    ))
                    // 打开徽标后列表空间紧张，来源列让位给分值/徽标；
                    // 来源文件在右侧详情区里仍然能看到。
                    .children((!show_badges).then(|| {
                        div()
                            .w(px(104.))
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(SharedString::from(hit.snippet.source.display()))
                            .into_any_element()
                    }))
                    .into_any_element()
            })
            .collect();

        let empty = self.hits.is_empty();

        // 直达模式有自己的查询串；显示时带上 `,,` 前缀，否则用户会以为
        // 自己按的标点被吃掉了。
        let (shown_query, placeholder) = match &self.mode {
            PanelMode::Direct { query } => (query.clone(), "直达触发词…"),
            _ => (self.query.clone(), "搜索片段…"),
        };
        let mode_label = match &self.mode {
            PanelMode::Direct { .. } => Some(",,"),
            _ => None,
        };
        // 解析条只在查询视图出现：直达模式匹配的是 `,,` 后面的裸串，
        // 把符号丢掉的规则套上去反而会误导。
        let parse_row = match &self.mode {
            PanelMode::Direct { .. } => None,
            _ => parse_bar(p, mono.clone(), &self.query),
        };

        let detail = self.build_detail(border, fg, muted, detail_bg, danger, mono.clone());

        let base =
            div()
                .relative()
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
                        .children(mode_label.map(|label| {
                            div()
                                .flex_none()
                                .rounded_sm()
                                .bg(selection)
                                .px_2()
                                .text_xs()
                                .text_color(fg)
                                .child(SharedString::from(label))
                                .into_any_element()
                        }))
                        .children(self.full_text.then(|| {
                            div()
                                .flex_none()
                                .rounded_sm()
                                .border_1()
                                .border_color(border)
                                .px_2()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from("' 全文"))
                                .into_any_element()
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(mono.clone())
                                .text_lg()
                                .text_color(fg)
                                .truncate()
                                .child(SharedString::from(if shown_query.is_empty() {
                                    placeholder.to_string()
                                } else {
                                    format!("{shown_query}▌")
                                })),
                        )
                        .child(div().text_xs().text_color(muted).child(SharedString::from(
                            format!(
                                "{}/{} · {}",
                                self.hits.len(),
                                self.snapshot().len(),
                                self.health_line()
                            ),
                        )))
                        .child(close_button(p)),
                )
                .children(parse_row)
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
                // 标签建议弹窗：绝对定位浮在列表上方，不挤占布局。
                .children(render_tag_suggestions(self, p, mono.clone()))
                // 和降级视图统一成 AnyElement：`render` 只有一个返回类型。
                .into_any_element();
        with_help(base, p, mono, self.help_open)
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
