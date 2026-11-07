//! 使用频率与最近使用时间。
//!
//! 只用于排序加权，丢了不影响正确性；因此写入失败只记日志。
//! key 是 `trigger`——同一 trigger 在多个文件里重复时共用一份统计。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 单条 trigger 的统计。
///
/// 两个字段都不设上限——`count` 用 `saturating_add` 防溢出，排序时再压到 60
/// 封顶（见 `search::score::boost`），这样既有长期积累的区分度，又不会让
/// 一条老片段永远霸榜。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    /// 累计使用次数。
    pub count: u32,
    /// 最后一次使用的时间戳（毫秒，0 表示从未用过）。
    pub last_used_ms: i64,
}

/// 全部统计。序列化到 `usage.json`，字段名即文件格式，改名等于破坏兼容。
///
/// `dirty` 不落盘（`#[serde(skip)]`）：它只是"自上次写入以来有没有变化"的标记，
/// 重启后从文件读到的状态天然是干净的。
#[derive(Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    entries: HashMap<String, Entry>,
    #[serde(skip)]
    dirty: bool,
}

impl Usage {
    /// 从磁盘读取。文件不存在、读不了、或 JSON 坏了都退化成空统计——
    /// 这份数据只是排序加权，没有它功能照常。
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// 取某条 trigger 的统计；没记录过时返回全零（而不是 `Option`），
    /// 让调用方不必到处解包。
    pub fn get(&self, trigger: &str) -> Entry {
        self.entries.get(trigger).copied().unwrap_or_default()
    }

    /// 记一次使用。只改内存并置 `dirty`，落盘由 `flush_if_dirty` 负责——
    /// 之所以分开，是因为将来可能想在批量场景里合并写入。
    pub fn record(&mut self, trigger: &str) {
        let now = now_ms();
        let entry = self.entries.entry(trigger.to_string()).or_default();
        entry.count = entry.count.saturating_add(1);
        entry.last_used_ms = now;
        self.dirty = true;
    }

    /// 只在有变更时落盘，避免每次回车都写文件。
    ///
    /// 写失败时**保持 `dirty`**，下一次还会再试；同时只记 warn 不返回错误，
    /// 因为调用点是"用户按了回车"这种不能失败的地方。
    /// 先建父目录：首次使用时 `~/Library/Application Support/semisnip` 可能还不存在。
    pub fn flush_if_dirty(&mut self, path: &Path) {
        if !self.dirty {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec_pretty(&self) {
            Ok(bytes) => {
                if let Err(err) = std::fs::write(path, bytes) {
                    log::warn!("使用记录写入失败：{err}");
                } else {
                    self.dirty = false;
                }
            }
            Err(err) => log::warn!("使用记录序列化失败：{err}"),
        }
    }
}

/// 当前时间戳（毫秒）。
///
/// 时钟回拨或系统时间未设置时 `duration_since` 会失败，返回 0 —— 调用方用它做
/// 新旧比较，0 的语义是"很久以前"，排序上等价于"没记录过"，不会出错。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_count() {
        let mut usage = Usage::default();
        usage.record(";;log");
        usage.record(";;log");
        assert_eq!(usage.get(";;log").count, 2);
        assert!(usage.get(";;log").last_used_ms > 0);
    }
}
