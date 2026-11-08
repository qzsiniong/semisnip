//! 偏好设置窗口。
//!
//! 用 gpui-component 的 `Settings` 组件做**外壳**（左侧页签 + 搜索 + 分组卡片），
//! 每个字段自己画：一行「标题 + ⓘ + 控件」。
//!
//! 为什么不用组件自带的 `SettingField`：
//!
//! 1. **说明文字要挂在 ⓘ 上**（hover 才显示）。组件的 `SettingItem::description`
//!    只能把文字直接排在标题下面，没有图标/tooltip 的位置，绕不过去。
//! 2. **枚举不能靠组件的下拉**。那个下拉是「按钮 + popover + 延迟绘制菜单」三层套，
//!    在 `Settings` 的虚拟列表里点击不生效（菜单能弹出，点选项没反应）。自绘成
//!    单选行之后，点击就是普通的 `on_click`，不跨延迟层、不依赖 keyed state。
//!
//! 四条约定，改代码时别破坏：
//!
//! 1. **改完立即落盘**，没有"保存"按钮。这些偏好都是单值、随时可改，
//!    多一步确认只会让人忘记按。写完投 [`Event::ConfigChanged`]，
//!    面板与触发状态机当场换值。
//! 2. **校验不过只改内存、不落盘**。输入框是逐字符提交的，用户打到一半的
//!    `,` 不该把磁盘上的 `;;` 覆盖掉，也不该让 `;;` 当场失灵——所以
//!    [`SettingsState::commit`] 先校验、再写、最后才通知。
//! 3. **窗口只有一个**。`GlobalSettings` 里存着句柄，再点一次菜单只是把
//!    已有窗口提上来；句柄失效（用户关掉了）才新建。
//! 4. **字段的读/写只写一份**。每个字段用 [`Field`] 描述（怎么读、怎么写、
//!    默认值），渲染、提交、重置都从它派生——否则「重置」和「保存」迟早
//!    会对同一个字段用上两套规则。

use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::setting::{SettingGroup, SettingItem, SettingPage, Settings};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Context, Entity, Focusable as _, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, WindowBounds, WindowKind,
    WindowOptions, div, prelude::FluentBuilder as _, px, relative, size,
};

use crate::app::event::{self, Event};
use crate::config::{Config, FocusMode, Placeholder, Position};
use crate::paths;

/// 设置窗口宽度。800 是 `Settings` 组件在「侧栏 200 + 两列字段」下刚好
/// 不触发堆叠布局的下限；再窄它会自动改成上下排（组件里的 container query）。
const WINDOW_W: f32 = 820.0;
const WINDOW_H: f32 = 620.0;

/// 说明文字（ⓘ 的 tooltip）的折行宽度。
///
/// 说明都是整句话，最长的「占位字符」有两百多字。不限宽的话 tooltip 会被拉成
/// 一条横穿屏幕的长条——既读不下去，也会顶到屏幕边缘。380 大约每行 24 个汉字，
/// 是中文长段落读起来比较顺的一档。
const HELP_WIDTH: f32 = 380.0;

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
/// 两处 `cx.activate(true)`（新建分支的结尾、已有窗口分支的开头）不是可选项：
/// daemon 是 `Accessory` 应用（不进 Dock），macOS 不会因为它有窗口就把整个 App
/// 切到前台——只调 `activate_window()`（底层是 `makeKeyAndOrderFront:`）时，
/// 窗口在后台 App 的窗口堆里被立成 key，用户看到的却是"又点了一次，什么都没发生"。
pub fn open(cx: &mut App) -> anyhow::Result<()> {
    // 已有窗口：先激活 App，再把这个窗口立起来。
    // 句柄失效（用户已经关掉窗口）时 `update` 返回 Err，继续往下新建。
    if let Some(entity) = cx.try_global::<GlobalSettings>().map(|g| g.0.clone()) {
        if let Some(handle) = entity.read(cx).handle {
            if handle
                .update(cx, |_, window, cx| {
                    // 先 App 后窗口：`activate_window` 内部是投递到前台队列的
                    // 异步任务（`makeKeyAndOrderFront:`），所以这一句先执行完，
                    // 等到真正立窗口时 App 已经在前台了。
                    cx.activate(true);
                    // 顺带把最小化的窗口还原——`makeKeyAndOrderFront:` 对
                    // 迷你化窗口会顺带 deminiaturize，不需要单独调。
                    window.activate_window();
                })
                .is_ok()
            {
                log::info!("设置窗口已存在，重新激活");
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

        // 页面要按当前配置决定「哪些行出现」（比如「自定义字符」只在占位符选
        // 「自定义」时才有），所以这里取一份快照传进去——闭包里再读全局也行，
        // 但一帧里读两次就可能拿到不同版本。
        let config = self.config.clone();

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
                        .page(general_page(&config))
                        .page(appearance_page(&config))
                        .page(behavior_page(&config))
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
    // 只有真正要用它时才拦：其它模式下这个值躺着不生效，没必要因此挡住保存。
    if config.placeholder == Placeholder::Custom {
        crate::config::validate_placeholder_char(&config.placeholder_char)?;
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
// 字段：读 / 写 / 默认值
// ---------------------------------------------------------------------------

/// 「怎么从配置里读一个值」。
type Getter<T> = Rc<dyn Fn(&Config) -> T>;

/// 「怎么把一个值写回配置」。
type Setter<T> = Rc<dyn Fn(&mut Config, T)>;

/// 「把某个值提交成一次改动」——输入框每次 Change、单选每次点击都调它。
///
/// `pick`（单选）和 `commit`（文本/数字）签名完全一样，共用这个别名；
/// 单独起名只是为了让调用处的语义清楚。
type Commit<T> = Rc<dyn Fn(T, &mut App)>;

/// 一个偏好字段的三件事：怎么读、怎么写、默认值是多少。
///
/// 三个用途都从这一份定义派生：渲染时读、控件回调里写、"重置"按钮判断
/// 脏没脏并写回默认值。分开写三套闭包的话，迟早有一个字段的重置值和
/// 保存值对不上。
struct Field<T> {
    get: Getter<T>,
    set: Setter<T>,
    default: T,
}

impl<T: Clone> Clone for Field<T> {
    fn clone(&self) -> Self {
        Self {
            get: self.get.clone(),
            set: self.set.clone(),
            default: self.default.clone(),
        }
    }
}

impl<T: 'static> Field<T> {
    fn new(
        get: impl Fn(&Config) -> T + 'static,
        set: impl Fn(&mut Config, T) + 'static,
        default: T,
    ) -> Self {
        Self {
            get: Rc::new(get),
            set: Rc::new(set),
            default,
        }
    }

    /// 把「读字段」包成一次配置改动。
    ///
    /// 每次调用都克隆一次值：闭包本身是 `Fn`（每帧可能被调用多次），
    /// 直接 move 进去会编译不过。
    fn commit(&self, value: T) -> Rc<dyn Fn(&mut App)>
    where
        T: Clone,
    {
        let set = self.set.clone();
        Rc::new(move |cx: &mut App| {
            let set = set.clone();
            let value = value.clone();
            edit(cx, move |config| set(config, value));
        })
    }
}

/// 让「重置」按钮判断这个字段脏没脏。
///
/// 收**值**而不是 `&Field<T>`：edition 2024 的 `impl Trait` 会把入参的
/// 生命周期一并捕获进返回类型，`&Field<T>` 会连带把「借了调用栈上的变量」
/// 写进返回类型，即使加了 `+ 'static` 也改不掉（`on_reset` 要的是真
/// `'static`，编译期会报 `field` 活不够久）。参数里没有任何引用，
/// 返回类型就只捕获 `T` 这个 `'static` 类型参数，彻底和调用栈脱钩。
fn dirty<T: PartialEq + Clone + 'static>(field: Field<T>) -> impl Fn(&App) -> bool + 'static {
    let get = field.get;
    let default = field.default;
    move |cx: &App| get(&editing(cx)) != default
}

/// 让「重置」按钮写回默认值。收值而非引用，理由同 [`dirty`]。
fn reset_to_default<T: Clone + 'static>(
    field: Field<T>,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let set = field.set;
    let default = field.default;
    move |_, cx: &mut App| {
        let set = set.clone();
        let default = default.clone();
        edit(cx, move |config| set(config, default));
    }
}

// ---------------------------------------------------------------------------
// 行：标题 + ⓘ + 控件
// ---------------------------------------------------------------------------

/// 标题 + 悬浮说明图标。
///
/// 说明文字**不直接显示**：它挂在 ⓘ 的 tooltip 上，鼠标悬停才出现。这样一屏
/// 能放下更多字段，也避免了「每行下面挂一段小字」那种半读不读的噪声。
///
/// tooltip 用 `Tooltip::element` 自绘内容而不是 `Tooltip::new(text)`：后者是
/// 一整行不折行的文本，中文说明动辄两三百字，弹出来会横穿屏幕。自绘一个
/// `max_w(HELP_WIDTH)` 的容器，文字就会在容器里正常折行（gpui 的文本默认
/// `whitespace_normal`，只要给了宽度上限就会自己换行）。
fn title_block(cx: &App, title: &'static str, help: &'static str) -> AnyElement {
    let muted = cx.theme().colors.muted_foreground;
    div()
        .flex()
        .flex_row()
        .gap_1()
        .items_center()
        .child(div().text_sm().child(title))
        .when(!help.is_empty(), |this| {
            this.child(
                // tooltip 需要元素有 id（gpui 用它挂 hover 状态）；用标题当键，
                // 同一个窗口里标题不重复。
                div()
                    .id(SharedString::from(format!("help-{title}")))
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Info))
                    .tooltip(move |window, cx| {
                        Tooltip::element(move |_, _| {
                            div()
                                .max_w(px(HELP_WIDTH))
                                // 折行之后行距要比默认松一点，否则多行中文会糊成一块。
                                .line_height(relative(1.5))
                                .child(help)
                        })
                        .build(window, cx)
                    }),
            )
        })
        .into_any_element()
}

/// 常规一行：左边标题，右边控件。
fn plain_row(cx: &App, title: &'static str, help: &'static str, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .w_full()
        .gap_3()
        .items_center()
        .justify_between()
        .child(div().flex_1().min_w_0().child(title_block(cx, title, help)))
        .child(control)
        .into_any_element()
}

/// 单选一行：标题在上，选项竖排在下。
///
/// 用竖排而不是「标题 + 右侧下拉」有三个原因：选项本身就要解释（空格 / 零宽 /
/// 触发串 / 自定义 / 不补 各是什么），横着塞不下；这几种选项值得看全，藏进
/// 下拉反而没人知道还有别的；组件自带的下拉在 `Settings` 的虚拟列表里点不动
/// （见模块头第 2 条），换成 `Radio` 之后点击就是普通 `on_click`。
fn choice_row<T: Copy + PartialEq + 'static>(
    cx: &App,
    title: &'static str,
    help: &'static str,
    options: &[(&'static str, T)],
    current: T,
    pick: Commit<T>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_2()
        .child(title_block(cx, title, help))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .children(options.iter().enumerate().map(|(ix, (label, value))| {
                    let pick = pick.clone();
                    let value = *value;
                    Radio::new((title, ix))
                        .label((*label).to_string())
                        // 受控组件：值由配置决定，点一下只负责把新值交出去。
                        // 回调只在「本来没选中」时触发，所以不必自己去重。
                        .checked(current == value)
                        .on_click(move |_checked, _, cx| pick(value, cx))
                })),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// 输入控件
// ---------------------------------------------------------------------------

/// 文本输入框的常驻状态。
///
/// `InputState` 必须活得比一帧长（光标、选区、输入法都在里面），所以存进
/// `window.use_keyed_state`。`_subscription` 一起放着：订阅是回调的生命线，
/// 掉了就再也收不到 Change。
struct TextFieldState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// 文本输入框。
///
/// 语义与组件自带的输入字段一致：**逐字符提交**（打到一半的 `,` 也让 daemon
/// 知道，校验不过只是不落盘）。文本在失焦后与配置对齐——用户打出非法触发串
/// 再点别处时，框里会回到生效值，同时底部状态行给出原因。
fn text_input(
    key: &'static str,
    value: SharedString,
    commit: Commit<String>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let state = window.use_keyed_state(SharedString::from(format!("text-field-{key}")), cx, {
        let initial = value.clone();
        move |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
            let subscription = cx.subscribe(
                &input,
                move |_: &mut TextFieldState, input: Entity<InputState>, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        commit(input.read(cx).value().to_string(), cx);
                    }
                },
            );
            TextFieldState {
                input,
                _subscription: subscription,
            }
        }
    });

    // 外部改动（重置、终端里 `config set` 之后重开窗口）要把文字同步回来。
    // 只在**没有焦点**时同步：正在打字的人不该被自己的每次输入打断。
    state.update(cx, |field, cx| {
        let focused = field.input.read(cx).focus_handle(cx).is_focused(window);
        if !focused && field.input.read(cx).value() != value {
            field
                .input
                .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
        }
    });

    let input = state.read(cx).input.clone();
    Input::new(&input).w_64().into_any_element()
}

/// 数字输入框的状态（与 [`TextFieldState`] 同构，独立成类型是为了让两个
/// keyed state 的类型不互相串）。
struct NumberFieldState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// 数字输入框。
///
/// 与文本输入的两点差别，都是为了「打字时别跟用户抢」：
/// - **只提交能解析且在范围内的值**。所以打字过程中经过的 `3`（`360` 的头一位）
///   不会先把宽度改成 3 再被校验拦下来刷一行红字；数字框旁边也没有 +/- 按钮，
///   免得点一下就把正在打的字覆盖掉。
/// - **失焦后**把框里的文字拉回生效值，越界的输入会自己消失。
fn number_input(
    key: &'static str,
    value: f64,
    min: f64,
    max: f64,
    commit: Commit<f64>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let state = window.use_keyed_state(SharedString::from(format!("number-field-{key}")), cx, {
        let initial = format_number(value);
        move |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
            let subscription = cx.subscribe(
                &input,
                move |_: &mut NumberFieldState, input: Entity<InputState>, event, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let Ok(parsed) = input.read(cx).value().trim().parse::<f64>() else {
                        return;
                    };
                    // 超范围就夹住：输入 5000 的宽度等于「我要最宽」，
                    // 而不是「拒绝这次输入、留着上一版」。
                    commit(parsed.clamp(min, max), cx);
                },
            );
            NumberFieldState {
                input,
                _subscription: subscription,
            }
        }
    });

    let expected = format_number(value);
    state.update(cx, |field, cx| {
        let focused = field.input.read(cx).focus_handle(cx).is_focused(window);
        if !focused && field.input.read(cx).value() != expected {
            field.input.update(cx, |input, cx| {
                input.set_value(expected.clone(), window, cx)
            });
        }
    });

    let input = state.read(cx).input.clone();
    Input::new(&input).w_20().into_any_element()
}

/// 数字写进输入框时的样子：整数不带 `.0`，否则 720 显示成 720.0 很碍眼。
fn format_number(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

// ---------------------------------------------------------------------------
// 三种 SettingItem 生成器
// ---------------------------------------------------------------------------

/// 文本字段（触发键、自定义占位字符）。
///
/// 四个生成器共用一个骨架：先把 `Field` 克隆两份（一份给渲染闭包、一份留给
/// `on_reset`），再用 `into_any_element()` 把输入框变成"不藏借用"的元素，
/// 最后才借 `&App` 画整行。顺序不能反——`&mut App` 与 `&App` 在同一句里
/// 同时有效是编译不过的。
fn text_item(
    key: &'static str,
    title: &'static str,
    help: &'static str,
    field: Field<String>,
) -> SettingItem {
    let render_field = field.clone();
    SettingItem::render(move |_options, window, cx| -> AnyElement {
        let value: SharedString = (render_field.get)(&editing(cx)).into();
        let commit = {
            let field = render_field.clone();
            Rc::new(move |value: String, cx: &mut App| (field.commit(value))(cx))
        };
        let input = text_input(key, value, commit, window, cx);
        plain_row(cx, title, help, input)
    })
    .keywords([title, help])
    .on_reset(dirty(field.clone()), reset_to_default(field))
}

/// 数字字段（间隔、条数、尺寸、延时）。
fn number_item(
    key: &'static str,
    title: &'static str,
    help: &'static str,
    field: Field<f64>,
    min: f64,
    max: f64,
) -> SettingItem {
    let render_field = field.clone();
    SettingItem::render(move |_options, window, cx| -> AnyElement {
        let value = (render_field.get)(&editing(cx));
        let commit = {
            let field = render_field.clone();
            Rc::new(move |value: f64, cx: &mut App| (field.commit(value))(cx))
        };
        let input = number_input(key, value, min, max, commit, window, cx);
        plain_row(cx, title, help, input)
    })
    .keywords([title, help])
    .on_reset(dirty(field.clone()), reset_to_default(field))
}

/// 开关字段。
///
/// `Switch` 也是受控组件：我们自己把值写进配置，它下一帧按配置重画。
fn switch_item(title: &'static str, help: &'static str, field: Field<bool>) -> SettingItem {
    let render_field = field.clone();
    SettingItem::render(move |_options, _window, cx| -> AnyElement {
        let checked = (render_field.get)(&editing(cx));
        let switch = Switch::new(SharedString::from(format!("switch-{title}")))
            .checked(checked)
            .on_click({
                let field = render_field.clone();
                move |checked, _, cx| (field.commit(*checked))(cx)
            });
        plain_row(cx, title, help, switch.into_any_element())
    })
    .keywords([title, help])
    .on_reset(dirty(field.clone()), reset_to_default(field))
}

/// 单选字段。
fn choice_item<T: Copy + PartialEq + 'static>(
    title: &'static str,
    help: &'static str,
    field: Field<T>,
    options: Vec<(&'static str, T)>,
) -> SettingItem {
    let render_field = field.clone();
    SettingItem::render(move |_options, _window, cx| -> AnyElement {
        let current = (render_field.get)(&editing(cx));
        let pick: Commit<T> = {
            let field = render_field.clone();
            Rc::new(move |value: T, cx: &mut App| (field.commit(value))(cx))
        };
        choice_row(cx, title, help, &options, current, pick)
    })
    .keywords([title, help])
    .on_reset(dirty(field.clone()), reset_to_default(field))
}

// ---------------------------------------------------------------------------
// 页面
// ---------------------------------------------------------------------------

/// 通用：触发方式与结果数量。
///
/// 这一页没有条件显示的字段，所以不吃配置快照；签名保留 `&Config` 是为了
/// 与另外两页一致，将来加条件字段时不用改调用处。
fn general_page(_config: &Config) -> SettingPage {
    SettingPage::new("通用").default_open(true).groups(vec![
        SettingGroup::new().title("触发").items(vec![
            text_item(
                "trigger",
                "触发键",
                "只支持 ASCII、最多 4 个字符——它会被编译成物理键码序列。\
                     改完要跑一次 `semisnip install`，否则 espanso 的降级定义还是旧键。",
                Field::new(|c| c.trigger.clone(), |c, v| c.trigger = v, ";;".into()),
            ),
            number_item(
                "trigger_timeout_ms",
                "触发键间隔",
                "两个触发键之间最多隔多久（毫秒）。超过就当作普通输入。",
                Field::new(
                    |c| c.trigger_timeout_ms as f64,
                    |c, v| c.trigger_timeout_ms = v.round() as u64,
                    800.0,
                ),
                200.0,
                3000.0,
            ),
        ]),
        SettingGroup::new().title("结果").items(vec![
            number_item(
                "max_results",
                "候选条数",
                "面板最多列几条。改大到超过屏幕高度没有意义，列表不会滚动。",
                Field::new(
                    |c| c.max_results as f64,
                    |c, v| c.max_results = v.round() as usize,
                    9.0,
                ),
                1.0,
                30.0,
            ),
            choice_item(
                "焦点模式",
                "非激活：目标 App 全程不失焦，回车不需要还原前台。激活分支尚未实现。",
                Field::new(
                    |c| c.focus_mode,
                    |c, v| c.focus_mode = v,
                    FocusMode::Nonactivating,
                ),
                vec![
                    ("非激活（推荐）", FocusMode::Nonactivating),
                    ("激活", FocusMode::Activating),
                ],
            ),
        ]),
    ])
}

/// 外观：面板位置、尺寸与详情栏。
fn appearance_page(_config: &Config) -> SettingPage {
    SettingPage::new("外观").groups(vec![SettingGroup::new().title("面板").items(vec![
        choice_item(
            "位置",
            "自动：贴着输入光标弹出（下方放不下就翻到上方），水平居中于光标所在屏。\
                 居中：屏幕可视区正中。两种都跟随光标 / 当前窗口所在的那块屏。",
            Field::new(|c| c.position, |c, v| c.position = v, Position::Auto),
            vec![
                ("自动（贴着光标）", Position::Auto),
                ("屏幕居中", Position::Center),
            ],
        ),
        number_item(
            "panel_width",
            "宽度",
            "逻辑像素。下一层面板打开时生效（窗口是每次新建的）。",
            Field::new(
                |c| c.panel_width as f64,
                |c, v| c.panel_width = v as f32,
                720.0,
            ),
            360.0,
            1400.0,
        ),
        number_item(
            "panel_height",
            "高度",
            "太矮会把详情区挤成一条缝，预览就失去意义了。",
            Field::new(
                |c| c.panel_height as f64,
                |c, v| c.panel_height = v as f32,
                420.0,
            ),
            240.0,
            900.0,
        ),
        switch_item(
            "显示详情栏",
            "关掉后列表占满整个面板宽度，适合只想要快速挑一条的人。",
            Field::new(|c| c.show_detail, |c, v| c.show_detail = v, true),
        ),
        switch_item(
            "预览剪贴板内容",
            "默认只显示长度。剪贴板里可能是密码，别让它出现在屏幕分享里。",
            Field::new(
                |c| c.preview_clipboard,
                |c, v| c.preview_clipboard = v,
                false,
            ),
        ),
    ])])
}

/// 「行为」页要不要显示「自定义字符」那一行。
///
/// 单拎出来是为了能单测：`SettingPage` 的 `groups` 是私有字段，测试里没法
/// 数「这一页有几行」，只能把"什么时候多一行"这个判断本身钉住。
fn shows_placeholder_char(config: &Config) -> bool {
    config.placeholder == Placeholder::Custom
}

/// 行为：回传与兜底。
fn behavior_page(config: &Config) -> SettingPage {
    let mut placeholder_items = vec![choice_item(
        "占位字符",
        "espanso 每次展开都会按片段触发串的长度回删字符（没有关闭入口），\
         所以确认前必须先打出同样多的「可删字符」。空格最不起眼，但少数编辑器会把它\
         tab 化、或在保存时 trim 掉行尾空白；零宽字符（U+200B）不会被改写，代价是个别\
         App 可能过滤它。选「片段触发串」时 semisnip 不再调 `espanso match exec`，而是把\
         触发串直接打出去、让 espanso 自己命中——此时成败不在 semisnip 手里，espanso\
         没运行的话触发串会留在文档里。「不补」只在 espanso 将来支持关闭补偿后才安全。",
        Field::new(
            |c| c.placeholder,
            |c, v| c.placeholder = v,
            Placeholder::Space,
        ),
        vec![
            ("空格（默认）", Placeholder::Space),
            ("零宽字符 U+200B", Placeholder::ZeroWidth),
            ("片段触发串", Placeholder::Trigger),
            ("自定义字符", Placeholder::Custom),
            ("不补（危险）", Placeholder::None),
        ],
    )];

    // 只有选了「自定义」才有必要露出这一格：其它时候它躺着不生效，
    // 摆在那儿只会让人以为自己配错了。
    if shows_placeholder_char(config) {
        placeholder_items.push(text_item(
            "placeholder_char",
            "自定义字符",
            "必须恰好一个字符，且不能是换行 / 制表符（会触发自动缩进，退格数就对不上）。",
            Field::new(
                |c| c.placeholder_char.clone(),
                |c, v| c.placeholder_char = v,
                "·".into(),
            ),
        ));
    }

    SettingPage::new("行为").groups(vec![
        SettingGroup::new().title("回传").items(vec![
            number_item(
                "post_exec_delay_ms",
                "回传前等待",
                "面板关闭到 `espanso match exec` 之间的等待（毫秒）。非激活模式下接近 0 也不会漏字。",
                Field::new(
                    |c| c.post_exec_delay_ms as f64,
                    |c, v| c.post_exec_delay_ms = v.round() as u64,
                    12.0,
                ),
                0.0,
                200.0,
            ),
            switch_item(
                "粘贴兜底",
                "espanso 调用失败时，用「安全渲染 + 剪贴板粘贴」兜底。需要辅助功能权限，且会覆盖剪贴板。",
                Field::new(
                    |c| c.allow_paste_fallback,
                    |c, v| c.allow_paste_fallback = v,
                    true,
                ),
            ),
        ]),
        SettingGroup::new().title("占位符").items(placeholder_items),
    ])
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

    #[test]
    fn number_format_drops_the_trailing_zero() {
        // 输入框里显示 720 而不是 720.0：这些值全是整数语义，多一个 `.0`
        // 只会让人怀疑自己填错了类型。
        assert_eq!(format_number(720.0), "720");
        assert_eq!(format_number(12.0), "12");
        assert_eq!(format_number(12.5), "12.5");
    }

    #[test]
    fn custom_char_row_only_shows_up_for_custom_placeholder() {
        // 「自定义字符」只在选「自定义」时出现：它躺在那儿不生效时
        // 最容易被当成"我配错了"。五个模式里只有 Custom 该多出这一行。
        let shows = |placeholder| {
            shows_placeholder_char(&Config {
                placeholder,
                ..Config::default()
            })
        };

        assert!(shows(Placeholder::Custom));
        assert!(!shows(Placeholder::Space));
        assert!(!shows(Placeholder::ZeroWidth));
        assert!(!shows(Placeholder::Trigger));
        assert!(!shows(Placeholder::None));
    }
}
