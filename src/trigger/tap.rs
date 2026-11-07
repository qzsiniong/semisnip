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
//! 1. 自己合成的事件（回删 / 粘贴 / 补触发词）会被自己的 tap 看到 → 用 source pid
//!    过滤，另加触发静默窗口（合成事件不一定带得到 pid，见 [`TapShared::mute_trigger`]）；
//! 2. tap 被系统超时摘除后要立刻重新启用，否则用户键盘"半瘫"；
//! 3. 带 Cmd/Ctrl 的组合键一律放行并关面板，绝不能把系统快捷键吞掉；
//! 4. 鼠标事件一律**放行**——我们只借它判断"点在哪"，绝不抢用户的点击。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::app::event::{Event, KeyMsg};
use crate::trigger::machine::{TriggerAction, TriggerConfig, TriggerMachine};

/// 补偿退格的吞掉窗口：espanso 收到 `match exec` 后几乎立刻发退格，
/// 但系统繁忙时可能晚几十毫秒。窗口只用来兜底，正常情况下会被
/// 「遇到其它按键就收手」提前结束。
const SWALLOW_WINDOW_MS: i64 = 800;

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
    /// 触发状态机。放在锁里是因为 `feed` 需要"读前缀 + 前进 + 可能复位"原子完成。
    machine: Mutex<TriggerMachine>,
    /// 事件发送端。tap 线程不直接改 UI 状态，只投事件。
    tx: Sender<Event>,
    /// 还要吞掉多少个「别的进程合成的」退格。
    pending_backspaces: AtomicUsize,
    /// 上述额度的截止时间（epoch ms）。
    swallow_deadline_ms: AtomicI64,
    /// 触发判定的静默截止时间（epoch ms）。见 [`TapShared::mute_trigger`]。
    mute_until_ms: AtomicI64,
    /// 面板窗口的屏幕矩形。鼠标按下时用来区分"点面板里"和"点面板外"。
    panel_rect: Mutex<PanelRect>,
}

impl TapShared {
    fn new(config: TriggerConfig, tx: Sender<Event>) -> Self {
        Self {
            panel_open: AtomicBool::new(false),
            machine: Mutex::new(TriggerMachine::new(config)),
            tx,
            pending_backspaces: AtomicUsize::new(0),
            swallow_deadline_ms: AtomicI64::new(0),
            mute_until_ms: AtomicI64::new(0),
            panel_rect: Mutex::new(PanelRect::default()),
        }
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

    /// 接下来 `ms` 毫秒内**不要**把按键喂给触发状态机。
    ///
    /// 取消面板时要把触发词补回文档（见 `handoff::keep_trigger`），那些字符是
    /// 我们自己合成的。不能只指望 source pid 过滤：`CGEventSourceCreate` 造出来的
    /// 事件在某些系统版本上 `kCGEventSourceUnixProcessID` 是 0，于是会被当成
    /// 用户输入。单字符触发串（`/`）一旦漏过去就是死循环——补 `/` → 立即命中 →
    /// 面板又开 → 再取消……
    fn mute_trigger(&self, ms: i64) {
        self.mute_until_ms
            .store(crate::usage::now_ms() + ms, Ordering::SeqCst);
    }

    /// 触发判定是否处于静默期。
    fn trigger_muted(&self) -> bool {
        crate::usage::now_ms() < self.mute_until_ms.load(Ordering::SeqCst)
    }

    /// 预告要吞的退格数量，并启动倒计时窗口。
    ///
    /// `store` 而不是 `fetch_add`：新的一轮展开会覆盖上一轮没吃完的额度——
    /// 上一轮的退格（如果还没来）已经不可能再来了，留着额度只会误吞用户按键。
    fn arm_backspace_swallow(&self, count: usize) {
        self.pending_backspaces.store(count, Ordering::SeqCst);
        self.swallow_deadline_ms
            .store(crate::usage::now_ms() + SWALLOW_WINDOW_MS, Ordering::SeqCst);
    }

    /// 撤销额度。`match exec` 连 IPC 都没成功时调——那些退格永远不会来，
    /// 留着额度会让用户接下来按的退格被吞掉。
    fn clear_backspace_swallow(&self) {
        self.pending_backspaces.store(0, Ordering::SeqCst);
    }

    /// 是否应该吞掉这一个退格。只对调用方确认过「不是自己发的」事件调用。
    ///
    /// `fetch_update` 做"检查并递减"的原子操作：`checked_sub` 在已经是 0 时返回
    /// `None`，`fetch_update` 便保持原值并返回 `Err`——这样两个并发退格不会都
    /// 拿到额度（虽然实际只有 tap 线程会调，但这个不变量值得用类型保证）。
    fn take_backspace(&self) -> bool {
        if self.pending_backspaces.load(Ordering::SeqCst) == 0 {
            return false;
        }
        if crate::usage::now_ms() > self.swallow_deadline_ms.load(Ordering::SeqCst) {
            self.clear_backspace_swallow();
            return false;
        }
        self.pending_backspaces
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
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

/// 面板是否正在接管键盘。回传逻辑用它判断"能不能拦下 espanso 的补偿退格"。
pub fn is_panel_open() -> bool {
    SHARED
        .get()
        .map(|s| s.panel_open.load(Ordering::SeqCst))
        .unwrap_or(false)
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

/// 预告：接下来会有 `count` 个由 espanso 合成的退格，别让它们落到目标 App。
///
/// `espanso match exec` 会按 trigger 全长回删字符（引擎里的 TriggerCompensation）。
/// 我们调用前已经把面板吞掉的触发符删掉了，如果放任这些退格通过，
/// 它们会顺着光标继续吃掉用户的真实字符（`9876543210` → `98765`）。
pub fn swallow_synthetic_backspaces(count: usize) {
    if let Some(shared) = SHARED.get() {
        shared.arm_backspace_swallow(count);
    }
}

/// 撤销预告（`match exec` 连 IPC 都没成功时用，避免误吞后续按键）。
pub fn clear_synthetic_backspaces() {
    if let Some(shared) = SHARED.get() {
        shared.clear_backspace_swallow();
    }
}

/// 接下来 `ms` 毫秒内不判定触发。
///
/// 只有一处调用：[`crate::handoff::keep_trigger`]——它要把触发词合成回文档，
/// 那些按键必须先被"当成不是用户打的"。见 [`TapShared::mute_trigger`]。
pub fn mute_trigger(ms: u64) {
    if let Some(shared) = SHARED.get() {
        shared.mute_trigger(ms as i64);
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

/// 权限补上之后重新装一次 tap，装上了返回 true。
///
/// [`install`] 只在启动时跑一次，结果就此固定：用户后来去系统设置里勾了
/// 「辅助功能」，daemon 这边不会自己知道——以前只能 `launchctl kickstart` 重启，
/// 而多数人根本不知道该敲这条命令，表现就是「授权了也没用」。
///
/// 这个函数给调度器每秒探一次用：授权一到，当场把 tap 接上，面板立刻能打字。
///
/// 三个提前返回都有意义：
/// - 已经就绪 → 别重复装（`imp::run` 会再建一个 run loop）；
/// - `SHARED` 还没设 → `install` 都没跑过，没东西可复用；
/// - 没有权限 → 现在重试必然失败，而且会每秒新建一个线程。
pub fn retry() -> bool {
    if READY.load(Ordering::SeqCst) {
        return true;
    }
    // 调试开关同上：强制降级时别让看门狗每 5 秒白建一个线程。
    if std::env::var_os("SEMISNIP_NO_TAP").is_some() {
        return false;
    }
    let Some(shared) = SHARED.get() else {
        return false;
    };
    if !accessibility_granted() {
        return false;
    }
    let shared = shared.clone();
    // 不复用 `install`：`SHARED` 已经设过了，再调只会打一行「已安装过」然后返回。
    // 状态机与 `tx` 都在 `shared` 里，重开线程即可。
    std::thread::Builder::new()
        .name("semisnip-tap".into())
        .spawn(move || imp::run(shared))
        .is_ok()
}

/// 启动 tap 线程。整个进程只会成功一次（`SHARED` 是 `OnceLock`）。
///
/// 不在这里判断权限：`imp::run` 会先检查，失败时投 `Event::TapUnavailable`
/// 让 UI 切到授权引导页。这样"没授权"是一种**正常状态**，而不是启动失败。
pub fn install(tx: Sender<Event>, config: TriggerConfig) {
    let shared = Arc::new(TapShared::new(config, tx));
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
    }

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

                // espanso 展开时会先按 trigger 全长发一串退格（TriggerCompensation）。
                // 这些是它合成的键盘事件（source 是 espanso 的 pid），必须在
                // 到达目标 App 之前吞掉，否则会删掉用户光标前的真实字符。
                if code == crate::trigger::keymap::BACKSPACE {
                    if source > 0
                        && !ctx.shared.panel_open.load(Ordering::SeqCst)
                        && ctx.shared.take_backspace()
                    {
                        log::debug!("吞掉 espanso 的补偿退格（source={source}）");
                        return std::ptr::null_mut();
                    }
                } else if source > 0 {
                    // 补偿退格总在展开文本之前；出现别的键就说明这一轮结束了，
                    // 立刻收手，避免误吞用户后续的退格。
                    ctx.shared.clear_backspace_swallow();
                }

                let msg = KeyMsg {
                    code,
                    shift: flags & FLAG_SHIFT != 0,
                    alt: flags & FLAG_ALTERNATE != 0,
                    control: flags & FLAG_CONTROL != 0,
                    command: flags & FLAG_COMMAND != 0,
                };

                if ctx.shared.panel_open.load(Ordering::SeqCst) {
                    if msg.is_shortcut() {
                        // 系统快捷键不归我们管：放行并收面板
                        let _ = ctx.shared.tx.send(Event::Cancel);
                        return event;
                    }
                    let _ = ctx.shared.tx.send(Event::Key(msg));
                    return std::ptr::null_mut();
                }

                if ctx.shared.trigger_muted() {
                    // 补触发词期间的合成按键：放行（字符要落到文档里），但不判定。
                    return event;
                }

                if msg.is_shortcut() || msg.alt {
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
                        let _ = ctx.shared.tx.send(Event::Open {
                            origin: crate::ipc::proto::Origin::Tap,
                            erase,
                        });
                        std::ptr::null_mut()
                    }
                    TriggerAction::Pass => event,
                }
            },
            KEY_UP => {
                if ctx.shared.panel_open.load(Ordering::SeqCst) {
                    return std::ptr::null_mut();
                }
                event
            }
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
        TapShared::new(TriggerConfig::default(), tx)
    }

    #[test]
    fn swallow_budget_is_exact() {
        let shared = shared();
        shared.arm_backspace_swallow(2);
        assert!(shared.take_backspace(), "第 1 个应该被吞");
        assert!(shared.take_backspace(), "第 2 个应该被吞");
        assert!(!shared.take_backspace(), "额度用完就不能再吞");
    }

    #[test]
    fn expired_budget_does_not_eat_keys() {
        let shared = shared();
        shared.arm_backspace_swallow(3);
        shared
            .swallow_deadline_ms
            .store(crate::usage::now_ms() - 1, Ordering::SeqCst);
        assert!(!shared.take_backspace(), "过期后必须放行");
        assert_eq!(
            shared.pending_backspaces.load(Ordering::SeqCst),
            0,
            "过期时应顺手清零，避免下一轮误吞"
        );
    }

    #[test]
    fn clearing_cancels_pending() {
        let shared = shared();
        shared.arm_backspace_swallow(4);
        shared.clear_backspace_swallow();
        assert!(!shared.take_backspace());
    }

    #[test]
    fn muting_skips_trigger_detection_until_it_expires() {
        let shared = shared();
        assert!(!shared.trigger_muted(), "默认不静默");
        shared.mute_trigger(250);
        assert!(shared.trigger_muted(), "刚设上必须静默");
        // 把截止时间拨到过去，等价于"窗口已经过去"
        shared
            .mute_until_ms
            .store(crate::usage::now_ms() - 1, Ordering::SeqCst);
        assert!(!shared.trigger_muted(), "过期后必须恢复判定");
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
