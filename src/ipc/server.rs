//! daemon 侧：UDS 监听 + 单实例。
//!
//! 只接受本机连接，并且 socket 权限设为 `0600`——同一台机器上的其他用户
//! 不该能打开你的片段面板（面板内容会暴露片段正文）。
//!
//! 每个连接一个线程：请求都是毫秒级的，线程数由连接数决定，实际不会超过个位数。
//! 刻意不做线程池——引入调度器换不来可测量的收益，却多一层出错可能。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use super::proto::{Request, Response};
use crate::app::event::Event;
use crate::paths;

/// 回答「daemon 的 tap 真的装上了吗」。由调用方注入，ipc 不认识 trigger。
pub type TapProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// 绑定 socket 并起监听线程。返回 socket 路径（调用方打日志用）。
///
/// 单实例靠"能不能连上"判断，而不是 pid 文件：pid 文件在崩溃后会留下错误的
/// 数字，而 socket 的死活是系统层面的真相——daemon 活着就能连上，崩了就
/// `ECONNREFUSED`，此时那个文件是垃圾，删掉重绑。
pub fn serve(tx: Sender<Event>, tap_ready: TapProbe) -> Result<PathBuf, ServeError> {
    paths::ensure_dirs();
    let path = paths::socket_path();

    // 单实例：能连上就说明有活的 daemon；连不上说明是上次崩溃留下的死文件。
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(ServeError::AlreadyRunning);
        }
        let _ = std::fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)
        .map_err(|e| ServeError::Io(format!("绑定 {} 失败：{e}", path.display())))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    std::thread::Builder::new()
        .name("semisnip-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let tx = tx.clone();
                        let tap_ready = tap_ready.clone();
                        std::thread::Builder::new()
                            .name("semisnip-ipc-conn".into())
                            .spawn(move || handle(stream, tx, tap_ready))
                            .ok();
                    }
                    Err(err) => log::warn!("accept 失败：{err}"),
                }
            }
        })
        .map_err(|e| ServeError::Io(format!("启动 IPC 线程失败：{e}")))?;

    Ok(path)
}

/// 启动失败的两类原因。分开是因为处理方式完全相反：
/// 「已有实例」要静默退出（退出码 0），真正的 IO 错误要退出码 1 让 launchd 重试。
#[derive(Debug)]
pub enum ServeError {
    AlreadyRunning,
    Io(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "已有 semisnip daemon 在运行"),
            Self::Io(message) => write!(f, "{message}"),
        }
    }
}

/// 处理一个连接：读一行请求 → 派发 → 写一行响应，然后关闭。
///
/// 协议是"一问一答一连接"，不复用连接：CLI 每次请求都是新进程，复用反而要
/// 处理连接生命周期与超时。读完一行就 `into_inner()` 拿回流写响应，
/// 因为 `BufReader` 独占所有权。
///
/// 解析失败也回一个 `Error` 响应而不是直接断开：CLI 那边能拿到具体原因，
/// 比"连接被重置"好排查。
fn handle(stream: UnixStream, tx: Sender<Event>, tap_ready: TapProbe) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => dispatch(req, &tx, &tap_ready),
        Err(err) => Response::error(format!("请求格式错误：{err}")),
    };

    if let Ok(mut payload) = serde_json::to_string(&response) {
        payload.push('\n');
        let _ = reader.into_inner().write_all(payload.as_bytes());
    }
}

/// 把请求翻成事件投给调度器。**不在这里直接改状态**：
/// 所有副作用都必须发生在那一个事件循环里，否则会和 tap、文件监听抢同一份状态。
fn dispatch(req: Request, tx: &Sender<Event>, tap_ready: &TapProbe) -> Response {
    use super::proto::PROTOCOL_VERSION;
    match req {
        // Ping 故意不校验版本：「还活着吗」不该因为版本号而答不上来。
        // 否则升级完 CLI、还没换 daemon 的那几分钟，doctor 会谎报「未运行」。
        Request::Ping { v } => {
            if v != PROTOCOL_VERSION {
                log::debug!("ping 版本不一致：daemon={PROTOCOL_VERSION} cli={v}");
            }
            Response::ok()
        }
        Request::Open { origin, .. } => {
            // 这条路径上 `;;` 已经被调用方（espanso 或用户）处理掉，无需回删。
            if tx.send(Event::Open { origin, erase: 0 }).is_err() {
                return Response::error("UI 已退出");
            }
            Response::ok()
        }
        Request::Reload { .. } => {
            let _ = tx.send(Event::Reload);
            Response::ok()
        }
        Request::Status { v } => {
            if v != PROTOCOL_VERSION {
                return Response::error(format!(
                    "协议版本不匹配：daemon={PROTOCOL_VERSION} cli={v}"
                ));
            }
            Response::Status {
                v: PROTOCOL_VERSION,
                tap_ready: tap_ready(),
            }
        }
    }
}
