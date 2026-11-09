//! 把选中的片段导出成一份独立的 espanso YAML。
//!
//! 不重新拼装 `Snippet`，而是按 `(文件, 序号)` 回到源文件复制原始 mapping。
//! 这样 `app_filter` / `propagate_case` / `force_mode` 这些没有建模的字段
//! 也不会在导出时悄悄消失。代价同样是注释不保留。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_yaml::Value;

use crate::index::model::Snapshot;

use super::SnippetKey;

#[derive(Debug, Default)]
pub struct ExportReport {
    pub file: Option<PathBuf>,
    pub entries: usize,
    pub skipped: Vec<String>,
}

/// 导出到 `out_dir`。只要有一条成功写出就算成功；完全没写出来返回错误。
pub fn export(
    _snapshot: &Snapshot,
    keys: &[SnippetKey],
    out_dir: &Path,
) -> anyhow::Result<ExportReport> {
    let mut report = ExportReport::default();
    let mut by_file: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for key in keys {
        by_file.entry(key.0.clone()).or_default().push(key.1);
    }
    if by_file.is_empty() {
        return Err(anyhow::anyhow!("没有选择片段"));
    }

    let mut copied: Vec<Value> = Vec::new();
    for (file, ordinals) in by_file {
        let raw = match std::fs::read_to_string(&file) {
            Ok(raw) => raw,
            Err(err) => {
                report
                    .skipped
                    .push(format!("{}：读取失败（{err}）", file.display()));
                continue;
            }
        };
        let mut value: Value = match serde_yaml::from_str(&raw) {
            Ok(value) => value,
            Err(err) => {
                report
                    .skipped
                    .push(format!("{}：解析失败（{err}）", file.display()));
                continue;
            }
        };
        let Some(matches) = matches_sequence_mut(&mut value) else {
            report
                .skipped
                .push(format!("{}：没有 matches 列表", file.display()));
            continue;
        };
        for ordinal in ordinals {
            match matches.get(ordinal) {
                Some(entry) => copied.push(entry.clone()),
                None => report.skipped.push(format!(
                    "{}：第 {} 条已不存在",
                    file.display(),
                    ordinal + 1
                )),
            }
        }
    }

    if copied.is_empty() {
        return Err(anyhow::anyhow!(
            "没有可导出的片段：{}",
            report.skipped.join("；")
        ));
    }

    // 快照里可能有重复 key，这里按出现次数算；UI 的选中集合本身不会重复。
    std::fs::create_dir_all(out_dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = out_dir.join(format!("semisnip-export-{stamp}.yml"));
    let mut root = serde_yaml::Mapping::new();
    root.insert(
        Value::String("matches".into()),
        Value::Sequence(copied.clone()),
    );
    std::fs::write(&path, serde_yaml::to_string(&Value::Mapping(root))?)?;

    report.entries = copied.len();
    report.file = Some(path);
    Ok(report)
}

fn matches_sequence_mut(value: &mut Value) -> Option<&mut Vec<Value>> {
    match value {
        Value::Sequence(seq) => Some(seq),
        Value::Mapping(map) => map.get_mut("matches")?.as_sequence_mut(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::loader::{SnippetSource, YamlSource};

    #[test]
    fn export_copies_only_selected_and_keeps_unknown_keys() {
        let root = std::env::temp_dir().join(format!("semisnip-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("match")).unwrap();
        let file = root.join("match/base.yml");
        std::fs::write(
            &file,
            "matches:\n  - trigger: \":one\"\n    replace: \"1\"\n    app_filter: [\"chrome\"]\n  - trigger: \":two\"\n    replace: \"2\"\n",
        )
        .unwrap();
        let snap = YamlSource {
            root: root.clone(),
            cli: crate::espanso::cli::EspansoCli::discover(),
            reconcile: false,
        }
        .load()
        .unwrap();

        let key = (file.clone(), 0usize);
        let report = export(&snap, &[key], &root.join("exports")).unwrap();
        assert_eq!(report.entries, 1);
        let raw = std::fs::read_to_string(report.file.unwrap()).unwrap();
        assert!(raw.contains(":one"), "{raw}");
        assert!(!raw.contains(":two"), "{raw}");
        assert!(raw.contains("app_filter"), "{raw}");
    }
}
