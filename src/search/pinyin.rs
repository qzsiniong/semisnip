//! 中文的拼音索引。
//!
//! 非激活面板收不到输入法（见计划 R3），所以搜索框里打不出汉字；
//! 但片段标签大多是中文——用全拼与首字母把这块补回来：
//! 打 `hyjy` 或 `hy` 都能找到「会议纪要」。

use pinyin::ToPinyin;

/// 一段文本的拼音形态。
///
/// 两个字段是"同一份信息的两种打法"：`full` 支持全拼（`huiyi`），`initials`
/// 支持首字母（`hy`）。它们都转成小写、只保留字母数字，与查询串的规整方式一致。
#[derive(Clone, Debug, Default)]
pub struct PinyinIndex {
    /// 全拼，如 `huiyijiyao`
    pub full: String,
    /// 首字母，如 `hyjy`
    pub initials: String,
}

impl PinyinIndex {
    /// 为一段文本建索引；没有任何汉字时返回 `None`。
    ///
    /// 返回 `None` 而不是空串是有意义的：调用方（`SearchIndex::build`）用它决定
    /// "这条需不需要走拼音匹配"，纯 ASCII 的 label 走拼音只会白费算力。
    ///
    /// 处理顺序是先汉字后非汉字，所以 `v2 会议` 会得到 `v2huiyi`——顺序与原文
    /// 一致，不会因为分类而打乱，这样 `huiyi` 这类查询才能前缀命中。
    pub fn build(text: &str) -> Option<Self> {
        if !text.chars().any(is_han) {
            return None;
        }
        let mut full = String::new();
        let mut initials = String::new();
        // 非汉字在这里不参与拼音，交给下面的 ASCII 分支保底
        for pinyin in text.to_pinyin().flatten() {
            let plain = pinyin.plain();
            if !plain.is_empty() {
                full.push_str(plain);
                initials.push_str(pinyin.first_letter());
            }
        }
        for ch in text.chars().filter(|c| !is_han(*c)) {
            if ch.is_alphanumeric() {
                full.push(ch.to_ascii_lowercase());
                initials.push(ch.to_ascii_lowercase());
            }
        }
        (!full.is_empty()).then_some(Self { full, initials })
    }
}

/// 是否是汉字。
///
/// 只认两个基本区：CJK 统一汉字（4E00–9FFF）与扩展 A（3400–4DBF）。
/// 扩展 B 及之后的生僻字在片段标题里几乎不会出现，而多音节/多音字的处理
/// 依赖 `pinyin` crate 的字表，超出范围的字会在 `to_pinyin` 里直接返回 `None`
/// （被 `flatten` 跳过），不会 panic。
fn is_han(ch: char) -> bool {
    matches!(ch as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_full_and_initials() {
        let index = PinyinIndex::build("会议纪要").unwrap();
        assert_eq!(index.full, "huiyijiyao");
        assert_eq!(index.initials, "hyjy");
    }

    #[test]
    fn plain_ascii_has_no_index() {
        assert!(PinyinIndex::build("console.log").is_none());
    }
}
