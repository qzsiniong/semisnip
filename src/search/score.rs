//! 打分与排序。
//!
//! 分档：精确 > 前缀 > 子串 > 子序列，再叠加使用频率与最近使用。
//! 纯函数 + 确定性排序：同样的输入永远给同样的顺序，方便回归测试。

use std::sync::Arc;

use crate::index::model::{Hit, MatchField, MatchFields, Snapshot, split_label};
use crate::library::Library;
use crate::search::pinyin::PinyinIndex;
use crate::search::query::{Query, core_of};
use crate::usage::{Entry as UsageEntry, Usage};

const DAY_MS: i64 = 86_400_000;

/// 查询与某条 trigger 完全相等时的置顶加成。
///
/// 它必须大到压过"其它档位 + 频次加成 + 可用性惩罚"的任意组合：1000 + 165 - 400
/// 仍然远小于 2000。语义是"用户把这个触发词原样打出来了，别再猜"。
const EXACT_TRIGGER_BOOST: i64 = 2000;

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
    /// label 去掉标签后的标题（小写），用于标题匹配。
    title_lower: String,
    /// `search_terms` 与本地库别名（小写）。单独一档，只低于 trigger。
    search_terms: Vec<String>,
    /// 标签（小写），用于 `:tag` 过滤。
    tags: Vec<String>,
    /// 标签的拼音索引。`:` 后面打 `bd` 也能命中中文标签 `表单`。
    tag_pinyin: Option<PinyinIndex>,
    /// 正文全文（小写）。只在全文模式下参与匹配，且是最低档。
    body_lower: String,
    /// 标题的拼音索引。优先按标题建，标题是纯 ASCII 时退到正文——
    /// 中文用户更可能想"打字首字母命中中文标题"，而不是反过来。
    pinyin: Option<PinyinIndex>,
}

impl SearchIndex {
    /// 从快照构建（不带本地别名）。单测与纯检索场景用。
    pub fn build(snapshot: &Snapshot) -> Self {
        Self::build_with_aliases(snapshot, &Library::default())
    }

    /// 带本地库别名构建。O(条数 × 字段长度)，只在索引变化时跑，不在按键路径上。
    pub fn build_with_aliases(snapshot: &Snapshot, library: &Library) -> Self {
        let entries = snapshot
            .snippets
            .iter()
            .map(|snippet| {
                let (title, tags) = split_label(&snippet.label);
                let mut search_terms: Vec<String> = snippet
                    .search_terms
                    .iter()
                    .map(|term| term.to_lowercase())
                    .collect();
                search_terms.extend(
                    library
                        .aliases_for(snippet.trigger())
                        .iter()
                        .map(|alias| alias.to_lowercase()),
                );
                IndexEntry {
                    triggers: snippet.triggers.iter().map(|t| t.to_lowercase()).collect(),
                    title_lower: title.to_lowercase(),
                    search_terms,
                    tags: tags.iter().map(|tag| tag.to_lowercase()).collect(),
                    tag_pinyin: PinyinIndex::build(&tags.join(" ")),
                    body_lower: snippet.body.as_text().to_lowercase(),
                    pinyin: PinyinIndex::build(&title).or_else(|| {
                        PinyinIndex::build(snippet.body.as_text().lines().next().unwrap_or(""))
                    }),
                }
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

/// 编辑距离（Levenshtein）。只在拼写容错里用，所以实现刻意保持直白。
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// 拼写容错：短 token 与字段编辑距离恰好为 1 时给 500 分。
///
/// 档位选择刻意卡在「子序列（~300–395）」与「子串（600）」之间：
/// typo 修正应该排在"记得大概拼写"前面，但不能压过"确实包含这个词"。
///
/// 限制条件都是为了压误命中：
/// - token 过长不做（长词 typo 概率低，且 edit distance 扫描贵）；
/// - 字段过短不做（避免 `a` 命中一切单字符）；
/// - 只认编辑距离恰好 1：2 个字符的偏差通常已经不是"同一个词"。
fn fuzzy_score(query: &str, field: &str) -> Option<i64> {
    let qlen = query.chars().count();
    if qlen == 0 || qlen > 8 {
        return None;
    }
    let flen = field.chars().count();
    if flen < 2 {
        return None;
    }
    if edit_distance(query, field) == 1 {
        Some(500)
    } else {
        None
    }
}

/// `search_terms` 的独立档位：只低于 trigger，高于标题与拼音。
///
/// 精确 900、前缀 650–750、子串 530。数值刻意与 trigger 的 1000/800–700/600
/// 错开，保证"用户明确声明过的同义词"永远排在"标题里碰巧出现"之前。
fn term_score(query: &str, term: &str) -> Option<i64> {
    if term.is_empty() {
        return None;
    }
    if term == query {
        return Some(900);
    }
    let qlen = query.chars().count() as i64;
    let tlen = term.chars().count() as i64;
    if term.starts_with(query) {
        return Some((750 - (tlen - qlen).max(0)).max(650));
    }
    if term.contains(query) {
        return Some(530);
    }
    None
}

/// 正文的最低档。只在全文模式启用——`'` 是显式开关，默认不扫正文，
/// 免得"正文里碰巧出现一个词"把真正想找的片段顶下去。
fn body_score(query: &str, body: &str) -> Option<i64> {
    if body.is_empty() {
        return None;
    }
    if body == query {
        return Some(200);
    }
    if body.starts_with(query) {
        return Some(170);
    }
    if body.contains(query) {
        return Some(120);
    }
    None
}

/// 标签过滤：`:tag` 必须命中某条标签。
///
/// 中文标签用拼音别名兜底（`:bd` → `表单`）；ASCII 标签本来就是精确比较。
fn tag_matches(tag: &str, entry: &IndexEntry) -> bool {
    if entry.tags.iter().any(|t| t == tag) {
        return true;
    }
    entry.tag_pinyin.as_ref().is_some_and(|pinyin| {
        pinyin.full == tag
            || pinyin.initials == tag
            || pinyin.full.starts_with(tag)
            || pinyin.initials.starts_with(tag)
    })
}

/// 对一条片段算出总分，并记下命中的 trigger 与字段。
///
/// 多个 token 之间是 **AND**：任一 token 在所有字段里都找不到就整条淘汰
/// （`best == 0` 直接 return None）。这是刻意的——用户多打一个词，通常是想收窄
/// 而不是放宽。
///
/// 字段优先级：trigger > search_terms（独立档）> 标题（打 8 折）> 拼音 >
/// 正文（仅全文模式，最低档）。
fn score_entry(
    entry: &IndexEntry,
    query: &Query,
    full_text: bool,
) -> Option<(i64, Option<usize>, MatchFields)> {
    // 标签过滤先做：不命中标签的片段连打分资格都没有。
    for tag in &query.tags {
        if !tag_matches(tag, entry) {
            return None;
        }
    }
    if query.tokens.is_empty() {
        // 只有标签：全部命中，按使用习惯排序。
        return Some((0, None, MatchFields::default()));
    }

    let mut total = 0i64;
    // 命中哪一个 trigger 必须记下来：`match exec` 只能按 trigger 展开。
    // 这里记下标而不是字符串——索引里的 trigger 是小写的，真正要传出去的
    // 原文（大小写、`;;`/`,,` 前缀）都在 snapshot 的 `Snippet` 上。
    let mut matched_trigger: Option<usize> = None;
    let mut fields = MatchFields::default();
    // 任一 token 与 trigger 完全相等 → 这条无条件置顶。
    let mut exact_trigger = false;

    for token in &query.tokens {
        let mut best: i64 = 0;
        let mut best_field: Option<MatchField> = None;

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
                best_field = Some(MatchField::Trigger);
                matched_trigger = Some(idx);
            }
            // 查询与 trigger（或它的核心部分）完全相等：无条件置顶。
            // 核心部分单独判断，因为它被打了 95 折，分值不再是 1000。
            if field_score(token, trigger) == Some(1000)
                || (core != trigger && field_score(token, core) == Some(1000))
            {
                exact_trigger = true;
            }
        }

        for term in &entry.search_terms {
            if let Some(score) = term_score(token, term) {
                if score > best {
                    best = score;
                    best_field = Some(MatchField::SearchTerms);
                }
            }
        }

        if let Some(score) = field_score(token, &entry.title_lower) {
            let score = score * 80 / 100;
            if score > best {
                best = score;
                best_field = Some(MatchField::Title);
            }
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
            if pinyin_score > best {
                best = pinyin_score;
                best_field = Some(MatchField::Pinyin);
            }
        }

        if full_text {
            if let Some(score) = body_score(token, &entry.body_lower) {
                if score > best {
                    best = score;
                    best_field = Some(MatchField::Body);
                }
            }
        }

        // 严格匹配全失败时，对短 token 做一次拼写容错。
        // 只对 trigger、检索词与标题做：正文可能很长，逐字段算编辑距离会拖慢
        // 按键路径，而且正文里碰巧差一个字符的词太多，误命中代价高。
        if best == 0 {
            for (idx, trigger) in entry.triggers.iter().enumerate() {
                if let Some(score) = fuzzy_score(token, trigger) {
                    if score > best {
                        best = score;
                        best_field = Some(MatchField::Trigger);
                        matched_trigger = Some(idx);
                    }
                }
                let core = core_of(trigger);
                if core != trigger {
                    if let Some(score) = fuzzy_score(token, core) {
                        let score = score * 95 / 100;
                        if score > best {
                            best = score;
                            best_field = Some(MatchField::Trigger);
                            matched_trigger = Some(idx);
                        }
                    }
                }
            }
            for term in &entry.search_terms {
                if let Some(score) = fuzzy_score(token, term) {
                    let score = score * 95 / 100;
                    if score > best {
                        best = score;
                        best_field = Some(MatchField::SearchTerms);
                    }
                }
            }
            if let Some(score) = fuzzy_score(token, &entry.title_lower) {
                let score = score * 80 / 100;
                if score > best {
                    best = score;
                    best_field = Some(MatchField::Title);
                }
            }
        }

        if best == 0 {
            return None;
        }
        total += best;
        if let Some(field) = best_field {
            fields.add(field);
        }
    }

    if exact_trigger {
        total += EXACT_TRIGGER_BOOST;
    }

    Some((total, matched_trigger, fields))
}

/// 使用习惯带来的加成，上限约 165 分（频率 ≈105 + 新鲜度 60）。
///
/// 刻意压在"档位差"之下（1000/800/600/300 之间至少差 100）但仍在同一量级：
/// - 频率只统计近 30 天（见 `usage::Entry::month_count`），用 `ln(1+n)` 而不是
///   线性：第 2 次到第 3 次的意义远大于第 50 次到第 51 次；`min(60)` 封顶避免
///   常用片段永远霸榜；
/// - 新鲜度分四档（≤1 天 / ≤7 天 / ≤30 天 / 更久），不连续变化——用户对"昨天用过"
///   和"两小时前用过"的感知差不了一倍，档位比分值更稳。
///
/// 两者相加意味着：一条精确命中的片段（1000）不会被一条常被使用但只是子序列命中
/// （300+165）的片段顶掉，但同档位之间使用习惯能决定先后。
fn boost(entry: UsageEntry, now_ms: i64) -> i64 {
    let freq = (entry.month_count.min(60) as f64).ln_1p() * 40.0;
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
    rank_filtered(
        index,
        snapshot,
        query,
        usage,
        &Library::default(),
        true,
        false,
        limit,
        now_ms,
    )
}

/// 带本地库过滤的排序。`include_disabled == false` 时跳过被停用的片段。
///
/// `full_text` 由面板的全文模式开关决定：为真时正文参与匹配（最低档），
/// 为假时只看 trigger / 检索词 / 标题 / 标签 / 拼音。
///
/// 停用只影响“是否出现”，不影响分数与排序规则——这样打开“显示已停用”后，
/// 列表顺序不会因为一次过滤开关而整体重排。
// 参数多但都是"检索上下文"，拆成结构体只会让调用点多一层解构；
// 这里保持直白，和 `rank_direct` 的签名形状一致。
#[allow(clippy::too_many_arguments)]
pub fn rank_filtered(
    index: &SearchIndex,
    snapshot: &Snapshot,
    query: &Query,
    usage: &Usage,
    library: &Library,
    include_disabled: bool,
    full_text: bool,
    limit: usize,
    now_ms: i64,
) -> Vec<Hit> {
    let mut scored: Vec<(Hit, u32, i64)> = Vec::with_capacity(index.entries.len());

    for (i, entry) in index.entries.iter().enumerate() {
        let Some(snippet) = snapshot.snippets.get(i) else {
            continue;
        };
        if !include_disabled && library.is_disabled(snippet.trigger()) {
            continue;
        }
        // 空查询时 `score_entry` 必定返回 `(0, None)`——直接内联掉这次调用，
        // 省掉一次函数进入与 `query.is_empty()` 判定。这是面板打开瞬间的热路径。
        let (base, matched, fields) = if query.is_empty() {
            (0, None, MatchFields::default())
        } else {
            let Some(triple) = score_entry(entry, query, full_text) else {
                continue;
            };
            triple
        };
        // espanso 不认的片段仍然展示，但明显降权：回车大概率什么都不发生。
        let usable_penalty = if snippet.usable { 0 } else { -400 };
        let boost = boost(usage.get(snippet.trigger()), now_ms);
        let score = base + boost + usable_penalty;

        let trigger = matched
            .and_then(|idx| snippet.triggers.get(idx))
            .or_else(|| snippet.triggers.first())
            .cloned()
            .unwrap_or_default();

        scored.push((
            Hit {
                snippet: Arc::clone(snippet),
                trigger,
                score,
                matched: fields,
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

/// 直达模式排序：只考虑带 `prefix` 的触发词，输入的是前缀后的核心部分。
///
/// 这里刻意**不做**子串/子序列匹配——直达模式是肌肉记忆路径，用户打 `log`
/// 就是想要 `,,log`；把 `,,logging-config` 也排进来只会让回车落错片段。
/// 空查询则列出全部直达项，按使用习惯排。
pub fn rank_direct(
    snapshot: &Snapshot,
    raw: &str,
    prefix: &str,
    library: &Library,
    usage: &Usage,
    limit: usize,
    now_ms: i64,
) -> Vec<Hit> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let query = raw.trim().to_lowercase();
    let mut scored: Vec<(Hit, u32, i64)> = Vec::with_capacity(snapshot.snippets.len());

    for snippet in &snapshot.snippets {
        if library.is_disabled(snippet.trigger()) {
            continue;
        }
        let Some(trigger) = snippet.direct_trigger(prefix) else {
            continue;
        };
        let core = trigger[prefix.len()..].to_lowercase();
        if core.is_empty() {
            continue;
        }
        let base = if query.is_empty() {
            0
        } else if core == query {
            1000
        } else if core.starts_with(&query) {
            800 - (core.chars().count() as i64 - query.chars().count() as i64).max(0)
        } else {
            continue;
        };
        let usable_penalty = if snippet.usable { 0 } else { -400 };
        let entry = usage.get(snippet.trigger());
        let score = base + boost(entry, now_ms) + usable_penalty;
        scored.push((
            Hit {
                snippet: Arc::clone(snippet),
                trigger: trigger.to_string(),
                score,
                matched: {
                    let mut fields = MatchFields::default();
                    fields.add(MatchField::Trigger);
                    fields
                },
            },
            entry.count,
            entry.last_used_ms,
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
        // 用前缀而不是精确触发词来测降权：精确触发词会走"无条件置顶"，
        // 那是另一条规则（见 exact_trigger_is_always_on_top）。
        let order = top("gho", items);
        assert_eq!(order[0], ";;ghost2");
        assert!(order.contains(&";;ghost".to_string()));
    }

    #[test]
    fn exact_trigger_is_always_on_top() {
        let mut bad = (*snippet(";;ghost", "ghost", "")).clone();
        bad.usable = false;
        let items = vec![
            Arc::new(bad),
            snippet(";;ghost2", "ghost", ""),
            snippet(";;other", "ghost", ""),
        ];
        let order = top("ghost", items);
        // 即使这条片段对 espanso 不可用，只要查询与 trigger 完全相等，
        // 就必须排第一：用户已经把这个触发词原样打出来了。
        assert_eq!(order[0], ";;ghost");
    }

    #[test]
    fn search_terms_outrank_title_and_pinyin() {
        let mut a = (*snippet(";;a", "会议纪要", "")).clone();
        a.search_terms = vec!["meeting".to_string()];
        let items = vec![Arc::new(a), snippet(";;b", "meeting 模板", "")];
        // `meeting` 精确命中 a 的检索词，而 b 只是标题前缀；检索词档位更高。
        let order = top("meeting", items);
        assert_eq!(order[0], ";;a");
    }

    #[test]
    fn tag_filter_matches_ascii_and_pinyin() {
        let items = vec![
            snippet(";;a", "日志 :dev", ""),
            snippet(";;b", "表单 :form :表单", ""),
            snippet(";;c", "周报 :表单", ""),
        ];
        assert_eq!(top(":dev", items.clone()), vec![";;a"]);
        assert_eq!(top(":form", items.clone()), vec![";;b"]);
        // 中文标签的拼音首字母也能过滤
        let mut got = top(":bd", items);
        got.sort();
        assert_eq!(got, vec![";;b", ";;c"]);
    }

    #[test]
    fn body_matches_only_in_full_text_mode() {
        let items = vec![snippet(";;a", "标题", "正文里藏着 needle")];
        let parsed = Query::parse("needle");
        let snap = snapshot(items);
        let index = SearchIndex::build(&snap);
        let normal = rank_filtered(
            &index,
            &snap,
            &parsed,
            &Usage::default(),
            &Library::default(),
            true,
            false,
            9,
            0,
        );
        assert!(normal.is_empty(), "默认不扫正文");
        let full = rank_filtered(
            &index,
            &snap,
            &parsed,
            &Usage::default(),
            &Library::default(),
            true,
            true,
            9,
            0,
        );
        assert_eq!(full.len(), 1, "全文模式下正文命中");
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

    #[test]
    fn matched_trigger_keeps_original_case_and_prefix() {
        let items = vec![snippet(",,Log", "JS", "")];
        assert_eq!(top("log", items)[0], ",,Log");
    }

    #[test]
    fn disabled_snippets_are_hidden_until_included() {
        let items = vec![snippet(";;a", "A", ""), snippet(";;b", "B", "")];
        let snap = snapshot(items);
        let index = SearchIndex::build(&snap);
        let mut library = Library::default();
        library.set_disabled(";;a", true);

        let hidden = rank_filtered(
            &index,
            &snap,
            &Query::parse(""),
            &Usage::default(),
            &library,
            false,
            false,
            9,
            0,
        );
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].trigger, ";;b");

        let shown = rank_filtered(
            &index,
            &snap,
            &Query::parse(""),
            &Usage::default(),
            &library,
            true,
            false,
            9,
            0,
        );
        assert_eq!(shown.len(), 2);
    }

    #[test]
    fn aliases_from_local_library_participate_in_search() {
        let dir = std::env::temp_dir().join(format!("semisnip-score-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.json");
        std::fs::write(&path, r#"{"aliases":{";;meet":["hyjy"]}}"#).unwrap();
        let library = Library::load(&path);

        let snap = snapshot(vec![snippet(";;meet", "会议纪要", "")]);
        let index = SearchIndex::build_with_aliases(&snap, &library);
        let hits = rank_filtered(
            &index,
            &snap,
            &Query::parse("hyjy"),
            &Usage::default(),
            &library,
            false,
            false,
            9,
            0,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].trigger, ";;meet");
    }

    #[test]
    fn direct_mode_matches_exact_and_prefix_only() {
        let items = vec![
            snippet(",,log", "log", ""),
            snippet(",,log2", "log2", ""),
            snippet(",,logging", "logging", ""),
            snippet(":sig", "sig", ""),
        ];
        let snap = snapshot(items);
        let library = Library::default();
        let usage = Usage::default();

        let hits = rank_direct(&snap, "log", ",,", &library, &usage, 9, 0);
        let triggers: Vec<&str> = hits.iter().map(|h| h.trigger.as_str()).collect();
        assert_eq!(triggers, vec![",,log", ",,log2", ",,logging"]);

        // 只匹配前缀；`gging` 不是任何直达触发词的前缀
        assert!(rank_direct(&snap, "gging", ",,", &library, &usage, 9, 0).is_empty());
    }

    #[test]
    fn direct_mode_skips_disabled_snippets() {
        let snap = snapshot(vec![snippet(",,log", "log", "")]);
        let mut library = Library::default();
        library.set_disabled(",,log", true);
        assert!(rank_direct(&snap, "log", ",,", &library, &Usage::default(), 9, 0).is_empty());
    }

    #[test]
    fn one_char_typo_is_fuzzily_matched() {
        // 用字符互不重复的 trigger，让 typo 无法被子序列匹配兜底，
        // 才能真正走到 fuzzy 分支。
        let items = vec![snippet(";;xqz", "XQZ", "")];
        // `xqa` 把 `z` 换成 `a`：严格匹配（含子序列）全失败，编辑距离恰好 1
        assert_eq!(top("xqa", items), vec![";;xqz"]);
        // 差两个字符（`x` 换成 `a`、`z` 换成 `b`）不应命中
        assert!(top("xab", vec![snippet(";;xqz", "XQZ", "")]).is_empty());
    }

    #[test]
    fn fuzzy_does_not_swamp_substring_matches() {
        let items = vec![
            snippet(";;log", "log snippet", ""),
            snippet(";;logging", "logging", ""),
        ];
        // `log` 是 `;;logging` 的前缀（800 档），必须排在只有 typo 命中的片段前面。
        // 这里两个都严格命中，只是确认排序不被容错分打乱。
        let order = top("log", items);
        assert_eq!(order[0], ";;log");
    }
}
