//! espanso match YAML 的反序列化形状。
//!
//! 设计原则：**宽容**。espanso 的 schema 比这里宽（图片、html、markdown、
//! form、regex、app filter……），任何不认识的键都不该让整个文件解析失败——
//! 解析失败的文件在面板里就"消失"了，比显示不全更糟。

use std::collections::BTreeMap;

use serde::Deserialize;

/// 一个 YAML 文件的顶层形状。espanso 规定顶层是 `matches:` 列表。
///
/// `#[serde(default)]` 不能省：空的 match 文件（只有注释）在 espanso 里是合法的，
/// 没有它会让整个文件解析失败。
#[derive(Debug, Default, Deserialize)]
pub struct MatchFile {
    #[serde(default)]
    pub matches: Vec<RawMatch>,
}

/// 一条 match 的原始字段，与 espanso 的键一一对应。
///
/// 这里**可以出现多个正文来源**（replace / markdown / html / image / form），
/// espanso 的实际行为是"按优先顺序取第一个存在的"，这里不校验互斥性——
/// 用户写错了我们照单全收，由 `loader::build_snippet` 决定用哪个。
///
/// 刻意不声明 `unknown` 之外的其他 espanso 字段（`regex` 除外）：serde 默认忽略
/// 未知键，所以 `app_filter` / `propagate_case` / `force_mode` 这些不会导致失败。
#[derive(Debug, Default, Deserialize)]
pub struct RawMatch {
    /// `trigger: ";;log"`
    #[serde(default)]
    pub trigger: Option<TriggerField>,
    /// `triggers: ["a", "b"]`
    #[serde(default)]
    pub triggers: Option<Vec<String>>,
    /// 正则 trigger。面板不把它当可输入的触发符，仅作信息展示。
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
    /// 图片路径。相对路径以所在 YAML 文件为基准解析（见 `resolve_image_path`）。
    #[serde(default)]
    pub image_path: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub search_terms: Vec<String>,
    #[serde(default)]
    pub word: bool,
    #[serde(default)]
    pub vars: Vec<RawVar>,
    /// `form:` 简写（多行 layout 字符串）
    #[serde(default)]
    pub form: Option<String>,
    /// `form:` 的完整写法：字段定义表。用 `BTreeMap` 是为了字段顺序稳定
    /// （预览里字段列表每次都一样，不会因为 HashMap 随机序抖动）。
    #[serde(default)]
    pub form_fields: Option<BTreeMap<String, RawFormField>>,
}

/// `trigger:` 的两种写法。
///
/// `untagged` 让 serde 按形状自己挑：单个字符串走 `One`，列表走 `Many`。
/// 不能写成 `Vec<String>` + 自定义反序列化，那样错误信息会变得难以定位。
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum TriggerField {
    One(String),
    Many(Vec<String>),
}

impl TriggerField {
    /// 归一成列表，调用方不必区分两种写法。
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

/// 一个变量的原始定义。`params` 保持成 `Value`，按类型取用（见 `VarSpec`）。
///
/// `name` / `type` 没有 `default`：缺了它们这条变量就没法用，让 serde 直接报错
/// 比解析出一个空名字要好——后者会在预览里显示成 `{{}}`。
#[derive(Debug, Deserialize)]
pub struct RawVar {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub params: serde_yaml::Value,
}

/// `form_fields` 里的单个字段。
///
/// `default` / `values` 用 `Value` 而不是 `String`：espanso 允许写数字和布尔
/// （比如 `default: 0`），直接按 `String` 解会让整条 match 失败。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawFormField {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
    #[serde(default)]
    pub values: Vec<serde_yaml::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shape_espanso_actually_uses() {
        let raw = r#"
matches:
  - trigger: ";;log"
    replace: "console.log($|$)"
    label: "JS: console.log"
    search_terms: [log, debug, js]
  - triggers: [":d1", ":d2"]
    replace: "{{mydate}}"
    vars:
      - name: mydate
        type: date
        params:
          format: "%m/%d/%Y"
  - trigger: ":form"
    form: |
      Hey [[name]]
    form_fields:
      name:
        type: text
        default: world
"#;
        let file: MatchFile = serde_yaml::from_str(raw).unwrap();
        assert_eq!(file.matches.len(), 3);
        assert_eq!(file.matches[1].triggers.as_ref().unwrap().len(), 2);
        assert_eq!(file.matches[2].form_fields.as_ref().unwrap().len(), 1);
    }
}
