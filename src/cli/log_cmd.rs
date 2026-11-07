//! `snipshot log`：读 daemon 日志。
//!
//! daemon 以 `.app` 启动时没有终端，出问题只能回看文件，所以「不离开终端
//! 就能看日志」是这个子命令的全部意义。行为对齐 `tail`，但多一层级别过滤。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use crate::config;

const DEFAULT_LINES: usize = 40;
/// 一次最多回看 1 MiB：日志滚很久也不会把内存吃穿。
const MAX_TAIL_BYTES: u64 = 1024 * 1024;

const USAGE: &str = "\
用法:
  snipshot log                 最近 40 行
  snipshot log -n 200          最近 200 行
  snipshot log -f              持续跟随（Ctrl-C 退出）
  snipshot log --grep 触发      只保留包含该串的行
  snipshot log --level warn    只保留 warn 及以上（info/warn/error）
  snipshot log path            打印日志路径
  snipshot log clear           清空日志";

#[derive(Debug, Default)]
struct Options {
    lines: Option<usize>,
    follow: bool,
    grep: Option<String>,
    level: Option<Level>,
    action: Action,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum Action {
    #[default]
    Tail,
    Path,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "info" | "debug" | "trace" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" | "err" => Some(Self::Error),
            _ => None,
        }
    }

    fn of_line(line: &str) -> Option<Self> {
        // env_logger 默认格式：`[2026-11-07T08:30:34Z INFO  target] msg`
        let start = line.find('[')? + 1;
        let end = line[start..].find(']')? + start;
        let token = line[start..end].split_whitespace().nth(1)?;
        match token {
            "ERROR" => Some(Self::Error),
            "WARN" => Some(Self::Warn),
            "INFO" => Some(Self::Info),
            // TRACE / DEBUG 归到 Info 档，--level info 时不额外放行
            _ => None,
        }
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("snipshot log: {err}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let path = config::log_path();
    match options.action {
        Action::Path => {
            println!("{}", path.display());
            return ExitCode::SUCCESS;
        }
        Action::Clear => return clear(&path),
        Action::Tail => {}
    }

    if !path.exists() {
        println!("{}（还没有日志：daemon 未启动过？）", path.display());
        return ExitCode::FAILURE;
    }

    let text = read_tail(&path, MAX_TAIL_BYTES);
    let total = text.lines().count();
    let lines: Vec<&str> = text.lines().filter(|line| keep(line, &options)).collect();
    if lines.is_empty() && !options.follow {
        // 空白屏幕最容易让人以为命令坏了，把「为什么空」说清楚。
        if total == 0 {
            println!("{}（空）", path.display());
        } else {
            println!("没有匹配的行（共 {total} 行，试试去掉 --grep / --level）");
        }
        return ExitCode::SUCCESS;
    }
    let take = options.lines.unwrap_or(DEFAULT_LINES);
    let skip = lines.len().saturating_sub(take);
    for line in &lines[skip..] {
        println!("{line}");
    }

    if options.follow {
        follow(&path, &options);
    }
    ExitCode::SUCCESS
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-f" | "--follow" => options.follow = true,
            "path" => options.action = Action::Path,
            "clear" => options.action = Action::Clear,
            "-n" | "--lines" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "-n 后面需要行数".to_string())?;
                options.lines = Some(value.parse().map_err(|_| format!("`{value}` 不是行数"))?);
                index += 1;
            }
            "-g" | "--grep" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--grep 后面需要关键词".to_string())?;
                options.grep = Some(value.clone());
                index += 1;
            }
            "-l" | "--level" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--level 后面需要级别".to_string())?;
                options.level =
                    Some(Level::parse(value).ok_or_else(|| format!("未知级别 `{value}`"))?);
                index += 1;
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => {
                // 裸数字当作 `-n`
                if let Ok(n) = other.parse::<usize>() {
                    options.lines = Some(n);
                } else {
                    return Err(format!("无法理解的参数 `{other}`"));
                }
            }
        }
        index += 1;
    }
    if options.level.is_some() && options.action != Action::Tail {
        // 过滤对 path/clear 没意义，直接忽略即可，不必报错
        options.level = None;
    }
    Ok(options)
}

fn keep(line: &str, options: &Options) -> bool {
    if let Some(pattern) = &options.grep
        && !line.contains(pattern.as_str())
    {
        return false;
    }
    if let Some(wanted) = options.level {
        // 解析不出来的行（比如栈回溯续行）跟着上一行走，统一放行
        match Level::of_line(line) {
            Some(level) if level < wanted => return false,
            _ => {}
        }
    }
    true
}

fn read_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0
        && let Some(newline) = text.find('\n')
    {
        // 从字节中间切进去，第一行多半是残片
        text.drain(..=newline);
    }
    text
}

fn follow(path: &Path, options: &Options) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut offset = file.metadata().map(|meta| meta.len()).unwrap_or(0);

    eprintln!("—— 跟随 {}（Ctrl-C 退出）——", path.display());
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        let len = meta.len();
        if len < offset {
            // 被 clear 或轮转过：从头再来
            offset = 0;
        }
        if len == offset {
            continue;
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            continue;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            continue;
        }
        offset = len;
        let chunk = String::from_utf8_lossy(&buf);
        for line in chunk.lines().filter(|line| keep(line, options)) {
            println!("{line}");
        }
        let _ = std::io::stdout().flush();
    }
}

fn clear(path: &Path) -> ExitCode {
    if !path.exists() {
        println!("{} 不存在，无需清理", path.display());
        return ExitCode::SUCCESS;
    }
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    match std::fs::File::create(path) {
        Ok(_) => {
            println!(
                "已清空 {}（原 {:.1} KB）",
                path.display(),
                size as f64 / 1024.0
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("snipshot log: 清空失败：{err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(grep: Option<&str>, level: Option<Level>) -> Options {
        Options {
            grep: grep.map(str::to_string),
            level,
            ..Options::default()
        }
    }

    #[test]
    fn parses_common_flags() {
        let args: Vec<String> = ["-n", "5", "-f", "--grep", "触发"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let options = parse(&args).unwrap();
        assert_eq!(options.lines, Some(5));
        assert!(options.follow);
        assert_eq!(options.grep.as_deref(), Some("触发"));
    }

    #[test]
    fn bare_number_means_lines() {
        let args = vec!["120".to_string()];
        assert_eq!(parse(&args).unwrap().lines, Some(120));
    }

    #[test]
    fn rejects_unknown_flag() {
        let args = vec!["--whatever".to_string()];
        assert!(parse(&args).is_err());
    }

    #[test]
    fn level_is_read_from_brackets() {
        let line = "[2026-11-07T08:30:34Z WARN  snipshot::handoff] match exec 失败";
        assert_eq!(Level::of_line(line), Some(Level::Warn));
        assert_eq!(
            Level::of_line("[2026-11-07T08:30:34Z ERROR snipshot] boom"),
            Some(Level::Error)
        );
        assert_eq!(Level::of_line("没有前缀的续行"), None);
    }

    #[test]
    fn filters_by_grep_and_level() {
        let info = "[2026-11-07T08:30:34Z INFO  snipshot::ipc] 面板已打开";
        let warn = "[2026-11-07T08:30:34Z WARN  snipshot::handoff] match exec 失败";

        assert!(keep(info, &opts(None, None)));
        assert!(keep(info, &opts(Some("面板"), None)));
        assert!(!keep(info, &opts(Some("失败"), None)));
        assert!(!keep(info, &opts(None, Some(Level::Warn))));
        assert!(keep(warn, &opts(None, Some(Level::Warn))));
        assert!(!keep(warn, &opts(None, Some(Level::Error))));
    }
}
