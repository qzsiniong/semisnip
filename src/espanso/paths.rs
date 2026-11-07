//! espanso 的目录发现。
//!
//! 一律先问 espanso 自己（`espanso path base`），问不到才退回平台默认路径——
//! 用户可以改 `$CONFIG`，硬编码会读错目录。

use std::path::PathBuf;
use std::sync::OnceLock;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn config_root() -> PathBuf {
    ROOT.get_or_init(|| {
        if let Some(root) = from_cli() {
            return root;
        }
        if let Ok(path) = std::env::var("CONFIG") {
            if !path.trim().is_empty() {
                return PathBuf::from(path);
            }
        }
        fallback()
    })
    .clone()
}

fn from_cli() -> Option<PathBuf> {
    // 注意：`espanso path base` 返回的是 base.yml **文件**，不是目录。
    // 要目录只能问 `path config`。
    let out = std::process::Command::new(super::cli::EspansoCli::discover().bin())
        .args(["path", "config"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // 输出形如 `Config: /Users/x/Library/Application Support/espanso`
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|line| {
            let candidate = line.rsplit(':').next().unwrap_or(line).trim();
            candidate.starts_with('/').then(|| PathBuf::from(candidate))
        })
        .next()
}

fn fallback() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("espanso")
    } else if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or(home)
            .join("espanso")
    } else {
        home.join(".config").join("espanso")
    }
}

pub fn match_dir() -> PathBuf {
    config_root().join("match")
}

/// espanso 的运行时目录（socket 在那里）。
pub fn runtime_dir() -> PathBuf {
    if let Ok(out) = std::process::Command::new(super::cli::EspansoCli::discover().bin())
        .args(["path", "runtime"])
        .output()
    {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(line) = text.lines().find(|l| l.contains('/')) {
                let candidate = line.rsplit(':').next().unwrap_or(line).trim();
                if candidate.starts_with('/') {
                    return PathBuf::from(candidate);
                }
            }
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library")
        .join("Caches")
        .join("espanso")
}

pub fn worker_socket() -> PathBuf {
    runtime_dir().join("espansoworkerv2.sock")
}
