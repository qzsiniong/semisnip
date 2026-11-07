//! 需要合成键盘事件的几处：回删残留触发符、无法拦截时的补位空格、
//! `match exec` 失败时的粘贴兜底。
//!
//! 都在后台线程调用；合成事件带自己的 source pid，会被 tap 的
//! "自己人"判断放行（见 `trigger::tap`）。

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    type CGEventRef = *mut c_void;

    const SESSION_TAP: u32 = 1;
    const HID_STATE: i32 = 1;

    const FLAG_COMMAND: u64 = 0x0010_0000;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, key_down: bool) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CGEventSourceCreate(state: i32) -> *mut c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *mut c_void);
    }

    fn press(code: u16, flags: u64) {
        unsafe {
            let source = CGEventSourceCreate(HID_STATE);
            for down in [true, false] {
                let event = CGEventCreateKeyboardEvent(source, code, down);
                if event.is_null() {
                    continue;
                }
                if flags != 0 {
                    CGEventSetFlags(event, flags);
                }
                CGEventPost(SESSION_TAP, event);
                CFRelease(event);
            }
            if !source.is_null() {
                CFRelease(source);
            }
        }
    }

    /// 回删 `count` 个字符（面板吞掉的触发符残留）。
    pub fn backspace(count: usize) {
        for _ in 0..count {
            press(crate::trigger::keymap::BACKSPACE as u16, 0);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// 打出 `count` 个空格。
    ///
    /// 仅在没有 tap、无法拦截 espanso 补偿退格时使用：用空格占住
    /// 「espanso 将要回删的字符数」，让它删的是空白而不是用户文本。
    pub fn type_spaces(count: usize) {
        for _ in 0..count {
            press(crate::trigger::keymap::SPACE as u16, 0);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// 合成一个命名按键（调试用）。
    pub fn press_named(name: &str) {
        let code = match name {
            "esc" | "escape" => crate::trigger::keymap::ESCAPE,
            "enter" | "return" => crate::trigger::keymap::RETURN,
            "up" => crate::trigger::keymap::UP,
            "down" => crate::trigger::keymap::DOWN,
            "tab" => crate::trigger::keymap::TAB,
            _ => return,
        };
        press(code as u16, 0);
    }

    /// 逐字符合成输入。仅用于 `snipshot type`（无辅助功能权限时的冒烟测试）。
    pub fn type_text(text: &str) {
        for c in text.chars() {
            let Some((code, shift)) = crate::trigger::keymap::keycode_and_shift(c) else {
                continue;
            };
            press(code as u16, if shift { 0x0002_0000 } else { 0 });
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }

    /// Cmd+V。
    pub fn paste() {
        press(crate::trigger::keymap::KEY_V as u16, FLAG_COMMAND);
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn backspace(_count: usize) {}
    pub fn type_spaces(_count: usize) {}
    pub fn paste() {}
    pub fn type_text(_text: &str) {}
    pub fn press_named(_name: &str) {}
}

pub use imp::{backspace, paste, press_named, type_spaces, type_text};

/// 把文本放进系统剪贴板（macOS 用 `pbcopy`，省一个 crate）。
pub fn set_clipboard(text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("启动 pbcopy 失败：{e}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("pbcopy 退出码 {status}");
    }
    Ok(())
}
