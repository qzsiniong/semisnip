//! 触发状态机：识别「连续输入触发串」。
//!
//! 与 snippets 的实现有一处关键差异：**命中时把最后一个触发键也吞掉**。
//!
//! 因为 semisnip 的 tap 可以吞键，第一次 `;` 已经落到目标文档里（不能吞，
//! 用户可能只打一个分号），第二次 `;` 直接吞掉，于是文档里只剩 1 个字符，
//! `erase` 恒等于 `keys.len() - 1`。这样面板出现的瞬间就不会有漏键，
//! 也不存在"过滤串进了目标 App"的竞态。

use std::time::{Duration, Instant};

use super::keymap;

/// 两个触发键之间的默认最大间隔（毫秒）。
///
/// 800ms 是量出来的：正常连打两个 `;` 约 80–150ms，慢速打字也在 400ms 内；
/// 再放宽会把"打完一句分号、过一会儿又打一个分号"误判成触发。
pub const DEFAULT_TIMEOUT_MS: u64 = 800;

/// 触发串的编译结果：把用户写的字符串变成键码序列。
///
/// 只存键码不存字符，是因为判定发生在 CGEventTap 的 `keyDown` 回调里，
/// 那一刻能拿到的只有物理键码——中文输入法组合期、Shift 变换都拿不到字符。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerConfig {
    /// 按顺序必须命中的键码。
    pub keycodes: Vec<u32>,
    /// 相邻两次按键的最大间隔（毫秒）。
    pub timeout_ms: u64,
}

impl Default for TriggerConfig {
    /// 默认触发串 `;;`。`expect` 在编译期常量上不会失败（`;` 一定在映射表里）。
    fn default() -> Self {
        Self {
            keycodes: vec![keymap::keycode_for_char(';').expect("';' 有键码"); 2],
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl TriggerConfig {
    /// 把配置里的字符串编译成键码序列。
    ///
    /// 三条限制：非空、≤4 字符、纯 ASCII。理由分别是：空的会让 tap 永远判定不命中；
    /// 太长的触发串在真实打字里几乎不可能连续出现，且每次都要维护更长的状态；
    /// 非 ASCII 无法映射到物理键码（`keycode_for_char` 的边界）。
    ///
    /// `timeout_ms` 被夹到 200–3000：低于 200 连打都跟不上，高于 3 秒等于没有超时，
    /// 用户隔十几秒打两个分号也会触发。
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

/// 喂一个键之后状态机给出的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerAction {
    /// 放行，不做拦截。
    ///
    /// 注意"部分命中"（`;;` 只打了一个 `;`）也返回 `Pass`：第一个分号必须留在
    /// 文档里，用户随时可能只是打个分号而不再按第二个。
    Pass,
    /// 命中：调用方要吞掉这个键。
    ///
    /// `erase` 是**已经落进目标文档**的触发符个数——除了最后一个触发键（这个
    /// 键被吞了），前面的都写进文档了。同一个数字有两个用途：确认时回删
    /// （给 espanso 一个干净的光标位置），取消时拿它算出还缺几个字（补齐）。
    /// 单字符触发串（如 `/`）的 `erase` 是 0：一个字符都没落下。
    Fired { erase: usize },
}

/// 触发状态机。
///
/// 只有三个字段：编译好的配置、当前已连续命中几个键、上一次命中的时刻。
/// 没有定时器线程——超时在 `feed` 里惰性判定，空闲时不占任何资源。
pub struct TriggerMachine {
    config: TriggerConfig,
    /// 已连续命中的前缀长度（0 表示没在序列中）。
    armed: usize,
    /// 上一次按键的时刻，用于超时判定。
    last: Option<Instant>,
}

impl TriggerMachine {
    /// 用给定配置新建。状态从"没有前缀"开始。
    pub fn new(config: TriggerConfig) -> Self {
        Self {
            config,
            armed: 0,
            last: None,
        }
    }

    /// 清空进行中的序列。
    ///
    /// 三处会用到：命中之后（必须复位，否则下次 `;` 会接着旧计数）、
    /// 超时之后、以及面板开关时（避免用户按 Esc 关窗后残留半截状态）。
    pub fn reset(&mut self) {
        self.armed = 0;
        self.last = None;
    }

    /// 当前已命中的前缀长度。给 UI/日志用（测试里也用它断言状态）。
    pub fn armed_len(&self) -> usize {
        self.armed
    }

    /// 喂一个 keyDown 键码（keyUp、重复键、带修饰键的事件由调用方过滤）。
    ///
    /// 逻辑分支只有三条：
    /// 1. 与期望的下一个键相同 → 前进；走完全程则 `Fired`；
    /// 2. 不匹配但等于首键 → 当作新序列的起点（覆盖 `;;;` 这种重叠输入，
    ///    否则第三个分号会被当成"不匹配"而复位，用户连打三下反而没有反应）；
    /// 3. 其他 → 复位并放行。
    ///
    /// 超时用"懒判定"：只在有按键进来时比较时间戳。这样不需要后台定时器，
    /// 也不会因为系统休眠导致 `Instant` 跳变而误判（`duration_since` 用的是单调时钟）。
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
