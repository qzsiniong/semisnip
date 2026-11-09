//! 全项目**唯一**知道「东西放在哪」的地方。
//!
//! 规矩只有一条：业务代码里不许拼路径。`tray.rs`、`uninstall.rs`、`ipc/` 这些
//! 地方只允许出现 [`config_path`]、[`socket_path`] 这样的调用，不许出现
//! `.join("Library")` 或 `"/Applications"` 这类字面量——路径一旦散开，改一个
//! 目录名要翻十几个文件，漏掉一处就是「配置写了，另一个进程读不到」。
//!
//! 系统目录一律问 [`dirs`]，不自己拼 `~/Library/...`：macOS 上
//! `dirs::config_dir()` 给的正好是 `~/Library/Application Support`，
//! `dirs::cache_dir()` 是 `~/Library/Caches`，换到 Linux/Windows 也是对的。
//! 我们是个常驻菜单栏的 macOS 应用，就该待在系统约定的位置。
//!
//! 路径分两类，界限很硬：
//!
//! - **我们自己的**：`Application Support/semisnip` 放配置、使用频率、日志、
//!   补全脚本；`Caches/semisnip` 放 socket。删掉只丢偏好，不丢任何片段。
//! - **espanso 的**：`match/` 里的片段是用户的，位置由 espanso 自己说了算
//!   （`espanso path config`），我们只往里写一个 `semisnip.yml`。

use std::path::PathBuf;
use std::sync::OnceLock;

/// 目录名。配置目录、缓存目录、CLI 软链名都用它。
pub const APP_DIR: &str = "semisnip";
/// 可执行文件名。`/usr/local/bin` 下的软链、LaunchAgent 的 program 都用它。
pub const BIN_NAME: &str = "semisnip";
/// `.app` 的显示名。
pub const APP_NAME: &str = "Semisnip";
/// LaunchAgent 的 label。改它等于换一个开机自启项。
pub const LABEL: &str = "com.semisnip.daemon";
/// bundle id，必须与 `packaging/macos/Info.plist` 的 `CFBundleIdentifier` 一致。
///
/// 只用来回答一个问题：当前进程是不是由 LaunchServices（Finder 双击）拉起来的
/// ——那种情况下启动参数里没有子命令，`__CFBundleIdentifier` 会是这个值。
pub const BUNDLE_ID: &str = "com.semisnip.app";

pub const CONFIG_FILE: &str = "config.yml";
/// 与配置同目录的 schema，供 `$schema=./config.schema.json` 解析。
pub const SCHEMA_FILE: &str = "config.schema.json";
pub const USAGE_FILE: &str = "usage.json";
/// 片段本地库：停用状态与拼音别名。**不写回用户 YAML**。
pub const LIBRARY_FILE: &str = "library.json";
pub const LOG_FILE: &str = "semisnip.log";
pub const SOCKET_FILE: &str = "semisnip.sock";

/// 家目录。取不到（极罕见）就退到 `/tmp`：调用方只是在拼路径，
/// 写进临时目录也好过让 daemon 因为一个路径 panic 起不来。
pub fn home() -> PathBuf {
    home_opt().unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// 家目录，取不到就是 `None`。
///
/// 给「不能瞎猜」的地方用：往 `/tmp` 里写一个 `.zshrc` 比直接报错糟得多。
pub fn home_opt() -> Option<PathBuf> {
    dirs::home_dir()
}

/// `~/Library/Application Support/semisnip`
///
/// 放一切需要活过重启、值得备份的东西。名字里没有 "config" 是因为它还装日志
/// 和补全脚本；叫 `config_dir` 是为了和 `dirs::config_dir()` 对上。
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join("Library").join("Application Support"))
        .join(APP_DIR)
}

/// `~/Library/Caches/semisnip`
///
/// 只放能随时重建的东西（socket）。系统清理工具删了也不影响使用。
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| home().join("Library").join("Caches"))
        .join(APP_DIR)
}

/// `config.yml` 的完整路径。
pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

/// 随配置一起落盘的 JSON Schema 路径（给编辑器补全用）。
pub fn schema_path() -> PathBuf {
    config_dir().join(SCHEMA_FILE)
}

/// 使用频率统计文件。放在配置目录而不是缓存目录：删掉它排序就"失忆"了，
/// 但它确实是用户数据（记录了使用习惯），不该被系统清理工具顺手删掉。
pub fn usage_path() -> PathBuf {
    config_dir().join(USAGE_FILE)
}

/// 片段本地库（停用 / 别名）。与使用统计一样属于用户数据，放配置目录。
pub fn library_path() -> PathBuf {
    config_dir().join(LIBRARY_FILE)
}

/// 最近一次「因 tap 被系统禁用而重启 daemon」的时间戳（Unix 秒）。
///
/// 只放一个数字。它的唯一用途是**跨进程冷却**：tap 被禁用后 daemon 会重启，
/// 新进程必须知道刚刚出过事，否则会立刻再建一个 tap、再被禁用、再重启。
pub fn tap_restart_marker() -> PathBuf {
    config_dir().join("tap-restart-at")
}

/// 导出目录。迁移/批量导出只往里写，不碰 espanso 的 match 目录。
pub fn exports_dir() -> PathBuf {
    config_dir().join("exports")
}

/// 迁移备份目录。按时间戳分目录，附 `manifest.tsv` 记录原路径。
pub fn backups_dir() -> PathBuf {
    config_dir().join("backups")
}

/// daemon 日志文件。放配置目录的理由同上：崩溃现场要留得住。
pub fn log_path() -> PathBuf {
    config_dir().join(LOG_FILE)
}

/// shell 补全脚本的存放目录（`completion install` 写这里）。
pub fn completions_dir() -> PathBuf {
    config_dir().join("completions")
}

/// daemon 与 CLI 的会合点。放在缓存目录：系统可清理，重建无副作用。
pub fn socket_path() -> PathBuf {
    cache_dir().join(SOCKET_FILE)
}

/// `/Applications/Semisnip.app`
pub fn app_bundle() -> PathBuf {
    PathBuf::from("/Applications").join(format!("{APP_NAME}.app"))
}

/// `/usr/local/bin/semisnip`
///
/// 选 `/usr/local/bin` 而不是 `~/.local/bin`：前者在所有 macOS 的默认 PATH 里。
pub fn cli_link() -> PathBuf {
    PathBuf::from("/usr/local/bin").join(BIN_NAME)
}

/// `~/Library/LaunchAgents/com.semisnip.daemon.plist`
///
/// 这是全家唯一必须待在 `~/Library` 下的路径——launchd 只认这里，没得选。
pub fn launch_agent_plist() -> PathBuf {
    home()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// 建好我们自己的两个目录。失败不报错：下一步的写入会把真正的问题暴露出来，
/// 而 daemon 起不来比「少一个空目录」严重得多。
pub fn ensure_dirs() {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::create_dir_all(cache_dir());
}

// ---------------------------------------------------------------------------
// espanso 的目录：只能问它自己
//
// 一律先跑 `espanso path ...`，问不到才退回平台默认——用户可以改 `$CONFIG`，
// 硬编码会读错目录。结果缓存一次（OnceLock）：`espanso` 是个进程调用，
// 每次查索引都 fork 一遍太浪费。
// ---------------------------------------------------------------------------

static ESPANSO_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// espanso 的配置根目录（`match/`、`config/` 的父目录）。
pub fn espanso_root() -> PathBuf {
    ESPANSO_ROOT
        .get_or_init(|| {
            if let Some(root) = espanso_root_from_cli() {
                return root;
            }
            if let Ok(path) = std::env::var("CONFIG") {
                if !path.trim().is_empty() {
                    return PathBuf::from(path);
                }
            }
            espanso_root_fallback()
        })
        .clone()
}

fn espanso_root_from_cli() -> Option<PathBuf> {
    // 注意：`espanso path base` 返回的是 base.yml **文件**，不是目录。
    // 要目录只能问 `path config`。
    let out = std::process::Command::new(crate::espanso::cli::EspansoCli::discover().bin())
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

/// espanso 自己的目录约定：macOS 是 `~/Library/Application Support/espanso`，
/// Linux 是 `~/.config/espanso`，Windows 是 `%APPDATA%\espanso`——正好都是各家
/// 平台的「配置目录」。注意这里**不能**用 [`config_dir`]：我们住在系统约定里是
/// 我们的选择，espanso 住哪儿由它自己决定。
fn espanso_root_fallback() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("espanso")
}

/// espanso 存放 match 定义的目录（`<root>/match`）。
pub fn espanso_match_dir() -> PathBuf {
    espanso_root().join("match")
}

/// semisnip 自己写进 espanso 的那一个 match 文件。
///
/// 只有这一个文件归 semisnip 管：同目录下的其它 YAML 是用户的片段，
/// 任何命令都不许碰。
pub fn espanso_match_file() -> PathBuf {
    espanso_match_dir().join(ESPANSO_MATCH_FILE)
}

pub const ESPANSO_MATCH_FILE: &str = "semisnip.yml";

/// espanso 的运行时目录（socket 在那里）。
pub fn espanso_runtime_dir() -> PathBuf {
    if let Ok(out) = std::process::Command::new(crate::espanso::cli::EspansoCli::discover().bin())
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
    dirs::cache_dir()
        .unwrap_or_else(|| home().join("Library").join("Caches"))
        .join("espanso")
}

/// espanso worker 的 socket。当前没有用到，保留作为将来"直接与 worker 通信"
/// （而不是每次 `match exec` 起一个进程）的入口；文件名是 espanso 2.4 的实际值。
pub fn espanso_worker_socket() -> PathBuf {
    espanso_runtime_dir().join("espansoworkerv2.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_dirs_follow_the_platform_convention() {
        // 别自创目录：macOS 上就该是 Application Support / Caches，
        // 这样 Time Machine、清理工具、用户的肌肉记忆都还认得。
        assert_eq!(
            config_dir(),
            dirs::config_dir().unwrap().join(APP_DIR),
            "配置目录没走系统约定"
        );
        assert_eq!(
            cache_dir(),
            dirs::cache_dir().unwrap().join(APP_DIR),
            "缓存目录没走系统约定"
        );
        #[cfg(target_os = "macos")]
        {
            assert!(config_dir().ends_with("Library/Application Support/semisnip"));
            assert!(cache_dir().ends_with("Library/Caches/semisnip"));
        }
    }

    #[test]
    fn every_file_sits_in_the_right_dir() {
        // 路径集中以后最容易犯的错是复制粘贴：把日志写进缓存目录、
        // 把 socket 写进配置目录。这里把每个文件钉在它该在的目录下。
        for path in [
            config_path(),
            schema_path(),
            usage_path(),
            library_path(),
            log_path(),
        ] {
            assert_eq!(path.parent(), Some(config_dir().as_path()), "{path:?}");
        }
        assert_eq!(socket_path(), cache_dir().join(SOCKET_FILE));
        assert_eq!(completions_dir(), config_dir().join("completions"));
        assert_eq!(exports_dir(), config_dir().join("exports"));
        assert_eq!(backups_dir(), config_dir().join("backups"));
    }

    #[test]
    fn install_targets_are_absolute_and_named_after_us() {
        // 这三个是 `uninstall` 的删除清单：路径写错等于删错东西，
        // 所以要求绝对路径、且名字里必须认得出来是我们（`Semisnip.app` 是大写开头）。
        for path in [app_bundle(), cli_link(), launch_agent_plist()] {
            assert!(path.is_absolute(), "{} 不是绝对路径", path.display());
            assert!(
                path.to_string_lossy().to_lowercase().contains(APP_DIR),
                "{} 名字里没有 {APP_DIR}",
                path.display()
            );
        }
        assert_eq!(
            launch_agent_plist().file_name().unwrap().to_str().unwrap(),
            format!("{LABEL}.plist")
        );
        assert_eq!(
            app_bundle().file_name().unwrap().to_str().unwrap(),
            format!("{APP_NAME}.app")
        );
    }

    #[test]
    fn the_espanso_file_we_own_is_named_after_us() {
        // 这个名字是唯一的：同一目录里还有用户的片段，写错文件名就会
        // 覆盖别人的 YAML。
        assert_eq!(ESPANSO_MATCH_FILE, "semisnip.yml");
    }
}
