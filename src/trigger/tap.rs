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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::app::event::{Event, KeyMsg};
use crate::trigger::machine::{TriggerAction, TriggerConfig, TriggerMachine};

pub struct TapShared {
    panel_open: AtomicBool,
    machine: Mutex<TriggerMachine>,
    tx: Sender<Event>,
}

impl TapShared {
    fn new(config: TriggerConfig, tx: Sender<Event>) -> Self {
        Self {
            panel_open: AtomicBool::new(false),
            machine: Mutex::new(TriggerMachine::new(config)),
            tx,
        }
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
