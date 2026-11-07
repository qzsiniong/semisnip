//! 回传：把"选中的 trigger"交给 espanso 展开。
//!
//! 顺序很重要，而且只有一种正确顺序：
//!   1) 面板已经 orderOut（调用方负责），否则注入会打在面板上；
//!   2) 回删被吞掉的触发符（`erase` 个）；
//!   3) 拦住 espanso 的 TriggerCompensation：它执行 `match exec` 时会
//!      **按 trigger 全长回删字符**，不拦就会吃掉光标前的真实用户文本；
//!   4) `espanso match exec -t <trigger>` —— espanso 求值并注入到前台 App。
//!
//! 因为面板是非激活的，前台 App 全程没变，所以这里**不需要**还原焦点、
//! 也不需要 espanso 官方 search bar 那套 200ms 等待。

pub mod inject;

use std::time::Duration;

use crate::config::Config;
use crate::espanso::cli::EspansoCli;
use crate::index::model::Hit;

/// 一次回传的结局。只用于日志与降级判断，不驱动 UI（面板此时已经关了）。
#[derive(Debug)]
pub enum Outcome {
    /// espanso 已接管并展开。
    Expanded,
    /// espanso 不可用，退化成"安全渲染 + 粘贴"。
    Pasted,
    /// 两条路都失败。字符串是给日志用的原因说明。
    Failed(String),
}

/// 阻塞执行一次回传；调用方放在后台线程，别卡 UI。
///
/// 参数含义：
/// - `hit`：选中的片段与**实际要展开的 trigger**（可能是第二个触发符）；
/// - `erase`：文档里残留的触发符个数，来自状态机（tap 路径通常 1，单字符触发为 0）；
/// - `config`：`post_exec_delay_ms` 与 `allow_paste_fallback` 在这里生效；
/// - `fallback_text`：espanso 不可用时粘贴什么，由 `AppState::fallback_text` 提供。
///
/// 失败路径的处理顺序值得注意：先 `clear_synthetic_backspaces` 撤掉"预告"，
/// 再在 tap 离线时把补位空格退掉，最后才决定要不要粘贴。顺序反了会在文档里
/// 留下一串空格或吃掉用户文本。
pub fn confirm(hit: &Hit, erase: usize, config: &Config, fallback_text: Option<&str>) -> Outcome {
    let trigger_len = hit.trigger.chars().count();
    let tap_online = crate::trigger::tap::ready();

    // 1) 文档里只剩面板吞掉的那几个触发符，先删干净。
    if erase > 0 {
        inject::backspace(erase);
    }

    // 2) espanso 会按 trigger 全长回删（engine 的 TriggerCompensation）。
    //    tap 在线时直接吞掉这串合成退格；离线时没法拦截，只能用空格补位，
    //    让它的退格只吃掉补位，而不是用户的真实文本。
    if tap_online {
        crate::trigger::tap::swallow_synthetic_backspaces(trigger_len);
    } else if trigger_len > 0 {
        inject::type_spaces(trigger_len);
    }

    log::info!(
        "回传 {}：erase={erase} trigger_len={trigger_len} tap={tap_online}",
        hit.trigger
    );

    if config.post_exec_delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(config.post_exec_delay_ms));
    }

    let cli = EspansoCli::discover();
    match cli.exec(&hit.trigger) {
        Ok(()) => Outcome::Expanded,
        Err(err) => {
            // IPC 都没成功，就不会有补偿退格：撤掉预告，并收回补位空格。
            crate::trigger::tap::clear_synthetic_backspaces();
            if !tap_online && trigger_len > 0 {
                inject::backspace(trigger_len);
            }
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

/// 粘贴兜底：写剪贴板 → 等 15ms → 合成 `Cmd+V`。
///
/// 15ms 不是玄学：`pbcopy` 与粘贴事件之间必须有一个可观测的间隔，否则
/// 目标 App 收到 `Cmd+V` 时读到的还是旧内容（系统剪贴板是跨进程的异步同步）。
/// 这个函数**不恢复**用户原来的剪贴板——恢复需要在粘贴完成后等待，而那段等待
/// 无法可靠判定，反而会覆盖用户新复制的内容。这条路径只在 espanso 挂掉时走，
/// 用户已经处于"降级"状态，能粘出东西比保住剪贴板更重要。
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
