//! 进程间通信：CLI ↔ daemon 的 NDJSON over Unix domain socket。
//!
//! - [`proto`]：线协议（请求/响应枚举与版本号）；
//! - [`client`]：CLI 侧，负责连接、失败时自愈拉起 daemon；
//! - [`server`]：daemon 侧，负责监听、单实例判定、把请求转成事件。
//!
//! 两侧**不共享任何状态**，只有这条 socket。故意不用 HTTP：本地通信要的只是
//! "一问一答 + 低延迟"，多一层协议栈换不来任何东西。

pub mod client;
pub mod proto;
pub mod server;
