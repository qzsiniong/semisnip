//! 需要合成键盘事件的几处：回删残留触发符、给补偿退格的占位空格、
//! `match exec` 失败时的粘贴兜底。
//!
//! 都在后台线程调用；合成事件带自己的 source pid，会被 tap 的
//! "自己人"判断放行（见 `trigger::tap`）。

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    type CGEventRef = *mut c_void;

    /// `kCGSessionEventTap`：与 tap 同一层，注入的事件对所有前台 App 可见。
    const SESSION_TAP: u32 = 1;
    /// `kCGEventSourceStateHIDSystemState`：伪造的事件"看起来像硬件发的"，
    /// 目标 App 不会因为来源可疑而忽略它。
    const HID_STATE: i32 = 1;

    /// `kCGEventFlagMaskCommand`（`NSEventModifierFlagCommand`）。
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

    /// 合成"按下 + 抬起"一对事件。
    ///
    /// 必须成对：只发 keyDown 会让目标 App 停在"按住"状态，下一次同样的键
    /// 会被系统当成自动重复。`flags` 只对可打印键有意义（`Cmd+V` 用它）。
    ///
    /// 事件与 source 都要 `CFRelease`：这两个都是 CoreFoundation 的 +1 引用，
    /// 漏释放会在每次展开时泄漏一点内存（daemon 是常驻进程，一天下来很可观）。
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

    /// 打出 `count` 个空格作为占位。
    ///
    /// `match exec` 时 espanso 会按 trigger 全长回删（TriggerCompensation），
    /// 这些空格就是给它删的：文档里恰好有 `trigger_len` 个可丢弃字符，
    /// 补偿退格无论何时到达，删的都是空白而不是用户文本（见 `handoff::confirm`）。
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

    /// 逐字符合成输入：取消面板时补回触发词尾巴，以及 `semisnip type`
    /// （无辅助功能权限时的冒烟测试）。
    ///
    /// 只映射 ASCII（`keymap::keycode_and_shift` 的边界）——触发串本来就只允许
    /// ASCII，所以这条限制在这里不会漏字符。8ms 的间隔是给目标 App 的输入法留的
    /// 处理时间：连发会丢字符，尤其是在非英文输入法激活时。
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
    //! 非 macOS 的空实现：保持签名一致，让上层代码不用到处 `cfg`。
    //! 这些函数"什么都没做"是刻意的——在别的平台上 semisnip 本来就只能
    //! 走 espanso 自己的触发，没有合成键盘事件的能力。
    pub fn backspace(_count: usize) {}
    /// 空实现：见上面的模块说明。
    pub fn type_spaces(_count: usize) {}
    /// 空实现。
    pub fn paste() {}
    /// 空实现。
    pub fn type_text(_text: &str) {}
    /// 空实现。
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
