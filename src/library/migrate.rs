//! 批量给存量触发词追加直达前缀。
//!
//! 设计上只做**追加**：`trigger: ":sig"` 变成 `triggers: [":sig", ",,sig"]`，
//! 旧触发词原样保留。这样迁移可回滚、不破坏既有肌肉记忆，也符合 semisnip
//! “增强层可拆卸”的定位。
//!
//! 写入用户 YAML 属于显式动作，仍然守住两条底线：
//! - 只处理非 package 文件；包目录视为只读。
//! - 每个文件先备份到 semisnip 自己的 `backups/<时间戳>/`，再原子替换。
//!
//! 代价是 YAML 注释会在 `serde_yaml` 回写时丢失——预览页必须把这一点写出来。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

use crate::index::model::Snapshot;
use crate::search::query::core_of;

use super::{SnippetKey, snippet_key};

/// 冲突原因分类。文案在 UI 层拼，这里只保留可判定的类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    PackageReadOnly,
    EmptyCore,
    NotAscii,
    DuplicateExisting,
    DuplicatePlan,
}

/// 一条待迁移项。`conflict` 为 `Some` 时不可勾选。
#[derive(Clone, Debug)]
pub struct MigrationPlan {
    pub key: SnippetKey,
    pub old_triggers: Vec<String>,
    pub new_trigger: String,
    pub conflict: Option<String>,
    pub conflict_kind: Option<ConflictKind>,
}

impl MigrationPlan {
    pub fn selectable(&self) -> bool {
        self.conflict.is_none()
    }
}

/// 扫描快照，生成迁移计划。冲突项也返回，只是不可勾选——用户需要看到
/// “为什么这条不能迁”，而不是被静默跳过。
pub fn plan(snapshot: &Snapshot, prefix: &str) -> Vec<MigrationPlan> {
    let mut plans = Vec::with_capacity(snapshot.snippets.len());
    let mut planned: HashSet<String> = HashSet::new();

    for snippet in &snapshot.snippets {
        // 已经有直达触发词：不需要迁移
        if snippet.direct_trigger(prefix).is_some() {
            continue;
        }
        let core = core_of(snippet.trigger());
        let new_trigger = format!("{prefix}{core}");

        let (conflict, conflict_kind) = if snippet.source.package.is_some() {
            (
                Some("package 文件只读，请在包里手动迁移".to_string()),
                Some(ConflictKind::PackageReadOnly),
            )
        } else if core.is_empty() {
            (
                Some("触发词没有可用的字母数字核心".to_string()),
                Some(ConflictKind::EmptyCore),
            )
        } else if core.chars().any(|c| !c.is_ascii() || c.is_whitespace()) {
            (
                Some("核心部分含空白或非 ASCII 字符".to_string()),
                Some(ConflictKind::NotAscii),
            )
        } else if snapshot.by_trigger.contains_key(&new_trigger) {
            (
                Some(format!("`{new_trigger}` 已存在")),
                Some(ConflictKind::DuplicateExisting),
            )
        } else if !planned.insert(new_trigger.clone()) {
            (
                Some(format!("与本次另一条迁移重复（`{new_trigger}`）")),
                Some(ConflictKind::DuplicatePlan),
            )
        } else {
            (None, None)
        };

        plans.push(MigrationPlan {
            key: snippet_key(snippet),
            old_triggers: snippet.triggers.clone(),
            new_trigger,
            conflict,
            conflict_kind,
        });
    }
    plans
}

/// 一次迁移的结果。失败不会中断其他文件：能写的先写，写不了的进 `errors`。
#[derive(Debug, Default)]
pub struct ApplyReport {
    pub files: usize,
    pub entries: usize,
    pub skipped: usize,
    pub backup_dir: Option<PathBuf>,
    pub errors: Vec<String>,
}

/// 应用迁移。只接受调用方已经筛过的计划；这里仍会防御性跳过冲突项。
pub fn apply(plans: &[MigrationPlan], config_root: &Path, backups_root: &Path) -> ApplyReport {
    let mut report = ApplyReport::default();
    let mut by_file: BTreeMap<PathBuf, Vec<&MigrationPlan>> = BTreeMap::new();
    for plan in plans {
        if !plan.selectable() {
            report.skipped += 1;
            continue;
        }
        by_file.entry(plan.key.0.clone()).or_default().push(plan);
    }
    if by_file.is_empty() {
        return report;
    }

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_dir = backups_root.join(&stamp);
    let mut manifest = String::new();

    for (file, items) in by_file {
        match apply_file(&file, &items, &backup_dir, config_root, &mut manifest) {
            Ok(count) => {
                report.files += 1;
                report.entries += count;
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

fn apply_file(
    file: &Path,
    items: &[&MigrationPlan],
    backup_dir: &Path,
    config_root: &Path,
    manifest: &mut String,
) -> anyhow::Result<usize> {
    let original = std::fs::read_to_string(file)?;
    let mut value: Value = serde_yaml::from_str(&original)?;
    let matches =
        matches_sequence_mut(&mut value).ok_or_else(|| anyhow::anyhow!("文件没有 matches 列表"))?;

    for plan in items {
        let ordinal = plan.key.1;
        let entry = matches
            .get_mut(ordinal)
            .ok_or_else(|| anyhow::anyhow!("文件已变化：第 {} 条不存在", ordinal + 1))?;
        let mapping = entry
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("第 {} 条不是映射", ordinal + 1))?;
        add_trigger(mapping, &plan.new_trigger, &plan.old_triggers)?;
    }

    // 备份 + 原子替换交给公共 helper：标签写回走完全同一套安全流程。
    write_with_backup(file, &value, backup_dir, config_root, manifest)?;
    Ok(items.len())
}

/// 备份到 `backup_dir`，再用「临时文件 + rename」原子替换原文件。
///
/// 迁移与标签写回共用。先备份再写是硬约束：`serde_yaml` 往返会丢注释，
/// 用户至少要能从 `backups/<时间戳>/` 里找回原样。
pub(crate) fn write_with_backup(
    file: &Path,
    value: &Value,
    backup_dir: &Path,
    config_root: &Path,
    manifest: &mut String,
) -> anyhow::Result<()> {
    // 备份：按相对路径拼文件名，避免不同目录的同名文件互相覆盖。
    std::fs::create_dir_all(backup_dir)?;
    let rel = file.strip_prefix(config_root).unwrap_or(file);
    let backup_name = rel.to_string_lossy().replace(['/', '\\'], "__");
    let backup = backup_dir.join(backup_name);
    std::fs::copy(file, &backup)?;
    manifest.push_str(&format!(
        "{}\t{}\t{}\n",
        file.display(),
        backup.display(),
        chrono::Local::now().to_rfc3339()
    ));

    // 原子替换：先写同目录临时文件，再 rename 覆盖。
    let rendered = serde_yaml::to_string(value)?;
    let file_name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "matches.yml".into());
    let temp = file.with_file_name(format!(".{file_name}.semisnip.tmp"));
    std::fs::write(&temp, rendered)?;
    if let Err(err) = std::fs::rename(&temp, file) {
        let _ = std::fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok(())
}

/// 找到文件顶层的 matches 序列，兼容 `matches:` 与裸列表两种写法。
pub(crate) fn matches_sequence_mut(value: &mut Value) -> Option<&mut Vec<Value>> {
    match value {
        Value::Sequence(seq) => Some(seq),
        Value::Mapping(map) => map.get_mut("matches")?.as_sequence_mut(),
        _ => None,
    }
}

/// 给一条 match 追加触发词：优先扩展现有 `triggers`，只有 `trigger` 时平移到列表。
fn add_trigger(
    mapping: &mut Mapping,
    new_trigger: &str,
    fallback: &[String],
) -> anyhow::Result<()> {
    let mut from_trigger: Vec<String> = Vec::new();
    // 两种写法同时出现时，以 triggers 里的为准，但把 trigger 里的值并进来。
    if let Some(value) = mapping.remove("trigger") {
        match value {
            Value::String(s) => from_trigger.push(s),
            Value::Sequence(seq) => {
                for item in seq {
                    let s = item
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("trigger 列表里有非字符串项"))?;
                    from_trigger.push(s.to_string());
                }
            }
            _ => return Err(anyhow::anyhow!("trigger 字段既不是字符串也不是列表")),
        }
    }

    if let Some(value) = mapping.get_mut("triggers") {
        let seq = value
            .as_sequence_mut()
            .ok_or_else(|| anyhow::anyhow!("triggers 字段不是列表"))?;
        // trigger 的值放前面，保持“主 trigger 仍是第一个”的直觉。
        for (offset, trigger) in from_trigger.into_iter().enumerate() {
            seq.insert(offset, Value::String(trigger));
        }
        seq.push(Value::String(new_trigger.to_string()));
    } else {
        let old = if from_trigger.is_empty() {
            fallback.to_vec()
        } else {
            from_trigger
        };
        let seq: Vec<Value> = old
            .iter()
            .map(|s| Value::String(s.clone()))
            .chain(std::iter::once(Value::String(new_trigger.to_string())))
            .collect();
        mapping.insert(Value::String("triggers".into()), Value::Sequence(seq));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::loader::{SnippetSource, YamlSource};

    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("semisnip-migrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("match")).unwrap();
        dir
    }

    fn snapshot(root: &Path) -> Snapshot {
        YamlSource {
            root: root.to_path_buf(),
            cli: crate::espanso::cli::EspansoCli::discover(),
            reconcile: false,
        }
        .load()
        .unwrap()
    }

    #[test]
    fn plan_adds_prefix_and_flags_conflicts() {
        let root = temp_root("plan");
        std::fs::write(
            root.join("match/base.yml"),
            "matches:\n  - trigger: \":sig\"\n    replace: \"sig\"\n  - trigger: \"::sig\"\n    replace: \"dup\"\n  - triggers: [\":a\", \":b\"]\n    replace: \"x\"\n",
        )
        .unwrap();
        let snap = snapshot(&root);

        let plans = plan(&snap, ",,");
        assert_eq!(plans.len(), 3);
        assert!(plans[0].selectable());
        assert_eq!(plans[0].new_trigger, ",,sig");
        // `::sig` 的核心也是 sig，与第 1 条的计划重复
        assert_eq!(plans[1].conflict_kind, Some(ConflictKind::DuplicatePlan));
        assert!(plans[2].selectable());
        assert_eq!(plans[2].new_trigger, ",,a");
    }

    #[test]
    fn apply_appends_and_backs_up() {
        let root = temp_root("apply");
        let file = root.join("match/base.yml");
        std::fs::write(
            &file,
            "matches:\n  - trigger: \":sig\"\n    replace: \"sig\"\n  - triggers: [\":a\", \":b\"]\n    replace: \"x\"\n",
        )
        .unwrap();
        let snap = snapshot(&root);
        let backups = root.join("backups");
        let plans = plan(&snap, ",,");
        let report = apply(&plans, &root, &backups);

        assert_eq!(report.entries, 2, "{:?}", report.errors);
        assert_eq!(report.files, 1);
        assert!(report.errors.is_empty());

        let raw = std::fs::read_to_string(&file).unwrap();
        let mut back: Value = serde_yaml::from_str(&raw).unwrap();
        let matches = matches_sequence_mut(&mut back).unwrap();
        let first = matches[0].as_mapping().unwrap();
        let triggers: Vec<&str> = first
            .get("triggers")
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(triggers, [":sig", ",,sig"], "{raw}");
        let second = matches[1].as_mapping().unwrap();
        let triggers: Vec<&str> = second
            .get("triggers")
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(triggers, [":a", ":b", ",,a"], "{raw}");
        assert!(report.backup_dir.unwrap().exists());
    }

    #[test]
    fn apply_survives_missing_ordinal() {
        let root = temp_root("missing");
        std::fs::write(
            root.join("match/base.yml"),
            "matches:\n  - trigger: \":sig\"\n    replace: \"sig\"\n",
        )
        .unwrap();
        let snap = snapshot(&root);
        let plans = plan(&snap, ",,");
        // 模拟文件在索引之后被改短
        std::fs::write(root.join("match/base.yml"), "matches: []\n").unwrap();
        let report = apply(&plans, &root, &root.join("backups"));
        assert_eq!(report.entries, 0);
        assert!(!report.errors.is_empty());
    }
}
