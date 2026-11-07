//! 偏好设置窗口。
//!
//! 用 gpui-component 的 `Settings` 组件：左侧页签 + 搜索 + 分组卡片，
//! 每个字段是「getter 读全局状态 / setter 写回去」的闭包，不用自己维护表单。
//!
//! 三条约定，改代码时别破坏：
//!
//! 1. **改完立即落盘**，没有"保存"按钮。这些偏好都是单值、随时可改，
//!    多一步确认只会让人忘记按。写完投 [`Event::ConfigChanged`]，
//!    面板与触发状态机当场换值。
//! 2. **校验不过只改内存、不落盘**。输入框是逐字符提交的，用户打到一半的
//!    `,` 不该把磁盘上的 `;;` 覆盖掉，也不该让 `;;` 当场失灵——所以
//!    [`SettingsState::commit`] 先校验、再写、最后才通知。
//! 3. **窗口只有一个**。`GlobalSettings` 里存着句柄，再点一次菜单只是把
//!    已有窗口提上来；句柄失效（用户关掉了）才新建。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, Global, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window, WindowBounds, WindowKind,
    WindowOptions, div, px, size,
};

use crate::app::event::{self, Event};
use crate::config::{Config, FocusMode, Position};
use crate::paths;

/// 设置窗口宽度。800 是 `Settings` 组件在「侧栏 200 + 两列字段」下刚好
/// 不触发堆叠布局的下限；再窄它会自动改成上下排（组件里的 container query）。
const WINDOW_W: f32 = 820.0;
const WINDOW_H: f32 = 620.0;

/// 设置窗口的全部状态。
///
/// 这里**不缓存** daemon 的 `AppState`：设置窗口改的是配置文件，
/// 而文件是两边唯一的事实来源。窗口开着的这段时间里，如果用户在终端
/// 用 `semisnip config set` 改了别的字段，我们这边看不到——这是刻意的，
/// 双向同步的复杂度远大于收益（重开窗口即可看到最新值）。
pub struct SettingsState {
    /// 正在编辑的配置。可能比磁盘新：校验不过时就停在这里。
    pub config: Config,
    /// 底部状态行：最近一次落盘的结果，或校验失败的原因。
    status: String,
    /// 状态行用错误色还是普通色。
    error: bool,
    /// 窗口句柄。用户关窗后它不会自动变 `None`，靠 `update` 失败来识别。
    handle: Option<AnyWindowHandle>,
}

/// 全局的设置窗口状态。
///
/// 字段闭包只拿得到 `&App`，没有 `self`，所以必须有个全局入口把
/// 「哪份配置正在被编辑」告诉它们。这也是唯一一个"窗口状态放进全局"的地方。
pub struct GlobalSettings(pub Entity<SettingsState>);

impl Global for GlobalSettings {}

impl SettingsState {
    fn new() -> Self {
        Self {
            // 从磁盘读，而不是从 daemon 内存读：设置窗口要展示的正是
            // "下次启动会读到什么"，两者不一致时以磁盘为准更不容易让人困惑。
            config: Config::load(),
            status: String::new(),
            error: false,
            handle: None,
        }
    }

    /// 校验 → 落盘 → 通知 daemon。
    ///
    /// 校验失败时**什么都不写**，只把原因放进状态行。输入框里的文字
    /// 仍然是用户刚打的那一版（`self.config` 已改），看起来就是
    /// "输入框里有字、下面提示还不合法"，比悄悄回滚友好。
    fn commit(&mut self, cx: &mut Context<Self>) {
        if let Err(reason) = self.validate() {
            self.status = reason;
            self.error = true;
            cx.notify();
            return;
        }

        match self.config.save() {
            Ok(()) => {
                self.status = "已保存".into();
                self.error = false;
                // 面板此刻可能正开着：让它换掉内存里的旧配置。
                if let Some(tx) = event::tx(cx) {
                    let _ = tx.send(Event::ConfigChanged);
                }
            }
            Err(err) => {
                self.status = format!("写配置失败：{err}");
                self.error = true;
            }
        }
        cx.notify();
    }

    /// 所有字段的合法性检查（实现见模块下方的 [`validate`]，那里好单测）。
    fn validate(&self) -> Result<(), String> {
        validate(&self.config)
    }
}

/// 打开设置窗口（已开着就提到最前）。
///
/// 顺序上有一个必须注意的点：**先建窗口、再存句柄**。`open_window` 的闭包里
/// 会立刻渲染一次（读 `GlobalSettings`），所以 `set_global` 必须先于
/// `open_window`，否则第一帧的 getter 拿不到全局、会画出一份默认配置。
///
/// 结尾的 `cx.activate(true)` 不是可选项：daemon 是 `Accessory` 应用，
/// 不主动激活的话窗口会开在别的 App 后面，用户以为"点了没反应"。
pub fn open(cx: &mut App) -> anyhow::Result<()> {
    // 已有窗口：`activate_window` 会把它提到最前并抢焦点。
    // 句柄失效（用户已经关掉窗口）时 `update` 返回 Err，继续往下新建。
    if let Some(entity) = cx.try_global::<GlobalSettings>().map(|g| g.0.clone()) {
        if let Some(handle) = entity.read(cx).handle {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    let state = cx.new(|_| SettingsState::new());
    cx.set_global(GlobalSettings(state.clone()));

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(WINDOW_W), px(WINDOW_H)), cx)),
        window_min_size: Some(size(px(680.0), px(480.0))),
        // 设置是**普通窗口**：原生标题栏 + 红绿灯。不用 `TitleBar::title_bar_options()`
        // 那套自绘标题栏——那需要窗口里再放一个组件，而这里只有一个内容区。
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some("semisnip 设置".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        kind: WindowKind::Normal,
        focus: true,
        show: true,
        is_resizable: true,
        is_movable: true,
        window_background: gpui_kit::WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };

    let view = state.clone();
    let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        crate::ui::sync_theme(window, cx);
        view.clone()
    })?;

    state.update(cx, |state, _| state.handle = Some(handle));
    // daemon 是 Accessory（不进 Dock），不主动激活的话窗口会开在别的 App 后面。
    cx.activate(true);
    log::info!("设置窗口已打开");
    Ok(())
}

impl Render for SettingsState {
    /// 画设置窗口：`Settings` 组件（页签 + 搜索 + 分组）+ 底部状态行。
    ///
    /// 底部那行是**唯一**反馈落盘结果的地方：改动没有"保存"按钮，用户需要
    /// 知道"我刚才打的值到底有没有生效"。空状态也要给一句话（"改动立即生效"），
    /// 留白会让人以为还差一步。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (bg, border, muted, danger) = {
            let theme = cx.theme();
            (
                theme.colors.background,
                theme.colors.border,
                theme.colors.muted_foreground,
                theme.colors.danger,
            )
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(bg)
            .child(
                // `Settings` 内部是「可拖拽侧栏 + 内容区」，自己会撑满，
                // 外面套一层 flex_1 是为了给底部状态行留位置。
                div().flex_1().min_h_0().child(
                    Settings::new("semisnip-settings")
                        .sidebar_width(px(190.))
                        .page(general_page())
                        .page(appearance_page())
                        .page(behavior_page())
                        .page(about_page()),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(if self.error { danger } else { muted })
                    .child(SharedString::from(if self.status.is_empty() {
                        // 空状态给一句"改完就生效"，比留白有用：
                        // 用户会想知道到底要不要点保存。
                        "改动立即生效，不需要保存".to_string()
                    } else {
                        self.status.clone()
                    })),
            )
    }
}

/// 校验一份配置能不能落盘。
///
/// schema 里也有一份同样的约束，但那是给**手改 YAML 的编辑器**看的；
/// 这里必须再写一遍——设置窗口直接改结构体，根本不经过 serde。
///
/// 抽成自由函数是为了能单测：`SettingsState` 需要 `Context<Self>`，在单测里
/// 造不出来，而"哪些值算非法"正是最值得钉住的部分。
pub(crate) fn validate(config: &Config) -> Result<(), String> {
    crate::trigger::TriggerConfig::from_text(&config.trigger, config.trigger_timeout_ms)
        .map_err(|err| format!("触发键不合法：{err}"))?;
    if config.panel_width < 360.0 {
        return Err("面板宽度至少 360".into());
    }
    if config.panel_height < 240.0 {
        return Err("面板高度至少 240".into());
    }
    if config.max_results == 0 {
        return Err("候选条数至少 1".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 读取 / 写入
// ---------------------------------------------------------------------------

/// 读当前正在编辑的配置。
///
/// 拿不到全局（理论上只会在测试里发生）就返回默认值：设置页要能画出
/// 一个完整的界面，而不是 panic。
fn editing(cx: &App) -> Config {
    cx.try_global::<GlobalSettings>()
        .map(|global| global.0.read(cx).config.clone())
        .unwrap_or_default()
}

/// 改配置并提交。
///
/// `f` 只负责改内存，落盘/校验/通知统一走 [`SettingsState::commit`]——
/// 保证每个字段的行为完全一致，不会出现"这个开关存了、那个没存"。
fn edit(cx: &mut App, f: impl FnOnce(&mut Config)) {
    let Some(entity) = cx.try_global::<GlobalSettings>().map(|g| g.0.clone()) else {
        return;
    };
    entity.update(cx, |state, cx| {
        f(&mut state.config);
        state.commit(cx);
    });
}

// ---------------------------------------------------------------------------
// 页面
// ---------------------------------------------------------------------------

/// 通用：触发方式与结果数量。
fn general_page() -> SettingPage {
    SettingPage::new("通用").default_open(true).groups(vec![
        SettingGroup::new().title("触发").items(vec![
            SettingItem::new(
                "触发键",
                SettingField::<SharedString>::input(
                    |cx| editing(cx).trigger.into(),
                    |value, cx| edit(cx, |config| config.trigger = value.to_string()),
                )
                .default_value(";;"),
            )
            .description(
                "只支持 ASCII、最多 4 个字符——它会被编译成物理键码序列。\
                     改完要跑一次 `semisnip install`，否则 espanso 的降级定义还是旧键。",
            ),
            SettingItem::new(
                "触发键间隔",
                SettingField::<f64>::number_input(
                    NumberFieldOptions {
                        min: 200.0,
                        max: 3_000.0,
                        step: 50.0,
                    },
                    |cx| editing(cx).trigger_timeout_ms as f64,
                    |value, cx| {
                        edit(cx, |config| {
                            config.trigger_timeout_ms = value.round() as u64
                        })
                    },
                )
                .default_value(800.0),
            )
            .description("两个触发键之间最多隔多久（毫秒）。超过就当作普通输入。"),
        ]),
        SettingGroup::new().title("结果").items(vec![
            SettingItem::new(
                "候选条数",
                SettingField::<f64>::number_input(
                    NumberFieldOptions {
                        min: 1.0,
                        max: 30.0,
                        step: 1.0,
                    },
                    |cx| editing(cx).max_results as f64,
                    |value, cx| edit(cx, |config| config.max_results = value.round() as usize),
                )
                .default_value(9.0),
            )
            .description("面板最多列几条。改大到超过屏幕高度没有意义，列表不会滚动。"),
            SettingItem::new(
                "焦点模式",
                SettingField::<SharedString>::dropdown(
                    vec![
                        ("非激活（推荐）".into(), "nonactivating".into()),
                        ("激活".into(), "activating".into()),
                    ],
                    |cx| match editing(cx).focus_mode {
                        FocusMode::Nonactivating => "nonactivating".into(),
                        FocusMode::Activating => "activating".into(),
                    },
                    |value, cx| {
                        edit(cx, |config| {
                            config.focus_mode = if value.as_ref() == "activating" {
                                FocusMode::Activating
                            } else {
                                FocusMode::Nonactivating
                            };
                        })
                    },
                )
                .default_value("nonactivating"),
            )
            .description("非激活：目标 App 全程不失焦，回车不需要还原前台。激活分支尚未实现。"),
        ]),
    ])
}

/// 外观：面板位置、尺寸与详情栏。
fn appearance_page() -> SettingPage {
    SettingPage::new("外观").groups(vec![SettingGroup::new().title("面板").items(vec![
            SettingItem::new(
                "位置",
                SettingField::<SharedString>::dropdown(
                    vec![
                        ("自动（贴着光标）".into(), "auto".into()),
                        ("屏幕居中".into(), "center".into()),
                    ],
                    |cx| match editing(cx).position {
                        Position::Auto => "auto".into(),
                        Position::Center => "center".into(),
                    },
                    |value, cx| {
                        edit(cx, |config| {
                            config.position = if value.as_ref() == "center" {
                                Position::Center
                            } else {
                                Position::Auto
                            };
                        })
                    },
                )
                .default_value("auto"),
            )
            .description("自动：贴着输入光标弹出（下方放不下就翻到上方），水平居中于光标所在屏。居中：屏幕可视区正中。两种都跟随光标 / 当前窗口所在的那块屏。"),
            SettingItem::new(
                "宽度",
                SettingField::<f64>::number_input(
                    NumberFieldOptions {
                        min: 360.0,
                        max: 1_400.0,
                        step: 10.0,
                    },
                    |cx| editing(cx).panel_width as f64,
                    |value, cx| edit(cx, |config| config.panel_width = value as f32),
                )
                .default_value(720.0),
            )
            .description("逻辑像素。下一层面板打开时生效（窗口是每次新建的）。"),
            SettingItem::new(
                "高度",
                SettingField::<f64>::number_input(
                    NumberFieldOptions {
                        min: 240.0,
                        max: 900.0,
                        step: 10.0,
                    },
                    |cx| editing(cx).panel_height as f64,
                    |value, cx| edit(cx, |config| config.panel_height = value as f32),
                )
                .default_value(420.0),
            )
            .description("太矮会把详情区挤成一条缝，预览就失去意义了。"),
            SettingItem::new(
                "显示详情栏",
                SettingField::<bool>::switch(
                    |cx| editing(cx).show_detail,
                    |value, cx| edit(cx, |config| config.show_detail = value),
                )
                .default_value(true),
            )
            .description("关掉后列表占满整个面板宽度，适合只想要快速挑一条的人。"),
            SettingItem::new(
                "预览剪贴板内容",
                SettingField::<bool>::switch(
                    |cx| editing(cx).preview_clipboard,
                    |value, cx| edit(cx, |config| config.preview_clipboard = value),
                )
                .default_value(false),
            )
            .description("默认只显示长度。剪贴板里可能是密码，别让它出现在屏幕分享里。"),
        ])])
}

/// 行为：回传与兜底。
fn behavior_page() -> SettingPage {
    SettingPage::new("行为")
        .groups(vec![SettingGroup::new().title("回传").items(vec![
            SettingItem::new(
                "回传前等待",
                SettingField::<f64>::number_input(
                    NumberFieldOptions {
                        min: 0.0,
                        max: 200.0,
                        step: 1.0,
                    },
                    |cx| editing(cx).post_exec_delay_ms as f64,
                    |value, cx| edit(cx, |config| config.post_exec_delay_ms = value.round() as u64),
                )
                .default_value(12.0),
            )
            .description("面板关闭到 `espanso match exec` 之间的等待（毫秒）。非激活模式下接近 0 也不会漏字。"),
            SettingItem::new(
                "粘贴兜底",
                SettingField::<bool>::switch(
                    |cx| editing(cx).allow_paste_fallback,
                    |value, cx| edit(cx, |config| config.allow_paste_fallback = value),
                )
                .default_value(true),
            )
            .description("espanso 调用失败时，用「安全渲染 + 剪贴板粘贴」兜底。需要辅助功能权限，且会覆盖剪贴板。"),
        ])])
}

/// 关于：版本、路径、几件"打开"的事。
///
/// 这一页全是自定义元素（`SettingItem::render`），没有可重置的值，
/// 所以 `resettable(false)`——不然页头会出现一个点了没反应的「重置」。
fn about_page() -> SettingPage {
    SettingPage::new("关于").resettable(false).groups(vec![
        SettingGroup::new().title("semisnip").items(vec![
            SettingItem::render(|_options, _window, cx| {
                let (fg, muted) = {
                    let theme = cx.theme();
                    (theme.colors.foreground, theme.colors.muted_foreground)
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(fg)
                            .child(SharedString::from(format!(
                                "版本 {} · 常驻菜单栏，不进 Dock",
                                env!("CARGO_PKG_VERSION")
                            ))),
                    )
                    .child(div().text_xs().text_color(muted).child(SharedString::from(
                        "只负责选片段；变量求值与注入全部由 espanso 完成。",
                    )))
                    .into_any_element()
            }),
            SettingItem::render(|_options, _window, cx| paths_block(cx).into_any_element()),
            SettingItem::render(|_options, _window, cx| actions_block(cx).into_any_element()),
        ]),
    ])
}

// ---------------------------------------------------------------------------
// 「关于」页的两个自定义块
// ---------------------------------------------------------------------------

/// 三行路径，等宽字体。路径是排查问题时最先要看的东西，所以直接摊在界面上，
/// 而不是藏进日志。
fn paths_block(cx: &App) -> impl IntoElement {
    let (fg, muted, border) = {
        let theme = cx.theme();
        (
            theme.colors.foreground,
            theme.colors.muted_foreground,
            theme.colors.border,
        )
    };
    let mono = cx.theme().mono_font_family.clone();

    let rows = [
        ("配置", paths::config_dir()),
        ("片段", paths::espanso_match_dir()),
        ("日志", paths::log_path()),
    ];

    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .p_2()
        .children(rows.into_iter().map(|(label, path)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(36.))
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(fg)
                        .truncate()
                        .child(SharedString::from(path.display().to_string())),
                )
        }))
}

/// 一排操作按钮。全部走系统 `open` 或事件通道，不在这里直接改文件。
fn actions_block(cx: &mut App) -> impl IntoElement {
    let muted = cx.theme().colors.muted_foreground;

    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            Button::new("about-config")
                .label("打开配置")
                .on_click(|_, _, _| {
                    crate::ui::reveal(paths::config_path(), paths::config_dir());
                }),
        )
        .child(
            Button::new("about-log")
                .label("打开日志")
                .on_click(|_, _, _| {
                    crate::ui::reveal(paths::log_path(), paths::config_dir());
                }),
        )
        .child(
            Button::new("about-reload")
                .label("重建索引")
                .on_click(|_, _, cx| {
                    if let Some(tx) = event::tx(cx) {
                        let _ = tx.send(Event::Reload);
                    }
                }),
        )
        .child(
            Button::new("about-guide")
                .label("重新看引导")
                .on_click(|_, _, cx| {
                    // 先把标记翻回 false 并落盘，再开窗：这样即使用户
                    // 中途关掉引导窗，下次启动还会再弹一次，不会"漏掉"。
                    edit(cx, |config| config.onboarded = false);
                    if let Err(err) = crate::ui::onboarding::open(cx) {
                        log::error!("打开引导失败：{err}");
                    }
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from("删掉配置目录等于恢复出厂设置")),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        // 默认值必须永远能落盘：设置窗口第一次打开、用户什么都没改，
        // 这时按一下「重置」也会走 commit，不该报错。
        assert_eq!(validate(&Config::default()), Ok(()));
    }

    #[test]
    fn invalid_values_are_rejected_with_a_reason() {
        // 触发键：空、超长、非 ASCII 都不行——它要编译成物理键码
        let empty = Config {
            trigger: String::new(),
            ..Config::default()
        };
        assert!(validate(&empty).unwrap_err().contains("触发键"));

        let long = Config {
            trigger: "abcde".into(),
            ..Config::default()
        };
        assert!(validate(&long).is_err());

        let cjk = Config {
            trigger: "，，".into(),
            ..Config::default()
        };
        assert!(validate(&cjk).unwrap_err().contains("ASCII"));

        // 尺寸与条数：低于下限就写不进去，否则面板会小到看不见
        let tiny = Config {
            panel_width: 200.0,
            ..Config::default()
        };
        assert!(validate(&tiny).unwrap_err().contains("宽度"));

        let short = Config {
            panel_height: 100.0,
            ..Config::default()
        };
        assert!(validate(&short).unwrap_err().contains("高度"));

        let zero = Config {
            max_results: 0,
            ..Config::default()
        };
        assert!(validate(&zero).unwrap_err().contains("条数"));
    }
}
