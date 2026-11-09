//! 使用频率、最近使用时间与本周展开来源。
//!
//! 排序加权只依赖 `count` / `last_used_ms`；新增的周计数只服务于管理面板的
//! “毕业率”展示。因为 espanso 原生展开不会通知 semisnip，`direct` 只能靠 tap
//! 观察“用户打出了直达触发词”来估算——它是趋势指标，不是精确埋点。
//!
//! 写入失败只记日志：丢了不影响正确性。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// 频次加成的统计窗口：近 30 天。超过就清零，避免一条老片段永远霸榜。
const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// 一次展开的来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageKind {
    /// 从面板回车。
    Panel,
    /// 面板外直接打出直达触发词（估算）。
    Direct,
}

/// 单条 trigger 的统计。
///
/// `count` 不设上限——用 `saturating_add` 防溢出，排序时再压到 60 封顶
/// （见 `search::score::boost`），既有长期积累的区分度，又不会让一条老片段
/// 永远霸榜。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    /// 累计使用次数。
    pub count: u32,
    /// 最后一次使用的时间戳（毫秒，0 表示从未用过）。
    pub last_used_ms: i64,
    /// 当前七天窗口的起点（毫秒，0 表示尚未开始统计）。
    #[serde(default)]
    pub week_anchor_ms: i64,
    /// 本窗口内从面板展开的次数。
    #[serde(default)]
    pub week_panel: u32,
    /// 本窗口内直接打出直达触发词的次数（估算）。
    #[serde(default)]
    pub week_direct: u32,
    /// 近 30 天窗口的起点（毫秒，0 表示尚未开始统计）。
    #[serde(default)]
    pub month_anchor_ms: i64,
    /// 近 30 天窗口内的使用次数，供排序加权使用。
    #[serde(default)]
    pub month_count: u32,
}

impl Entry {
    /// 若当前窗口已满七天（或时钟回拨），清零周计数并重设锚点。
    fn roll_week(&mut self, now: i64) {
        if self.week_anchor_ms == 0
            || now < self.week_anchor_ms
            || now - self.week_anchor_ms >= WEEK_MS
        {
            self.week_anchor_ms = now;
            self.week_panel = 0;
            self.week_direct = 0;
        }
    }

    /// 近 30 天窗口同样滚动：排序只看这段时间的使用量。
    fn roll_month(&mut self, now: i64) {
        if self.month_anchor_ms == 0
            || now < self.month_anchor_ms
            || now - self.month_anchor_ms >= MONTH_MS
        {
            self.month_anchor_ms = now;
            self.month_count = 0;
        }
    }
}

/// 全部统计。序列化到 `usage.json`，字段名即文件格式，改名等于破坏兼容。
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

    /// 记一次面板展开。保留旧名字，避免所有调用点一起改。
    pub fn record(&mut self, trigger: &str) {
        self.record_kind(trigger, UsageKind::Panel);
    }

    /// 记一次直达展开（由 tap 的字符后缀检测触发）。
    pub fn record_direct(&mut self, trigger: &str) {
        self.record_kind(trigger, UsageKind::Direct);
    }

    /// 记一次展开。只改内存并置 `dirty`，落盘由 `flush_if_dirty` 负责。
    pub fn record_kind(&mut self, trigger: &str, kind: UsageKind) {
        self.record_kind_at(trigger, kind, now_ms());
    }

    /// 可注入时间的版本，供单测固定窗口。
    pub fn record_kind_at(&mut self, trigger: &str, kind: UsageKind, now: i64) {
        let entry = self.entries.entry(trigger.to_string()).or_default();
        entry.roll_week(now);
        entry.roll_month(now);
        entry.count = entry.count.saturating_add(1);
        entry.month_count = entry.month_count.saturating_add(1);
        entry.last_used_ms = now;
        match kind {
            UsageKind::Panel => entry.week_panel = entry.week_panel.saturating_add(1),
            UsageKind::Direct => entry.week_direct = entry.week_direct.saturating_add(1),
        }
        self.dirty = true;
    }

    /// 汇总本周两种来源的次数。调用方自己算比例，数据层不产出展示文案。
    pub fn week_stats(&self) -> (u32, u32) {
        let mut panel = 0u32;
        let mut direct = 0u32;
        for entry in self.entries.values() {
            panel = panel.saturating_add(entry.week_panel);
            direct = direct.saturating_add(entry.week_direct);
        }
        (panel, direct)
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

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("semisnip-usage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn old_json_without_week_fields_still_loads() {
        let dir = temp_dir("legacy");
        let path = dir.join("usage.json");
        std::fs::write(
            &path,
            r#"{"entries":{";;log":{"count":4,"last_used_ms":123}}}"#,
        )
        .unwrap();
        let usage = Usage::load(&path);
        let entry = usage.get(";;log");
        assert_eq!(entry.count, 4);
        assert_eq!(entry.week_panel, 0);
        assert_eq!(entry.week_direct, 0);
    }

    #[test]
    fn week_counters_roll_after_seven_days_and_keep_total() {
        let mut usage = Usage::default();
        let day = 24 * 60 * 60 * 1000;
        let base = 1_000;
        usage.record_kind_at(";;log", UsageKind::Panel, base);
        usage.record_kind_at(";;log", UsageKind::Direct, base);
        assert_eq!(usage.get(";;log").week_panel, 1);
        assert_eq!(usage.get(";;log").week_direct, 1);

        usage.record_kind_at(";;log", UsageKind::Direct, base + 7 * day);
        let entry = usage.get(";;log");
        assert_eq!(entry.week_panel, 0);
        assert_eq!(entry.week_direct, 1);
        assert_eq!(entry.count, 3);
        assert_eq!(entry.week_anchor_ms, base + 7 * day);
    }

    #[test]
    fn month_counter_rolls_after_thirty_days() {
        let mut usage = Usage::default();
        let day = 24 * 60 * 60 * 1000;
        usage.record_kind_at(";;log", UsageKind::Panel, 1_000);
        assert_eq!(usage.get(";;log").month_count, 1);
        // 29 天后仍然在窗口内
        usage.record_kind_at(";;log", UsageKind::Panel, 1_000 + 29 * day);
        assert_eq!(usage.get(";;log").month_count, 2);
        // 第 30 天开始是新窗口
        usage.record_kind_at(";;log", UsageKind::Panel, 1_000 + 30 * day);
        assert_eq!(usage.get(";;log").month_count, 1);
    }

    #[test]
    fn week_stats_sums_both_kinds() {
        let mut usage = Usage::default();
        usage.record(":a");
        usage.record(":b");
        usage.record_direct(":a");
        assert_eq!(usage.week_stats(), (2, 1));
    }
}
