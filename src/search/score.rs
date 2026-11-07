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
///
/// 每次重建索引时构建一次，之后每次按键只做比较，不再分配字符串。
/// 这个"同下标"约定是整个检索路径的前提：`rank` 里 `index.entries[i]` 与
/// `snapshot.snippets[i]` 必须指同一条片段，所以两张表只能同时重建。
pub struct SearchIndex {
    entries: Vec<IndexEntry>,
}

/// 单条片段的预折叠形态。
///
/// 全部字段都是小写——`Query::parse` 也会把查询串转小写，比较时不必再规整。
struct IndexEntry {
    /// 全部 trigger（已小写）。可能有多个，搜索命中任意一个都算。
    triggers: Vec<String>,
    /// label 小写，用于次级匹配。
    label_lower: String,
    /// `Snippet::search_blob()` 的结果，用于覆盖检索词与正文首行。
    blob_lower: String,
    /// 拼音索引。优先按 label 建，label 是纯 ASCII 时退到正文——
    /// 中文用户更可能想"打字首字母命中中文标题"，而不是反过来。
    pinyin: Option<PinyinIndex>,
}

impl SearchIndex {
    /// 从快照构建。O(条数 × 字段长度)，只在索引变化时跑，不在按键路径上。
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

/// 单个字段对单个 token 的相关度。
///
/// 四档，分数刻意留出大间隔，方便后面叠加使用时按比例打折而不打乱顺序：
/// - 完全相等 1000
/// - 前缀命中 700–800（字段越短分越高：`;;log` 优于 `;;logging-config`）
/// - 子串命中 600
/// - 子序列命中 300+（见 `subsequence_score`）
///
/// 返回 `None` 表示这个 token 在该字段里完全找不到。空字段也返回 `None`——
/// 否则空串的 `starts_with` 会命中一切。
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

/// 子序列匹配：query 的每个字符按顺序出现在 field 里即可，不要求连续。
///
/// 这是"记得大概拼写"时最有用的一档。三个加分因素：
/// - 连续命中（`prev + 1 == idx`）+6，比断开 +1 明显高：`sig` 应该优先命中
///   `sig` 而不是 `s...i...g` 散落的字符串；
/// - 首个字符越靠前越好（`+ (20 - first*2)`，最低 0）：前缀像的总更像目标；
/// - 任一 query 字符找不到就返回 `None`（`?` 直接短路整个函数）。
///
/// 分值上限约 300 + 6×len + 20，不会越过子串档的 600，保证档位顺序不被
/// "很长的子序列"顶翻。
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

/// 对一条片段算出总分，并记下命中的 trigger。
///
/// 多个 token 之间是 **AND**：任一 token 在所有字段里都找不到就整条淘汰
/// （`best == 0` 直接 return None）。这是刻意的——用户多打一个词，通常是想收窄
/// 而不是放宽。
///
/// 各字段的权重递减（trigger 100% > label 80% > 折叠文本 60%），因为 trigger 是
/// 用户最可能记得的东西，而正文里碰巧出现的词只能算弱证据。拼音单独一套绝对分
/// （见下），不参与打折——它不是"字段"，而是同一字段的另一种写法。
///
/// 返回的 `Option<String>` 是命中的那个 trigger 原文（哪怕命中的是 label，
/// 这里回落到第一个 trigger），`match exec` 要用它。
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

/// 使用习惯带来的加成，上限约 165 分（频率 ≈105 + 新鲜度 60）。
///
/// 刻意压在"档位差"之下（1000/800/600/300 之间至少差 100）但仍在同一量级：
/// - 频率用 `ln(1+n)` 而不是线性：第 2 次到第 3 次的意义远大于第 50 次到第 51 次；
///   `min(60)` 封顶避免常用片段永远霸榜；
/// - 新鲜度分四档（≤1 天 / ≤7 天 / ≤30 天 / 更久），不连续变化——用户对"昨天用过"
///   和"两小时前用过"的感知差不了一倍，档位比分值更稳。
///
/// 两者相加意味着：一条精确命中的片段（1000）不会被一条常被使用但只是子序列命中
/// （300+165）的片段顶掉，但同档位之间使用习惯能决定先后。
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
///
/// 排序是全序的（每个比较链最后都落到 label 上），所以同样的输入永远给同样的
/// 顺序——这一条对回归测试很重要，也让用户不会看到列表在两次按键之间无故抖动。
///
/// 比较顺序：总分 → 使用次数 → 最后使用时间 → trigger 长度（短的优先）→ label。
/// 后两级只在"这两条几乎一样"时起作用，属于兜底去随机性。
///
/// `limit` 只做截断不参与打分：先给全部候选打分再取前 N，避免"提前截断导致
/// 高分项被漏掉"。片段量级（几百到几千）下这点开销可以忽略。
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
