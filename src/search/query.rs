//! 查询解析：严格字符集 + 空白分词 + `:tag` 标签过滤。
//!
//! 面板没有输入法，也刻意不支持复杂语法——要的是"边打边快"。
//! 输入层只认 **小写字母、数字、空格、冒号**：
//! - 大写字母自动转小写（按 Shift 后仍然打得进去）；
//! - 其余字符（标点、中文、粘贴进来的引号……）直接丢弃，并在解析条里标成「忽略」。
//!
//! 这样做的收益是查询串永远干净：粘贴一份带 `;;`、逗号、引号的文本过来，
//! 也不会把词元切成碎片。

/// 词元的类型。解析条按这个着色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// 关键词：参与 AND 匹配。
    Keyword,
    /// 标签：`:tag`，只做过滤，不参与打分。
    Tag,
    /// 被丢弃的字符。解析条显示"忽略"，让用户知道为什么某个字符没生效。
    Ignored,
}

/// 解析条里的一段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedToken {
    pub text: String,
    pub kind: TokenKind,
}

/// 一次查询的解析结果。
///
/// `raw` 保留原始输入（用于回显），`tokens` / `tags` 是规整后的可匹配部分。
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// 去掉首尾空白后的原始输入。
    pub raw: String,
    /// 关键词词元，全部小写。空 `Vec` 表示"没有关键词"。
    pub tokens: Vec<String>,
    /// 标签过滤词元（不含冒号），全部小写。
    pub tags: Vec<String>,
    /// 解析条用：关键词、标签与被忽略的字符，按输入顺序。
    pub terms: Vec<ParsedToken>,
}

impl Query {
    /// 解析：严格过滤字符集 → 按空白分词 → 归类成关键词 / 标签。
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim().to_string();

        // 先按字符集过滤。被丢弃的字符聚成一段，解析条只显示一次「忽略 xx」，
        // 免得一串粘贴文本把解析条撑爆。
        let mut kept = String::new();
        let mut ignored = String::new();
        for ch in raw.chars() {
            if ch.is_whitespace() {
                kept.push(' ');
            } else if ch == ':' {
                kept.push(':');
            } else if ch.is_ascii_alphanumeric() {
                kept.push(ch.to_ascii_lowercase());
            } else {
                ignored.push(ch);
            }
        }

        let mut tokens = Vec::new();
        let mut tags = Vec::new();
        let mut terms = Vec::new();

        for word in kept.split_whitespace() {
            if let Some(tag) = word.strip_prefix(':') {
                if tag.is_empty() {
                    // 一个孤零零的冒号：当成忽略，不产生空标签。
                    ignored.push(':');
                    continue;
                }
                tags.push(tag.to_string());
                terms.push(ParsedToken {
                    text: format!(":{tag}"),
                    kind: TokenKind::Tag,
                });
            } else {
                tokens.push(word.to_string());
                terms.push(ParsedToken {
                    text: word.to_string(),
                    kind: TokenKind::Keyword,
                });
            }
        }

        // 标签去重（不区分大小写），保留第一次出现的顺序。
        let mut seen = Vec::new();
        tags.retain(|tag| {
            if seen.iter().any(|t: &String| t == tag) {
                false
            } else {
                seen.push(tag.clone());
                true
            }
        });

        if !ignored.is_empty() {
            terms.push(ParsedToken {
                text: ignored,
                kind: TokenKind::Ignored,
            });
        }

        Self {
            raw,
            tokens,
            tags,
            terms,
        }
    }

    /// 是否为空查询。空查询走"全部命中、按使用习惯排"的快路径。
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.tags.is_empty()
    }

    /// 是否只有标签过滤、没有关键词。
    pub fn tag_only(&self) -> bool {
        self.tokens.is_empty() && !self.tags.is_empty()
    }
}

/// trigger 的"核心部分"：去掉开头的符号（`;;`、`:`、`,` ……）。
///
/// 查询侧已经丢弃符号了，这个函数只服务于 trigger 一侧：`;;log` 与 `log`
/// 应该被视为同一个词。
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

    #[test]
    fn illegal_characters_are_dropped_and_reported() {
        // 标点、中文、引号一律丢弃；大写自动转小写。
        let q = Query::parse("JS, 日志 \"log\"");
        assert_eq!(q.tokens, vec!["js", "log"]);
        assert!(
            q.terms.iter().any(|t| t.kind == TokenKind::Ignored),
            "解析条必须能告诉用户有字符被忽略"
        );
    }

    #[test]
    fn pasted_trigger_prefix_is_stripped_by_charset_filter() {
        // `;;` / `,,` 里的符号不属于查询字符集，会被直接丢掉。
        assert_eq!(Query::parse(",,log").tokens, vec!["log"]);
        assert_eq!(Query::parse(";;sig").tokens, vec!["sig"]);
        assert_eq!(Query::parse("/usr log").tokens, vec!["usr", "log"]);
    }

    #[test]
    fn colon_prefix_becomes_a_tag_filter() {
        let q = Query::parse(":js log :表单");
        assert_eq!(q.tokens, vec!["log"]);
        // 标签只保留 ASCII 字符：中文标签无法从键盘输入，会被字符集挡掉。
        assert_eq!(q.tags, vec!["js"]);
        // 中文在 `:表单` 这个词里被丢弃，只剩一个空标签，因此不产生标签。
        assert!(q.terms.iter().any(|t| t.kind == TokenKind::Tag));
    }
}
