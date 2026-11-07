//! 查询解析：空白分词，全部 AND。
//!
//! 刻意不支持复杂语法——面板要的是"边打边快"，不是查询语言。

/// 一次查询的解析结果。
///
/// 保留 `raw` 是为了让界面能原样回显用户输入（包括大小写与空格），
/// 而匹配只用 `tokens`——两者分开省掉"回显时重新拼串"的麻烦。
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// 去掉首尾空白后的原始输入。
    pub raw: String,
    /// 小写化、去空的词元。空 `Vec` 表示"列出全部"。
    pub tokens: Vec<String>,
}

impl Query {
    /// 解析：去首尾空白 → 按空白分词 → 转小写 → 丢空词。
    ///
    /// 转小写发生在分词之后，而不是先整体转小写：两者结果相同，但分词后再转
    /// 可以顺手把 `filter(|t| !t.is_empty())` 一次做完。
    /// 连续空格不会产生空词元（`split_whitespace` 的语义），所以 `js   log`
    /// 与 `js log` 等价。
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim().to_string();
        let tokens: Vec<String> = raw
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        Self { raw, tokens }
    }

    /// 是否为空查询。空查询在 `rank` 里走"全部命中、按使用习惯排"的分支。
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// trigger 的"核心部分"：去掉开头的符号（`;;`、`:`、`,` ……）。
/// 让用户打 `log` 就能精确命中 `;;log`。
pub fn core_of(trigger: &str) -> &str {
    trigger.trim_start_matches(|c: char| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_lowercases() {
        let q = Query::parse("  JS   log ");
        assert_eq!(q.tokens, vec!["js", "log"]);
        assert!(!q.is_empty());
    }

    #[test]
    fn core_strips_leading_symbols() {
        assert_eq!(core_of(";;log"), "log");
        assert_eq!(core_of(":date"), "date");
        assert_eq!(core_of("plain"), "plain");
    }
}
