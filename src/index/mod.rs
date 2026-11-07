//! 索引：读 espanso 的 YAML，产出一份不可变快照。
//!
//! - [`dto`]：YAML 的反序列化形状（宽容，认不全也不报错）；
//! - [`loader`]：把 dto 归一成 [`model::Snippet`]，并做 espanso 交叉校验；
//! - [`model`]：上层唯一认的数据结构；
//! - [`service`]：持有当前快照，负责在后台重建与文件监听。
//!
//! 对外只暴露 `Snapshot` / `Snippet` / `Hit` 三个类型——上层不需要知道 YAML，
//! 也不需要知道 notifier 的存在。

pub mod dto;
pub mod loader;
pub mod model;
pub mod service;

pub use model::{Hit, Snapshot, Snippet};
