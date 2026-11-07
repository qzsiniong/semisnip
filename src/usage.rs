//! 使用频率与最近使用时间。
//!
//! 只用于排序加权，丢了不影响正确性；因此写入失败只记日志。
//! key 是 `trigger`——同一 trigger 在多个文件里重复时共用一份统计。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    pub count: u32,
    pub last_used_ms: i64,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    entries: HashMap<String, Entry>,
    #[serde(skip)]
    dirty: bool,
}

impl Usage {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn get(&self, trigger: &str) -> Entry {
        self.entries.get(trigger).copied().unwrap_or_default()
    }

    pub fn record(&mut self, trigger: &str) {
        let now = now_ms();
        let entry = self.entries.entry(trigger.to_string()).or_default();
        entry.count = entry.count.saturating_add(1);
        entry.last_used_ms = now;
        self.dirty = true;
    }

    /// 只在有变更时落盘，避免每次回车都写文件。
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
