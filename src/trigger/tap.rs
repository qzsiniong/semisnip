//! 全局键盘接管（macOS CGEventTap）。
//!
//! 用 `cgSessionEventTap` + `HeadInsert` + 可修改模式：能**吞键**，
//! 因而面板打开期间目标 App 一个字符都收不到，也就不会出现
//! "过滤串漏进文档"的竞态。
//!
//! 三处必须小心：
//! 1. 自己合成的事件（回删 / 粘贴）会被自己的 tap 看到 → 用 source pid 过滤；
//! 2. tap 被系统超时摘除后要立刻重新启用，否则用户键盘"半瘫"；
//! 3. 带 Cmd/Ctrl 的组合键一律放行并关面板，绝不能把系统快捷键吞掉。

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

pub struct TapShared {
    panel_open: AtomicBool,
    machine: Mutex<TriggerMachine>,
    tx: Sender<Event>,
    /// 还要吞掉多少个「别的进程合成的」退格。
    pending_backspaces: AtomicUsize,
    /// 上述额度的截止时间（epoch ms）。
    swallow_deadline_ms: AtomicI64,
}

impl TapShared {
    fn new(config: TriggerConfig, tx: Sender<Event>) -> Self {
        Self {
            panel_open: AtomicBool::new(false),
            machine: Mutex::new(TriggerMachine::new(config)),
            tx,
            pending_backspaces: AtomicUsize::new(0),
            swallow_deadline_ms: AtomicI64::new(0),
        }
    }

    fn arm_backspace_swallow(&self, count: usize) {
        self.pending_backspaces.store(count, Ordering::SeqCst);
        self.swallow_deadline_ms
            .store(crate::usage::now_ms() + SWALLOW_WINDOW_MS, Ordering::SeqCst);
    }

    fn clear_backspace_swallow(&self) {
        self.pending_backspaces.store(0, Ordering::SeqCst);
    }

    /// 是否应该吞掉这一个退格。只对调用方确认过「不是自己发的」事件调用。
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

    pub fn set_trigger(&self, config: TriggerConfig) {
        if let Ok(mut machine) = self.machine.lock() {
            *machine = TriggerMachine::new(config);
        }
    }
}

static SHARED: OnceLock<Arc<TapShared>> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);

/// 面板是否处于接管状态：只有它为 true 时 tap 才吞键。
pub fn set_panel_open(open: bool) {
    if let Some(shared) = SHARED.get() {
        shared.panel_open.store(open, Ordering::SeqCst);
    }
}

pub fn is_panel_open() -> bool {
    SHARED
        .get()
        .map(|s| s.panel_open.load(Ordering::SeqCst))
        .unwrap_or(false)
}

/// tap 是否已装上（缺权限时为 false，此时走 espanso shell 降级路径）。
pub fn ready() -> bool {
    READY.load(Ordering::SeqCst)
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

/// 是否已获得辅助功能授权（决定走 tap 还是 espanso shell）。
pub fn accessibility_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        imp::ax_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    false
}

pub fn install(tx: Sender<Event>, config: TriggerConfig) {
    let shared = Arc::new(TapShared::new(config, tx));
    if SHARED.set(shared.clone()).is_err() {
        log::warn!("tap 已安装过，忽略重复安装");
        return;
    }

    std::thread::Builder::new()
        .name("snipshot-tap".into())
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
        fn AXIsProcessTrusted() -> bool;
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

    struct Ctx {
        shared: Arc<TapShared>,
        tap: AtomicUsize,
        self_pid: u32,
    }

    // 只在 tap 线程访问；生命周期由进程保证（Box::leak）。
    unsafe impl Send for Ctx {}
    unsafe impl Sync for Ctx {}

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

    pub fn ax_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    pub fn run(shared: Arc<TapShared>) {
        unsafe {
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

            let mask: u64 = (1u64 << KEY_DOWN) | (1u64 << KEY_UP);
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
}
