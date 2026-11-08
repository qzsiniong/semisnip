//! 回传：把"选中的 trigger"交给 espanso 展开。
//!
//! 顺序很重要，而且只有一种正确顺序：
//!   1) 面板已经 orderOut（调用方负责），否则注入会打在面板上；
//!   2) 回删落进文档的触发符（`erase` 个——触发键不吞，见 `trigger::machine`）；
//!   3) 打 `trigger_len` 个**占位空格**；
//!   4) `espanso match exec -t <trigger>` —— espanso 求值，并按 trigger 全长
//!      回删字符（TriggerCompensation）——删掉的恰好是那串占位空格——
//!      然后把展开文本注入到前台 App。
//!
//! 为什么用占位空格、而不是把 espanso 的补偿退格"吞掉"：补偿退格到达的时机
//! 不可预估。espanso 的注入管线是串行的，前面还压着一轮慢展开（shell 变量一跑
//! 几百毫秒）时，这一轮的退格可能 800ms 之后才来。早期版本用"时间窗口 + 数量"
//! 的预算去猜哪串退格属于自己，espanso 原生触发（`:shell`）与面板确认交替使用
//! 时前后两轮在时间上重叠，预算互相误伤：原生轮的退格被吞 → 文档留半截触发词
//! （"删除不足"）；展开文本一到达又把预算清空 → 真正的补偿退格放行 → 删掉
//! 光标前的真实字符（"展开结果与预期不符"）。占位空格不依赖任何时序假设：
//! 退格无论何时来、属于哪一轮，删的都是恰好 `trigger_len` 个可丢弃的空格。
//! tap 那边配套的规则是"外部进程合成的事件一律放行"（`trigger::tap`）。
//!
//! 因为面板是非激活的，前台 App 全程没变，所以这里**不需要**还原焦点、
//! 也不需要 espanso 官方 search bar 那套 200ms 等待。
//!
//! 取消面板**不经过这里**：触发串的键从未被吞（见 `trigger::machine`），
//! 文档里本来就是用户打的 `;;`，取消是纯关闭。历史上这里有个反方向的
//! `keep_trigger`（把被吞的触发键补回文档），不吞键之后整个消失了——
//! 如果哪天要恢复"吞键"设计，记得连同触发静默窗口一起，否则单字符触发串
//! 会「补回 → 立即命中 → 面板又开」死循环。
//!
//! 序列本身抽在 [`confirm_with`] 里，系统动作经 [`Backend`] 注入：生产走
//! [`RealBackend`]，端到端单测（`sim_tests`）走假后端——两路共用同一份顺序
//! 逻辑，改坏顺序测试会当场红。

pub mod inject;

use std::time::Duration;

use crate::config::Config;
use crate::espanso::cli::EspansoCli;
use crate::index::model::Hit;

/// 回传序列里所有「要碰系统」的动作的抽象。
///
/// 抽出来只有一个目的：让 `confirm_with` 的**顺序逻辑**可以被端到端单测验证
/// （见 `sim_tests`）——那套顺序（先删残留、再垫占位空格、失败先退空格再兜底）
/// 是全项目最容易改坏的部分，而它依赖的三个系统服务（CGEvent 合成、espanso
/// CLI、剪贴板）在测试进程里要么没权限要么不存在。
///
/// 实现约定：所有方法都是"发出去就算完成"，不做任何时序假设——补偿退格
/// 什么时候到、属于哪一轮，是 espanso 的事（见模块注释）。
pub trait Backend {
    /// 回删 `count` 个字符。
    fn backspace(&mut self, count: usize);
    /// 打出 `count` 个占位空格。
    fn type_spaces(&mut self, count: usize);
    /// `espanso match exec -t <trigger>`。`Ok` 表示 espanso 已接管（它会自己
    /// 按 trigger 全长补退格 + 注入展开文本）；`Err` 表示连 IPC 都没成功，
    /// 不会有任何补偿退格到来——占位空格要自己收走。
    fn exec_match(&mut self, trigger: &str) -> anyhow::Result<()>;
    /// 把文本放进系统剪贴板（粘贴兜底第一步）。
    fn set_clipboard(&mut self, text: &str) -> anyhow::Result<()>;
    /// 合成 `Cmd+V`（粘贴兜底第二步）。
    fn paste(&mut self);
}

/// 生产后端：CGEvent 合成键盘事件 + espanso CLI 子进程。
///
/// 无状态——每条命令起一个新进程（见 `EspansoCli`），事件合成自带 source，
/// 会被 tap 的「外部事件一律放行」规则放行（见 `trigger::tap`）。
pub struct RealBackend;

impl Backend for RealBackend {
    fn backspace(&mut self, count: usize) {
        inject::backspace(count);
    }

    fn type_spaces(&mut self, count: usize) {
        inject::type_spaces(count);
    }

    fn exec_match(&mut self, trigger: &str) -> anyhow::Result<()> {
        EspansoCli::discover().exec(trigger)
    }

    fn set_clipboard(&mut self, text: &str) -> anyhow::Result<()> {
        inject::set_clipboard(text)
    }

    fn paste(&mut self) {
        inject::paste();
    }
}

/// 一次回传的结局。只用于日志与降级判断，不驱动 UI（面板此时已经关了）。
///
/// `PartialEq` 给测试用（`sim_tests` 要断言结局）。
#[derive(Debug, PartialEq, Eq)]
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
/// - `erase`：文档里残留的触发符个数，来自状态机（触发串全长，见 `trigger::machine`；
///   espanso 降级路径是降级触发串长度，菜单栏 / 手动唤起为 0）；
/// - `config`：`post_exec_delay_ms` 与 `allow_paste_fallback` 在这里生效；
/// - `fallback_text`：espanso 不可用时粘贴什么，由 `AppState::fallback_text` 提供。
pub fn confirm(hit: &Hit, erase: usize, config: &Config, fallback_text: Option<&str>) -> Outcome {
    confirm_with(hit, erase, config, fallback_text, &mut RealBackend)
}

/// [`confirm`] 的可注入版本：逻辑完全相同，系统动作走 `backend`。
///
/// 单独存在的原因见 [`Backend`]。生产调用方一律走 [`confirm`]，这个函数只给
/// 测试用——把它和 `confirm` 并排读，就是为了确认两条路的顺序一字不差。
///
/// 失败路径的处理顺序值得注意：`match exec` 连 IPC 都没成功就不会有补偿退格，
/// 占位空格原封不动留在文档里，要先退掉它们、再决定要不要粘贴。顺序反了会
/// 在文档里留下一串空格。
pub fn confirm_with(
    hit: &Hit,
    erase: usize,
    config: &Config,
    fallback_text: Option<&str>,
    backend: &mut dyn Backend,
) -> Outcome {
    let trigger_len = hit.trigger.chars().count();

    // 1) 删掉文档里的触发符残留（触发键不吞，它们真的在文档里）。
    if erase > 0 {
        backend.backspace(erase);
    }

    // 2) 占位空格：给 espanso 的补偿退格预备恰好 trigger_len 个可删字符。
    //    详见模块注释——不要试图"吞掉"这些退格来省掉空格，时序上猜不准。
    if trigger_len > 0 {
        backend.type_spaces(trigger_len);
    }

    log::info!("回传 {}：erase={erase} pad={trigger_len}", hit.trigger);

    if config.post_exec_delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(config.post_exec_delay_ms));
    }

    match backend.exec_match(&hit.trigger) {
        Ok(()) => Outcome::Expanded,
        Err(err) => {
            // IPC 都没成功，就没有补偿退格来收走占位空格：自己退掉。
            if trigger_len > 0 {
                backend.backspace(trigger_len);
            }
            log::warn!("match exec 失败：{err}");
            if !config.allow_paste_fallback {
                return Outcome::Failed(err.to_string());
            }
            let Some(text) = fallback_text.filter(|text| !text.trim().is_empty()) else {
                return Outcome::Failed(err.to_string());
            };
            match paste_with(backend, text) {
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
fn paste_with(backend: &mut dyn Backend, text: &str) -> anyhow::Result<()> {
    backend.set_clipboard(text)?;
    std::thread::sleep(Duration::from_millis(15));
    backend.paste();
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
