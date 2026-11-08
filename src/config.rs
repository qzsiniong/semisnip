//! 自有配置。
//!
//! 片段本身永远只存在 espanso 的 YAML 里；这里只放 semisnip 自己的偏好，
//! 保证卸载 semisnip 不会动用户任何片段数据。
//!
//! 配置用 YAML 而不是 JSON：手改时能写注释、写错有行列号，文件头那行
//! `# yaml-language-server: $schema=` 还能让编辑器（VS Code + YAML 扩展、
//! JetBrains 系）边写边补全、当场标红。schema 与配置**同目录**、用相对路径
//! 引用：目录整体搬走（换机器、改 `$HOME`）也不会指向旧位置。
//!
//! **这个文件里没有路径**。所有目录、文件名都在 [`crate::paths`]，
//! 这里只负责「配置长什么样、怎么读写」。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::{SCHEMA_FILE, config_dir, config_path};

/// 编译期嵌入：装好的 app 自带 schema，不依赖仓库目录，也不会和代码漂移。
const SCHEMA: &str = include_str!("../assets/config.schema.json");

/// 写在 YAML 开头的抬头。
///
/// 手改的人第一眼要看到「改完要重启」；而菜单栏里的设置窗口是**立即生效**的，
/// 两条路的行为不一样，所以两句都得写——不然用户会以为设置窗口也需要重启。
const HEADER: &str = "\
# semisnip 配置。手改只在 daemon 启动时读一次，改完执行：
#   semisnip config set <key> <value>              # 推荐：带类型与范围校验
#   launchctl kickstart -k gui/$(id -u)/com.semisnip.daemon
# 菜单栏 →「偏好设置…」里改会立即生效，不需要重启。
# 改坏了也不至于起不来：解析失败时 daemon 会用默认值，并把原因写进日志。
# yaml-language-server: $schema=./config.schema.json
";

/// 面板的激活方式。
///
/// `Activating` 目前只是留位：实现它要处理"回传前把焦点还给原 App"，
/// 而那条路径没有稳定可靠的焦点还原方式，所以 `ui::window` 始终按
/// `Nonactivating` 建窗。值保留在配置里是为了将来能改，不是误导。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusMode {
    /// 非激活面板：目标 App 不失焦，按键靠 CGEventTap 转发（默认）。
    #[default]
    Nonactivating,
    /// 抢焦点：面板可正常用输入法，回传时要先把前台还回去。
    Activating,
}

/// 面板出现在哪儿。
///
/// 两种都**不再是"主显示器居中"**：多屏环境下主屏往往不是用户正在打字的那块，
/// 面板跑到另一块屏上等于没打开。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    /// 跟随插入点：垂直贴光标下方（下方不够就翻到上方），水平居中于该屏。
    #[default]
    Auto,
    /// 居中：屏幕可视区的正中。
    Center,
}

/// 确认回传时用什么字符「占位」。
///
/// 为什么需要占位：`espanso match exec` 一定会先按**被选中片段 trigger 的字符数**
/// 发一串退格（`TriggerCompensation`，见 `espanso-engine` 的 `action.rs`），
/// 且没有关闭入口。这些退格删的是光标左边的字符，所以必须提前放好同样多的
/// 「一退格一个」的字符，否则被删的就是用户正文。
///
/// 各种选择的差别只在于「那串字符会不会被目标 App 或 espanso 改写」，详见
/// `docs/troubleshooting.md#占位符展开时文档里为什么会多出几个字符`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placeholder {
    /// 空格（默认）：最不起眼，但属于「有语义的空白」，少数场景会被改写。
    #[default]
    Space,
    /// 零宽空格 U+200B：不可见、不是 Unicode 空白，不会被 tab 化/trim。
    ZeroWidth,
    /// 用被选中片段自己的触发串：打出去之后**不再调 `match exec`**，交给
    /// espanso 的被动匹配自己命中、自己回删、自己展开（详见 `handoff::confirm_with`）。
    Trigger,
    /// 用户指定的单个字符，见 [`Config::placeholder_char`]。
    Custom,
    /// 一个字都不补。**只有将来 espanso 提供「不补偿」入口时才安全**，
    /// 现在选它等于每次展开都会吃掉光标前的真实字符。
    None,
}

/// 全部偏好。
///
/// `#[serde(default)]` 放在结构体上而不是逐字段：这样**任何**字段缺失（用户
/// 手删了一行、或我们从旧版本加了个新字段）都退化成该字段的默认值，而不是
/// 让整份配置解析失败。配合 `Default` 实现，升级不需要写迁移代码。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 是否接管 `;;`（菜单栏里叫「暂停 / 恢复触发」）。
    ///
    /// 为 `false` 时**只有 tap 路径停下**：`;;` 原样落进文档，面板不弹出来。
    /// 菜单栏的「打开片段面板」与 espanso 的降级触发（`;;;`）仍然可用——
    /// 那两条都是用户明确做出来的动作，不是"顺手打的两个分号"；而且没有辅助
    /// 功能权限的机器上，espanso 降级触发是唯一的唤起方式，一起停掉就等于
    /// semisnip 凭空消失了。
    ///
    /// 放在配置里而不是内存里：暂停是个会持续几小时的状态（比如在终端里干活），
    /// 重启 daemon 之后还该保持。
    pub enabled: bool,
    /// 触发串；只允许 ASCII，且会被编译成物理键码序列。
    pub trigger: String,
    /// 两个触发键之间的最大间隔。
    pub trigger_timeout_ms: u64,
    pub focus_mode: FocusMode,
    pub max_results: usize,
    /// 面板位置策略，见 [`Position`]。
    pub position: Position,
    pub panel_width: f32,
    pub panel_height: f32,
    /// 面板关闭到 `match exec` 之间的等待；非激活模式下可接近 0。
    pub post_exec_delay_ms: u64,
    /// 回传时的占位符策略，见 [`Placeholder`]。
    pub placeholder: Placeholder,
    /// [`Placeholder::Custom`] 用的字符：必须**恰好一个字符**（一个 Unicode 标量），
    /// 且不能是控制字符（换行/制表会引发自动缩进、数量对不上）。
    ///
    /// 存 `String` 而不是 `char`：YAML 里没有字符类型，serde 对 `char` 的支持
    /// 也只在单字符时可用；存字符串、校验时再数长度，报错信息更好写。
    pub placeholder_char: String,
    /// 详情区是否展示剪贴板内容（默认只显示长度）。
    pub preview_clipboard: bool,
    pub show_detail: bool,
    /// `match exec` 失败时用剪贴板粘贴兜底（需要辅助功能权限）。
    pub allow_paste_fallback: bool,
    /// 是否已经看过首次引导。
    ///
    /// 这是唯一一个**不是用户偏好**的字段：它记的是「这台机器上的人已经知道
    /// 这个程序怎么用了」。daemon 启动时读它决定要不要弹引导窗——
    /// 第一次启动（配置文件都还不存在）时它是 `false`，走完引导置 `true`。
    ///
    /// 放在配置里而不是单独的标记文件，是因为它天然跟着配置走：用户把
    /// `config.yml` 复制到另一台机器，那边也不会再被引导一次；删掉配置
    /// 重来，就等于「重新开始」，行为符合直觉。
    pub onboarded: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger: ";;".into(),
            trigger_timeout_ms: 800,
            focus_mode: FocusMode::Nonactivating,
            max_results: 9,
            position: Position::Auto,
            panel_width: 720.0,
            panel_height: 420.0,
            post_exec_delay_ms: 12,
            placeholder: Placeholder::Space,
            placeholder_char: "·".into(),
            preview_clipboard: false,
            show_detail: true,
            allow_paste_fallback: true,
            onboarded: false,
        }
    }
}

impl Config {
    /// 从默认位置读。所有调用方都走这里，路径只由 `paths` 决定。
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

    /// 保存到默认路径。
    ///
    /// 调用方（设置窗口、`config set`）在落盘后必须投 `Event::ConfigChanged`，
    /// 否则面板还活在旧配置里——这个函数只负责写文件，不负责通知。
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&config_path())
    }

    /// 写到指定路径（测试与迁移用）。
    fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        write_yaml_to(path, &serde_yaml::to_value(self)?)
    }

    /// 把旧的 `config.json` 迁移成 `config.yml`。
    ///
    /// 三种结局：
    /// - 没有旧文件 → `None`，调用方退默认值；
    /// - 有但坏了 → 记 warn 后 `None`（不因为一份坏文件就起不来）；
    /// - 有且能读 → 写新文件，旧文件**改名**成 `.json.bak` 而不是删除。
    ///
    /// 改名而不是删除：万一新写的 YAML 有问题，用户还能回去看原来的值。
    /// 写新文件失败时也返回解析出的值（内存里继续用），只是没落盘。
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

/// 校验 [`Placeholder::Custom`] 的字符，返回它本身。
///
/// 规则只有两条，但都必须卡住：
/// - **恰好一个字符**：espanso 会按「字符数」发退格，一个 emoji 组合（多个标量）
///   占多个字符位，数量就会对不上；
/// - **不能是控制字符**：换行会触发编辑器的自动缩进（凭空多出空格），制表符在
///   代码编辑器里会被展开成若干空格——两种情况都会让退格数错位。
///
/// 抽成自由函数是为了让 `config set` 与设置窗口共用同一套文案，别各写一遍。
pub fn validate_placeholder_char(text: &str) -> Result<char, String> {
    let mut chars = text.chars();
    let Some(c) = chars.next() else {
        return Err("占位字符不能为空".into());
    };
    if chars.next().is_some() {
        return Err(format!(
            "占位字符必须是单个字符，收到 {} 个（一个 emoji 组合也算多个）",
            text.chars().count()
        ));
    }
    if c.is_control() {
        return Err(
            "占位字符不能是换行 / 制表符等控制字符：它们会让编辑器自动缩进，退格数就对不上了"
                .into(),
        );
    }
    Ok(c)
}

/// 只有空白与注释的 YAML 视为「没写」。
fn is_blank(raw: &str) -> bool {
    raw.lines().all(|line| {
        let text = line.trim();
        text.is_empty() || text.starts_with('#')
    })
}

/// 把任意 YAML 值写到默认路径（`config set` 走这条）。
///
/// 传 `Value` 而不是 `Config`：`config set` 只改一个键，没必要先反序列化成
/// 完整结构再序列化回去——那样会丢掉用户手写在文件里的注释和未知字段。
pub fn write_yaml(value: &serde_yaml::Value) -> anyhow::Result<()> {
    write_yaml_to(&config_path(), value)
}

/// 原子落盘：先写临时文件再 rename，daemon 不会读到半截配置。
///
/// 三步缺一不可：
/// 1. 建目录 + 落 schema（编辑器要真实文件才肯补全）；
/// 2. 写 `.yml.tmp` 并 `sync_all()`——不 sync 的话断电后 rename 可能指向空文件；
/// 3. `rename` 在同一目录内是原子的，读到的要么是旧内容要么是新内容。
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
///
/// 返回路径供调用方打日志/提示。`ensure_schema_in` 只在内容不同时才写，
/// 这一条很重要：编辑器按 mtime 判断要不要重载 schema，每次启动都重写会让
/// 打开的编辑器反复弹"文件已更改"。
pub fn ensure_schema() -> anyhow::Result<PathBuf> {
    ensure_schema_in(&config_dir())
}

fn ensure_schema_in(dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(SCHEMA_FILE);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SCHEMA) {
        std::fs::write(&path, SCHEMA)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("semisnip-config-{tag}-{}", std::process::id()));
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
        // 相对路径：写死绝对路径的话，配置目录一搬就指到旧地方去了
        assert!(!text.contains("$schema=/"), "抬头不该出现绝对路径");

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
        let yml = dir.join(crate::paths::CONFIG_FILE);
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
        assert!(text.starts_with("# semisnip 配置"));
    }

    #[test]
    fn enabled_defaults_to_true_and_round_trips() {
        // 老配置里没有这个键：不能因为"缺失"就把 `;;` 关掉——
        // 升级之后触发键不灵，是最难让人想到原因的那种故障。
        assert!(Config::from_yaml("trigger: \"<<\"\n").unwrap().enabled);
        assert!(!Config::from_yaml("enabled: false\n").unwrap().enabled);
        // 存进去也是 bool，不是字符串 "false"（后者在 YAML 里会被读成真值）
        let text = Config::to_yaml(&Config {
            enabled: false,
            ..Config::default()
        })
        .unwrap();
        assert!(text.contains("enabled: false"), "{text}");
    }

    #[test]
    fn placeholder_settings_round_trip() {
        // 默认是空格：不写这两个键的老配置必须原样按默认值跑。
        let default = Config::from_yaml("trigger: \"<<\"\n").unwrap();
        assert_eq!(default.placeholder, Placeholder::Space);
        assert_eq!(default.placeholder_char, "·");

        let config =
            Config::from_yaml("placeholder: zero_width\nplaceholder_char: \"▪\"\n").unwrap();
        assert_eq!(config.placeholder, Placeholder::ZeroWidth);
        assert_eq!(config.placeholder_char, "▪");

        // 五个取值都要能解析，拼错则整份失败（而不是悄悄退回默认值）
        for (text, want) in [
            ("space", Placeholder::Space),
            ("zero_width", Placeholder::ZeroWidth),
            ("trigger", Placeholder::Trigger),
            ("custom", Placeholder::Custom),
            ("none", Placeholder::None),
        ] {
            let yaml = format!("placeholder: {text}\n");
            assert_eq!(Config::from_yaml(&yaml).unwrap().placeholder, want);
        }
        assert!(Config::from_yaml("placeholder: tab\n").is_err());
    }

    #[test]
    fn placeholder_char_must_be_one_visible_character() {
        assert_eq!(validate_placeholder_char("·"), Ok('·'));
        assert_eq!(validate_placeholder_char("\u{200b}"), Ok('\u{200b}'));
        // 汉字、全角标点都是单字符，允许
        assert_eq!(validate_placeholder_char("。"), Ok('。'));

        assert!(
            validate_placeholder_char("")
                .unwrap_err()
                .contains("不能为空")
        );
        assert!(
            validate_placeholder_char("ab")
                .unwrap_err()
                .contains("单个字符")
        );
        // 组合 emoji：一个字形、多个标量 → 数量对不上，必须拒
        assert!(validate_placeholder_char("👨‍👩‍👦").is_err());
        // 换行 / 制表：会改变编辑器行为
        assert!(
            validate_placeholder_char("\n")
                .unwrap_err()
                .contains("控制字符")
        );
        assert!(
            validate_placeholder_char("\t")
                .unwrap_err()
                .contains("控制字符")
        );
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
