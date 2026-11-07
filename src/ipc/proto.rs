//! daemon ↔ CLI 的线协议：NDJSON，一行一个 JSON，双向都带版本号。
//!
//! 约定（源文件是 `cli::render_match_file`，随时可用 `semisnip match show` 查看）：
//! - `semisnip trigger` 成功时 **stdout 必须为空**——espanso 会把 stdout
//!   当作展开内容注入，多打印一个字节就会污染用户文档。
//! - 所以这里所有错误都走 stderr，响应只用于 CLI 判断成败。

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u8 = 2;

/// 面板是被谁唤起的。只影响日志与降级判断，不改变面板行为。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// semisnip 自己的 tap 命中。
    Tap,
    /// espanso 的 shell match 唤醒（无辅助功能权限时的降级路径）。
    EspansoShell,
    /// 手动执行 `semisnip trigger`。
    #[default]
    Manual,
}

fn version() -> u8 {
    PROTOCOL_VERSION
}

/// CLI → daemon 的请求。
///
/// 每个变体都带 `v`（协议版本），默认值取 [`PROTOCOL_VERSION`]，所以手写一条
/// `{"op":"open"}` 也能被接受——这条对调试很有用（`nc -U socket` 就能试）。
///
/// 版本策略：`Ping` 不校验（"还活着吗"不该因为版本而答不上来），
/// `Status` 严格校验（它要返回字段，字段含义可能变了），`Open`/`Reload` 目前
/// 不校验——它们的载荷足够简单，跨版本也能正确处理。
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
    /// 打听 daemon 自己的状态。TCC 授权是「按进程」判的：同一个二进制，
    /// 从终端跑是继承终端的授权，从 launchd 起则要单独授权。所以
    /// `doctor` 不能只查自己，必须问 daemon。
    Status {
        #[serde(default = "version")]
        v: u8,
    },
}

/// daemon → CLI 的响应。`status` 字段既是标签也是"这一条是什么"的说明。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok {
        v: u8,
    },
    Error {
        v: u8,
        message: String,
    },
    Status {
        v: u8,
        /// daemon 的键盘 tap 是否真的可用。
        tap_ready: bool,
    },
}

impl Response {
    /// 成功。带上当前协议版本，方便 CLI 发现"daemon 比客户端新"。
    pub fn ok() -> Self {
        Self::Ok {
            v: PROTOCOL_VERSION,
        }
    }

    /// 失败。`message` 直接给人看，调用方（CLI）原样打到 stderr。
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
    fn status_roundtrip_keeps_tap_flag() {
        let response = Response::Status {
            v: PROTOCOL_VERSION,
            tap_ready: true,
        };
        let raw = serde_json::to_string(&response).unwrap();
        assert!(raw.contains("\"status\":\"status\""));
        let back: Response = serde_json::from_str(&raw).unwrap();
        assert!(matches!(
            back,
            Response::Status {
                tap_ready: true,
                ..
            }
        ));
    }

    #[test]
    fn open_accepts_missing_optional_fields() {
        let req: Request = serde_json::from_str(r#"{"op":"open"}"#).unwrap();
        assert!(matches!(req, Request::Open { v, .. } if v == PROTOCOL_VERSION));
    }
}
