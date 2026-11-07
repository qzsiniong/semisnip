//! `snipshot config`：查看与修改 `config.json`。
//!
//! 只碰 snipshot 自己的偏好，**不碰** espanso 的 YAML——片段永远归 espanso 管。
//! 所有写入都走「读旧 JSON → 改一个键 → 反序列化校验 → 原子落盘」，
//! 保证手改坏了会被发现，而不是悄悄用默认值跑起来。

use std::io::Write;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::trigger::machine::TriggerConfig;

/// 字段顺序即展示顺序；说明同时用作 `config keys` 的输出。
const FIELDS: &[(&str, &str)] = &[
    (
        "trigger",
        "开面板的触发串，ASCII，最多 4 个字符（如 \";;\"）",
    ),
    ("trigger_timeout_ms", "两次触发键之间的最大间隔，200–3000"),
    (
        "focus_mode",
        "nonactivating（目标 App 不失焦）｜activating（抢焦点，未实现）",
    ),
    ("max_results", "列表最多显示几条，1–100"),
    ("panel_width", "面板宽度（逻辑像素）"),
    ("panel_height", "面板高度（逻辑像素）"),
    (
        "post_exec_delay_ms",
        "面板关闭到 match exec 之间的等待，单位毫秒",
    ),
    (
        "preview_clipboard",
        "详情区是否显示剪贴板内容（只显示长度，不显示全文）",
    ),
    ("show_detail", "是否显示右侧详情区"),
    ("allow_paste_fallback", "match exec 失败时用剪贴板粘贴兜底"),
];

const USAGE: &str = "\
用法:
  snipshot config                显示当前配置
  snipshot config path           打印配置文件路径
  snipshot config get <key>      读取单个键
  snipshot config set <key> <v>  修改单个键（自动校验，必要时重启 daemon）
  snipshot config unset <key>    恢复某个键的默认值
  snipshot config reset          全部恢复默认
  snipshot config keys           列出所有键与含义
  snipshot config edit           用 $EDITOR 打开配置文件";

pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None | Some("show") => show(),
        Some("path") => {
            println!("{}", config::config_path().display());
            ExitCode::SUCCESS
        }
        Some("get") => get(args.get(1).map(String::as_str)),
        Some("set") => set(args.get(1).map(String::as_str), &args[2..]),
        Some("unset") => unset(args.get(1).map(String::as_str)),
        Some("reset") => reset(),
        Some("keys") => {
            for (key, hint) in FIELDS {
                println!("  {key:<22} {hint}");
            }
            ExitCode::SUCCESS
        }
        Some("edit") => edit(),
        Some("-h") | Some("--help") | Some("help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("未知的 config 子命令：{other}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("snipshot config: {message}");
    ExitCode::FAILURE
}

fn show() -> ExitCode {
    let path = config::config_path();
    let exists = path.exists();
    println!(
        "配置文件  {}{}",
        path.display(),
        if exists {
            ""
        } else {
            "（不存在，正在用默认值）"
        }
    );

    let effective = serde_json::to_value(Config::load()).unwrap_or(Value::Null);
    let default = serde_json::to_value(Config::default()).unwrap_or(Value::Null);

    let mut changed = Vec::new();
    for (key, _) in FIELDS {
        let value = effective.get(*key).cloned().unwrap_or(Value::Null);
        let same = default.get(*key) == Some(&value);
        if !same {
            changed.push(*key);
        }
        println!("  {key:<22} = {}{}", value, if same { "" } else { "   *" });
    }
    if !changed.is_empty() {
        println!("\n  * = 与默认值不同");
    }

    // 旧版本残留的键：反序列化时会丢掉，落盘即消失，先说一声。
    if let Some(raw) = read_file() {
        let unknown: Vec<&String> = raw
            .as_object()
            .map(|map| {
                map.keys()
                    .filter(|k| !FIELDS.iter().any(|(known, _)| known == &k.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        if !unknown.is_empty() {
            println!(
                "\n  ⚠️  未知键（会被忽略，下次写入时清除）：{}",
                unknown
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    println!("\n  `config keys` 查看每个键的含义");
    ExitCode::SUCCESS
}

fn get(key: Option<&str>) -> ExitCode {
    let Some(key) = key else {
        return fail("需要一个键名，`snipshot config keys` 查看全部");
    };
    if let Err(err) = known_key(key) {
        return fail(&err);
    }
    let value = serde_json::to_value(Config::load()).unwrap_or(Value::Null);
    println!("{}", plain(value.get(key).unwrap_or(&Value::Null)));
    ExitCode::SUCCESS
}

fn set(key: Option<&str>, rest: &[String]) -> ExitCode {
    let Some(key) = key else {
        return fail("用法：snipshot config set <key> <value>");
    };
    if let Err(err) = known_key(key) {
        return fail(&err);
    }
    if rest.is_empty() {
        return fail(&format!(
            "`{key}` 缺少取值，`snipshot config keys` 查看含义"
        ));
    }
    // 允许 `set trigger ";;"` 这种带空格的写法，所以把剩下的都拼起来。
    let raw = rest.join(" ");

    let value = match coerce(key, &raw).and_then(|value| validate(key, value)) {
        Ok(value) => value,
        Err(err) => return fail(&err),
    };

    let mut root = read_file().unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    root[key] = value.clone();

    // 用整份反序列化兜底：字段之间若有耦合，这里会直接报出来。
    if let Err(err) = serde_json::from_value::<Config>(root.clone()) {
        return fail(&format!("`{key}` 取值不合法：{err}"));
    }
    if let Err(err) = write_file(&root) {
        return fail(&format!("写入失败：{err}"));
    }

    println!("{key} = {}", plain(&value));
    hint_restart();
    ExitCode::SUCCESS
}

fn unset(key: Option<&str>) -> ExitCode {
    let Some(key) = key else {
        return fail("用法：snipshot config unset <key>");
    };
    if let Err(err) = known_key(key) {
        return fail(&err);
    }
    let mut root = read_file().unwrap_or_else(|| json!({}));
    let default = serde_json::to_value(Config::default()).unwrap_or(Value::Null);
    root[key] = default.get(key).cloned().unwrap_or(Value::Null);
    if let Err(err) = write_file(&root) {
        return fail(&format!("写入失败：{err}"));
    }
    println!("{key} = {}（默认值）", plain(&root[key]));
    hint_restart();
    ExitCode::SUCCESS
}

fn reset() -> ExitCode {
    let config = Config::default();
    match config.save() {
        Ok(()) => {
            println!("已重置为默认值：{}", config::config_path().display());
            hint_restart();
            ExitCode::SUCCESS
        }
        Err(err) => fail(&format!("写入失败：{err}")),
    }
}

fn edit() -> ExitCode {
    let path = config::config_path();
    if !path.exists() {
        let _ = Config::default().save();
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "open".into());
    let status = std::process::Command::new(&editor).arg(&path).status();
    match status {
        Ok(status) if status.success() => {
            // 手改最容易写坏 JSON，顺手校验一次。
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Config>(&raw).ok())
            {
                Some(_) => {
                    println!("{} 校验通过", path.display());
                    hint_restart();
                    ExitCode::SUCCESS
                }
                None => fail(&format!("{} 解析失败，daemon 会退回默认值", path.display())),
            }
        }
        Ok(status) => fail(&format!("编辑器退出码 {status}")),
        Err(err) => fail(&format!("无法启动编辑器 {editor}：{err}")),
    }
}

fn hint_restart() {
    // 配置在 daemon 启动时读一次（AppState::new），所以必须重启才生效。
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "$(id -u)".into());
    println!("重启 daemon 生效：launchctl kickstart -k gui/{uid}/com.snipshot.daemon");
}

fn known_key(key: &str) -> Result<(), String> {
    if FIELDS.iter().any(|(known, _)| *known == key) {
        return Ok(());
    }
    Err(format!(
        "未知键 `{key}`，可用：{}",
        FIELDS
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// 按默认值的 JSON 类型决定怎么解析用户输入，避免"全都当字符串"。
fn coerce(key: &str, raw: &str) -> Result<Value, String> {
    let default = serde_json::to_value(Config::default()).map_err(|e| e.to_string())?;
    let template = default
        .get(key)
        .ok_or_else(|| format!("未知键 `{key}`"))?
        .clone();

    match template {
        Value::Bool(_) => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(json!(true)),
            "false" | "0" | "no" | "off" => Ok(json!(false)),
            _ => Err(format!("`{key}` 只接受 true / false")),
        },
        Value::Number(n) if n.is_i64() || n.is_u64() => {
            let parsed: i64 = raw
                .trim()
                .parse()
                .map_err(|_| format!("`{key}` 需要一个整数，收到 `{raw}`"))?;
            Ok(json!(parsed))
        }
        Value::Number(_) => {
            let parsed: f64 = raw
                .trim()
                .parse()
                .map_err(|_| format!("`{key}` 需要一个小数，收到 `{raw}`"))?;
            Ok(json!(parsed))
        }
        _ => Ok(Value::String(raw.trim_matches('"').to_string())),
    }
}

/// 语义校验：类型对了不代表值合理（比如 trigger 里的中文、负数宽度）。
fn validate(key: &str, value: Value) -> Result<Value, String> {
    match key {
        "trigger" => {
            let text = value.as_str().unwrap_or_default();
            TriggerConfig::from_text(text, 800)?;
            Ok(value)
        }
        "trigger_timeout_ms" => {
            let ms = value.as_i64().unwrap_or_default();
            if !(200..=3_000).contains(&ms) {
                return Err("trigger_timeout_ms 需在 200–3000 之间".into());
            }
            Ok(value)
        }
        "max_results" => {
            let n = value.as_i64().unwrap_or_default();
            if !(1..=100).contains(&n) {
                return Err("max_results 需在 1–100 之间".into());
            }
            Ok(value)
        }
        "panel_width" | "panel_height" => {
            let n = value.as_f64().unwrap_or_default();
            if !(200.0..=2_000.0).contains(&n) {
                return Err(format!("{key} 需在 200–2000 之间"));
            }
            Ok(value)
        }
        "focus_mode" => match value.as_str() {
            Some("nonactivating") | Some("activating") => Ok(value),
            _ => Err("focus_mode 只接受 nonactivating / activating".into()),
        },
        _ => Ok(value),
    }
}

fn plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn read_file() -> Option<Value> {
    let raw = std::fs::read_to_string(config::config_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_file(value: &Value) -> anyhow::Result<()> {
    let path = config::config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 先写临时文件再改名：daemon 可能正在读，不能让它看到半截 JSON。
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coercion_follows_default_type() {
        assert_eq!(coerce("show_detail", "false").unwrap(), json!(false));
        assert_eq!(coerce("show_detail", "ON").unwrap(), json!(true));
        assert_eq!(coerce("max_results", "12").unwrap(), json!(12));
        assert_eq!(coerce("panel_width", "800.5").unwrap(), json!(800.5));
        assert_eq!(coerce("trigger", ";;").unwrap(), json!(";;"));
    }

    #[test]
    fn coercion_rejects_wrong_shapes() {
        assert!(coerce("max_results", "很多").is_err());
        assert!(coerce("show_detail", "maybe").is_err());
        assert!(coerce("nope", "1").is_err());
    }

    #[test]
    fn validation_catches_semantic_errors() {
        assert!(validate("trigger", json!(";;")).is_ok());
        // 中文没有物理键码，早失败好过让 daemon 起不来
        assert!(validate("trigger", json!("，，")).is_err());
        assert!(validate("trigger", json!(";;;;;")).is_err());
        assert!(validate("max_results", json!(0)).is_err());
        assert!(validate("panel_width", json!(-10.0)).is_err());
        assert!(validate("focus_mode", json!("activating")).is_ok());
        assert!(validate("focus_mode", json!("whatever")).is_err());
    }

    #[test]
    fn every_field_is_settable() {
        // 保证 FIELDS 与 Config 的字段集合一致，避免只加结构体忘加表
        let default = serde_json::to_value(Config::default()).unwrap();
        for (key, _) in FIELDS {
            assert!(default.get(*key).is_some(), "Config 里没有字段 {key}");
            assert!(known_key(key).is_ok());
        }
        assert_eq!(
            default.as_object().unwrap().len(),
            FIELDS.len(),
            "Config 新增了字段但 FIELDS 没跟上"
        );
    }
}
