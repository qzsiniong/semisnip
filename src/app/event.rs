//! 进程间消息：tap 线程 / UDS 线程 → UI 线程的唯一入口。

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
    TapUnavailable(String),
    Shutdown,
}
