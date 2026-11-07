//! daemon 侧：UDS 监听 + 单实例。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use super::proto::{Request, Response};
use crate::app::event::Event;
use crate::config;

/// 绑定 socket 并起监听线程。已有 daemon 在跑时返回 Err。
pub fn serve(tx: Sender<Event>) -> Result<PathBuf, String> {
    config::ensure_dirs();
    let path = config::socket_path();

    // 单实例：能连上就说明有活的 daemon；连不上说明是上次崩溃留下的死文件。
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err("已有 snipshot daemon 在运行".into());
        }
        let _ = std::fs::remove_file(&path);
    }

    let listener =
        UnixListener::bind(&path).map_err(|e| format!("绑定 {} 失败：{e}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    std::thread::Builder::new()
        .name("snipshot-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let tx = tx.clone();
                        std::thread::Builder::new()
                            .name("snipshot-ipc-conn".into())
                            .spawn(move || handle(stream, tx))
                            .ok();
                    }
                    Err(err) => log::warn!("accept 失败：{err}"),
                }
            }
        })
        .map_err(|e| format!("启动 IPC 线程失败：{e}"))?;

    Ok(path)
}

fn handle(stream: UnixStream, tx: Sender<Event>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => dispatch(req, &tx),
        Err(err) => Response::error(format!("请求格式错误：{err}")),
    };

    if let Ok(mut payload) = serde_json::to_string(&response) {
        payload.push('\n');
        let _ = reader.into_inner().write_all(payload.as_bytes());
    }
}

fn dispatch(req: Request, tx: &Sender<Event>) -> Response {
    use super::proto::PROTOCOL_VERSION;
    match req {
        Request::Ping { v } => {
            if v != PROTOCOL_VERSION {
                return Response::error(format!(
                    "协议版本不匹配：daemon={PROTOCOL_VERSION} cli={v}"
                ));
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
    }
}
