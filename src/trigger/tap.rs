//! 全局键盘接管（macOS CGEventTap）。
//!
//! 用 `cgSessionEventTap` + `HeadInsert` + 可修改模式：能**吞键**，
//! 因而面板打开期间目标 App 一个字符都收不到，也就不会出现
//! "过滤串漏进文档"的竞态。
//!
//! 同一个 tap 也订阅鼠标按下，只为一件事：**点面板外面就收起面板**。
//! 放在一起而不是另开 `NSEvent` 全局监听，是因为两者要同一份"谁在前台"的事实，
//! 而且 tap 已经有辅助功能授权，不需要再申请一种权限。
//!
//! 四处必须小心：
//! 1. 自己合成的事件（回删 / 粘贴 / 占位空格）会被自己的 tap 看到 → 用 source pid
//!    过滤（合成事件不一定带得到 pid，但它们的键码也凑不齐触发串，见
//!    `handoff::confirm`）；
//! 2. 外部进程（espanso / 其他注入工具）合成的事件**一律放行**，即使面板开着，
//!    也**不喂触发状态机**——它们可能是某轮尚未结束的展开（补偿退格 + 展开文本），
//!    吞了就是"文档留半截触发词"或"展开结果缺一截"，喂进状态机则可能被展开
//!    文本里的字符误触发（见 [`route_panel_key`]）；
//! 3. tap 被系统超时摘除后要立刻重新启用，否则用户键盘"半瘫"；
//! 4. 带 Cmd/Ctrl 的组合键一律放行并关面板，绝不能把系统快捷键吞掉；
//! 5. 鼠标事件一律**放行**——我们只借它判断"点在哪"，绝不抢用户的点击。
//!
//! 另外有一条"不反应"的路径：配置里 `enabled` 为假（菜单栏「暂停触发」或设置
//! 窗口里的开关）时，回调在喂状态机之前直接放行。tap 仍然装着，只是不识别触发串
//! ——恢复不需要重启进程，权限与鼠标监听也不受影响。
//!
//! 触发命中时也**不吞键**：触发串的所有字符都落进目标文档（见
//! `trigger::machine`）。tap 对文档的全部写入只剩确认路径的"回删 + 占位空格"，
//! 取消路径是纯关闭——文档始终是诚实的。
//!
//! 注意：这里**不**拦截 espanso 给 `match exec` 发的补偿退格（占位空格方案，
//! 见 `handoff::confirm`）。早期版本用"时间窗口 + 数量"的预算去猜哪串退格是
//! 自己那轮的，两种触发方式交替使用时会互相误伤，已废弃。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::app::event::{Event, KeyMsg};
use crate::trigger::direct::DirectBuffer;
use crate::trigger::keymap;
use crate::trigger::machine::{TriggerAction, TriggerConfig, TriggerMachine};

/// 面板窗口在屏幕上的位置与大小（点，原点在屏幕左上角）。
///
/// 只在开窗时写一次：面板不可拖动、不可缩放，位置在整个生命周期里不变。
/// `width/height` 为 0 表示"还没量到"，此时鼠标判定一律跳过（宁可不关面板，
/// 也不要因为坐标缺失变成"点哪都关"）。
#[derive(Clone, Copy, Default)]
struct PanelRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl PanelRect {
    /// 点是否落在面板窗口里。矩形为退化值时返回 `true`——把"不知道"当成"在里面"，
    /// 这样缺坐标时最坏是退化成旧行为（点外面不关），而不是误关。
    fn contains(&self, x: f64, y: f64) -> bool {
        if self.width <= 0.0 || self.height <= 0.0 {
            return true;
        }
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

/// tap 线程与外部（UI 线程、回传线程）共享的全部状态。
///
/// 每个字段的原子性选择都跟着访问频率走：`panel_open` 在每一次按键上都会被读，
/// 用 `AtomicBool`；状态机只在按键时短暂加锁（`Mutex`），因为它的更新必须原子，
/// 而且锁内没有 IO。
pub struct TapShared {
    /// 面板是否打开。**只有为真时 tap 才吞键**，否则用户正常打字会被吃掉。
    panel_open: AtomicBool,
    /// 是否接管触发串（配置里的 `enabled`，菜单栏可以暂停）。
    ///
    /// 为假时回调**不喂状态机**：`;;` 原样落进文档，面板不会弹出来。tap 本身
    /// 仍然装着——暂停只是"不反应"，不是"卸载"，这样恢复时不需要重启进程。
    enabled: AtomicBool,
    /// 触发状态机。放在锁里是因为 `feed` 需要"读前缀 + 前进 + 可能复位"原子完成。
    machine: Mutex<TriggerMachine>,
    /// 事件发送端。tap 线程不直接改 UI 状态，只投事件。
    tx: Sender<Event>,
    /// 放行下一个物理 keyUp。触发命中的那个键是**放行**的（字符要落进文档），
    /// 它的 keyUp 到达时面板已经打开——没有这个标志就会被面板分支吞掉，
    /// 目标 App 会看到"按下没抬起"。
    pass_next_key_up: AtomicBool,
    /// 面板窗口的屏幕矩形。鼠标按下时用来区分"点面板里"和"点面板外"。
    panel_rect: Mutex<PanelRect>,
    /// 已知的直达触发词（带 `,,` 前缀），由 UI 线程在索引变化后整体替换。
    /// 读多写少，用 `ArcSwap` 避免每按一个键都抢锁。
    direct_triggers: ArcSwap<Vec<String>>,
    /// 最近输入的字符缓冲，只在面板关闭时喂。
    direct_buffer: Mutex<DirectBuffer>,
    /// 同一个触发词的冷却，防止长按/连续输入把统计刷爆。
    direct_last: Mutex<Option<(String, Instant)>>,
}

impl TapShared {
    fn new(config: TriggerConfig, tx: Sender<Event>, enabled: bool) -> Self {
        Self {
            panel_open: AtomicBool::new(false),
            enabled: AtomicBool::new(enabled),
            machine: Mutex::new(TriggerMachine::new(config)),
            tx,
            pass_next_key_up: AtomicBool::new(false),
            panel_rect: Mutex::new(PanelRect::default()),
            direct_triggers: ArcSwap::from_pointee(Vec::new()),
            direct_buffer: Mutex::new(DirectBuffer::default()),
            direct_last: Mutex::new(None),
        }
    }

    /// 把一个可打印字符喂进直达缓冲，命中时返回触发词。
    ///
    /// 面板打开时不会走到这里；密码框/安全输入下 tap 收不到事件，统计自然缺失。
    fn observe_direct(&self, ch: char) -> Option<String> {
        if ch.is_control() {
            return None;
        }
        let mut buffer = self.direct_buffer.lock().ok()?;
        buffer.push(ch);
        let triggers = self.direct_triggers.load();
        let hit = buffer.detect(&triggers)?;

        let now = Instant::now();
        if let Ok(mut last) = self.direct_last.lock() {
            if let Some((previous, at)) = last.as_ref() {
                if previous == &hit && now.saturating_duration_since(*at) < Duration::from_secs(1) {
                    return None;
                }
            }
            *last = Some((hit.clone(), now));
        }
        Some(hit)
    }

    /// 放行下一个物理 keyUp（触发命中时调），返回是否确实有预约。
    fn take_pass_key_up(&self) -> bool {
        self.pass_next_key_up.swap(false, Ordering::SeqCst)
    }

    /// 记下面板窗口的位置（开窗时调一次）。
    fn set_panel_rect(&self, rect: PanelRect) {
        if let Ok(mut slot) = self.panel_rect.lock() {
            *slot = rect;
        }
    }

    /// 这个点是不是落在面板窗口里。拿不到锁时按"在里面"处理——见
    /// [`PanelRect::contains`] 的退化约定。
    fn panel_contains(&self, x: f64, y: f64) -> bool {
        self.panel_rect
            .lock()
            .map(|rect| rect.contains(x, y))
            .unwrap_or(true)
    }

    /// 运行期换触发键（设置窗口改完之后调）。
    ///
    /// 直接换掉整个状态机而不是改 `keycodes`：旧序列的前缀在新配置下没有意义，
    /// 换新的等于顺便 reset。
    pub fn set_trigger(&self, config: TriggerConfig) {
        if let Ok(mut machine) = self.machine.lock() {
            *machine = TriggerMachine::new(config);
        }
    }

    /// 运行期暂停 / 恢复触发（菜单栏与设置窗口都走这里）。
    ///
    /// 暂停时多做两件事，都不只是为了"干净"：
    /// - **放开 `panel_open`**：暂停的语义是"不再接管键盘"，面板要是还能收按键，
    ///   用户会以为开关没生效；
    /// - **复位状态机**：暂停前打了半个 `;;`，恢复后不该接上那半截。
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.panel_open.store(false, Ordering::SeqCst);
            if let Ok(mut machine) = self.machine.lock() {
                machine.reset();
            }
        }
    }

    /// 此刻是否接管触发串。
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
}

static SHARED: OnceLock<Arc<TapShared>> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);

/// 面板是否处于接管状态：只有它为 true 时 tap 才吞键。
///
/// 由 `app::scheduler` 在开合面板时调用。放在全局函数里是因为 tap 回调运行在
/// 独立的 run loop 线程上，拿不到 `AppState`——它只能读这几个原子量。
pub fn set_panel_open(open: bool) {
    if let Some(shared) = SHARED.get() {
        shared.panel_open.store(open, Ordering::SeqCst);
    }
}

/// 记下面板窗口在屏幕上的矩形（单位：点，**全局坐标**、原点在主屏左上角）。
///
/// 由 `ui::window::open` 在窗口建好之后调用。**必须在打开时写**，因为鼠标按下
/// 到达 tap 时只认这几个数：面板不可拖动，所以整个生命周期量一次就够。
///
/// 注意换算：gpui 的 `Window::bounds()` 给的是**选定那块屏的本地**坐标，而
/// `CGEventGetLocation` 是全局坐标。面板现在可能出现在任意一块屏上（跟随插入
/// 点），所以调用方会先把屏幕原点加回来再交进来——多屏时不加，判定就会整体偏
/// 出去一块屏，表现为"点外面不关"或"点里面也关"。
///
/// 传 0 宽高表示「还没量到」，此时鼠标判定直接跳过（见 [`PanelRect::contains`]）。
pub fn set_panel_bounds(x: f64, y: f64, width: f64, height: f64) {
    if let Some(shared) = SHARED.get() {
        shared.set_panel_rect(PanelRect {
            x,
            y,
            width,
            height,
        });
    }
}

/// tap 是否已装上（缺权限时为 false，此时走 espanso shell 降级路径）。
pub fn ready() -> bool {
    READY.load(Ordering::SeqCst)
}

/// 主动把「已就绪」撤回：授权被撤销、或 tap 被系统禁用时由看门狗调用。
///
/// `READY` 过去是单向的（装成功就永远 true），所以用户在系统设置里取消勾选
/// 之后，面板仍会画候选列表——按键却一个都收不到。这个函数补上回退那半程：
/// 同步收回 `panel_open`、复位状态机，并投一条 [`Event::TapUnavailable`]
/// 让面板切到授权页。只在确实从 true 变 false 时动作（幂等）。
pub fn mark_unavailable(reason: &str) {
    if !READY.swap(false, Ordering::SeqCst) {
        return;
    }
    log::warn!("{reason}");
    if let Some(shared) = SHARED.get() {
        shared.panel_open.store(false, Ordering::SeqCst);
        if let Ok(mut machine) = shared.machine.lock() {
            machine.reset();
        }
        let _ = shared.tx.send(Event::TapUnavailable(reason.to_string()));
    }
}

/// [`retry`] 的结果。返回值而不是 `bool`，因为面板上的「重新检查」要把
/// 三种失败（没授权 / 没初始化 / 线程起不来）分开告诉用户，不能再静默。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// 已经就绪，不必再装。
    AlreadyReady,
    /// 调试开关 `SEMISNIP_NO_TAP` 强制降级。
    Disabled,
    /// `install` 还没跑过，没有可复用的 shared。
    NotInstalled,
    /// 系统里还没勾选 Semisnip。
    NotTrusted,
    /// 已授权，tap 线程正在重装（几秒内 `ready()` 会变 true）。
    Spawned,
    /// 线程创建失败（极罕见，通常是资源耗尽）。
    SpawnFailed,
}

/// 运行期换触发键。
///
/// 设置窗口改完 `trigger` 之后调它，不必重启 daemon。**别改成再调一次
/// [`install`]**：`SHARED` 是 `OnceLock`，第二次安装只会打一行 warn 然后
/// 返回，看着没报错，实际上状态机还是旧的。
pub fn set_trigger(config: TriggerConfig) {
    if let Some(shared) = SHARED.get() {
        shared.set_trigger(config);
    }
}

/// 运行期暂停 / 恢复触发串接管（配置里 `enabled` 变了之后调）。
///
/// 与 [`set_trigger`] 一样是"改现有 shared"，不重装 tap：`SHARED` 是
/// `OnceLock`，重装只会打一行 warn 然后返回。
pub fn set_enabled(enabled: bool) {
    if let Some(shared) = SHARED.get() {
        shared.set_enabled(enabled);
    }
}

/// 更新“已知直达触发词”列表。UI 线程在索引或本地库变化后调用。
///
/// 只影响统计：真正的展开仍由 espanso 完成，semisnip 一个字符都不碰。
pub fn set_direct_triggers(triggers: Vec<String>) {
    if let Some(shared) = SHARED.get() {
        shared.direct_triggers.store(Arc::new(triggers));
    }
}

/// 面板打开时，一个按键事件该不该被当成「面板输入」。
///
/// 规则只有一条且必须记住：**面板只吃物理按键**（source pid ≤ 0 的 HID 事件）。
/// 外部进程合成的事件——最典型的是 espanso 某一轮尚未结束的展开（补偿退格 +
/// 展开文本），也可能是输入法提交、剪贴板工具注入——即使在面板打开期间也要
/// 原样放行：
///
/// - 吞掉**退格**：那一轮展开要删的触发词删不掉，文档里留下半截 `:shel`；
/// - 吞掉**文本**：展开结果缺一截，或者整段被当成过滤词塞进面板。
///
/// 两种触发方式（espanso 原生 `:shell` 与面板确认）交替使用时，前后两轮的
/// 注入在时间上很容易重叠（shell 变量一跑几百毫秒），所以"面板开着 = 键都是
/// 打给面板的"这个假设必然误伤。KEY_DOWN 与 KEY_UP 共用本判定，否则会出现
/// "放行了 keyDown 却吞了 keyUp"的卡键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRoute {
    /// 交给面板（物理按键且面板打开）。
    Panel,
    /// 原样放行给系统。
    Pass,
}

fn route_panel_key(panel_open: bool, source_pid: i64) -> KeyRoute {
    if panel_open && source_pid <= 0 {
        KeyRoute::Panel
    } else {
        KeyRoute::Pass
    }
}

/// 是否已获得辅助功能授权（决定走 tap 还是 espanso shell）。
///
/// 这是**当前进程**的授权状态。从终端跑 `doctor` 会继承终端的授权，
/// 所以它不能代表 daemon 的状态——那个要问 daemon（见 `ipc::client::status`）。
pub fn accessibility_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        imp::ax_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// 向系统申请辅助功能授权：会把当前进程**登记进 TCC 列表**并弹一次系统提示。
///
/// 只读的 [`accessibility_granted`] 不会登记，所以新机器上用户按引导打开设置后
/// 列表里可能根本没有 Semisnip，无从勾选。这个调用补上"先露面再让用户勾"的
/// 那一步，只在用户明确点了「去授权 / 打开辅助功能设置」时调用。
///
/// 返回值是**调用当下**的授权状态（`false` 不代表弹窗失败），因此调用方仍应
/// 打开设置面板作为兜底。
pub fn request_trust_prompt() -> bool {
    #[cfg(target_os = "macos")]
    {
        imp::request_trust_prompt()
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// 权限补上之后重新装一次 tap，结果用 [`Retry`] 区分。
///
/// [`install`] 只在启动时跑一次，结果就此固定：用户后来去系统设置里勾了
/// 「辅助功能」，daemon 这边不会自己知道——以前只能 `launchctl kickstart` 重启，
/// 而多数人根本不知道该敲这条命令，表现就是「授权了也没用」。
///
/// 这个函数给调度器的看门狗用：授权一到，当场把 tap 接上，面板立刻能打字。
///
/// 三个提前返回都有意义：
/// - 已经就绪 → 别重复装（`imp::run` 会再建一个 run loop）；
/// - `SHARED` 还没设 → `install` 都没跑过，没东西可复用；
/// - 没有权限 → 现在重试必然失败，而且会每秒新建一个线程。
pub fn retry() -> Retry {
    if READY.load(Ordering::SeqCst) {
        return Retry::AlreadyReady;
    }
    // 调试开关同上：强制降级时别让看门狗每 5 秒白建一个线程。
    if std::env::var_os("SEMISNIP_NO_TAP").is_some() {
        return Retry::Disabled;
    }
    let Some(shared) = SHARED.get() else {
        return Retry::NotInstalled;
    };
    if !accessibility_granted() {
        return Retry::NotTrusted;
    }
    let shared = shared.clone();
    // 不复用 `install`：`SHARED` 已经设过了，再调只会打一行「已安装过」然后返回。
    // 状态机与 `tx` 都在 `shared` 里，重开线程即可。
    match std::thread::Builder::new()
        .name("semisnip-tap".into())
        .spawn(move || imp::run(shared))
    {
        Ok(_) => Retry::Spawned,
        Err(err) => {
            log::error!("键盘 tap 线程启动失败：{err}");
            Retry::SpawnFailed
        }
    }
}

/// 启动 tap 线程。整个进程只会成功一次（`SHARED` 是 `OnceLock`）。
///
/// 不在这里判断权限：`imp::run` 会先检查，失败时投 `Event::TapUnavailable`
/// 让 UI 切到授权引导页。这样"没授权"是一种**正常状态**，而不是启动失败。
///
/// `enabled` 是配置里的「启用触发」：为假时 tap 照样装上（权限、鼠标监听都还要
/// 用），只是不喂触发状态机。恢复由 [`set_enabled`] 在运行期改，不用重启。
pub fn install(tx: Sender<Event>, config: TriggerConfig, enabled: bool) {
    let shared = Arc::new(TapShared::new(config, tx, enabled));
    if SHARED.set(shared.clone()).is_err() {
        log::warn!("tap 已安装过，忽略重复安装");
        return;
    }

    std::thread::Builder::new()
        .name("semisnip-tap".into())
        .spawn(move || imp::run(shared))
        .ok();
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    type CGEventRef = *mut c_void;
    type CGEventTapProxy = *mut c_void;

    const SESSION_TAP: u32 = 1; // kCGSessionEventTap
    const HEAD_INSERT: u32 = 0; // kCGHeadInsertEventTap
    const OPTION_DEFAULT: u32 = 0; // kCGEventTapOptionDefault（可吞键）

    const KEY_DOWN: u32 = 10;
    const KEY_UP: u32 = 11;
    /// `kCGEventLeftMouseDown` / `kCGEventRightMouseDown` / `kCGEventOtherMouseDown`。
    /// 只订阅"按下"：抬起没有信息量，多订阅一类事件就多一次回调。
    const LEFT_MOUSE_DOWN: u32 = 1;
    const RIGHT_MOUSE_DOWN: u32 = 3;
    const OTHER_MOUSE_DOWN: u32 = 25;
    const DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
    const DISABLED_USER: u32 = 0xFFFF_FFFF;

    const FIELD_AUTOREPEAT: u32 = 8;
    const FIELD_KEYCODE: u32 = 9;
    /// kCGEventSourceUnixProcessID：用来忽略自己合成的事件。
    const FIELD_SOURCE_PID: u32 = 41;

    const FLAG_SHIFT: u64 = 0x0002_0000;
    const FLAG_CONTROL: u64 = 0x0004_0000;
    const FLAG_ALTERNATE: u64 = 0x0008_0000;
    const FLAG_COMMAND: u64 = 0x0010_0000;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef,
            user_info: *mut c_void,
        ) -> CGEventRef;
        fn CGEventTapEnable(tap: CGEventRef, enable: bool);
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
        fn CGEventGetFlags(event: CGEventRef) -> u64;
        /// 鼠标事件的全局位置：屏幕左上角为原点、单位为点，与 `Window::bounds()` 同一套。
        fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
        fn AXIsProcessTrusted() -> bool;
        /// 带选项的版本：传入 `kAXTrustedCheckOptionPrompt = true` 时，系统会把
        /// 当前进程登记进「辅助功能」列表并弹出授权提示。
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        /// `CFStringRef` 常量。用 `SyncPtr` 包一层：裸指针不是 `Sync`，
        /// 没法直接声明成 `extern static`，但它本身只读、跨线程读安全。
        static kAXTrustedCheckOptionPrompt: SyncPtr;
    }

    /// `extern static` 需要 `Sync`；这个包装只为过类型检查，布局仍是裸指针。
    #[repr(transparent)]
    struct SyncPtr(*const c_void);

    // SAFETY: 指向 CoreFoundation 的不可变常量，只读访问。
    unsafe impl Sync for SyncPtr {}

    /// `CGPoint` 的镜像。自己定义而不是引 `core_graphics`：这里只需要传值，
    /// 布局是两个连续的 `f64`（`#[repr(C)]` 保证与 C 一致）。
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopCommonModes: *const c_void;
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: CGEventRef,
            order: isize,
        ) -> *mut c_void;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
        fn CFRunLoopRun();
    }

    /// 回调线程的可变上下文。
    ///
    /// `Box::leak` 出去（见 `run`），生命周期就是进程——回调里只能拿到裸指针，
    /// 必须保证它永不释放。`Send/Sync` 手动实现：`AtomicUsize` 与 `Arc` 本身线程
    /// 安全，`Ctx` 会被回调线程与「重新启用 tap」的路径同时读到。
    struct Ctx {
        shared: Arc<TapShared>,
        /// tap 句柄（`CGEventRef` 的整数形态）。被系统禁用时用它重新启用。
        tap: AtomicUsize,
        /// 自己的 pid，用来识别"这是我合成的事件"。
        self_pid: u32,
    }

    // 只在 tap 线程访问；生命周期由进程保证（Box::leak）。
    unsafe impl Send for Ctx {}
    unsafe impl Sync for Ctx {}

    /// tap 回调。**运行在独立的 run loop 线程上**，必须快：任何阻塞都会让
    /// 系统键盘变卡。所以这里只做判定与投事件，不做 IO、不加长锁。
    ///
    /// 返回 `null` 表示吞掉这个事件（目标 App 收不到），返回 `event` 表示放行。
    extern "C" fn callback(
        _proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef {
        if event.is_null() || user_info.is_null() {
            return event;
        }
        let ctx = unsafe { &*(user_info as *const Ctx) };

        match etype {
            KEY_DOWN => unsafe {
                if CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) != 0 {
                    return event;
                }
                let source = CGEventGetIntegerValueField(event, FIELD_SOURCE_PID);
                if source > 0 && source as u32 == ctx.self_pid {
                    // 自己合成的回删 / 粘贴事件，直接放行
                    return event;
                }

                let code = CGEventGetIntegerValueField(event, FIELD_KEYCODE) as u32;
                let flags = CGEventGetFlags(event);

                let msg = KeyMsg {
                    code,
                    shift: flags & FLAG_SHIFT != 0,
                    alt: flags & FLAG_ALTERNATE != 0,
                    control: flags & FLAG_CONTROL != 0,
                    command: flags & FLAG_COMMAND != 0,
                };

                match route_panel_key(ctx.shared.panel_open.load(Ordering::SeqCst), source) {
                    KeyRoute::Panel => {
                        if msg.is_shortcut() {
                            // 系统快捷键不归我们管：放行并收面板
                            let _ = ctx.shared.tx.send(Event::Cancel);
                            return event;
                        }
                        let _ = ctx.shared.tx.send(Event::Key(msg));
                        return std::ptr::null_mut();
                    }
                    KeyRoute::Pass if source > 0 => {
                        // 外部进程合成的事件：放行。面板关着也不喂触发状态机——
                        // 这里放行的很可能是某轮尚未结束的 espanso 展开（补偿
                        // 退格或展开文本），吞了就是"文档留半截触发词"或"展开
                        // 结果缺一截"，喂进状态机则可能被展开文本里的字符误触发。
                        // 见 `route_panel_key`。
                        return event;
                    }
                    KeyRoute::Pass => {}
                }

                if msg.is_shortcut() || msg.alt {
                    // 组合键打断了连续输入；不清缓冲的话，下一次输入可能把
                    // “快捷键前的半截文本 + 触发词”误判成一个直达触发。
                    if let Ok(mut buffer) = ctx.shared.direct_buffer.lock() {
                        buffer.clear();
                    }
                    return event;
                }

                // 直达触发词统计：只观察，不拦截。面板打开时不会走到这里
                // （keydown 在 Panel 分支已经返回），所以不会和面板回车重复计数。
                if let Some(ch) = keymap::char_for_keycode(code, flags & FLAG_SHIFT != 0) {
                    if let Some(trigger) = ctx.shared.observe_direct(ch) {
                        let _ = ctx.shared.tx.send(Event::DirectUse(trigger));
                    }
                }

                // 暂停：不喂状态机、不开面板。**键照常放行**——触发键本来就不吞，
                // 所以用户打出来的 `;;` 会原样留在文档里，这正是"暂停"该有的样子。
                // espanso 的降级触发（`;;;`）走的是 espanso → CLI → IPC，不经过
                // 这里，暂停期间仍然能把面板叫出来（见 `cli::render_match_file`）。
                if !ctx.shared.is_enabled() {
                    return event;
                }

                let action = {
                    let Ok(mut machine) = ctx.shared.machine.lock() else {
                        return event;
                    };
                    machine.feed(code, Instant::now())
                };

                match action {
                    TriggerAction::Fired { erase } => {
                        log::info!("触发命中：keycode={code} erase={erase}");
                        ctx.shared.panel_open.store(true, Ordering::SeqCst);
                        // 这个键照常落进文档（见 `trigger::machine`），它的 keyUp
                        // 到达时面板已经打开，得预约放行，否则目标 App 看到
                        // "按下没抬起"。
                        ctx.shared.pass_next_key_up.store(true, Ordering::SeqCst);
                        let _ = ctx.shared.tx.send(Event::Open {
                            origin: crate::ipc::proto::Origin::Tap,
                            erase,
                        });
                        event
                    }
                    TriggerAction::Pass => event,
                }
            },
            KEY_UP => unsafe {
                if ctx.shared.panel_open.load(Ordering::SeqCst)
                    && CGEventGetIntegerValueField(event, FIELD_SOURCE_PID) <= 0
                {
                    // 触发命中的那个键是放行的，它的 keyUp 也必须放行，否则目标
                    // App 看到"按下没抬起"；其余物理 keyUp 与被吞的 keyDown 配对，
                    // 一并吞掉。外部合成事件的 keyUp 一律放行（见 KEY_DOWN）。
                    if ctx.shared.take_pass_key_up() {
                        return event;
                    }
                    return std::ptr::null_mut();
                }
                event
            },
            // 鼠标按下：只为"点面板外 = 收起面板"。**永远放行**——面板是辅助窗口，
            // 用户的点击该落在哪就落在哪，我们绝不抢（`✕`、授权页按钮都靠它）。
            LEFT_MOUSE_DOWN | RIGHT_MOUSE_DOWN | OTHER_MOUSE_DOWN => {
                if !ctx.shared.panel_open.load(Ordering::SeqCst) {
                    return event;
                }
                let point = unsafe { CGEventGetLocation(event) };
                if ctx.shared.panel_contains(point.x, point.y) {
                    return event;
                }
                log::info!("点击面板外（{:.0},{:.0}），收起面板", point.x, point.y);
                // 与按 Esc 不同的事件：点击会把前台窗口换掉，收尾时不能去动文档。
                let _ = ctx.shared.tx.send(Event::ClickOutside);
                event
            }
            DISABLED_TIMEOUT | DISABLED_USER => {
                // 授权被撤销时重新启用也没意义：撤回「已就绪」，让面板切到授权页，
                // 而不是继续吞键或假装可用。
                if !unsafe { AXIsProcessTrusted() } {
                    mark_unavailable("辅助功能授权已失效，键盘接管停止");
                    return event;
                }
                log::warn!("事件 tap 被系统禁用，尝试重新启用");
                let tap = ctx.tap.load(Ordering::SeqCst);
                if tap != 0 {
                    unsafe { CGEventTapEnable(tap as CGEventRef, true) };
                }
                event
            }
            _ => event,
        }
    }

    /// `AXIsProcessTrusted()` 的薄包装。**结果会被系统缓存**：用户刚在设置里
    /// 勾上时它可能仍返回 false，需要 tap 真正装成功后 `ready()` 才会变 true。
    /// 这就是引导页要提供「重新检查」按钮的原因。
    pub fn ax_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// 向系统弹一次授权提示，并把本进程登记进 TCC 列表。
    ///
    /// 用 `core_foundation` 组装选项字典，而不是手写 CFDictionary 回调：
    /// 常量 `kAXTrustedCheckOptionPrompt` 直接作为 key（不能拿字符串近似替代，
    /// 否则系统会静默忽略选项，用户看不到任何弹窗）。
    pub fn request_trust_prompt() -> bool {
        use core_foundation::base::TCFType as _;
        use core_foundation::boolean::CFBoolean;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::string::{CFString, CFStringRef};

        unsafe {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt.0 as CFStringRef);
            let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as *const c_void)
        }
    }

    /// 建 tap、挂 run loop、阻塞运行（直到进程退出）。
    ///
    /// `CGEventTapCreate` 必须在**有 run loop 的线程**上调用，所以整个函数都在
    /// tap 线程里跑，最后进 `CFRunLoopRun()` 不再返回。
    ///
    /// 事件掩码只订阅 keyDown/keyUp 与三种"鼠标按下"：滚动、移动、抬起一概不接，
    /// 减少回调次数，也避免无意中看到用户的鼠标轨迹（我们只需要知道"点在哪"）。
    pub fn run(shared: Arc<TapShared>) {
        unsafe {
            // 调试开关：强制走降级路径，用来看面板的「需要授权」页长什么样。
            // 没有这个开关，那页只有"真的没授权"的机器上才看得到——开发机上
            // 恰恰永远是有授权的。
            if std::env::var_os("SEMISNIP_NO_TAP").is_some() {
                log::warn!("SEMISNIP_NO_TAP 已设置：强制降级（只看授权页时用）");
                let _ = shared.tx.send(Event::TapUnavailable(
                    "调试：SEMISNIP_NO_TAP 关闭了键盘接管".into(),
                ));
                return;
            }

            if !AXIsProcessTrusted() {
                log::warn!("缺少辅助功能权限：改用 espanso shell 触发（降级模式）");
                let _ = shared.tx.send(Event::TapUnavailable(
                    "缺少「辅助功能」权限，已降级为 espanso 触发".into(),
                ));
                return;
            }

            let ctx = Box::leak(Box::new(Ctx {
                shared: shared.clone(),
                tap: AtomicUsize::new(0),
                self_pid: std::process::id(),
            }));

            // 鼠标按下也要订阅：见 callback 里的 `LEFT_MOUSE_DOWN` 分支。
            let mask: u64 = (1u64 << KEY_DOWN)
                | (1u64 << KEY_UP)
                | (1u64 << LEFT_MOUSE_DOWN)
                | (1u64 << RIGHT_MOUSE_DOWN)
                | (1u64 << OTHER_MOUSE_DOWN);
            let tap = CGEventTapCreate(
                SESSION_TAP,
                HEAD_INSERT,
                OPTION_DEFAULT,
                mask,
                callback,
                ctx as *mut Ctx as *mut c_void,
            );
            if tap.is_null() {
                log::error!("创建键盘 tap 失败（权限不足？）");
                let _ = shared.tx.send(Event::TapUnavailable(
                    "无法创建键盘监听，请授予「辅助功能」权限".into(),
                ));
                return;
            }

            ctx.tap.store(tap as usize, Ordering::SeqCst);
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            if source.is_null() {
                log::error!("创建 run loop source 失败");
                return;
            }
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            READY.store(true, Ordering::SeqCst);
            log::info!("键盘 tap 已就绪");

            CFRunLoopRun();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::sync::Arc;

    use super::*;

    /// 非 macOS：没有 CGEventTap，直接告诉 UI "降级"。
    /// 幂等——`retry` 每 5 秒会再调一次，重复投递同一个事件没有副作用。
    pub fn run(shared: Arc<TapShared>) {
        let _ = shared.tx.send(Event::TapUnavailable(
            "当前平台需要 espanso shell 触发".into(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn shared() -> TapShared {
        let (tx, _rx) = channel();
        TapShared::new(TriggerConfig::default(), tx, true)
    }

    #[test]
    fn pausing_stops_feeding_and_releases_the_panel() {
        let shared = shared();
        // 模拟"面板开着时被暂停"：面板必须一起放开，否则暂停之后键盘还在被吞。
        shared.panel_open.store(true, Ordering::SeqCst);
        shared.set_enabled(false);
        assert!(!shared.is_enabled());
        assert!(
            !shared.panel_open.load(Ordering::SeqCst),
            "暂停必须同时放开 panel_open，否则面板会继续吃键"
        );

        // 恢复之后回到接管状态（tap 没被卸载，所以这里是同一个 shared）
        shared.set_enabled(true);
        assert!(shared.is_enabled());
    }

    #[test]
    fn panel_takes_physical_keys_only() {
        // 面板打开 + 物理按键（HID 事件没有 source pid）→ 面板输入
        assert_eq!(route_panel_key(true, 0), KeyRoute::Panel);
        // 面板打开 + 外部进程合成的事件 → 放行：可能是某轮未结束的 espanso
        // 展开（补偿退格 / 展开文本），吞了就是"删除不足"或"展开缺一截"
        assert_eq!(route_panel_key(true, 231), KeyRoute::Pass);
        // 面板关闭：一律放行（外部事件也不该喂触发状态机）
        assert_eq!(route_panel_key(false, 0), KeyRoute::Pass);
        assert_eq!(route_panel_key(false, 231), KeyRoute::Pass);
    }

    #[test]
    fn pass_key_up_is_one_shot() {
        let shared = shared();
        assert!(!shared.take_pass_key_up(), "没有预约时不能放行");
        shared.pass_next_key_up.store(true, Ordering::SeqCst);
        assert!(shared.take_pass_key_up(), "触发命中的 keyUp 要放行");
        assert!(
            !shared.take_pass_key_up(),
            "预约是一次性的：只放行第一个 keyUp"
        );
    }

    #[test]
    fn panel_rect_tells_inside_from_outside() {
        let rect = PanelRect {
            x: 100.0,
            y: 50.0,
            width: 200.0,
            height: 100.0,
        };
        // 四个角都算里面（边界含）
        assert!(rect.contains(100.0, 50.0), "左上角");
        assert!(rect.contains(300.0, 150.0), "右下角");
        assert!(!rect.contains(99.0, 100.0), "左边一点");
        assert!(!rect.contains(301.0, 100.0), "右边一点");
        assert!(!rect.contains(200.0, 151.0), "下边一点");
    }

    #[test]
    fn unknown_rect_never_closes_the_panel() {
        // 还没量到窗口位置（或量失败）时按"在里面"处理：最坏是老行为——点外面
        // 不关面板；反过来（按"在外面"）就会变成"点哪都关"，连 `✕` 都点不动。
        let rect = PanelRect::default();
        assert!(rect.contains(0.0, 0.0));
        assert!(rect.contains(9999.0, 9999.0));
    }
}
