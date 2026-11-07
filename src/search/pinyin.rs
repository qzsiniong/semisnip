//! 中文的拼音索引。
//!
//! 非激活面板收不到输入法（见计划 R3），所以搜索框里打不出汉字；
//! 但片段标签大多是中文——用全拼与首字母把这块补回来：
//! 打 `hyjy` 或 `hy` 都能找到「会议纪要」。

use pinyin::ToPinyin;

#[derive(Clone, Debug, Default)]
pub struct PinyinIndex {
    /// 全拼，如 `huiyijiyao`
    pub full: String,
    /// 首字母，如 `hyjy`
    pub initials: String,
}

impl PinyinIndex {
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
