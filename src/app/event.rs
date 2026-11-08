//! 进程间消息：tap 线程 / UDS 线程 / UI 窗口 → UI 线程的唯一入口。

use std::sync::mpsc::Sender;

use gpui_kit::{App, Global};

use crate::ipc::proto::Origin;

/// 一次按键的完整描述（由 tap 线程解析后投递过来）。
///
/// 之所以连修饰键一起带过来而不是只给字符：面板需要判断"这是不是系统快捷键"，
/// 而那个判断必须看原始修饰键——`Cmd+V` 与 `v` 是同一个键码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyMsg {
    /// macOS 虚拟键码（见 `trigger::keymap`）。
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

/// 跨线程消息。**所有**对 UI 状态的修改都由这些消息驱动，
/// 没有任何线程会绕过事件通道直接改 `AppState`。
#[derive(Clone, Debug)]
pub enum Event {
    /// 打开面板。`erase` 是触发词里落进文档的字符数：tap 路径 = 触发串全长
    /// （触发键不吞，见 `trigger::machine`），手动唤起 = 0。
    ///
    /// **espanso 降级路径也是 0**：那份降级 match 只让 espanso 擦掉触发串、
    /// 不再注回（见 `cli::render_match_file`），所以面板出现时文档里已经干净。
    /// 之前这里按"降级触发串长度"回删，前提是 replace 会把 `;;;` 注回文档——
    /// 那个设计让 espanso 的注入和面板的回删抢同一个光标位置，已经废掉。
    Open { origin: Origin, erase: usize },
    /// 面板里的一次按键。只由 tap 线程产生。
    Key(KeyMsg),
    /// 收起面板（Esc、关闭按钮、菜单项都汇聚到这里）。
    Cancel,
    /// 点了面板窗口外面（面板开着时，由 tap 的鼠标监听判定）。
    ///
    /// 与 [`Event::Cancel`] 分开是因为**收尾动作不同**：点击会把前台窗口换掉，
    /// 之后任何合成按键都可能落到新窗口里。不过取消已经不合成任何按键了
    /// （触发键不吞，文档无需修复），两条路实际都落在纯关闭上；分开保留是
    /// 给将来"收尾要动文档"的改动留位置。
    ClickOutside,
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
    /// 键盘 tap 装不上（原因见字符串），面板要切到授权引导页。
    ///
    /// 与 `ConfigChanged` 一样来自"别的线程"：看门狗线程检测到切换时投递。
    TapUnavailable(String),
    /// 退出进程。先关面板再退，避免 launchd 看到"异常退出"而立刻重启。
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
