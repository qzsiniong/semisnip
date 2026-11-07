//! 打分与排序。
//!
//! 分档：精确 > 前缀 > 子串 > 子序列，再叠加使用频率与最近使用。
//! 纯函数 + 确定性排序：同样的输入永远给同样的顺序，方便回归测试。

use std::sync::Arc;

use crate::index::model::{Hit, Snapshot};
use crate::search::pinyin::PinyinIndex;
use crate::search::query::{Query, core_of};
use crate::usage::{Entry as UsageEntry, Usage};

const DAY_MS: i64 = 86_400_000;

/// 预折叠的检索表，与 `Snapshot.snippets` 下标一一对应。
/// 每次重建索引时构建一次，之后每次按键只做比较，不再分配字符串。
pub struct SearchIndex {
    entries: Vec<IndexEntry>,
}

struct IndexEntry {
    triggers: Vec<String>,
    label_lower: String,
    blob_lower: String,
    pinyin: Option<PinyinIndex>,
}

impl SearchIndex {
    pub fn build(snapshot: &Snapshot) -> Self {
        let entries = snapshot
            .snippets
            .iter()
            .map(|snippet| IndexEntry {
                triggers: snippet.triggers.iter().map(|t| t.to_lowercase()).collect(),
                label_lower: snippet.label.to_lowercase(),
                blob_lower: snippet.search_blob(),
                pinyin: PinyinIndex::build(&snippet.label)
                    .or_else(|| PinyinIndex::build(snippet.body.as_text())),
            })
            .collect();
        Self { entries }
    }
}

fn field_score(query: &str, field: &str) -> Option<i64> {
    if field.is_empty() {
        return None;
    }
    if field == query {
        return Some(1000);
    }
    let qlen = query.chars().count() as i64;
    let flen = field.chars().count() as i64;
    if field.starts_with(query) {
        return Some((800 - (flen - qlen).max(0)).max(700));
    }
    if field.contains(query) {
        return Some(600);
    }
    subsequence_score(query, field)
}

fn subsequence_score(query: &str, field: &str) -> Option<i64> {
    let hay: Vec<char> = field.chars().collect();
    let mut cursor = 0usize;
    let mut score = 300i64;
    let mut prev: Option<usize> = None;
    let mut first: Option<usize> = None;

    for qc in query.chars() {
        let idx = hay
            .iter()
            .enumerate()
            .skip(cursor)
            .find(|(_, hc)| **hc == qc)
            .map(|(i, _)| i)?;
        if first.is_none() {
            first = Some(idx);
        }
        if prev == Some(idx.wrapping_sub(1)) {
            score += 6;
        } else {
            score += 1;
        }
        prev = Some(idx);
        cursor = idx + 1;
    }

    if let Some(first) = first {
        score += (20 - first as i64 * 2).max(0);
    }
    Some(score)
}

fn score_entry(entry: &IndexEntry, query: &Query) -> Option<(i64, Option<String>)> {
    if query.is_empty() {
        return Some((0, None));
    }

    let mut total = 0i64;
    // 命中哪一个 trigger 必须记下来：`match exec` 只能按 trigger 展开。
    let mut matched_trigger: Option<String> = None;

    for token in &query.tokens {
        let mut best: i64 = 0;

        for (idx, trigger) in entry.triggers.iter().enumerate() {
            let core = core_of(trigger);
            let trigger_score = field_score(token, trigger).unwrap_or(0);
            let core_score = if core == trigger {
                0
            } else {
                field_score(token, core).unwrap_or(0) * 95 / 100
            };
            let score = trigger_score.max(core_score);
            if score > best {
                best = score;
                if idx < entry.triggers.len() {
                    matched_trigger = Some(entry.triggers[idx].clone());
                }
            }
        }

        if let Some(score) = field_score(token, &entry.label_lower) {
            best = best.max(score * 80 / 100);
        }
        if let Some(score) = field_score(token, &entry.blob_lower) {
            best = best.max(score * 60 / 100);
        }
        if let Some(pinyin) = &entry.pinyin {
            let pinyin_score = if pinyin.full == *token {
                520
            } else if pinyin.initials == *token {
                500
            } else if pinyin.full.starts_with(token.as_str()) {
                470
            } else if pinyin.initials.starts_with(token.as_str()) {
                450
            } else if pinyin.full.contains(token.as_str()) {
                380
            } else {
                0
            };
            best = best.max(pinyin_score);
        }

        if best == 0 {
            return None;
        }
        total += best;
    }

    Some((total, matched_trigger))
}

fn boost(entry: UsageEntry, now_ms: i64) -> i64 {
    let freq = (entry.count.min(60) as f64).ln_1p() * 40.0;
    let recency = if entry.last_used_ms <= 0 {
        0
    } else {
        let days = (now_ms - entry.last_used_ms) / DAY_MS;
        match days {
            i64::MIN..=1 => 60,
            2..=7 => 30,
            8..=30 => 10,
            _ => 0,
        }
    };
    freq as i64 + recency
}

/// 返回最多 `limit` 条命中，已按相关度排好。
pub fn rank(
    index: &SearchIndex,
    snapshot: &Snapshot,
    query: &Query,
    usage: &Usage,
    limit: usize,
    now_ms: i64,
) -> Vec<Hit> {
    let mut scored: Vec<(Hit, u32, i64)> = Vec::with_capacity(index.entries.len());

    for (i, entry) in index.entries.iter().enumerate() {
        let Some(snippet) = snapshot.snippets.get(i) else {
            continue;
        };
        let Some((base, matched)) = score_entry(entry, query) else {
            continue;
        };
        // espanso 不认的片段仍然展示，但明显降权：回车大概率什么都不发生。
        let usable_penalty = if snippet.usable { 0 } else { -400 };
        let boost = boost(usage.get(snippet.trigger()), now_ms);
        let score = base + boost + usable_penalty;

        let trigger = matched
            .or_else(|| snippet.triggers.first().cloned())
            .unwrap_or_default();

        scored.push((
            Hit {
                snippet: Arc::clone(snippet),
                trigger,
                score,
            },
            usage.get(snippet.trigger()).count,
            usage.get(snippet.trigger()).last_used_ms,
        ));
    }

    scored.sort_by(|a, b| {
        b.0.score
            .cmp(&a.0.score)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| {
                a.0.trigger
                    .chars()
                    .count()
                    .cmp(&b.0.trigger.chars().count())
            })
            .then_with(|| a.0.snippet.label.cmp(&b.0.snippet.label))
    });

    scored
        .into_iter()
        .take(limit)
        .map(|(hit, _, _)| hit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{Body, Snippet, SourceRef};
    use std::path::PathBuf;

    fn snippet(trigger: &str, label: &str, body: &str) -> Arc<Snippet> {
        Arc::new(Snippet {
            id: 0,
            triggers: vec![trigger.to_string()],
            label: label.to_string(),
            body: Body::Text(body.to_string()),
            vars: vec![],
            form_fields: vec![],
            search_terms: vec![],
            source: SourceRef {
                file: PathBuf::from("base.yml"),
                ordinal: 0,
                package: None,
            },
            word: false,
            usable: true,
        })
    }

    fn snapshot(items: Vec<Arc<Snippet>>) -> Snapshot {
        let mut by_trigger = std::collections::HashMap::new();
        for (i, s) in items.iter().enumerate() {
            for t in &s.triggers {
                by_trigger.entry(t.clone()).or_insert_with(Vec::new).push(i);
            }
        }
        Snapshot {
            built_at_ms: 0,
            config_root: PathBuf::new(),
            snippets: items,
            by_trigger,
            diagnostics: vec![],
            espanso_known: Default::default(),
            espanso_checked: false,
        }
    }

    fn top(query: &str, items: Vec<Arc<Snippet>>) -> Vec<String> {
        let snap = snapshot(items);
        let index = SearchIndex::build(&snap);
        rank(&index, &snap, &Query::parse(query), &Usage::default(), 9, 0)
            .into_iter()
            .map(|h| h.trigger)
            .collect()
    }

    #[test]
    fn exact_beats_prefix_beats_contains() {
        let items = vec![
            snippet(";;sigx", "A", ""),
            snippet(";;signature", "B", ""),
            snippet(";;sig", "C", ""),
        ];
        assert_eq!(top(";;sig", items)[0], ";;sig");
    }

    #[test]
    fn core_match_finds_trigger_without_symbols() {
        let items = vec![snippet(";;log", "JS: console.log", "console.log($|$)")];
        assert_eq!(top("log", items), vec![";;log"]);
    }

    #[test]
    fn chinese_label_matches_by_char_and_pinyin() {
        let items = vec![snippet(";;meet", "会议纪要", "")];
        assert_eq!(top("纪要", items.clone()).len(), 1, "汉字子串应命中");
        assert_eq!(top("hyjy", items.clone()).len(), 1, "首字母应命中");
        assert_eq!(top("huiyi", items).len(), 1, "全拼前缀应命中");
    }

    #[test]
    fn multiple_tokens_are_anded() {
        let items = vec![
            snippet(";;log", "JS console", ""),
            snippet(";;warn", "JS warning", ""),
        ];
        assert_eq!(top("js warn", items), vec![";;warn"]);
    }

    #[test]
    fn unknown_to_espanso_is_downranked_not_hidden() {
        let mut bad = (*snippet(";;ghost", "ghost", "")).clone();
        bad.usable = false;
        let items = vec![Arc::new(bad), snippet(";;ghost2", "ghost", "")];
        let order = top("ghost", items);
        assert_eq!(order[0], ";;ghost2");
        assert!(order.contains(&";;ghost".to_string()));
    }

    #[test]
    fn no_match_returns_empty() {
        assert!(top("zzzz", vec![snippet(";;a", "a", "")]).is_empty());
    }

    #[test]
    fn empty_query_lists_everything() {
        let items = vec![snippet(";;a", "A", ""), snippet(";;b", "B", "")];
        assert_eq!(top("", items).len(), 2);
    }
}
