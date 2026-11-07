//! 从 espanso 的 YAML 树构建快照。
//!
//! 加载规则对齐 espanso 2.4：递归读 `$CONFIG/match/**/[!_]*.{yml,yaml}`，
//! `_` 开头的文件不自动加载（`_manifest.yml` 靠这个排除）。
//! 与 espanso 的差异只有一处：我们不解析 app-specific 配置，那部分交给
//! `match list --json` 的交叉校验兜底。

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::espanso::cli::{AppFilter, EspansoCli};
use crate::index::dto::{MatchFile, RawFormField, RawMatch};
use crate::index::model::{
    Body, Diagnostic, FormField, Snapshot, Snippet, SourceRef, VarKind, VarSpec,
};
use crate::usage;

pub trait SnippetSource {
    fn load(&self) -> anyhow::Result<Snapshot>;
}

pub struct YamlSource {
    pub root: PathBuf,
    pub cli: EspansoCli,
    /// 是否跑一次 CLI 做交叉校验（测试里关掉，避免依赖 espanso 进程）。
    pub reconcile: bool,
}

impl YamlSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            cli: EspansoCli::discover(),
            reconcile: true,
        }
    }
}

impl SnippetSource for YamlSource {
    fn load(&self) -> anyhow::Result<Snapshot> {
        let match_dir = self.root.join("match");
        let mut files = Vec::new();
        collect_yaml(&match_dir, &mut files);
        files.sort();

        let mut snippets: Vec<Arc<Snippet>> = Vec::new();
        let mut diagnostics = Vec::new();

        for file in &files {
            match std::fs::read_to_string(file) {
                Ok(raw) => match parse_file(&raw) {
                    Ok(matches) => {
                        for (ordinal, raw_match) in matches.into_iter().enumerate() {
                            if let Some(snippet) = build_snippet(file, ordinal, raw_match) {
                                snippets.push(Arc::new(snippet));
                            }
                        }
                    }
                    Err(err) => diagnostics.push(Diagnostic::ParseFailed {
                        file: file.display().to_string(),
                        error: err,
                    }),
                },
                Err(err) => diagnostics.push(Diagnostic::ParseFailed {
                    file: file.display().to_string(),
                    error: err.to_string(),
                }),
            }
        }

        // 让常见 trigger 排在前面：同分时 Map 的遍历顺序不影响排序稳定性，
        // 因为打分里还有 usage 与长度做次级键。
        let mut by_trigger: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, snippet) in snippets.iter().enumerate() {
            for trigger in &snippet.triggers {
                by_trigger.entry(trigger.clone()).or_default().push(index);
            }
        }

        for (trigger, indexes) in &by_trigger {
            if indexes.len() > 1 {
                diagnostics.push(Diagnostic::DuplicateTrigger {
                    trigger: trigger.clone(),
                    count: indexes.len(),
                });
            }
        }

        // 交叉校验：espanso 眼中的 trigger 全集。
        let (espanso_known, espanso_checked) = if self.reconcile {
            match self.cli.list_json(None::<&AppFilter>) {
                Ok(list) => {
                    let known: HashSet<String> =
                        list.into_iter().flat_map(|m| m.triggers).collect();
                    (known, true)
                }
                Err(err) => {
                    log::warn!("交叉校验失败（按全部可用处理）：{err}");
                    (HashSet::new(), false)
                }
            }
        } else {
            (HashSet::new(), false)
        };

        if espanso_checked {
            for snippet in &mut snippets {
                let usable = snippet.triggers.iter().any(|t| espanso_known.contains(t));
                if !usable {
                    if let Some(trigger) = snippet.triggers.first() {
                        diagnostics.push(Diagnostic::UnknownToEspanso {
                            trigger: trigger.clone(),
                        });
                    }
                }
                if let Some(inner) = Arc::get_mut(snippet) {
                    inner.usable = usable;
                }
            }
        }

        Ok(Snapshot {
            built_at_ms: usage::now_ms(),
            config_root: self.root.clone(),
            snippets,
            by_trigger,
            diagnostics,
            espanso_known,
            espanso_checked,
        })
    }
}

/// 递归收集 YAML；`_` 开头的文件与 espanso 一样跳过。
fn collect_yaml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_yaml(&path, out);
            continue;
        }
        if name.starts_with('_') {
            continue;
        }
        let is_yaml = name.ends_with(".yml") || name.ends_with(".yaml");
        if is_yaml {
            out.push(path);
        }
    }
}

fn parse_file(raw: &str) -> Result<Vec<RawMatch>, String> {
    if let Ok(file) = serde_yaml::from_str::<MatchFile>(raw) {
        return Ok(file.matches);
    }
    // 少数手写文件会直接给一个顶层列表；再给一次机会，别整份丢掉。
    serde_yaml::from_str::<Vec<RawMatch>>(raw).map_err(|err| err.to_string())
}

fn build_snippet(file: &Path, ordinal: usize, raw: RawMatch) -> Option<Snippet> {
    let mut triggers = raw.triggers.clone().unwrap_or_default();
    if let Some(field) = raw.trigger.clone() {
        triggers.extend(field.into_vec());
    }
    if let Some(regex) = &raw.regex {
        // 正则 trigger 直接打字命不中，但用户仍可能想看它；用原串占位。
        triggers.push(format!("/{regex}/"));
    }
    triggers.retain(|t| !t.trim().is_empty());
    triggers.dedup();
    if triggers.is_empty() {
        return None;
    }

    let body = if let Some(text) = raw.replace.clone() {
        Body::Text(text)
    } else if let Some(md) = raw.markdown.clone() {
        Body::Markdown(md)
    } else if let Some(html) = raw.html.clone() {
        Body::Html(html)
    } else if let Some(path) = raw.image_path.clone() {
        Body::Image(resolve_image_path(file, &path))
    } else if let Some(layout) = raw.form.clone() {
        Body::Form(layout)
    } else {
        Body::Text(String::new())
    };

    let label = raw
        .label
        .clone()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .or_else(|| {
            let first = body.as_text().lines().next().unwrap_or("").trim();
            (!first.is_empty()).then(|| truncate(first, 60))
        })
        .unwrap_or_else(|| triggers[0].clone());

    let vars: Vec<VarSpec> = raw
        .vars
        .iter()
        .map(|v| VarSpec {
            name: v.name.clone(),
            kind: VarKind::parse(&v.kind),
            params: v.params.clone(),
        })
        .collect();

    let form_fields: Vec<FormField> = raw
        .form_fields
        .as_ref()
        .map(|map| {
            map.iter()
                .map(|(name, field)| form_field(name, field))
                .collect()
        })
        .unwrap_or_default();

    let package = package_name(file);
    let source = SourceRef {
        file: file.to_path_buf(),
        ordinal,
        package,
    };

    Some(Snippet {
        id: hash_id(file, ordinal, &triggers),
        triggers,
        label,
        body,
        vars,
        form_fields,
        search_terms: raw.search_terms.clone(),
        source,
        word: raw.word,
        usable: true,
    })
}

fn form_field(name: &str, field: &RawFormField) -> FormField {
    FormField {
        name: name.to_string(),
        kind: field.kind.clone().unwrap_or_else(|| "text".into()),
        default: field.default.as_ref().and_then(yaml_scalar),
        values: field.values.iter().filter_map(yaml_scalar).collect(),
        multiline: field.multiline,
    }
}

fn yaml_scalar(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn resolve_image_path(file: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() || path.exists() {
        return path;
    }
    file.parent().map(|p| p.join(&path)).unwrap_or(path)
}

fn package_name(file: &Path) -> Option<String> {
    let mut parts = file.components().rev();
    // .../packages/<name>/package.yml
    parts.next()?;
    let owner = parts.next()?.as_os_str().to_string_lossy();
    if owner == "packages" {
        return None;
    }
    let grand = parts.next()?.as_os_str().to_string_lossy();
    (grand == "packages").then(|| owner.to_string())
}

fn hash_id(file: &Path, ordinal: usize, triggers: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    file.hash(&mut hasher);
    ordinal.hash(&mut hasher);
    triggers.hash(&mut hasher);
    hasher.finish()
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_for(dir: &Path) -> YamlSource {
        YamlSource {
            root: dir.to_path_buf(),
            cli: EspansoCli::discover(),
            reconcile: false,
        }
    }

    #[test]
    fn loads_nested_files_and_skips_underscore() {
        let dir = std::env::temp_dir().join(format!("snipshot-test-{}", std::process::id()));
        let nested = dir.join("match").join("packages").join("demo");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            dir.join("match").join("base.yml"),
            "matches:\n  - trigger: \";;log\"\n    replace: \"console.log()\"\n",
        )
        .unwrap();
        std::fs::write(
            nested.join("package.yml"),
            "matches:\n  - trigger: \":pkg\"\n    replace: \"from package\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("match").join("_manifest.yml"), "name: hidden\n").unwrap();

        let snapshot = source_for(&dir).load().unwrap();
        assert_eq!(snapshot.len(), 2);
        let pkg = snapshot
            .snippets
            .iter()
            .find(|s| s.trigger() == ":pkg")
            .expect("package 里的 match 应被加载");
        assert_eq!(pkg.source.package.as_deref(), Some("demo"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn label_falls_back_to_first_line() {
        let dir = std::env::temp_dir().join(format!("snipshot-test-label-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("match")).unwrap();
        std::fs::write(
            dir.join("match").join("a.yml"),
            "matches:\n  - trigger: \":x\"\n    replace: \"first line\\nsecond\"\n",
        )
        .unwrap();
        let snapshot = source_for(&dir).load().unwrap();
        assert_eq!(snapshot.snippets[0].label, "first line");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
