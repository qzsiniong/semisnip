//! 标签写回：只改 label 字段，其余字段原样保留。
//!
//! 标签是**用户可见的 YAML 数据**，所以这是本项目里少有的"写用户文件"路径。
//! 安全流程与迁移完全一致（见 [`super::migrate::write_with_backup`]）：
//! 先备份到 `backups/<时间戳>/`，再写同目录临时文件并 rename 覆盖。
//!
//! 写入语义：追加到 label 尾部、去重（不区分大小写）。
//! 绝不能"重写整条 match"——espanso 的字段很多，我们只认识其中一部分，
//! 往返一次就可能丢掉不认识的键。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_yaml::Value;

use super::SnippetKey;
use super::migrate::{ApplyReport, matches_sequence_mut, write_with_backup};
use crate::index::model::append_tags;

/// 一条待写回的标签目标：定位 + 要追加的标签。
#[derive(Clone, Debug)]
pub struct TagTarget {
    pub key: SnippetKey,
    pub tags: Vec<String>,
}

/// 给多条片段追加标签。
///
/// 失败不会中断其他文件：能写的先写，写不了的进 `errors`。
/// 标签没变化（全部已存在）的条目计入 `skipped`，不会产生无意义的备份。
pub fn append(targets: &[TagTarget], config_root: &Path, backups_root: &Path) -> ApplyReport {
    let mut report = ApplyReport::default();
    let mut by_file: BTreeMap<PathBuf, Vec<&TagTarget>> = BTreeMap::new();
    for target in targets {
        if target.tags.is_empty() {
            report.skipped += 1;
            continue;
        }
        by_file
            .entry(target.key.0.clone())
            .or_default()
            .push(target);
    }
    if by_file.is_empty() {
        return report;
    }

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_dir = backups_root.join(&stamp);
    let mut manifest = String::new();

    for (file, items) in by_file {
        match append_file(
            &file,
            &items,
            &backup_dir,
            config_root,
            &mut manifest,
            &mut report,
        ) {
            Ok(written) => {
                report.files += 1;
                report.entries += written;
            }
            Err(err) => report.errors.push(format!("{}：{err}", file.display())),
        }
    }

    if report.entries > 0 {
        if let Err(err) = std::fs::create_dir_all(&backup_dir) {
            report.errors.push(format!("备份目录创建失败：{err}"));
        } else {
            if let Err(err) = std::fs::write(backup_dir.join("manifest.tsv"), manifest) {
                report.errors.push(format!("备份清单写入失败：{err}"));
            }
            report.backup_dir = Some(backup_dir);
        }
    }
    report
}

fn append_file(
    file: &Path,
    items: &[&TagTarget],
    backup_dir: &Path,
    config_root: &Path,
    manifest: &mut String,
    report: &mut ApplyReport,
) -> anyhow::Result<usize> {
    let original = std::fs::read_to_string(file)?;
    let mut value: Value = serde_yaml::from_str(&original)?;
    let matches =
        matches_sequence_mut(&mut value).ok_or_else(|| anyhow::anyhow!("文件没有 matches 列表"))?;

    let mut written = 0usize;
    for target in items {
        let ordinal = target.key.1;
        let Some(entry) = matches.get_mut(ordinal) else {
            report.skipped += 1;
            report
                .errors
                .push(format!("{}：第 {} 条已不存在", file.display(), ordinal + 1));
            continue;
        };
        let Some(mapping) = entry.as_mapping_mut() else {
            report.skipped += 1;
            continue;
        };
        let old = mapping
            .get(Value::String("label".into()))
            .and_then(Value::as_str)
            .unwrap_or("");
        let new_label = append_tags(old, &target.tags);
        if new_label == old {
            report.skipped += 1;
            continue;
        }
        mapping.insert(Value::String("label".into()), Value::String(new_label));
        written += 1;
    }

    if written > 0 {
        write_with_backup(file, &value, backup_dir, config_root, manifest)?;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("semisnip-tags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn appends_tags_and_keeps_other_fields() {
        let dir = temp_dir("append");
        let file = dir.join("matches.yml");
        std::fs::write(
            &file,
            "matches:\n  - trigger: \";;log\"\n    label: \"日志 :dev\"\n    replace: \"tail -f\"\n",
        )
        .unwrap();

        let target = TagTarget {
            key: (file.clone(), 0),
            tags: vec!["work".into(), "DEV".into()],
        };
        let report = append(&[target], &dir, &dir.join("backups"));
        assert_eq!(report.entries, 1);
        assert!(report.backup_dir.is_some());

        let raw = std::fs::read_to_string(&file).unwrap();
        let value: Value = serde_yaml::from_str(&raw).unwrap();
        let matches = value.get("matches").and_then(Value::as_sequence).unwrap();
        let entry = &matches[0];
        let label = entry.get("label").and_then(Value::as_str).unwrap();
        assert_eq!(label, "日志 :dev :work");
        // 其他字段原样保留
        assert_eq!(
            entry.get("replace").and_then(Value::as_str),
            Some("tail -f")
        );
    }

    #[test]
    fn existing_tags_are_deduped_case_insensitively() {
        let dir = temp_dir("dedupe");
        let file = dir.join("matches.yml");
        std::fs::write(
            &file,
            "matches:\n  - trigger: \"x\"\n    label: \"T :Dev\"\n",
        )
        .unwrap();
        let report = append(
            &[TagTarget {
                key: (file, 0),
                tags: vec!["dev".into()],
            }],
            &dir,
            &dir.join("backups"),
        );
        assert_eq!(report.entries, 0);
        assert_eq!(report.skipped, 1);
        assert!(report.backup_dir.is_none());
    }

    #[test]
    fn missing_ordinal_is_skipped_without_touching_the_file() {
        let dir = temp_dir("missing");
        let file = dir.join("matches.yml");
        std::fs::write(&file, "matches:\n  - trigger: \"x\"\n").unwrap();
        let before = std::fs::read_to_string(&file).unwrap();
        let report = append(
            &[TagTarget {
                key: (file.clone(), 9),
                tags: vec!["dev".into()],
            }],
            &dir,
            &dir.join("backups"),
        );
        assert_eq!(report.entries, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }
}
