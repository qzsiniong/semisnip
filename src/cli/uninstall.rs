//! `snipshot uninstall`：把 snipshot 自己装进去的东西拆掉。
//!
//! 边界很硬：**只删 snipshot 自己创建的文件**。espanso 的 `match/**`（用户的
//! 片段）、`config/default.yml`、`~/.config/espanso` 一律不碰——除了 snipshot
//! 生成的那一个 `snipshot.yml`。
//!
//! 默认是「演练」：只打印计划。真删要显式 `--yes`，因为这是不可逆操作。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::config;

const APP: &str = "/Applications/Snipshot.app";
const LINK: &str = "/usr/local/bin/snipshot";
const LABEL: &str = "com.snipshot.daemon";

const USAGE: &str = "\
用法:
  snipshot uninstall            演练：只列出将被删除的内容
  snipshot uninstall --yes      真正执行
  snipshot uninstall --yes --purge   同时删除配置、使用频率与日志
  snipshot uninstall --yes --keep-app  保留 /Applications/Snipshot.app";

struct Entry {
    /// 展示用的说明
    label: &'static str,
    path: PathBuf,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 普通文件
    File,
    /// 目录（会递归删）
    Dir,
    /// 指向 .app 的软链：只在自己是我们的链接时才删
    Link,
}

impl Entry {
    fn present(&self) -> bool {
        match self.kind {
            Kind::Link => {
                std::fs::symlink_metadata(&self.path)
                    .map(|meta| meta.file_type().is_symlink())
                    .unwrap_or(false)
                    && points_into_app(&self.path)
            }
            Kind::File => self.path.is_file(),
            Kind::Dir => self.path.is_dir(),
        }
    }
}

pub fn run(args: &[String]) -> ExitCode {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let dry_run = !args.iter().any(|arg| arg == "--yes" || arg == "-y");
    let purge = args.iter().any(|arg| arg == "--purge");
    let keep_app = args.iter().any(|arg| arg == "--keep-app");

    let mut entries = vec![
        Entry {
            label: "LaunchAgent（开机自启）",
            path: launch_agent_path(),
            kind: Kind::File,
        },
        Entry {
            label: "espanso 降级触发定义",
            path: crate::espanso::paths::match_dir().join("snipshot.yml"),
            kind: Kind::File,
        },
        Entry {
            label: "CLI 软链",
            path: PathBuf::from(LINK),
            kind: Kind::Link,
        },
    ];
    if !keep_app {
        entries.push(Entry {
            label: "应用程序",
            path: PathBuf::from(APP),
            kind: Kind::Dir,
        });
    }
    if purge {
        entries.push(Entry {
            label: "配置 / 使用频率 / 日志",
            path: config::support_dir(),
            kind: Kind::Dir,
        });
        entries.push(Entry {
            label: "socket 与缓存",
            path: config::cache_dir(),
            kind: Kind::Dir,
        });
    }

    // 先给个总览，让用户能一眼看出删的是什么。
    println!(
        "{}",
        if dry_run {
            "将要删除（演练，未执行）："
        } else {
            "正在删除："
        }
    );
    let mut present = 0;
    for entry in &entries {
        if entry.present() {
            present += 1;
            println!("  ✅ {}", entry.label);
            println!("     {}", entry.path.display());
        } else {
            println!("  ·  {}（不存在）", entry.label);
        }
    }
    let daemon_running = daemon_pids().is_some_and(|pids| !pids.is_empty());
    println!(
        "  {} daemon 进程{}",
        if daemon_running { "✅" } else { "· " },
        if daemon_running {
            "（会先停掉）"
        } else {
            "（未运行）"
        }
    );

    if dry_run {
        if present == 0 && !daemon_running {
            println!("\n没有发现需要清理的内容。");
            return ExitCode::SUCCESS;
        }
        println!("\n以上不会动 espanso 的片段：你的 YAML 原样保留。");
        println!("确认无误后加 --yes 执行（可再加 --purge 一并删除配置与日志）。");
        return ExitCode::SUCCESS;
    }

    // 停 daemon 必须在删 app 之前：launchd 的 ProgramArguments 指向 app 内部。
    let stopped = stop_daemon();
    if !stopped.killed.is_empty() {
        println!(
            "\n已停止 daemon（pid {}）",
            stopped
                .killed
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    } else if stopped.booted_out {
        println!("\n已从 launchd 卸载 daemon");
    } else {
        println!("\ndaemon 未在运行");
    }

    let mut failures = 0;
    for entry in &entries {
        if !entry.present() {
            continue;
        }
        match remove(entry) {
            Ok(()) => println!("已删除 {}", entry.path.display()),
            Err(err) => {
                failures += 1;
                println!("删除 {} 失败：{err}", entry.path.display());
            }
        }
    }

    println!();
    if config::config_path().exists() {
        println!("配置与日志保留在 {}", config::support_dir().display());
        println!("想彻底清干净：snipshot uninstall --yes --purge");
    } else {
        println!("配置、使用频率与日志已删除。");
    }
    println!("记得执行 `espanso restart`，让触发定义从 espanso 里消失。");

    if failures > 0 {
        println!("\n有 {failures} 项没能删掉，可能需要 sudo 或已被占用。");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn launch_agent_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn points_into_app(link: &Path) -> bool {
    std::fs::read_link(link)
        .map(|target| target.starts_with(APP))
        .unwrap_or(false)
}

fn remove(entry: &Entry) -> std::io::Result<()> {
    match entry.kind {
        Kind::File | Kind::Link => std::fs::remove_file(&entry.path),
        Kind::Dir => {
            // 纵深防御：只允许删自己名下的目录，别让路径拼错变成删家目录。
            let safe = entry.path.ends_with("snipshot") || entry.path == Path::new(APP);
            if !safe {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "路径不在白名单内，拒绝删除",
                ));
            }
            std::fs::remove_dir_all(&entry.path)
        }
    }
}

fn daemon_pids() -> Option<Vec<i32>> {
    let out = std::process::Command::new("pgrep")
        .arg("-x")
        .arg("snipshot")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let me = std::process::id() as i32;
    Some(
        text.split_whitespace()
            .filter_map(|pid| pid.parse::<i32>().ok())
            .filter(|pid| *pid != me) // 别把自己杀了
            .collect(),
    )
}

#[derive(Debug, Default)]
struct Stop {
    killed: Vec<i32>,
    booted_out: bool,
}

fn stop_daemon() -> Stop {
    let mut stop = Stop::default();

    // 先让 launchd 忘掉它，否则 KeepAlive 会立刻把 daemon 拉回来。
    // 用 output() 而不是 status()：job 不存在时 launchctl 会往 stderr 吐
    // "Boot-out failed: 3: No such process"，那是正常情况，不该吓用户。
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| text.trim().to_string());
    if let Some(uid) = uid
        && let Ok(out) = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{LABEL}")])
            .output()
    {
        stop.booted_out = out.status.success();
    }

    let pids = daemon_pids().unwrap_or_default();
    for pid in &pids {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
    }
    // 给 tap 一点时间释放，再确认进程确实走了。
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if daemon_pids().map(|left| left.is_empty()).unwrap_or(true) {
            break;
        }
    }
    stop.killed = pids;
    stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_delete_paths_outside_whitelist() {
        let entry = Entry {
            label: "伪装成我们的目录",
            path: PathBuf::from("/tmp/not-snipshot"),
            kind: Kind::Dir,
        };
        assert!(remove(&entry).is_err());
    }

    #[test]
    fn link_is_only_ours_when_it_points_into_the_app() {
        let dir = std::env::temp_dir().join("snipshot-uninstall-test");
        let _ = std::fs::create_dir_all(&dir);
        let link = dir.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/Applications/Snipshot.app/Contents/MacOS/snipshot", &link)
            .unwrap();
        assert!(points_into_app(&link));

        let other = dir.join("other");
        let _ = std::fs::remove_file(&other);
        std::os::unix::fs::symlink("/usr/bin/true", &other).unwrap();
        assert!(!points_into_app(&other));

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&other);
        let _ = std::fs::remove_dir(&dir);
    }
}
