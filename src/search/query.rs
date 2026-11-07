//! 查询解析：空白分词，全部 AND。
//!
//! 刻意不支持复杂语法——面板要的是"边打边快"，不是查询语言。

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub raw: String,
    pub tokens: Vec<String>,
}

impl Query {
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim().to_string();
        let tokens: Vec<String> = raw
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        Self { raw, tokens }
    }

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
