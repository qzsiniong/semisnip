//! daemon ↔ CLI 的线协议：NDJSON，一行一个 JSON，双向都带版本号。
//!
//! 约定（见 packaging/espanso/snipshot.yml）：
//! - `snipshot trigger` 成功时 **stdout 必须为空**——espanso 会把 stdout
//!   当作展开内容注入，多打印一个字节就会污染用户文档。
//! - 所以这里所有错误都走 stderr，响应只用于 CLI 判断成败。

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// snipshot 自己的 tap 命中。
    Tap,
    /// espanso 的 shell match 唤醒（无辅助功能权限时的降级路径）。
    EspansoShell,
    /// 手动执行 `snipshot trigger`。
    #[default]
    Manual,
}

fn version() -> u8 {
    PROTOCOL_VERSION
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Open {
        #[serde(default = "version")]
        v: u8,
        #[serde(default)]
        origin: Origin,
        #[serde(default)]
        trigger: String,
        #[serde(default)]
        at_ms: u64,
    },
    Ping {
        #[serde(default = "version")]
        v: u8,
    },
    Reload {
        #[serde(default = "version")]
        v: u8,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { v: u8 },
    Error { v: u8, message: String },
}

impl Response {
    pub fn ok() -> Self {
        Self::Ok {
            v: PROTOCOL_VERSION,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            v: PROTOCOL_VERSION,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_roundtrip_uses_short_op_names() {
        let raw = r#"{"op":"open","v":1,"origin":"espanso_shell","trigger":";;","at_ms":7}"#;
        let req: Request = serde_json::from_str(raw).unwrap();
        match req {
            Request::Open {
                origin, trigger, ..
            } => {
                assert_eq!(origin, Origin::EspansoShell);
                assert_eq!(trigger, ";;");
            }
            _ => panic!("应解析为 Open"),
        }
    }

    #[test]
    fn open_accepts_missing_optional_fields() {
        let req: Request = serde_json::from_str(r#"{"op":"open"}"#).unwrap();
        assert!(matches!(req, Request::Open { v: 1, .. }));
    }
}
