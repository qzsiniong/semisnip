//! CLI 侧：把「请求 daemon」收敛成一个函数，失败时自愈拉起。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use super::proto::{Request, Response};
use crate::{config, usage};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(150);

fn connect(path: &Path, timeout: Duration) -> anyhow::Result<UnixStream> {
    let _ = timeout;
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_millis(600)))?;
    stream.set_write_timeout(Some(Duration::from_millis(600)))?;
    Ok(stream)
}

pub fn call(req: &Request, timeout: Duration) -> anyhow::Result<Response> {
    let path = config::socket_path();
    let mut stream = connect(&path, timeout)?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut out = String::new();
    reader.read_line(&mut out)?;
    Ok(serde_json::from_str(out.trim())?)
}

pub fn ping() -> anyhow::Result<()> {
    match call(
        &Request::Ping {
            v: super::proto::PROTOCOL_VERSION,
        },
        CONNECT_TIMEOUT,
    )? {
        Response::Ok { .. } => Ok(()),
        Response::Error { message, .. } => Err(anyhow::anyhow!(message)),
    }
}

/// 发 `open`。socket 不在时先拉起 daemon 再重试一次——首次安装后无需手动启动。
pub fn open(origin: super::proto::Origin, trigger: &str) -> anyhow::Result<()> {
    let req = Request::Open {
        v: super::proto::PROTOCOL_VERSION,
        origin,
        trigger: trigger.to_string(),
        at_ms: usage::now_ms() as u64,
    };

    match call(&req, CONNECT_TIMEOUT) {
        Ok(Response::Ok { .. }) => return Ok(()),
        Ok(Response::Error { message, .. }) => return Err(anyhow::anyhow!(message)),
        Err(err) => log::debug!("首连失败，尝试拉起 daemon：{err}"),
    }

    spawn_daemon()?;
    let deadline = Instant::now() + Duration::from_millis(600);
    loop {
        std::thread::sleep(Duration::from_millis(25));
        if let Ok(Response::Ok { .. }) = call(&req, CONNECT_TIMEOUT) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(anyhow::anyhow!("daemon 未能在 600ms 内就绪"));
        }
    }
}

pub fn reload() -> anyhow::Result<()> {
    call(
        &Request::Reload {
            v: super::proto::PROTOCOL_VERSION,
        },
        CONNECT_TIMEOUT,
    )?;
    Ok(())
}

fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}
