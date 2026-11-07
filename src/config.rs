//! 自有配置与路径。
//!
//! 片段本身永远只存在 espanso 的 YAML 里；这里只放 snipshot 自己的偏好，
//! 保证卸载 snipshot 不会动用户任何片段数据。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusMode {
    /// 非激活面板：目标 App 不失焦，按键靠 CGEventTap 转发（默认）。
    #[default]
    Nonactivating,
    /// 抢焦点：面板可正常用输入法，回传时要先把前台还回去。
    Activating,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 触发串；只允许 ASCII，且会被编译成物理键码序列。
    pub trigger: String,
    /// 两个触发键之间的最大间隔。
    pub trigger_timeout_ms: u64,
    pub focus_mode: FocusMode,
    pub max_results: usize,
    pub panel_width: f32,
    pub panel_height: f32,
    /// 面板关闭到 `match exec` 之间的等待；非激活模式下可接近 0。
    pub post_exec_delay_ms: u64,
    /// 详情区是否展示剪贴板内容（默认只显示长度）。
    pub preview_clipboard: bool,
    pub show_detail: bool,
    /// `match exec` 失败时用剪贴板粘贴兜底（需要辅助功能权限）。
    pub allow_paste_fallback: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            trigger: ";;".into(),
            trigger_timeout_ms: 800,
            focus_mode: FocusMode::Nonactivating,
            max_results: 9,
            panel_width: 720.0,
            panel_height: 420.0,
            post_exec_delay_ms: 12,
            preview_clipboard: false,
            show_detail: true,
            allow_paste_fallback: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(cfg) => cfg,
            Err(err) => {
                log::warn!("配置解析失败（用默认值）：{}：{err}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `~/Library/Application Support/snipshot`
pub fn support_dir() -> PathBuf {
    home()
        .join("Library")
        .join("Application Support")
        .join("snipshot")
}

/// `~/Library/Caches/snipshot`
pub fn cache_dir() -> PathBuf {
    home().join("Library").join("Caches").join("snipshot")
}

pub fn config_path() -> PathBuf {
    support_dir().join("config.json")
}

pub fn usage_path() -> PathBuf {
    support_dir().join("usage.json")
}

pub fn log_path() -> PathBuf {
    support_dir().join("snipshot.log")
}

/// daemon 与 CLI 的会合点。放在 Cache 下：系统可清理，重建无副作用。
pub fn socket_path() -> PathBuf {
    cache_dir().join("snipshot.sock")
}

pub fn ensure_dirs() {
    let _ = std::fs::create_dir_all(support_dir());
    let _ = std::fs::create_dir_all(cache_dir());
}
