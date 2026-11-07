//! 进程间消息：tap 线程 / UDS 线程 / UI 窗口 → UI 线程的唯一入口。

use std::sync::mpsc::Sender;

use gpui_kit::{App, Global};

use crate::ipc::proto::Origin;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyMsg {
    pub code: u32,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub command: bool,
}

impl KeyMsg {
    /// 带 Cmd/Ctrl 的组合键不归面板消费：放行并关面板，避免把系统快捷键吞掉。
    pub fn is_shortcut(&self) -> bool {
        self.command || self.control
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// 打开面板。`erase` 是被自己吞掉、需要回删的字符数。
    Open {
        origin: Origin,
        erase: usize,
    },
    Key(KeyMsg),
    Cancel,
    /// 索引快照更新，重新计算候选。
    IndexUpdated,
    /// 请求从磁盘重建索引。
    Reload,
    /// 配置文件刚被某个窗口改写，重新读一次并应用。
    ///
    /// 设置窗口自己就能改配置，但它改完之后**面板还活在旧值里**：
    /// `AppState.config` 是启动时读的一份拷贝。所以每次落盘都要投这个事件，
    /// 由 scheduler 在 UI 线程里换掉那份拷贝——顺带处理触发键换键码这类
    /// 必须通知 tap 线程的副作用。
    ConfigChanged,
    TapUnavailable(String),
    Shutdown,
}

/// 全局事件发送端。
///
/// 窗口（设置 / 引导）也要能投事件，但它们不是被 scheduler 创建的，
/// 拿不到那条 `Sender`；把发送端放进全局，任何 `&mut App` 都能取一份。
/// 这里存的是 `Sender` 而不是 `Receiver`，因此可以随意 clone、跨线程用。
pub struct GlobalTx(pub Sender<Event>);

impl Global for GlobalTx {}

/// 取一份事件发送端。没装（比如单测里）就返回 `None`，调用方自己决定忽略还是报错。
pub fn tx(cx: &App) -> Option<Sender<Event>> {
    cx.try_global::<GlobalTx>().map(|tx| tx.0.clone())
}
