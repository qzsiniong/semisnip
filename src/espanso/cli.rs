//! espanso CLI 封装。
//!
//! 只用到三个能力（都是 2.4.x 的公开命令）：
//! - `espanso match list --json`：拿 espanso 眼中的权威 trigger 集合
//! - `espanso match exec -t <trigger>`：让 espanso 展开（求值 + 注入）
//! - `espanso path ...`：目录发现（见 paths.rs）

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct EspansoCli {
    bin: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct CliMatch {
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

impl EspansoCli {
    pub fn discover() -> Self {
        if let Ok(bin) = std::env::var("SEMISNIP_ESPANSO_BIN") {
            if !bin.trim().is_empty() {
                return Self {
                    bin: PathBuf::from(bin),
                };
            }
        }
        for candidate in [
            "/opt/homebrew/bin/espanso",
            "/usr/local/bin/espanso",
            "/usr/bin/espanso",
        ] {
            if Path::new(candidate).exists() {
                return Self {
                    bin: PathBuf::from(candidate),
                };
            }
        }
        Self {
            bin: PathBuf::from("espanso"),
        }
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    pub fn version(&self) -> Option<String> {
        let out = Command::new(&self.bin).arg("--version").output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// espanso 自己维护的 match 清单。可选按前台 App 过滤——这正好让
    /// app-specific 的 `filter_class/filter_exec` 生效（我们不必复刻那套规则）。
    pub fn list_json(&self, app: Option<&AppFilter>) -> anyhow::Result<Vec<CliMatch>> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["match", "list", "--json"]);
        if let Some(app) = app {
            if let Some(class) = &app.class {
                cmd.args(["--class", class]);
            }
            if let Some(exec) = &app.exec {
                cmd.args(["--exec", exec]);
            }
            if let Some(title) = &app.title {
                cmd.args(["--title", title]);
            }
        }
        let out = cmd.output()?;
        if !out.status.success() {
            anyhow::bail!(
                "espanso match list 失败：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let parsed: Vec<CliMatch> = serde_json::from_slice(&out.stdout)?;
        Ok(parsed)
    }

    /// 异步触发一次展开：espanso 会渲染变量并注入到当前前台 App。
    /// 没有回执——失败只能靠退出码与 `espanso log`。
    pub fn exec(&self, trigger: &str) -> anyhow::Result<()> {
        let out = Command::new(&self.bin)
            .args(["match", "exec", "-t", trigger])
            .output()?;
        if !out.status.success() {
            anyhow::bail!(
                "espanso match exec 失败（{}）：{}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// 前台 App 的过滤上下文；缺字段表示该维度不过滤。
#[derive(Clone, Debug, Default)]
pub struct AppFilter {
    pub class: Option<String>,
    pub exec: Option<String>,
    pub title: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_json_parses_espanso_shape() {
        let raw = r#"[
            {"triggers":[";;log"],"replace":"console.log($|$)","label":"JS: console.log"},
            {"triggers":[":date"],"replace":"{{mydate}}","label":null}
        ]"#;
        let matches: Vec<CliMatch> = serde_json::from_str(raw).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].triggers, vec![";;log"]);
        assert!(matches[1].label.is_none());
    }
}
