//! espanso CLI 封装。
//!
//! 只用到三个能力（都是 2.4.x 的公开命令）：
//! - `espanso match list --json`：拿 espanso 眼中的权威 trigger 集合
//! - `espanso match exec -t <trigger>`：让 espanso 展开（求值 + 注入）
//! - `espanso path ...`：目录发现（见 paths.rs）

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// espanso 可执行文件的定位结果。
///
/// 只存路径不存进程：每条命令都是"起一个新进程、等它退出"，因为 espanso 的 CLI
/// 本身是无状态的客户端，真正干活的 daemon 由它自己找。
#[derive(Clone, Debug)]
pub struct EspansoCli {
    bin: PathBuf,
}

/// `match list --json` 的单项。
///
/// 只声明我们真正用到的三个字段；espanso 的 JSON 还有十几个键，serde 默认忽略，
/// 所以它升级加字段不会让我们解析失败。
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
    /// 找 espanso 可执行文件。
    ///
    /// 优先级：环境变量 `SEMISNIP_ESPANSO_BIN`（覆盖用，测试和非常规安装都靠它）
    /// → Homebrew 的两个前缀（Apple Silicon / Intel）→ `/usr/bin` → 最后退化成
    /// 裸名字 `espanso`，交给 `PATH` 去解析。
    ///
    /// 为什么不用 `which`：launchd 起的 daemon 继承的 `PATH` 极简（通常只有
    /// `/usr/bin:/bin:/usr/sbin:/sbin`），Homebrew 目录根本不在里面，
    /// 必须自己按绝对路径找。
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

    /// 实际使用的路径（可能只是裸名字）。诊断输出与日志用。
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// 取版本号。跑不起来、非零退出、或输出为空都给 `None`——
    /// 调用方（`doctor` / 首次引导的体检）只需要"有没有"这一位信息。
    pub fn version(&self) -> Option<String> {
        let out = Command::new(&self.bin).arg("--version").output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// espanso 自己维护的 match 清单。可选按前台 App 过滤——这正好让
    /// app-specific 的 `filter_class/filter_exec` 生效（我们不必复刻那套规则）。
    ///
    /// 用途只有一个：拿到"espanso 真正认识的 trigger 集合"，用来给本地索引里的
    /// 每条片段打 `usable` 标记。所以这里不传 `app` 读全量——按 App 过滤会让
    /// 别处的片段被误判成"espanso 不认识"。
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
///
/// 三个维度对应 espanso 的 `filter_class`（bundle id）、`filter_exec`（可执行路径）、
/// `filter_title`（窗口标题）。当前 semisnip 只在需要精确复刻 espanso 判断时才传，
/// 主路径（`loader::load`）传的是 `None`，读全量。
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
