//! `semisnip log`：读 daemon 日志。
//!
//! daemon 以 `.app` 启动时没有终端，出问题只能回看文件，所以「不离开终端
//! 就能看日志」是这个子命令的全部意义。行为对齐 `tail`，但多一层级别过滤。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};

use crate::config;

const DEFAULT_LINES: usize = 40;
/// 一次最多回看 1 MiB：日志滚很久也不会把内存吃穿。
const MAX_TAIL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Args)]
pub struct LogArgs {
    /// 显示最近 N 行（默认 40）
    #[arg(short = 'n', long, value_name = "N")]
    lines: Option<usize>,

    /// 持续跟随新写入的行（Ctrl-C 退出）
    #[arg(short, long)]
    follow: bool,

    /// 只保留包含该串的行
    #[arg(short = 'g', long, value_name = "PATTERN")]
    grep: Option<String>,

    /// 只保留该级别及以上的行
    #[arg(
        short = 'l',
        long,
        value_enum,
        ignore_case = true,
        value_name = "LEVEL"
    )]
    level: Option<Level>,

    #[command(subcommand)]
    action: Option<LogAction>,
}

#[derive(Debug, Subcommand)]
pub enum LogAction {
    /// 打印日志文件路径
    Path,
    /// 清空日志
    Clear,
}

/// 过滤用的级别。`--level info` 就是"什么都看"，所以 DEBUG/TRACE 也归到 Info。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Level {
    #[value(name = "info", alias = "debug", alias = "trace")]
    Info,
    #[value(name = "warn", alias = "warning")]
    Warn,
    #[value(name = "error", alias = "err")]
    Error,
}

impl Level {
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

#[derive(Debug)]
struct Options {
    lines: usize,
    follow: bool,
    grep: Option<String>,
    level: Option<Level>,
}

pub fn run(args: LogArgs) -> ExitCode {
    let path = config::log_path();
    match args.action {
        Some(LogAction::Path) => {
            println!("{}", path.display());
            return ExitCode::SUCCESS;
        }
        Some(LogAction::Clear) => return clear(&path),
        None => {}
    }

    // 过滤对 path/clear 没意义，其余参数原样生效。
    let options = Options {
        lines: args.lines.unwrap_or(DEFAULT_LINES),
        follow: args.follow,
        grep: args.grep,
        level: args.level,
    };

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
    let skip = lines.len().saturating_sub(options.lines);
    for line in &lines[skip..] {
        println!("{line}");
    }

    if options.follow {
        follow(&path, &options);
    }
    ExitCode::SUCCESS
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
            eprintln!("semisnip log: 清空失败：{err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Parser, Subcommand};

    /// 真 CLI 里 `log` 是子命令，测试也走同一条路径，避免用 flatten 造出
    /// 一个和线上不一样的命令树。
    #[derive(Parser)]
    struct Wrapper {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[derive(Subcommand)]
    enum Cmd {
        Log(LogArgs),
    }

    fn parse(args: &[&str]) -> LogArgs {
        let mut full = vec!["semisnip", "log"];
        full.extend_from_slice(args);
        match Wrapper::try_parse_from(full).unwrap().cmd {
            Cmd::Log(log) => log,
        }
    }

    fn opts(grep: Option<&str>, level: Option<Level>) -> Options {
        Options {
            lines: DEFAULT_LINES,
            follow: false,
            grep: grep.map(str::to_string),
            level,
        }
    }

    #[test]
    fn parses_common_flags() {
        let log = parse(&["-n", "5", "-f", "--grep", "触发", "--level", "warn"]);
        assert_eq!(log.lines, Some(5));
        assert!(log.follow);
        assert_eq!(log.grep.as_deref(), Some("触发"));
        assert_eq!(log.level, Some(Level::Warn));
        assert!(log.action.is_none());
    }

    #[test]
    fn actions_are_subcommands() {
        assert!(matches!(parse(&["path"]).action, Some(LogAction::Path)));
        assert!(matches!(parse(&["clear"]).action, Some(LogAction::Clear)));
        assert!(Wrapper::try_parse_from(["semisnip", "log", "--whatever"]).is_err());
        assert!(
            Wrapper::try_parse_from(["semisnip", "log", "--level", "fatal"]).is_err(),
            "瞎写的级别该被挡住"
        );
    }

    #[test]
    fn level_accepts_legacy_spellings() {
        let level = |text: &str| parse(&["--level", text]).level;
        assert_eq!(level("info"), Some(Level::Info));
        assert_eq!(level("trace"), Some(Level::Info));
        assert_eq!(level("warning"), Some(Level::Warn));
        assert_eq!(level("ERROR"), Some(Level::Error));
    }

    #[test]
    fn level_is_read_from_brackets() {
        let line = "[2026-11-07T08:30:34Z WARN  semisnip::handoff] match exec 失败";
        assert_eq!(Level::of_line(line), Some(Level::Warn));
        assert_eq!(
            Level::of_line("[2026-11-07T08:30:34Z ERROR semisnip] boom"),
            Some(Level::Error)
        );
        assert_eq!(Level::of_line("没有前缀的续行"), None);
    }

    #[test]
    fn filters_by_grep_and_level() {
        let info = "[2026-11-07T08:30:34Z INFO  semisnip::ipc] 面板已打开";
        let warn = "[2026-11-07T08:30:34Z WARN  semisnip::handoff] match exec 失败";

        assert!(keep(info, &opts(None, None)));
        assert!(keep(info, &opts(Some("面板"), None)));
        assert!(!keep(info, &opts(Some("失败"), None)));
        assert!(!keep(info, &opts(None, Some(Level::Warn))));
        assert!(keep(warn, &opts(None, Some(Level::Warn))));
        assert!(!keep(warn, &opts(None, Some(Level::Error))));
    }
}
