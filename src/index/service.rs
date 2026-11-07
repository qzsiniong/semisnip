//! 快照的持有与刷新：UI 永远只读 `Arc<Snapshot>`，重载在后台线程完成。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use arc_swap::ArcSwap;

use crate::app::event::Event;
use crate::index::loader::{SnippetSource, YamlSource};
use crate::index::model::Snapshot;
use crate::paths;

/// 索引服务：持有当前快照，并负责在后台重建它。
///
/// 三个角色共用一份实例（daemon 主循环、CLI、`doctor`），但只有 daemon 会
/// 起后台线程；CLI 走 `reload_blocking` 拿一份新鲜快照就退出。
///
/// 并发模型：读路径完全无锁（`ArcSwap` 一次原子读），写路径是"整个快照替换"。
/// 这样面板每帧读取候选时不会因为后台正在重建索引而卡顿。
///
/// **刻意不做"正在重建"的去重。** 两个触发源（文件监听、手动重建）撞在一起
/// 时最坏结果是多扫一遍磁盘，代价几百毫秒且发生在后台线程；而做去重就得回答
/// "被吞掉的那次要不要补一次"——答不好就会出现"点了重建索引但界面没变"，
/// 那是用户会当成 bug 的现象。宁可多干一次活。
pub struct IndexService {
    /// 当前快照。`ArcSwap` 而不是 `RwLock`：读多写极少，且读的时候不该有锁竞争。
    snap: ArcSwap<Snapshot>,
    /// 被监听的 espanso 根目录（`match/` 的父目录）。
    root: PathBuf,
}

impl IndexService {
    /// 创建服务，初始是一份空快照。
    ///
    /// 刻意**不**在这里读盘：构造发生在 App 启动时，`espanso --version` 之类的
    /// 子进程调用会把窗口拖到几百毫秒之后才出现。真正的首次加载由调用方决定时机
    /// （daemon 在 `lib.rs` 里异步触发，CLI 在用到时同步触发）。
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snap: ArcSwap::from_pointee(Snapshot::empty()),
            root: paths::espanso_root(),
        })
    }

    /// 取当前快照。`load_full` 会克隆 `Arc`（引用计数 +1），保证返回期间
    /// 即使后台替换了快照，这份数据也不会被释放。
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.load_full()
    }

    /// 同步重建；CLI 与 doctor 用。
    ///
    /// 失败时**保留旧快照**（只把时间戳推到当前），面板不会突然空掉——
    /// 用户改坏一个 YAML 不应该让整个面板变成"未读到片段"。
    /// 时间戳必须更新，否则前台会以为索引没变、继续用旧的检索表。
    pub fn reload_blocking(&self) -> Arc<Snapshot> {
        let source = YamlSource::new(self.root.clone());
        let snapshot = match source.load() {
            Ok(snapshot) => {
                log::info!(
                    "索引重建：{} 条片段，{} 条诊断，espanso 校验={}",
                    snapshot.len(),
                    snapshot.diagnostics.len(),
                    snapshot.espanso_checked
                );
                snapshot
            }
            Err(err) => {
                log::error!("索引重建失败：{err}");
                (*self.snap.load_full()).clone_for_failure()
            }
        };
        let arc = Arc::new(snapshot);
        self.snap.store(arc.clone());
        arc
    }

    /// 后台重建，完成后通知 UI 重算候选。
    ///
    /// 通知走事件通道而不是直接改 `AppState`：索引线程不该碰 gpui 的对象。
    /// `spawn` 失败只记日志——索引下次打开面板时还会重建，不是致命问题。
    pub fn reload_async(self: &Arc<Self>, tx: Sender<Event>) {
        let this = self.clone();
        std::thread::Builder::new()
            .name("semisnip-index".into())
            .spawn(move || {
                this.reload_blocking();
                let _ = tx.send(Event::IndexUpdated);
            })
            .ok();
    }

    /// 监听 espanso 配置目录：改完 YAML 存盘，面板下一次打开就是新的。
    ///
    /// 用 `notify` 的推荐后端（macOS 上是 FSEvents）。两个降级动作都只记 warn：
    /// 监听不可用就退化成"只在打开面板时重建"，功能不缺，只是不够即时。
    ///
    /// `recv_timeout` 那段是为了合并抖动：编辑器保存一个文件通常会连发好几个事件
    /// （截断、写入、改属性），300ms 窗口内的事件全部吞掉，只重建一次。
    pub fn spawn_watcher(self: &Arc<Self>, tx: Sender<Event>) {
        let root = self.root.clone();
        let this = self.clone();
        std::thread::Builder::new()
            .name("semisnip-watch".into())
            .spawn(move || {
                use notify::{RecursiveMode, Watcher};

                let (raw_tx, raw_rx) = std::sync::mpsc::channel();
                let mut watcher = match notify::recommended_watcher(raw_tx) {
                    Ok(watcher) => watcher,
                    Err(err) => {
                        log::warn!("文件监听不可用（改为仅手动刷新）：{err}");
                        return;
                    }
                };
                if let Err(err) = watcher.watch(&root.join("match"), RecursiveMode::Recursive) {
                    log::warn!("监听 {} 失败：{err}", root.display());
                    return;
                }
                log::info!("已监听 {}", root.join("match").display());

                loop {
                    // 阻塞等第一个事件，然后吞掉 300ms 内的抖动。
                    if raw_rx.recv().is_err() {
                        return;
                    }
                    while let Ok(_more) = raw_rx.recv_timeout(Duration::from_millis(300)) {}
                    log::info!("检测到配置变更，重建索引");
                    this.reload_blocking();
                    let _ = tx.send(Event::IndexUpdated);
                }
            })
            .ok();
    }
}

trait CloneForFailure {
    fn clone_for_failure(&self) -> Snapshot;
}

impl CloneForFailure for Snapshot {
    /// 重建失败时保留旧数据，只更新时间戳，避免面板突然空掉。
    fn clone_for_failure(&self) -> Snapshot {
        Snapshot {
            built_at_ms: crate::usage::now_ms(),
            config_root: self.config_root.clone(),
            snippets: self.snippets.clone(),
            by_trigger: self.by_trigger.clone(),
            diagnostics: self.diagnostics.clone(),
            espanso_known: self.espanso_known.clone(),
            espanso_checked: self.espanso_checked,
        }
    }
}
