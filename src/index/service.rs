//! 快照的持有与刷新：UI 永远只读 `Arc<Snapshot>`，重载在后台线程完成。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use arc_swap::ArcSwap;

use crate::app::event::Event;
use crate::espanso::paths;
use crate::index::loader::{SnippetSource, YamlSource};
use crate::index::model::Snapshot;

pub struct IndexService {
    snap: ArcSwap<Snapshot>,
    reloading: AtomicBool,
    root: PathBuf,
}

impl IndexService {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snap: ArcSwap::from_pointee(Snapshot::empty()),
            reloading: AtomicBool::new(false),
            root: paths::config_root(),
        })
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.load_full()
    }

    /// 同步重建；CLI 与 doctor 用。
    pub fn reload_blocking(&self) -> Arc<Snapshot> {
        self.reloading.store(true, Ordering::SeqCst);
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
        self.reloading.store(false, Ordering::SeqCst);
        arc
    }

    /// 后台重建，完成后通知 UI 重算候选。
    pub fn reload_async(self: &Arc<Self>, tx: Sender<Event>) {
        let this = self.clone();
        std::thread::Builder::new()
            .name("snipshot-index".into())
            .spawn(move || {
                this.reload_blocking();
                let _ = tx.send(Event::IndexUpdated);
            })
            .ok();
    }

    /// 监听 espanso 配置目录：改完 YAML 存盘，面板下一次打开就是新的。
    pub fn spawn_watcher(self: &Arc<Self>, tx: Sender<Event>) {
        let root = self.root.clone();
        let this = self.clone();
        std::thread::Builder::new()
            .name("snipshot-watch".into())
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
