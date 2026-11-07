//! 回传：把"选中的 trigger"交给 espanso 展开。
//!
//! 顺序很重要，而且只有一种正确顺序：
//!   1) 面板已经 orderOut（调用方负责），否则注入会打在面板上；
//!   2) 回删被吞掉的触发符（`erase` 个）；
//!   3) `espanso match exec -t <trigger>` —— espanso 求值并注入到前台 App。
//!
//! 因为面板是非激活的，前台 App 全程没变，所以这里**不需要**还原焦点、
//! 也不需要 espanso 官方 search bar 那套 200ms 等待。

pub mod inject;

use std::time::Duration;

use crate::config::Config;
use crate::espanso::cli::EspansoCli;
use crate::index::model::Hit;

#[derive(Debug)]
pub enum Outcome {
    /// espanso 已接管并展开。
    Expanded,
    /// espanso 不可用，退化成"安全渲染 + 粘贴"。
    Pasted,
    Failed(String),
}

/// 阻塞执行一次回传；调用方放在后台线程，别卡 UI。
pub fn confirm(hit: &Hit, erase: usize, config: &Config, fallback_text: Option<&str>) -> Outcome {
    log::info!("回传 {}：回删 {erase} 个字符", hit.trigger);
    if erase > 0 {
        inject::backspace(erase);
    }
    if config.post_exec_delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(config.post_exec_delay_ms));
    }

    let cli = EspansoCli::discover();
    match cli.exec(&hit.trigger) {
        Ok(()) => Outcome::Expanded,
        Err(err) => {
            log::warn!("match exec 失败：{err}");
            if !config.allow_paste_fallback {
                return Outcome::Failed(err.to_string());
            }
            let Some(text) = fallback_text.filter(|text| !text.trim().is_empty()) else {
                return Outcome::Failed(err.to_string());
            };
            match paste(text) {
                Ok(()) => Outcome::Pasted,
                Err(paste_err) => {
                    log::error!("粘贴兜底也失败：{paste_err}");
                    Outcome::Failed(format!("{err}；粘贴兜底失败：{paste_err}"))
                }
            }
        }
    }
}

fn paste(text: &str) -> anyhow::Result<()> {
    inject::set_clipboard(text)?;
    std::thread::sleep(Duration::from_millis(15));
    inject::paste();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_is_debug_printable() {
        // 只是防止枚举被误改成非 Debug（日志要用）
        let _ = format!("{:?}", Outcome::Expanded);
    }
}
