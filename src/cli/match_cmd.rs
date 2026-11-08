//! `semisnip match`：看/改 semisnip 写进 espanso 的那一个 YAML。
//!
//! 这个文件是**生成物**——`semisnip install` / `semisnip match regenerate` 会按
//! 当前可执行文件路径和 trigger 把它整份重写。所以 `edit` 之后必须提醒用户：
//! 想长期改 trigger 请改 semisnip 的 config，手改这里只是临时调试。
//!
//! 另一个坑是 espanso 的 shell 变量会把 stdout 原样注入文档，所以这个文件里
//! 命令**必须重定向掉 stdout**，`check` 会把漏掉重定向的写成警告。

use std::process::{Command, ExitCode};

use clap::Subcommand;
use serde_yaml::Value;

use super::fail;
use crate::config::Config;
use crate::paths;

/// `match` 的子命令。`regenerate` 是唯一的"写"操作。
#[derive(Debug, Subcommand)]
pub enum MatchCmd {
    /// 打印 semisnip.yml 的路径
    Path,
    /// 打印文件内容（默认动作）
    Show,
    /// 用 $EDITOR 打开，退出后校验 YAML
    Edit,
    /// 用系统默认程序打开；--reveal 在访达里选中它
    Open {
        /// 不打开文件，只在访达里定位（macOS）
        #[arg(long)]
        reveal: bool,
    },
    /// 按当前可执行文件与配置重新生成（install 里写文件那一步）
    Regenerate,
}

/// 分派 `match` 的子命令；不带子命令时等价于 `show`。
///
/// `check` 只做只读校验（YAML 能否解析、cmd 有没有重定向 stdout），
/// `edit` / `open` 会起外部进程，所以它们返回后不再做任何写操作。
pub fn run(cmd: Option<MatchCmd>) -> ExitCode {
    match cmd.unwrap_or(MatchCmd::Show) {
        MatchCmd::Path => {
            println!("{}", paths::espanso_match_file().display());
            ExitCode::SUCCESS
        }
        MatchCmd::Show => show(),
        MatchCmd::Edit => edit(),
        MatchCmd::Open { reveal } => open(reveal),
        MatchCmd::Regenerate => regenerate(),
    }
}

fn missing() -> ExitCode {
    fail(&format!(
        "{} 还不存在，先执行 `semisnip install`（或 `semisnip match regenerate`）",
        paths::espanso_match_file().display()
    ))
}

fn show() -> ExitCode {
    let path = paths::espanso_match_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            // 原样输出，方便 `semisnip match show | pbcopy`，不加任何装饰
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
            ExitCode::SUCCESS
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => missing(),
        Err(err) => fail(&format!("读取 {} 失败：{err}", path.display())),
    }
}

fn open(reveal: bool) -> ExitCode {
    let path = paths::espanso_match_file();
    if !path.exists() {
        return missing();
    }
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        if reveal {
            command.arg("-R");
        }
        command
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("explorer");
        if reveal {
            command.arg("/select,");
        }
        command
    } else {
        // Linux 的"在文件管理器里选中"没有统一做法，退化成打开所在目录
        if reveal {
            return match Command::new("xdg-open")
                .arg(path.parent().unwrap_or(&path))
                .status()
            {
                Ok(status) if status.success() => ExitCode::SUCCESS,
                Ok(status) => fail(&format!("打开目录失败：退出码 {status}")),
                Err(err) => fail(&format!("无法启动 xdg-open：{err}")),
            };
        }
        Command::new("xdg-open")
    };
    match command.arg(&path).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => fail(&format!("打开失败：退出码 {status}")),
        Err(err) => fail(&format!("无法启动打开命令：{err}")),
    }
}

fn edit() -> ExitCode {
    let path = paths::espanso_match_file();
    if !path.exists() {
        return missing();
    }

    println!("# 这是生成物：下次 `semisnip install` 会整份覆盖。");
    println!("# 想长期改触发串请用 `semisnip config set trigger <串>`。");

    let status = match std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map(|editor| (editor, Vec::<String>::new()))
    {
        Ok((editor, args)) => Command::new(editor).args(args).arg(&path).status(),
        // 没设编辑器就交给系统默认程序；-W 会等窗口关掉，才谈得上校验
        Err(_) if cfg!(target_os = "macos") => {
            Command::new("open").args(["-W", "-t"]).arg(&path).status()
        }
        Err(_) => {
            return fail("没有设置 $EDITOR / $VISUAL，也不知道该用什么打开这个文件");
        }
    };

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => return fail(&format!("编辑器退出码 {status}")),
        Err(err) => return fail(&format!("无法启动编辑器：{err}")),
    }

    let Ok(text) = std::fs::read_to_string(&path) else {
        return fail(&format!("读回 {} 失败", path.display()));
    };
    let report = check(&text, &super::degraded_trigger(&Config::load().trigger));
    report.print();
    if !report.errors.is_empty() {
        return ExitCode::FAILURE;
    }
    println!("\n记得 `espanso restart` 让它生效。");
    ExitCode::SUCCESS
}

fn regenerate() -> ExitCode {
    match super::write_match_file() {
        Ok(path) => {
            println!("已重新生成 {}", path.display());
            println!("让它生效：espanso restart");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&format!("{err}")),
    }
}

/// 手改 YAML 的护栏：只做"能不能被 espanso 用起来"这一层的检查，
/// 不求完备——毕竟真正的解析器是 espanso 自己。
#[derive(Debug, Default)]
struct Report {
    errors: Vec<String>,
    warns: Vec<String>,
}

impl Report {
    fn print(&self) {
        if self.errors.is_empty() && self.warns.is_empty() {
            println!("\n校验通过：YAML 合法，触发与当前配置一致。");
            return;
        }
        for warn in &self.warns {
            println!("  ⚠️  {warn}");
        }
        for error in &self.errors {
            println!("  ❌  {error}");
        }
    }
}

fn check(text: &str, expected_trigger: &str) -> Report {
    let mut report = Report::default();

    let parsed: Value = match serde_yaml::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            report.errors.push(format!("YAML 解析失败：{err}"));
            return report;
        }
    };

    let matches = parsed.get("matches").and_then(Value::as_sequence);
    let Some(matches) = matches.filter(|list| !list.is_empty()) else {
        report
            .errors
            .push("缺少非空的 `matches:` 列表，espanso 不会从这里读到任何东西".into());
        return report;
    };

    let trigger = matches[0]
        .get("trigger")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if trigger.is_empty() {
        report.errors.push("第一条 match 没有 `trigger`".into());
    } else if trigger != expected_trigger {
        // 这里的期望值是**降级触发串**（配置触发串 + 末字符重复一次，见
        // `degraded_trigger`）。两边不一致时，tap 失效后降级触发对不上号，
        // 面板就再也唤不出来了——值得一条警告。
        report.warns.push(format!(
            "trigger 是 `{trigger}`，但配置对应的降级触发是 `{expected_trigger}`：tap 失效时这个降级触发对不上"
        ));
    }

    if !text.contains("semisnip") {
        report
            .warns
            .push("文件里没出现 semisnip，确认它还在调用本程序".into());
    }

    // 旧格式（`replace: ";;;{{output}}"`）会把触发串注回文档：espanso 的注入
    // 与面板的回删/垫空格抢同一个光标位置，实测表现为文档里多出 `;;;`、甚至
    // 删掉用户正文。升级后残留这份旧文件是最难查的一类问题，单独报出来。
    let replace = matches[0].get("replace").and_then(Value::as_str);
    if let Some(replace) = replace {
        if !expected_trigger.is_empty() && replace.contains(expected_trigger) {
            report.warns.push(format!(
                "replace 里注回了触发串 `{expected_trigger}`（旧格式）：会和面板的回删/垫空格抢光标，跑 `semisnip match regenerate` 换成空输出格式"
            ));
        }
    }

    // espanso 会把 shell 变量的 stdout 原样注入文档，少一个重定向就是往用户
    // 文档里塞一行垃圾。这是手改这份文件最容易犯的错，值得单独盯一眼。
    let polluted = matches.iter().any(|entry| {
        entry
            .get("vars")
            .and_then(Value::as_sequence)
            .map(|vars| {
                vars.iter().any(|var| {
                    var.get("type").and_then(Value::as_str) == Some("shell")
                        && var
                            .get("params")
                            .and_then(|params| params.get("cmd"))
                            .and_then(Value::as_str)
                            .is_some_and(|cmd| !cmd.contains("/dev/null"))
                })
            })
            .unwrap_or(false)
    });
    if polluted {
        report.warns.push(
            "有 shell 变量的 cmd 没把 stdout 重定向到 /dev/null：它的输出会被原样注入用户文档"
                .into(),
        );
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generated() -> String {
        super::super::render_match_file("/Applications/Semisnip.app/Contents/MacOS/semisnip", ";;")
    }

    #[test]
    fn generated_file_passes_its_own_check() {
        // 自己生成的东西必须能过自己的校验，否则等于给用户埋雷
        let report = check(&generated(), ";;;");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.warns.is_empty(), "{:?}", report.warns);
    }

    #[test]
    fn degraded_trigger_repeats_the_last_char() {
        assert_eq!(super::super::degraded_trigger(";;"), ";;;");
        assert_eq!(super::super::degraded_trigger("/"), "//");
        assert_eq!(super::super::degraded_trigger(";;;x"), ";;;xx");
        // 空串不该 panic（虽然配置层不允许空触发串）
        assert_eq!(super::super::degraded_trigger(""), "");
    }

    #[test]
    fn generated_file_does_not_reinject_the_trigger() {
        // 降级 match 只负责擦掉触发串。注回触发串会让 espanso 的注入与面板的
        // 回删/垫空格抢同一个光标位置（历史 bug：文档多出 `;;;`、正文被删）。
        let text = generated();
        assert!(
            text.contains(r#"replace: "{{output}}""#),
            "replace 必须只留空输出：\n{text}"
        );
        assert!(
            !text.contains(r#"replace: ";;;"#),
            "不许把触发串注回文档：\n{text}"
        );
        // stdout 也必须重定向掉：留着管道就等于允许 CLI 的意外输出污染文档。
        assert!(text.contains(">/dev/null 2>&1"), "两条流都要静音：\n{text}");
    }

    #[test]
    fn reinjecting_trigger_is_flagged() {
        // 旧格式（升级前写进磁盘的那份）要被 check 认出来，否则用户会带着它跑很久。
        let text = generated().replace(r#"replace: "{{output}}""#, r#"replace: ";;;{{output}}""#);
        let report = check(&text, ";;;");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(
            report.warns.iter().any(|warn| warn.contains("注回")),
            "旧格式必须给出警告：{:?}",
            report.warns
        );
    }

    #[test]
    fn broken_yaml_is_an_error() {
        let report = check("matches: [\n", ";;");
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("YAML 解析失败"));
    }

    #[test]
    fn empty_matches_is_an_error() {
        let report = check("matches: []\n", ";;");
        assert!(!report.errors.is_empty());
        let report = check("foo: bar\n", ";;");
        assert!(!report.errors.is_empty());
    }

    #[test]
    fn trigger_mismatch_is_a_warning_not_an_error() {
        let text = generated().replace("trigger: \";;;\"", "trigger: \",,,\"");
        let report = check(&text, ";;;");
        assert!(report.errors.is_empty());
        assert_eq!(report.warns.len(), 1);
        assert!(report.warns[0].contains(",,,"));
    }

    #[test]
    fn missing_stdout_redirect_is_flagged() {
        let text = generated().replace(" >/dev/null 2>&1", "");
        let report = check(&text, ";;");
        assert!(report.errors.is_empty());
        assert!(
            report.warns.iter().any(|warn| warn.contains("/dev/null")),
            "{:?}",
            report.warns
        );
    }

    #[test]
    fn match_subcommand_defaults_to_show() {
        use clap::{Parser, Subcommand};

        #[derive(Parser)]
        struct Wrapper {
            #[command(subcommand)]
            cmd: Cmd,
        }

        #[derive(Subcommand)]
        enum Cmd {
            Match {
                #[command(subcommand)]
                action: Option<MatchCmd>,
            },
        }

        let parse = |extra: &[&str]| {
            let mut args = vec!["semisnip", "match"];
            args.extend_from_slice(extra);
            match Wrapper::try_parse_from(args).unwrap().cmd {
                Cmd::Match { action } => action,
            }
        };
        assert!(parse(&[]).is_none(), "不带子命令要落到 show");
        assert!(matches!(parse(&["path"]), Some(MatchCmd::Path)));
        assert!(matches!(parse(&["edit"]), Some(MatchCmd::Edit)));
        assert!(matches!(
            parse(&["open", "--reveal"]),
            Some(MatchCmd::Open { reveal: true })
        ));
        assert!(matches!(parse(&["regenerate"]), Some(MatchCmd::Regenerate)));
    }
}
