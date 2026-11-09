//! 直达触发词的滚动缓冲检测。
//!
//! espanso 原生展开不会通知 semisnip，所以“毕业率”只能靠观察用户**打出了**
//! 哪条直达触发词来估算。这里只做纯字符串后缀匹配：tap 线程把最近输入字符
//! 压进一个很小的环形缓冲，命中就投一次事件。
//!
//! 两条边界：
//! - 只保留 24 个字符。再长也不会多命中，反而让每次按键多扫一遍。
//! - 取**最长**后缀。`,,log` 与 `,,log2` 同时存在时，打 `,,log2` 不该记到
//!   `,,log` 头上。

use std::collections::VecDeque;

/// 缓冲容量。够覆盖“前缀 + 合理长度的触发词”，又不至于让比较变重。
pub const MAX_CHARS: usize = 24;

#[derive(Default)]
pub struct DirectBuffer {
    chars: VecDeque<char>,
}

impl DirectBuffer {
    pub fn push(&mut self, ch: char) {
        self.chars.push_back(ch);
        while self.chars.len() > MAX_CHARS {
            self.chars.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.chars.clear();
    }

    /// 返回缓冲尾部命中的最长直达触发词。
    ///
    /// 大小写不敏感：espanso 的 trigger 通常大小写敏感，但统计层多一点误差，
    /// 比漏记好——这项数据只用于展示毕业率。
    pub fn detect(&self, triggers: &[String]) -> Option<String> {
        let tail: String = self.chars.iter().collect::<String>().to_lowercase();
        let mut best: Option<&String> = None;
        for trigger in triggers {
            let lowered = trigger.to_lowercase();
            if tail.ends_with(&lowered) {
                match best {
                    Some(current) if current.len() >= trigger.len() => {}
                    _ => best = Some(trigger),
                }
            }
        }
        best.cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_longest_suffix() {
        let mut buffer = DirectBuffer::default();
        for ch in ",,log2".chars() {
            buffer.push(ch);
        }
        let triggers = vec![",,log".to_string(), ",,log2".to_string()];
        assert_eq!(buffer.detect(&triggers), Some(",,log2".to_string()));
    }

    #[test]
    fn ignores_unrelated_text_and_is_case_insensitive() {
        let mut buffer = DirectBuffer::default();
        for ch in "hello ,,SIG".chars() {
            buffer.push(ch);
        }
        assert_eq!(
            buffer.detect(&[",,sig".to_string()]),
            Some(",,sig".to_string())
        );
        assert_eq!(buffer.detect(&[",,date".to_string()]), None);
    }

    #[test]
    fn buffer_is_bounded() {
        let mut buffer = DirectBuffer::default();
        for _ in 0..MAX_CHARS + 10 {
            buffer.push('x');
        }
        assert_eq!(buffer.chars.len(), MAX_CHARS);
    }
}
