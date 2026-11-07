//! 自有配置与路径。
//!
//! 片段本身永远只存在 espanso 的 YAML 里；这里只放 snipshot 自己的偏好，
//! 保证卸载 snipshot 不会动用户任何片段数据。
//!
//! 配置用 YAML 而不是 JSON：手改时能写注释、写错有行列号，文件头那行
//! `# yaml-language-server: $schema=` 还能让编辑器（VS Code + YAML 扩展、
//! JetBrains 系）边写边补全、当场标红。schema 与配置**同目录**、用相对路径
//! 引用——`Application Support` 里有空格，绝对路径容易被编辑器解析歪。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 配置文件名。改这个常量等于改用户路径，慎重。
pub const CONFIG_FILE: &str = "config.yml";
/// 与配置同目录的 schema 文件名，供 `$schema=./config.schema.json` 解析。
pub const SCHEMA_FILE: &str = "config.schema.json";

/// 编译期嵌入：装好的 app 自带 schema，不依赖仓库目录，也不会和代码漂移。
const SCHEMA: &str = include_str!("../assets/config.schema.json");

/// 写在 YAML 开头的抬头。改配置的人第一眼就该看到「改完要重启」。
const HEADER: &str = "\
# snipshot 配置。daemon 只在启动时读一次，改完执行：
#   snipshot config set <key> <value>              # 推荐：带类型与范围校验
#   launchctl kickstart -k gui/$(id -u)/com.snipshot.daemon
# 改坏了也不至于起不来：解析失败时 daemon 会用默认值，并把原因写进日志。
# yaml-language-server: $schema=./config.schema.json
";

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
        Self::load_from(&config_path())
    }

    /// 读取顺序：`config.yml` → 旧版 `config.json`（顺带迁移）→ 默认值。
    ///
    /// 迁移不能省：静默退回默认值等于「用户没动它，trigger 却变回去了」，
    /// 而 `trigger` 一变，espanso 的降级 match 就对不上。
    fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => Self::from_yaml(&raw).unwrap_or_else(|err| {
                log::warn!("配置解析失败（用默认值）：{}：{err}", path.display());
                Self::default()
            }),
            Err(_) => Self::migrate_legacy(path).unwrap_or_default(),
        }
    }

    /// 空文件、只有注释的文件都当「没写」，用默认值——否则用户删光内容会报错。
    pub fn from_yaml(raw: &str) -> anyhow::Result<Self> {
        if is_blank(raw) {
            return Ok(Self::default());
        }
        Ok(serde_yaml::from_str(raw)?)
    }

    /// 带上抬头的完整文本（`config show --raw` 之类将来要用）。
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(format!("{HEADER}{}", serde_yaml::to_string(self)?))
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&config_path())
    }

    fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        write_yaml_to(path, &serde_yaml::to_value(self)?)
    }

    fn migrate_legacy(path: &Path) -> Option<Self> {
        let legacy = path.with_file_name("config.json");
        let raw = std::fs::read_to_string(&legacy).ok()?;
        let cfg: Self = match serde_json::from_str(&raw) {
            Ok(cfg) => cfg,
            Err(err) => {
                log::warn!("旧配置 {} 解析失败，忽略：{err}", legacy.display());
                return None;
            }
        };
        match cfg.save_to(path) {
            Ok(()) => {
                let backup = legacy.with_extension("json.bak");
                let _ = std::fs::rename(&legacy, &backup);
                log::info!(
                    "已把 {} 迁移为 {}（旧文件留作 {}）",
                    legacy.display(),
                    path.display(),
                    backup.display()
                );
            }
            Err(err) => log::warn!("迁移旧配置失败（继续用内存里的值）：{err}"),
        }
        Some(cfg)
    }
}

/// 只有空白与注释的 YAML 视为「没写」。
fn is_blank(raw: &str) -> bool {
    raw.lines().all(|line| {
        let text = line.trim();
        text.is_empty() || text.starts_with('#')
    })
}

/// 把任意 YAML 值写到默认路径（`config set` 走这条）。
pub fn write_yaml(value: &serde_yaml::Value) -> anyhow::Result<()> {
    write_yaml_to(&config_path(), value)
}

/// 原子落盘：先写临时文件再 rename，daemon 不会读到半截配置。
fn write_yaml_to(path: &Path, value: &serde_yaml::Value) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        // 编辑器要一个真实存在的 schema 文件才肯补全。
        ensure_schema_in(dir)?;
    }
    let tmp = path.with_extension("yml.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(HEADER.as_bytes())?;
    file.write_all(serde_yaml::to_string(value)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 把 schema 落到配置旁边；内容一致就不动 mtime。
pub fn ensure_schema() -> anyhow::Result<PathBuf> {
    ensure_schema_in(&support_dir())
}

fn ensure_schema_in(dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(SCHEMA_FILE);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SCHEMA) {
        std::fs::write(&path, SCHEMA)?;
    }
    Ok(path)
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
    support_dir().join(CONFIG_FILE)
}

pub fn schema_path() -> PathBuf {
    support_dir().join(SCHEMA_FILE)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "snipshot-config-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn yaml_round_trips_and_carries_the_schema_hint() {
        let config = Config {
            trigger: ",,".into(),
            max_results: 12,
            ..Config::default()
        };

        let text = config.to_yaml().unwrap();
        assert!(
            text.contains("# yaml-language-server: $schema=./config.schema.json"),
            "缺少编辑器认的那行提示：\n{text}"
        );
        // 相对路径：绝对路径里有 "Application Support" 的空格
        assert!(!text.contains("Application Support"));

        let back = Config::from_yaml(&text).unwrap();
        assert_eq!(back.trigger, ",,");
        assert_eq!(back.max_results, 12);
    }

    #[test]
    fn blank_and_comment_only_files_fall_back_to_defaults() {
        assert_eq!(Config::from_yaml("").unwrap().trigger, ";;");
        assert_eq!(
            Config::from_yaml("# 只有注释\n\n# 还是注释\n")
                .unwrap()
                .trigger,
            ";;"
        );
        // 只写一半的配置也要能用：其余字段吃 default
        let partial = Config::from_yaml("max_results: 20\n").unwrap();
        assert_eq!(partial.max_results, 20);
        assert_eq!(partial.trigger, ";;");
    }

    #[test]
    fn broken_yaml_is_an_error_not_a_silent_default() {
        let err = Config::from_yaml("trigger: [;;\n").unwrap_err();
        assert!(!err.to_string().is_empty());
        // 类型写错同样要报出来，而不是变成 0
        assert!(Config::from_yaml("max_results: 很多\n").is_err());
    }

    #[test]
    fn legacy_json_is_migrated_and_kept_as_backup() {
        let dir = temp_dir("migrate");
        let yml = dir.join(CONFIG_FILE);
        std::fs::write(
            dir.join("config.json"),
            r#"{"trigger":",,","max_results":3}"#,
        )
        .unwrap();

        let config = Config::load_from(&yml);
        assert_eq!(config.trigger, ",,");
        assert_eq!(config.max_results, 3);

        // 新文件写出来了，旧文件改名留档而不是删掉
        assert!(yml.exists());
        assert!(!dir.join("config.json").exists());
        assert!(dir.join("config.json.bak").exists());
        assert!(dir.join(SCHEMA_FILE).exists());

        let text = std::fs::read_to_string(&yml).unwrap();
        assert!(text.starts_with("# snipshot 配置"));
    }

    #[test]
    fn schema_file_matches_the_struct() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        let props = schema["properties"].as_object().unwrap();
        let defaults = serde_json::to_value(Config::default()).unwrap();

        for (key, value) in defaults.as_object().unwrap() {
            let prop = props
                .get(key)
                .unwrap_or_else(|| panic!("schema 里没有字段 {key}"));
            assert_eq!(
                &prop["default"], value,
                "schema 里 {key} 的 default 与代码不一致"
            );
            // 没有 description 的字段在编辑器里悬停是一片空白，等于白写 schema
            assert!(
                prop["description"].as_str().is_some_and(|d| !d.is_empty()),
                "schema 里 {key} 缺 description"
            );
            assert!(prop["type"].is_string(), "schema 里 {key} 缺 type");
        }
        assert_eq!(
            props.len(),
            defaults.as_object().unwrap().len(),
            "schema 多出或缺少字段"
        );
        // 拼错键名要能被编辑器标出来
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    }

    #[test]
    fn schema_is_embedded_verbatim() {
        let dir = temp_dir("schema");
        let path = ensure_schema_in(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SCHEMA);
        // 第二次调用不该重写（mtime 稳定，编辑器不会反复重载）
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        ensure_schema_in(&dir).unwrap();
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);
    }
}
