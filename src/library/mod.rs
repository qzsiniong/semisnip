//! 片段本地库：停用状态与拼音别名。
//!
//! 这一层**只写 semisnip 自己的 `library.json`**，绝不把字段偷偷塞回用户的
//! espanso YAML。停用是"从面板与直达统计里隐藏"，不是删除；别名只参与搜索。
//! key 与 `usage.json` 一致，用主 `trigger`：同一 trigger 在多处定义时共用状态。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod export;
pub mod migrate;
pub mod tags;

/// 片段在磁盘上的稳定定位：文件 + 文件内序号（0 基）。
///
/// 不用自增 id：索引重建时它会变，而管理面板的选中状态要跨重载保留。
pub type SnippetKey = (std::path::PathBuf, usize);

/// 从片段取稳定定位。
pub fn snippet_key(snippet: &crate::index::model::Snippet) -> SnippetKey {
    (snippet.source.file.clone(), snippet.source.ordinal)
}

/// 本地库文件。字段名即磁盘格式，改名等于破坏兼容。
///
/// `dirty` 不落盘：它只表示"自上次写入以来有没有变化"，重启后从文件读到的
/// 状态天然是干净的。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Library {
    /// 被停用的主 trigger。
    #[serde(default)]
    disabled: BTreeSet<String>,
    /// trigger → 别名（拼音、英文缩写等）。本轮只读展示，可手改文件。
    #[serde(default)]
    aliases: BTreeMap<String, Vec<String>>,
    #[serde(skip)]
    dirty: bool,
}

impl Library {
    /// 从磁盘读取。文件不存在、读不了或 JSON 坏了都退化成空库——
    /// 这份数据只是过滤与搜索辅助，没有它功能照常。
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn is_disabled(&self, trigger: &str) -> bool {
        self.disabled.contains(trigger)
    }

    pub fn set_disabled(&mut self, trigger: &str, disabled: bool) {
        let changed = if disabled {
            self.disabled.insert(trigger.to_string())
        } else {
            self.disabled.remove(trigger)
        };
        if changed {
            self.dirty = true;
        }
    }

    /// 返回切换后的状态（true = 已停用），方便 UI 直接提示。
    pub fn toggle_disabled(&mut self, trigger: &str) -> bool {
        let disabled = !self.is_disabled(trigger);
        self.set_disabled(trigger, disabled);
        disabled
    }

    pub fn aliases_for(&self, trigger: &str) -> &[String] {
        self.aliases
            .get(trigger)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn disabled_len(&self) -> usize {
        self.disabled.len()
    }

    /// 只在有变更时落盘；写失败保持 `dirty`，下次再试。
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
                    log::warn!("本地库写入失败：{err}");
                } else {
                    self.dirty = false;
                }
            }
            Err(err) => log::warn!("本地库序列化失败：{err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("semisnip-library-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bad_file_falls_back_to_empty_library() {
        let dir = temp_dir("bad");
        let path = dir.join("library.json");
        std::fs::write(&path, "{ not json").unwrap();
        let lib = Library::load(&path);
        assert!(!lib.is_disabled(":sig"));
        assert_eq!(lib.disabled_len(), 0);
    }

    #[test]
    fn disabled_can_be_toggled_and_flushed() {
        let dir = temp_dir("toggle");
        let path = dir.join("library.json");
        let mut lib = Library::default();
        assert!(lib.toggle_disabled(":sig"));
        assert!(lib.is_disabled(":sig"));
        lib.flush_if_dirty(&path);

        let mut back = Library::load(&path);
        assert!(back.is_disabled(":sig"));
        assert!(!back.toggle_disabled(":sig"));
        back.flush_if_dirty(&path);
        assert!(!Library::load(&path).is_disabled(":sig"));
    }

    #[test]
    fn aliases_are_keyed_by_primary_trigger() {
        let dir = temp_dir("aliases");
        let path = dir.join("library.json");
        std::fs::write(
            &path,
            r#"{"disabled":[":sig"],"aliases":{":sig":["签名","qianming"]}}"#,
        )
        .unwrap();
        let lib = Library::load(&path);
        assert_eq!(lib.aliases_for(":sig"), ["签名", "qianming"]);
        assert!(lib.aliases_for("unknown").is_empty());
    }
}
