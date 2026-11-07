//! CLI 侧：把「请求 daemon」收敛成一个函数，失败时自愈拉起。
//!
//! CLI 与 daemon 是两个进程，通信走 `$TMPDIR/semisnip.sock` 上的 NDJSON（见 `proto`）。
//! 这里的每个函数都只在 CLI 里跑，daemon 从不调用它们——所以可以放心用阻塞 IO。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use super::proto::{Request, Response};
use crate::{paths, usage};

/// 单次请求的读写超时。
///
/// 600ms 是"用户能感觉到卡"的阈值：`open` 这条路径由按键触发，慢于此值面板
/// 还没出现用户就会以为没响应。daemon 处理这些请求都是内存操作，正常在 1ms 内。
const IO_TIMEOUT: Duration = Duration::from_millis(600);

/// 连接 daemon 的 socket。
///
/// 没有"连接超时"可言：Unix domain socket 的 `connect` 要么立刻成功，要么立刻
/// 返回 `ENOENT`/`ECONNREFUSED`（daemon 没起、或上次崩了留下死文件），不会像 TCP
/// 那样挂住。所以只需要给之后的读写设超时——那是防"daemon 活着但不回话"。
fn connect(path: &Path) -> anyhow::Result<UnixStream> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

/// 发一条请求、读一行响应。
///
/// 协议是"一问一答"：写完换行立刻等一行。不做重试——重试语义应该由调用方
/// 决定（`open` 会重试，`ping` 不会）。
fn call(req: &Request) -> anyhow::Result<Response> {
    let path = paths::socket_path();
    let mut stream = connect(&path)?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    // 裸 `UnixStream` 没有写缓冲，`write_all` 返回即为已发出，不需要 flush。
    let mut reader = BufReader::new(stream);
    let mut out = String::new();
    reader.read_line(&mut out)?;
    Ok(serde_json::from_str(out.trim())?)
}

/// 探活。成功仅代表 daemon 能应答，不代表它的 tap 可用（那要 `status`）。
pub fn ping() -> anyhow::Result<()> {
    match call(&Request::Ping {
        v: super::proto::PROTOCOL_VERSION,
    })? {
        Response::Ok { .. } => Ok(()),
        Response::Error { message, .. } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("意外的响应：{other:?}")),
    }
}

/// 问 daemon「你的 tap 装上了吗」。
///
/// 不能本地自查：TCC 授权按进程判，`doctor` 从终端跑会继承终端的授权，
/// 而 launchd 起的 daemon 是另一份授权。这里问的是 daemon 的真实状态。
pub fn status() -> anyhow::Result<bool> {
    match call(&Request::Status {
        v: super::proto::PROTOCOL_VERSION,
    })? {
        Response::Status { tap_ready, .. } => Ok(tap_ready),
        Response::Error { message, .. } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("意外的响应：{other:?}")),
    }
}

/// 发 `open`。socket 不在时先拉起 daemon 再重试一次——首次安装后无需手动启动。
///
/// 重试窗口 600ms 是经验值：冷启动要加载配置、扫描 match 目录、还可能跑一次
/// `espanso match list`。超过这个时间就报错，让用户看到明确失败而不是无限等待。
///
/// **stdout 必须保持为空**：这条路径被 espanso 的降级 match（shell 变量）调用，
/// espanso 会把命令的 stdout 当作展开内容注入文档。所有输出都走 stderr 或日志。
pub fn open(origin: super::proto::Origin, trigger: &str) -> anyhow::Result<()> {
    let req = Request::Open {
        v: super::proto::PROTOCOL_VERSION,
        origin,
        trigger: trigger.to_string(),
        at_ms: usage::now_ms() as u64,
    };

    match call(&req) {
        Ok(Response::Ok { .. }) => return Ok(()),
        Ok(Response::Error { message, .. }) => return Err(anyhow::anyhow!(message)),
        Ok(other) => return Err(anyhow::anyhow!("意外的响应：{other:?}")),
        Err(err) => log::debug!("首连失败，尝试拉起 daemon：{err}"),
    }

    spawn_daemon()?;
    let deadline = Instant::now() + Duration::from_millis(600);
    loop {
        std::thread::sleep(Duration::from_millis(25));
        if let Ok(Response::Ok { .. }) = call(&req) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(anyhow::anyhow!("daemon 未能在 600ms 内就绪"));
        }
    }
}

/// 请 daemon 重建索引。响应体不关心——成功与否由下一次 `status`/`ping` 体现。
pub fn reload() -> anyhow::Result<()> {
    call(&Request::Reload {
        v: super::proto::PROTOCOL_VERSION,
    })?;
    Ok(())
}

/// 以 `daemon` 子命令拉起自己。
///
/// 三个标准流全部 `null`：CLI 进程可能马上退出，子进程若还握着终端会把 shell
/// 挂住；而我们也不想让 daemon 的输出混进 CLI 的 stdin/stdout。
/// 不用 `nohup`/`setsid`：daemon 自己会 detach 到 gpui 事件循环，父进程退出不影响它。
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
