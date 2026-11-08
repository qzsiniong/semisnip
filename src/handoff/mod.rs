//! 回传：把"选中的 trigger"交给 espanso 展开。
//!
//! 顺序很重要，而且只有一种正确顺序：
//!   1) 面板已经 orderOut（调用方负责），否则注入会打在面板上；
//!   2) 回删落进文档的触发符（`erase` 个——触发键不吞，见 `trigger::machine`）；
//!   3) 打 `trigger_len` 个**占位字符**（用哪种由 `config::Placeholder` 决定）；
//!   4) `espanso match exec -t <trigger>` —— espanso 求值，并按 trigger 全长
//!      回删字符（TriggerCompensation）——删掉的恰好是那串占位字符——
//!      然后把展开文本注入到前台 App。
//!
//! 为什么必须有占位符：espanso 的补偿退格**没有关闭入口**，它一定按"被选中
//! 片段 trigger 的字符数"发退格（`espanso-engine/src/process/middleware/action.rs`），
//! 删的是光标左边的字符。所以确认前必须放好同样多的可删字符，否则被删的就是
//! 用户正文。为什么占位符要可配置：空格在少数场景会被目标 App 改写（tab 化、
//! 行尾 trim、输入法上屏键），换个字符就能绕开——五种取值的代价与坑见
//! `docs/troubleshooting.md` 的「占位符」一节。
//!
//! 为什么用占位符、而不是把 espanso 的补偿退格"吞掉"：补偿退格到达的时机
//! 不可预估。espanso 的注入管线是串行的，前面还压着一轮慢展开（shell 变量一跑
//! 几百毫秒）时，这一轮的退格可能 800ms 之后才来。早期版本用"时间窗口 + 数量"
//! 的预算去猜哪串退格属于自己，espanso 原生触发（`:shell`）与面板确认交替使用
//! 时前后两轮在时间上重叠，预算互相误伤：原生轮的退格被吞 → 文档留半截触发词
//! （"删除不足"）；展开文本一到达又把预算清空 → 真正的补偿退格放行 → 删掉
//! 光标前的真实字符（"展开结果与预期不符"）。占位符不依赖任何时序假设：
//! 退格无论何时来、属于哪一轮，删的都是恰好 `trigger_len` 个可丢弃的字符。
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

use crate::config::{Config, Placeholder};
use crate::espanso::cli::EspansoCli;
use crate::index::model::Hit;

/// 回传序列里所有「要碰系统」的动作的抽象。
///
/// 抽出来只有一个目的：让 `confirm_with` 的**顺序逻辑**可以被端到端单测验证
/// （见 `sim_tests`）——那套顺序（先删残留、再垫占位串、失败先退占位再兜底）
/// 是全项目最容易改坏的部分，而它依赖的三个系统服务（CGEvent 合成、espanso
/// CLI、剪贴板）在测试进程里要么没权限要么不存在。
///
/// 实现约定：所有方法都是"发出去就算完成"，不做任何时序假设——补偿退格
/// 什么时候到、属于哪一轮，是 espanso 的事（见模块注释）。
pub trait Backend {
    /// 回删 `count` 个字符。
    fn backspace(&mut self, count: usize);
    /// 打出占位串（内容由 [`placeholder_text`] 生成，长度必须恰好等于
    /// espanso 将要回删的字符数）。
    fn type_placeholder(&mut self, text: &str);
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

    fn type_placeholder(&mut self, text: &str) {
        inject::type_placeholder(text);
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
    /// 交给 espanso 的**被动匹配**：触发串已经打出去了，它会自己命中、自己
    /// 回删、自己注入。semisnip 拿不到成败——只有 `placeholder = trigger` 会
    /// 走到这里（见 [`confirm_with`]）。
    Delegated,
    /// espanso 不可用，退化成"安全渲染 + 粘贴"。
    Pasted,
    /// 两条路都失败。字符串是给日志用的原因说明。
    Failed(String),
}

/// 生成回传前的占位串：字符数必须**恰好**等于 espanso 将要回删的数量。
///
/// 为什么长度是 `count` 而不是"差不多"：espanso 的补偿退格是定长的
/// （`trigger.chars().count()`，见模块注释）。少一个字符，它就往前多吃一个
/// 真实字符；多一个，就会留下尾巴。
///
/// 五种内容的取舍见 `docs/troubleshooting.md` 的「占位符」一节，这里只说两条
/// 与实现强相关的：
/// - `Trigger` 模式返回触发串本身，**并且不走 `match exec`**：espanso 只跳过
///   自己注入的事件，我们合成的按键在它眼里就是"用户又打了一遍触发串"，
///   于是它会自己命中并展开。再调 `match exec` 就是同一段文字展开两次
///   （见 [`confirm_with`] 里的分支）。
/// - `None` 模式返回空串，但 espanso 仍会回删 `count` 个字符 —— 等于每次展开
///   吃掉光标前的 `count` 个真实字符。
pub fn placeholder_text(config: &Config, trigger: &str, count: usize) -> String {
    if count == 0 {
        return String::new();
    }
    match config.placeholder {
        Placeholder::Space => " ".repeat(count),
        Placeholder::ZeroWidth => "\u{200b}".repeat(count),
        Placeholder::Custom => {
            match crate::config::validate_placeholder_char(&config.placeholder_char) {
                Ok(c) => c.to_string().repeat(count),
                Err(err) => {
                    // 配置层已经校验过，走到这里说明用户手改了 YAML。宁可退回
                    // 空格（已知能用），也不要打一串数量不明的字符。
                    log::warn!("占位字符不合法（{err}），本次改用空格");
                    " ".repeat(count)
                }
            }
        }
        Placeholder::None => String::new(),
        // 触发串模式由 espanso 自己命中，本函数只负责给出"要打的文本"，
        // 不参与 `match exec` 那条路（见 `confirm_with`）。
        Placeholder::Trigger => trigger.to_string(),
    }
}

/// 面板收起之后、注入之前的等待（`post_exec_delay_ms`）。
///
/// 两条回传路径（委托 espanso 自己命中 / 调 `match exec`）都要等这一下：
/// 少了它，键盘事件可能在面板还没 orderOut 完就发出去，被打在面板上。
fn sleep_before_exec(config: &Config) {
    if config.post_exec_delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(config.post_exec_delay_ms));
    }
}

/// 阻塞执行一次回传；调用方放在后台线程，别卡 UI。
///
/// 参数含义：
/// - `hit`：选中的片段与**实际要展开的 trigger**（可能是第二个触发符）；
/// - `erase`：文档里残留的触发符个数，来自状态机（触发串全长，见 `trigger::machine`；
///   espanso 降级路径是降级触发串长度，菜单栏 / 手动唤起为 0）；
/// - `config`：`post_exec_delay_ms`、`placeholder` 与 `allow_paste_fallback`
///   在这里生效；
/// - `fallback_text`：espanso 不可用时粘贴什么，由 `AppState::fallback_text` 提供。
pub fn confirm(hit: &Hit, erase: usize, config: &Config, fallback_text: Option<&str>) -> Outcome {
    confirm_with(hit, erase, config, fallback_text, &mut RealBackend)
}

/// [`confirm`] 的可注入版本：逻辑完全相同，系统动作走 `backend`。
///
/// 单独存在的原因见 [`Backend`]。生产调用方一律走 [`confirm`]，这个函数只给
/// 测试用——把它和 `confirm` 并排读，就是为了确认两条路的顺序一字不差。
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

    // 2) 先决定这次怎么回传：委托 espanso 自己命中，还是垫串 + `match exec`。
    //    判断逻辑全在 [`plan`]（纯函数，可单测），这里只负责按计划执行。
    match plan(config, &hit.trigger, trigger_len) {
        Plan::Delegate(text) => {
            // 打完就收工：触发串一旦出现在文档里，espanso 的被动匹配就会自己命中、
            // 自己按 trigger 全长回删、自己注入——与 `match exec` 的补偿完全等价。
            // 此时再调 `match exec` 就是同一段文字展开两遍（见 [`plan`]）。
            sleep_before_exec(config);
            backend.type_placeholder(&text);
            log::info!("回传 {text}：已打出触发串，交给 espanso 被动命中（不调 match exec）");
            Outcome::Delegated
        }
        Plan::Pad(pad) => {
            let pad_len = pad.chars().count();
            if pad_len > 0 {
                backend.type_placeholder(&pad);
            }

            log::info!(
                "回传 {}：erase={erase} pad={pad_len} placeholder={:?}",
                hit.trigger,
                config.placeholder
            );
            if config.placeholder == Placeholder::None && trigger_len > 0 {
                // 这条偏好目前只有研究/将来兼容的价值，误选了要能一眼看出来。
                log::warn!(
                    "placeholder=none：espanso 仍会回删 {trigger_len} 个字符，光标前的正文会被吃掉"
                );
            }

            sleep_before_exec(config);
            exec(backend, hit, config, fallback_text, pad_len)
        }
    }
}

/// 这一次回传走哪条路。
#[derive(Debug)]
enum Plan {
    /// 把 `text` 打进文档就收工——espanso 的被动匹配会自己命中并展开。
    Delegate(String),
    /// 先垫 `pad`（字符数必须等于片段的 trigger 长度），再调 `match exec`。
    Pad(String),
}

/// 决定回传路径与占位串内容。
///
/// 三条规则，按优先级：
/// 1. `placeholder = trigger` 且触发串是**纯 ASCII** → 委托 espanso 自己命中；
/// 2. 同上但触发串含非 ASCII → 不能委托，退回空格（见下）；
/// 3. 其余模式 → 按 [`placeholder_text`] 生成。
///
/// 为什么非 ASCII 不能委托：espanso 靠键盘事件里的字符做匹配，而 semisnip 打
/// 非 ASCII 只能走 unicode 注入（键码 0）。它认不认得出这是"用户打的字"取决于
/// 版本与输入法：认不出，触发串就留在文档里；认得出，又会和 `match exec` 双打。
/// 两头都不靠谱，所以退回空格——空格绝不可能被当成触发串。
fn plan(config: &Config, trigger: &str, count: usize) -> Plan {
    if config.placeholder == Placeholder::Trigger && !trigger.is_empty() {
        if trigger.is_ascii() {
            return Plan::Delegate(trigger.to_string());
        }
        log::warn!(
            "placeholder=trigger 但触发串「{trigger}」含非 ASCII 字符，espanso 未必认得出合成按键；本次退回空格"
        );
        return Plan::Pad(" ".repeat(count));
    }
    Plan::Pad(placeholder_text(config, trigger, count))
}

/// 垫完占位串之后的那一步：`match exec`，失败再兜底。
///
/// 失败路径的处理顺序值得注意：`match exec` 连 IPC 都没成功就不会有补偿退格，
/// 占位串原封不动留在文档里，要先退掉它们、再决定要不要粘贴。顺序反了会
/// 在文档里留下一串占位字符。
fn exec(
    backend: &mut dyn Backend,
    hit: &Hit,
    config: &Config,
    fallback_text: Option<&str>,
    pad_len: usize,
) -> Outcome {
    match backend.exec_match(&hit.trigger) {
        Ok(()) => Outcome::Expanded,
        Err(err) => {
            // IPC 都没成功，就没有补偿退格来收走占位串：自己退掉。
            if pad_len > 0 {
                backend.backspace(pad_len);
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

    /// 造一份指定占位模式的配置，其余字段用默认值。
    fn with(mode: Placeholder, custom: &str) -> Config {
        Config {
            placeholder: mode,
            placeholder_char: custom.into(),
            ..Config::default()
        }
    }

    #[test]
    fn each_placeholder_mode_yields_the_right_length() {
        // 长度是第一约束：espanso 按 trigger 字符数发退格，差一个就吃正文
        assert_eq!(
            placeholder_text(&with(Placeholder::Space, ""), ":shell", 6),
            "      "
        );
        assert_eq!(
            placeholder_text(&with(Placeholder::ZeroWidth, ""), ":shell", 6),
            "\u{200b}".repeat(6)
        );
        assert_eq!(
            placeholder_text(&with(Placeholder::Custom, "·"), "sh", 2),
            "··"
        );
        // 自定义字符本身是零宽也照用（用户明确选的）
        assert_eq!(
            placeholder_text(&with(Placeholder::Custom, "\u{200b}"), "sh", 2),
            "\u{200b}\u{200b}"
        );
        // 不补：返回空串，调用方据此跳过注入与失败回退
        assert_eq!(
            placeholder_text(&with(Placeholder::None, ""), ":shell", 6),
            ""
        );
    }

    #[test]
    fn trigger_mode_returns_the_trigger_itself() {
        // 这个模式不靠"字符错位"防二次命中，而是干脆不调 `match exec`
        // （见 `confirm_with`）：打出去的触发串就是让 espanso 自己命中的。
        let pad = placeholder_text(&with(Placeholder::Trigger, ""), ":shell", 6);
        assert_eq!(pad, ":shell");
        assert_eq!(pad.chars().count(), 6, "长度仍须精确");
    }

    #[test]
    fn plan_delegates_only_for_ascii_triggers() {
        let trigger_mode = with(Placeholder::Trigger, "");
        // 纯 ASCII：交给 espanso 自己命中，一个字符都不多打
        match plan(&trigger_mode, "sh", 2) {
            Plan::Delegate(text) => assert_eq!(text, "sh"),
            Plan::Pad(pad) => panic!("ASCII 触发串应该委托，实际垫了 {pad:?}"),
        }
        // 非 ASCII：espanso 未必认得出合成按键，退回空格 + match exec
        match plan(&trigger_mode, ":中文", 3) {
            Plan::Pad(pad) => assert_eq!(pad, "   "),
            Plan::Delegate(text) => panic!("非 ASCII 不该委托，实际打了 {text:?}"),
        }
        // 空触发串：没有可委托的东西
        assert!(matches!(plan(&trigger_mode, "", 0), Plan::Pad(_)));
        // 其余模式一律走垫串
        assert!(matches!(
            plan(&with(Placeholder::Space, ""), "sh", 2),
            Plan::Pad(_)
        ));
        assert!(matches!(
            plan(&with(Placeholder::ZeroWidth, ""), "sh", 2),
            Plan::Pad(_)
        ));
    }

    #[test]
    fn illegal_custom_char_falls_back_to_spaces() {
        // 手改 YAML 绕过校验的情况：宁可打空格，也不要打数量不明的字符
        let config = with(Placeholder::Custom, "");
        assert_eq!(placeholder_text(&config, "sh", 2), "  ");
    }
}
