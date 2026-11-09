//! 触发：键码映射、触发状态机、全局键盘接管。
//!
//! - [`keymap`]：字符 ↔ macOS 虚拟键码。只认物理键码，不认字符；
//! - [`machine`]：判断"是否连续按出了触发串"。纯逻辑，可单测；
//! - [`tap`]：CGEventTap 的安装与回调。**平台专有**，非 macOS 下退化成通知降级。
//!
//! 三者串起来是一条链：tap 拿到键码 → machine 判断是否命中 → tap 投 `Event::Open`。

pub mod direct;
pub mod keymap;
pub mod machine;
pub mod tap;

pub use machine::{TriggerAction, TriggerConfig, TriggerMachine};
