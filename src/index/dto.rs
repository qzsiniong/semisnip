//! espanso match YAML 的反序列化形状。
//!
//! 设计原则：**宽容**。espanso 的 schema 比这里宽（图片、html、markdown、
//! form、regex、app filter……），任何不认识的键都不该让整个文件解析失败——
//! 解析失败的文件在面板里就"消失"了，比显示不全更糟。

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct MatchFile {
    #[serde(default)]
    pub matches: Vec<RawMatch>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawMatch {
    /// `trigger: ";;log"`
    #[serde(default)]
    pub trigger: Option<TriggerField>,
    /// `triggers: ["a", "b"]`
    #[serde(default)]
    pub triggers: Option<Vec<String>>,
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
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
    #[serde(default)]
    pub form_fields: Option<BTreeMap<String, RawFormField>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum TriggerField {
    One(String),
    Many(Vec<String>),
}

impl TriggerField {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RawVar {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub params: serde_yaml::Value,
}

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
