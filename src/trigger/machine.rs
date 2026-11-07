//! 触发状态机：识别「连续输入触发串」。
//!
//! 与 snippets 的实现有一处关键差异：**命中时把最后一个触发键也吞掉**。
//!
//! 因为 snipshot 的 tap 可以吞键，第一次 `;` 已经落到目标文档里（不能吞，
//! 用户可能只打一个分号），第二次 `;` 直接吞掉，于是文档里只剩 1 个字符，
//! `erase` 恒等于 `keys.len() - 1`。这样面板出现的瞬间就不会有漏键，
//! 也不存在"过滤串进了目标 App"的竞态。

use std::time::{Duration, Instant};

use super::keymap;

pub const DEFAULT_TIMEOUT_MS: u64 = 800;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerConfig {
    pub keycodes: Vec<u32>,
    pub timeout_ms: u64,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            keycodes: vec![keymap::keycode_for_char(';').expect("';' 有键码"); 2],
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl TriggerConfig {
    pub fn from_text(text: &str, timeout_ms: u64) -> Result<Self, String> {
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return Err("触发串不能为空".into());
        }
        if chars.len() > 4 {
            return Err("触发串最多 4 个字符".into());
        }
        let mut keycodes = Vec::with_capacity(chars.len());
        for c in chars {
            match keymap::keycode_for_char(c) {
                Some(code) => keycodes.push(code),
                None => return Err(format!("字符 `{c}` 无法映射到键码（仅支持 ASCII）")),
            }
        }
        Ok(Self {
            keycodes,
            timeout_ms: timeout_ms.clamp(200, 3_000),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerAction {
    /// 放行，不做拦截。
    Pass,
    /// 命中：调用方要吞掉这个键，并在收起面板时回删 `erase` 个字符。
    Fired { erase: usize },
}

pub struct TriggerMachine {
    config: TriggerConfig,
    armed: usize,
    last: Option<Instant>,
}

impl TriggerMachine {
    pub fn new(config: TriggerConfig) -> Self {
        Self {
            config,
            armed: 0,
            last: None,
        }
    }

    pub fn reset(&mut self) {
        self.armed = 0;
        self.last = None;
    }

    pub fn armed_len(&self) -> usize {
        self.armed
    }

    /// 喂一个 keyDown 键码（keyUp、重复键、带修饰键的事件由调用方过滤）。
    pub fn feed(&mut self, keycode: u32, now: Instant) -> TriggerAction {
        if self.config.keycodes.is_empty() {
            return TriggerAction::Pass;
        }

        // 懒超时：不装定时器，只在有按键时判定。
        let expired = self
            .last
            .map(|t| now.duration_since(t) > Duration::from_millis(self.config.timeout_ms))
            .unwrap_or(false);
        if expired {
            self.reset();
        }

        if self.config.keycodes[self.armed] == keycode {
            self.armed += 1;
            self.last = Some(now);
            if self.armed == self.config.keycodes.len() {
                let erase = self.config.keycodes.len() - 1;
                self.reset();
                return TriggerAction::Fired { erase };
            }
            return TriggerAction::Pass;
        }

        // 正好是首键：视为新一轮起点（覆盖 `;;;` 这种重叠序列）。
        if self.config.keycodes[0] == keycode {
            self.armed = 1;
            self.last = Some(now);
        } else {
            self.reset();
        }
        TriggerAction::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semi() -> u32 {
        keymap::keycode_for_char(';').unwrap()
    }

    #[test]
    fn second_semicolon_fires_and_is_swallowed() {
        let mut m = TriggerMachine::new(TriggerConfig::default());
        let t = Instant::now();
        assert_eq!(m.feed(semi(), t), TriggerAction::Pass);
        assert_eq!(
            m.feed(semi(), t + Duration::from_millis(60)),
            TriggerAction::Fired { erase: 1 },
            "最后一个触发键被吞，只需回删 1 个字符"
        );
        assert_eq!(m.armed_len(), 0);
    }

    #[test]
    fn timeout_breaks_the_sequence() {
        let mut m = TriggerMachine::new(TriggerConfig::default());
        let t = Instant::now();
        m.feed(semi(), t);
        assert_eq!(
            m.feed(semi(), t + Duration::from_millis(900)),
            TriggerAction::Pass
        );
        assert_eq!(m.armed_len(), 1);
    }

    #[test]
    fn other_key_resets() {
        let mut m = TriggerMachine::new(TriggerConfig::default());
        let t = Instant::now();
        m.feed(semi(), t);
        m.feed(keymap::keycode_for_char('a').unwrap(), t);
        assert_eq!(m.armed_len(), 0);
    }

    #[test]
    fn single_char_trigger_needs_no_erase() {
        let mut m = TriggerMachine::new(TriggerConfig::from_text("/", 800).unwrap());
        assert_eq!(
            m.feed(keymap::keycode_for_char('/').unwrap(), Instant::now()),
            TriggerAction::Fired { erase: 0 }
        );
    }

    #[test]
    fn config_rejects_non_ascii() {
        assert!(TriggerConfig::from_text("中文", 800).is_err());
        assert!(TriggerConfig::from_text("", 800).is_err());
        assert!(TriggerConfig::from_text("abcde", 800).is_err());
    }
}
