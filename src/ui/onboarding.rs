//! 首次引导窗口。
//!
//! 只有四步，顺序照**用户的心路**排，不是照功能排：
//!
//! 1. 这是什么 —— 一句话说清「敲 `;;`，挑一条，交给 espanso」；
//! 2. 环境够不够 —— espanso / 辅助功能 / 片段库三项体检，缺什么当场给入口；
//! 3. 怎么用 —— 三个动作，配键盘帽；
//! 4. 完成 —— 把 `onboarded` 写进配置，下次启动不再弹。
//!
//! 几个刻意的取舍：
//!
//! - **不强迫走完**：窗口有关闭按钮，随时能关。没走完就不会写 `onboarded`，
//!   下次启动会再弹——比"关一次就永远看不到"更符合"我还没看懂，下次再说"。
//! - **体检只在打开时算一次**（以及用户点「重新检查」时），不是每帧算。
//!   `espanso --version` 要起子进程，每帧调一次窗口会卡成幻灯片。
//! - **结算页不自动开面板**：面板是给"正在某个输入框里打字"的人准备的，
//!   刚从引导窗出来的人没有那个上下文，弹出来只会莫名其妙。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, ClickEvent, Context, Entity, FontWeight,
    Global, Hsla, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window,
    WindowBounds, WindowKind, WindowOptions, div, px, size,
};

use crate::app::event::{self, Event};
use crate::config::Config;
use crate::paths;

/// 窗口尺寸。高度按"最长的那一步"（体检：三行卡片 + 说明 + 按钮）定，
/// 引导页最不该出现滚动条。
const WINDOW_W: f32 = 560.0;
const WINDOW_H: f32 = 640.0;
/// 内容最大宽度。窗口看着挺宽，但文字行宽必须收住：
/// 一行超过 45 个汉字就开始难读，居中排版尤其明显。
const CONTENT_W: f32 = 430.0;
/// 一共几步。写成常量，让 `step + 1 == STEPS` 这种判断只有一处。
const STEPS: usize = 4;

/// 一次渲染要用到的一组颜色。
///
/// 从 `cx.theme()` 里取一次、往下传：一是避免每个小函数都去拿 `&App`，
/// 二是深色 / 浅色只在这一个地方分叉，改配色不用满文件找 `.text_color(..)`。
#[derive(Clone, Copy)]
struct Palette {
    /// 窗口底色。
    bg: Hsla,
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
    card: Hsla,
    /// 品牌琥珀色，用在标记的 `;;` 上。
    amber: Hsla,
    /// 成功绿，结算页的对勾用它。
    success: Hsla,
    /// 主色，用在键盘帽和链接按钮上。
    accent: Hsla,
}

impl Palette {
    /// 从当前主题取一次色。所有取值都走组件库的 token，不写死颜色——
    /// 这样系统切深浅色时整页跟着变，不需要在每个 `text_color` 上分叉。
    fn of(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            bg: theme.colors.background,
            fg: theme.colors.foreground,
            muted: theme.colors.muted_foreground,
            border: theme.colors.border,
            card: theme.colors.secondary,
            amber: theme.colors.button_warning,
            success: theme.colors.button_success,
            accent: theme.colors.primary,
        }
    }
}

/// 环境体检结果。
///
/// 全部是"算的那一刻"的快照；只有用户点「重新检查」才会重算。
#[derive(Clone, Debug, Default)]
struct Checks {
    /// espanso 的版本号；`None` 表示找不到可执行文件。
    espanso: Option<String>,
    /// 索引里的片段条数。
    snippets: usize,
    /// 键盘 tap 是否已装上——等价于"辅助功能授权过了"。
    tap: bool,
    /// espanso 的降级 match 文件在不在（不在时，tap 失效就没法触发了）。
    match_file: bool,
}

impl Checks {
    /// 现算一次。
    ///
    /// 片段条数从 daemon 的快照里读、不重新扫盘：引导页要回答的是
    /// "现在能用多少条"，而不是"磁盘上有多少条"。
    fn probe(cx: &App) -> Self {
        let snippets = cx
            .try_global::<crate::app::state::GlobalApp>()
            .map(|global| global.0.read(cx).snapshot().len())
            .unwrap_or(0);

        Self {
            espanso: crate::espanso::cli::EspansoCli::discover().version(),
            snippets,
            tap: crate::trigger::tap::ready(),
            match_file: paths::espanso_match_file().exists(),
        }
    }
}

/// 引导窗口状态。
pub struct OnboardingState {
    /// 当前第几步，0..STEPS。
    step: usize,
    /// 完成时要写回配置，所以留一份可变的。
    config: Config,
    checks: Checks,
    handle: Option<AnyWindowHandle>,
}

/// 全局的引导窗状态。与设置窗口同一套模式：窗口存在与否由 `update` 的成败判断。
pub struct GlobalOnboarding(pub Entity<OnboardingState>);

impl Global for GlobalOnboarding {}

impl OnboardingState {
    /// 从第 0 步开始，并在打开时做一次体检。
    ///
    /// 体检放在这里而不是 `open` 里：`open` 还要处理"已有窗口提到最前"的分支，
    /// 只有真正新建窗口才需要算这些（算一次要起 `espanso --version` 子进程）。
    fn new(cx: &App) -> Self {
        Self {
            step: 0,
            config: Config::load(),
            checks: Checks::probe(cx),
            handle: None,
        }
    }

    /// 最后一步的「开始使用」：写标记、落盘。
    ///
    /// 落盘失败**不阻止关窗**：引导已经看完了，不该因为磁盘只读把人困在
    /// 这一页。代价只是下次启动会再弹一次——比"关不掉"好得多。
    fn finish(&mut self) {
        self.config.onboarded = true;
        match self.config.save() {
            Ok(()) => log::info!("首次引导已完成"),
            Err(err) => log::warn!("写入 onboarded 失败（下次启动会再弹一次）：{err}"),
        }
    }
}

/// 打开引导窗口（已开着就提到最前）。
///
/// 与设置窗口同一套模式：先 `set_global` 再 `open_window`（第一帧渲染就要读全局），
/// 最后 `activate(true)` 把窗口提到前面——daemon 是 Accessory 应用，
/// 不激活的话用户会以为引导没弹出来。
pub fn open(cx: &mut App) -> anyhow::Result<()> {
    if let Some(entity) = cx.try_global::<GlobalOnboarding>().map(|g| g.0.clone()) {
        if let Some(handle) = entity.read(cx).handle {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    let state = cx.new(|cx| OnboardingState::new(cx));
    cx.set_global(GlobalOnboarding(state.clone()));

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(WINDOW_W), px(WINDOW_H)), cx)),
        // 不允许缩放：版式是算好的，拉宽只会让行长变得难读。
        is_resizable: false,
        is_movable: true,
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some("semisnip".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        kind: WindowKind::Normal,
        focus: true,
        show: true,
        window_background: gpui_kit::WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };

    let view = state.clone();
    let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        crate::ui::sync_theme(window, cx);
        view.clone()
    })?;

    state.update(cx, |state, _| state.handle = Some(handle));
    // daemon 是 Accessory（不进 Dock），不主动激活窗口会开在别的 App 后面。
    cx.activate(true);
    log::info!("首次引导已打开");
    Ok(())
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

impl Render for OnboardingState {
    /// 画整页：主体（图标/标题/卡片/说明）+ 页脚（返回 · 圆点 · 继续）。
    ///
    /// 版式的三条硬约束，改动时别破坏：
    /// - 内容区限宽 `CONTENT_W` 并居中：行宽超过这个数中文就很难读；
    /// - `flex_1 + min_h_0` 给主体，保证卡片变高时页脚仍贴着底部；
    /// - 页脚用 `justify_between` 三块排开，按钮文案变化不会把圆点推歪。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(palette.bg)
            .pt(px(34.))
            .pb(px(20.))
            .px(px(28.))
            .items_center()
            .child(
                // 主体：图标 → 标题 → 副标题 → 卡片 → 说明，整块限宽居中。
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .w_full()
                    .max_w(px(CONTENT_W))
                    .flex_1()
                    .min_h_0()
                    .child(hero(&palette, self.step))
                    .child(card(&palette, self.step, &self.checks, cx))
                    .child(footnote(&palette, self.step, cx)),
            )
            // 页脚：返回 · 圆点 · 继续。三块 `justify_between` 排开，
            // 圆点自然落在中间；按钮文案变化不会把它推歪。
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .max_w(px(CONTENT_W))
                    .mt(px(8.))
                    .child(back_button(self.step, cx))
                    .child(dots(&palette, self.step))
                    .child(next_button(self.step, self.checks.tap, cx)),
            )
    }
}

impl OnboardingState {
    /// 第 0 步的「去修改」：把触发键交给设置窗口，引导本身停在这一步不动。
    /// 用户改完关掉设置，引导还在，不会丢进度。
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::ui::settings::open(cx) {
            log::error!("打开设置失败：{err}");
        }
    }

    /// 重新体检。授权之后必须点一下——`AXIsProcessTrusted` 是查出来缓存住的，
    /// 只有 tap 装成功了 `ready()` 才会变 true，而 tap 是启动时装的。
    fn recheck(&mut self, cx: &mut Context<Self>) {
        self.checks = Checks::probe(cx);
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// 顶部
// ---------------------------------------------------------------------------

/// 标记 / 对勾 + 标题 + 副标题。
fn hero(palette: &Palette, step: usize) -> AnyElement {
    let (title, subtitle) = match step {
        0 => (
            "欢迎使用 semisnip",
            "敲 ;; 打开浮层，挑一条片段，剩下的交给 espanso。",
        ),
        1 => (
            "先确认环境",
            "三项都正常就能直接用；缺辅助功能也能跑，只是慢一点。",
        ),
        2 => (
            "三步取一条片段",
            "面板全程不抢焦点，所以不会丢字、也不用切回来。",
        ),
        _ => ("一切就绪", "现在去任意输入框敲 ;; 试试。"),
    };

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .pb(px(18.))
        .child(if step + 1 == STEPS {
            check_badge(palette)
        } else {
            mark_badge(palette)
        })
        .child(
            div()
                .text_2xl()
                .font_weight(FontWeight::BOLD)
                .text_color(palette.fg)
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .text_sm()
                .text_color(palette.muted)
                .text_center()
                .child(SharedString::from(subtitle)),
        )
        .into_any_element()
}

/// 应用标记的替身：一个圆角方块里放「斜体 I + 两个分号」。
///
/// 真正的图标是构建期画出来的 PNG（`build.rs`），运行期没有可用的图像源；
/// 但引导页只需要一个"认得出来"的符号，用等宽字体排版同一句话就够了。
/// 顺便还保住了双色：`I` 是前景色，`;;` 是品牌琥珀。
fn mark_badge(palette: &Palette) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .size(px(72.))
        .rounded(px(18.))
        .border_1()
        .border_color(palette.border)
        .bg(palette.card)
        .font_family("Menlo")
        .text_2xl()
        .child(div().text_color(palette.fg).child(SharedString::from("I")))
        .child(
            div()
                .text_color(palette.amber)
                .child(SharedString::from(";;")),
        )
        .into_any_element()
}

/// 结算页的对勾。
fn check_badge(palette: &Palette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(72.))
        .rounded_full()
        .bg(palette.success)
        .text_3xl()
        // 对勾用前景色：深色主题下是白勾、浅色下是深灰勾，
        // 两种都比纯黑/纯白更贴着绿色底。
        .text_color(palette.fg)
        .child(SharedString::from("✓"))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// 卡片
// ---------------------------------------------------------------------------

/// 每一步的卡片。四种内容都收在这里，保持同一套行高与分割线。
fn card(
    palette: &Palette,
    step: usize,
    checks: &Checks,
    cx: &mut Context<OnboardingState>,
) -> AnyElement {
    let mut rows: Vec<AnyElement> = Vec::new();

    match step {
        0 => {
            rows.push(card_row(
                palette,
                "触发键",
                "默认 ;;。想换成 ,, 之类的也行，只支持 ASCII。",
                keycap(palette, ";;"),
                false,
            ));
            rows.push(card_row(
                palette,
                "片段不用搬",
                "它们仍然住在 espanso 的 match 目录里；semisnip 只读，从不改写。",
                div()
                    .text_xs()
                    .text_color(palette.muted)
                    .child(SharedString::from("只读"))
                    .into_any_element(),
                false,
            ));
            rows.push(card_row(
                palette,
                "什么时候都能改",
                "菜单栏 → 偏好设置，或者再点一次「使用引导…」。",
                Button::new("onboarding-open-settings")
                    .ghost()
                    .label("去修改")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.open_settings(cx);
                    }))
                    .into_any_element(),
                true,
            ));
        }
        1 => {
            rows.push(card_row(
                palette,
                "espanso",
                match &checks.espanso {
                    Some(version) => format!("{version} · 负责求值与注入"),
                    None => "找不到可执行文件：先装 espanso，否则选完也没人展开".into(),
                },
                status(palette, checks.espanso.is_some()),
                false,
            ));
            rows.push(card_row(
                palette,
                "辅助功能",
                if checks.tap {
                    "已授权：面板靠它接管键盘，才能不抢焦点".to_string()
                } else if checks.match_file {
                    "没授权：退化成 espanso 触发，每次要多等一会儿".to_string()
                } else {
                    "没授权、降级定义也不在：现在 `;;` 打不开面板，先授权或跑一次 semisnip install"
                        .to_string()
                },
                if checks.tap {
                    status(palette, true)
                } else {
                    Button::new("onboarding-grant")
                        .ghost()
                        .label("去授权")
                        .on_click(|_, _, _| crate::ui::open_accessibility_settings())
                        .into_any_element()
                },
                false,
            ));
            rows.push(card_row(
                palette,
                "片段库",
                if checks.snippets == 0 {
                    format!("没读到片段。检查 {}", paths::espanso_match_dir().display())
                } else {
                    format!(
                        "{} 条 · 来自 {}",
                        checks.snippets,
                        paths::espanso_match_dir().display()
                    )
                },
                status(palette, checks.snippets > 0),
                true,
            ));
        }
        2 => {
            rows.push(card_row(
                palette,
                "1 · 敲触发键",
                "在任意输入框里连敲两下分号。第二个属于 semisnip，不会留在文档里。",
                keycap(palette, ";;"),
                false,
            ));
            rows.push(card_row(
                palette,
                "2 · 输入关键词",
                "空格分词、全部命中才算；中文标签可以打拼音首字母。",
                div()
                    .text_xs()
                    .text_color(palette.muted)
                    .child(SharedString::from("rq → 日期"))
                    .into_any_element(),
                false,
            ));
            rows.push(card_row(
                palette,
                "3 · 回车",
                "把 trigger 交给 espanso 求值并注入；Esc 取消，`;;` 会原样留在输入框里。",
                keycap(palette, "↵"),
                true,
            ));
        }
        _ => {
            rows.push(card_row(
                palette,
                "触发键",
                "随时能在设置里改。",
                keycap(palette, ";;"),
                false,
            ));
            rows.push(card_row(
                palette,
                "片段",
                format!("{} 条可用", checks.snippets),
                status(palette, checks.snippets > 0),
                false,
            ));
            rows.push(card_row(
                palette,
                "espanso",
                checks
                    .espanso
                    .clone()
                    .unwrap_or_else(|| "未找到".to_string()),
                status(palette, checks.espanso.is_some()),
                true,
            ));
        }
    }

    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(12.))
        .border_1()
        .border_color(palette.border)
        .bg(palette.card)
        .overflow_hidden()
        .children(rows)
        .into_any_element()
}

/// 卡片里的一行：左边标题 + 说明，右边控件。
///
/// `last` 控制底部分割线——最后一行画了就会在圆角处多出一道"半截线"。
fn card_row(
    palette: &Palette,
    title: &str,
    description: impl Into<String>,
    right: AnyElement,
    last: bool,
) -> AnyElement {
    let description: String = description.into();
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .px(px(14.))
        .py(px(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .text_color(palette.fg)
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(SharedString::from(description)),
                ),
        )
        .child(right);

    // 最后一行不画分割线：画了会在圆角处多出一道"半截线"。
    let row = if last {
        row
    } else {
        row.border_b_1().border_color(palette.border)
    };
    row.into_any_element()
}

/// 键盘帽。
fn keycap(palette: &Palette, text: &str) -> AnyElement {
    div()
        .px(px(7.))
        .py(px(2.))
        .rounded(px(5.))
        .border_1()
        .border_color(palette.border)
        .bg(palette.bg)
        .font_family("Menlo")
        .text_xs()
        .text_color(palette.accent)
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

/// 状态点：合格的打勾，不合格的给一个感叹号（不用红叉——还没装不代表出错）。
fn status(palette: &Palette, ok: bool) -> AnyElement {
    div()
        .text_sm()
        .text_color(if ok { palette.success } else { palette.amber })
        .child(SharedString::from(if ok { "✓" } else { "!" }))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// 页脚
// ---------------------------------------------------------------------------

/// 每一步下面那行小字。第 1 步多一个「重新检查」——
/// 授权是个外部动作，用户很可能在我们算完之后才去勾选。
fn footnote(palette: &Palette, step: usize, cx: &mut Context<OnboardingState>) -> AnyElement {
    let text = match step {
        0 => "两步就能用：授权辅助功能 → 敲 ;;",
        1 => "授权之后 semisnip 要重启一次才会接管键盘；这里的检查也会跟着变绿。",
        2 => "↑ ↓ 选择 · ↵ 确认 · Esc 取消",
        _ => "这些都能在菜单栏的「偏好设置…」里改。",
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .pt(px(12.))
        .text_xs()
        .text_color(palette.muted)
        .child(SharedString::from(text))
        // 体检是"打开那一刻"的快照，而授权发生在窗口外面。
        // 没有这个按钮，用户勾完权限回来只能看到一个红的感叹号。
        .when(step == 1, |this| {
            this.child(
                Button::new("onboarding-recheck")
                    .ghost()
                    .label("重新检查")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.recheck(cx))),
            )
        })
        .into_any_element()
}

/// 进度点。当前步实心，其余描边色。
fn dots(palette: &Palette, step: usize) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .children((0..STEPS).map(|index| {
            div().size(px(6.)).rounded_full().bg(if index == step {
                palette.fg
            } else {
                palette.border
            })
        }))
        .into_any_element()
}

/// 返回按钮。第 0 步没有返回，用一个等宽占位顶住，免得圆点被推到左边。
fn back_button(step: usize, cx: &mut Context<OnboardingState>) -> AnyElement {
    if step == 0 {
        return div().w(px(56.)).into_any_element();
    }
    Button::new("onboarding-back")
        .ghost()
        .label("返回")
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.step = this.step.saturating_sub(1);
            cx.notify();
        }))
        .into_any_element()
}

/// 主按钮。最后一步改文案、落盘并关窗。
///
/// `tap_ok` 只在第 1 步用到：没授权时把「继续」降级成 ghost，
/// 让人先注意到上面那张「去授权」而不是一路点过去。
fn next_button(step: usize, tap_ok: bool, cx: &mut Context<OnboardingState>) -> AnyElement {
    let last = step + 1 == STEPS;
    let primary = step != 1 || tap_ok;

    let button = Button::new("onboarding-next")
        .rounded(ButtonRounded::Large)
        .label(if last { "开始使用" } else { "继续" })
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            if last {
                this.finish();
                // 用户可能刚在别处补了片段，关窗顺手刷一次索引。
                if let Some(tx) = event::tx(cx) {
                    let _ = tx.send(Event::IndexUpdated);
                }
                window.remove_window();
            } else {
                this.step += 1;
                cx.notify();
            }
        }));

    if primary {
        button.primary().into_any_element()
    } else {
        button.ghost().into_any_element()
    }
}
