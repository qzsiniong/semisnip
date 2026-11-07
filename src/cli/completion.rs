//! `snipshot completion`：生成并安装 shell 补全。
//!
//! 子命令有十几个，靠人记不现实，所以补全不是"锦上添花"而是主路径的一部分。
//! 设计上分成两半：
//!
//! * `snipshot completion <SHELL>` 把脚本打到 stdout，给 `eval "$(...)"` 用；
//! * `snipshot completion install` 落一个脚本文件，再往 shell 的启动文件里
//!   塞一段**带标记**的接管块——标记是为了 `uninstall` 能精确地原样摘掉，
//!   不去猜用户配置里哪几行是我们写的。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use clap_complete::Shell;

use super::{command, fail};

const BIN: &str = "snipshot";
const BEGIN: &str = "# >>> snipshot completion >>>";
const END: &str = "# <<< snipshot completion <<<";

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct CompletionArgs {
    /// 只把该 shell 的补全脚本打到 stdout（配合 eval 使用）
    #[arg(value_name = "SHELL", value_enum)]
    shell: Option<Shell>,

    /// 不带子命令时是安装动作，这个开关让它只演练
    #[arg(long)]
    dry_run: bool,

    #[command(subcommand)]
    action: Option<CompletionAction>,
}

#[derive(Debug, Subcommand)]
pub enum CompletionAction {
    /// 写补全脚本并接进 shell 启动文件（不带子命令时的默认动作）
    Install {
        /// 目标 shell；默认按 $SHELL 判断
        #[arg(long, value_enum, value_name = "SHELL")]
        shell: Option<Shell>,
        /// 只说明将要做什么，不落盘
        #[arg(long)]
        dry_run: bool,
    },
    /// 撤销安装：删脚本文件，并摘掉启动文件里的接管块
    Uninstall {
        /// 目标 shell；默认按 $SHELL 判断
        #[arg(long, value_enum, value_name = "SHELL")]
        shell: Option<Shell>,
        /// 只说明将要做什么，不落盘
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run(args: CompletionArgs) -> ExitCode {
    match (args.shell, args.action) {
        // `snipshot completion zsh` → 打印脚本
        (Some(shell), None) => {
            print_script(shell);
            ExitCode::SUCCESS
        }
        // `snipshot completion` → 直接装好，省掉一步
        (None, None) => install(None, args.dry_run),
        (Some(_), Some(_)) => fail(
            "SHELL 与子命令只能二选一：`snipshot completion zsh` 或 `snipshot completion install`",
        ),
        (None, Some(CompletionAction::Install { shell, dry_run })) => {
            install(shell, dry_run || args.dry_run)
        }
        (None, Some(CompletionAction::Uninstall { shell, dry_run })) => {
            uninstall(shell, dry_run || args.dry_run)
        }
    }
}

fn print_script(shell: Shell) {
    let mut cmd = command();
    clap_complete::generate(shell, &mut cmd, BIN, &mut std::io::stdout());
}

/// 每种 shell 的落点不一样：fish 会自动扫目录，其余得在启动文件里 source 一下。
struct Plan {
    /// 补全脚本写到哪
    script: PathBuf,
    /// 需要 source 它的启动文件；`None` 表示该 shell 会自动加载
    startup: Option<PathBuf>,
    /// 写进启动文件的那一行
    hook: String,
}

impl Plan {
    fn build(shell: Shell) -> Result<Self, String> {
        let dir = completions_dir();
        let quote = |path: &Path| format!("\"{}\"", path.display());
        match shell {
            Shell::Bash => {
                let script = dir.join("snipshot.bash");
                let startup = bash_startup()?;
                let hook = format!("[ -r {} ] && . {}", quote(&script), quote(&script));
                Ok(Self {
                    script,
                    startup: Some(startup),
                    hook,
                })
            }
            Shell::Zsh => {
                let script = dir.join("snipshot.zsh");
                // compinit 没跑过时 `compdef` 还不存在，直接 source 会每次开
                // 终端都报错，所以先探一下。
                let hook = format!(
                    "if (( $+functions[compdef] )) && [ -r {0} ]; then\n  source {0}\nfi",
                    quote(&script)
                );
                Ok(Self {
                    script,
                    startup: Some(home()?.join(".zshrc")),
                    hook,
                })
            }
            Shell::Fish => {
                // fish 自动加载 ~/.config/fish/completions/*.fish，不用改 rc。
                let script = home()?
                    .join(".config")
                    .join("fish")
                    .join("completions")
                    .join("snipshot.fish");
                Ok(Self {
                    script,
                    startup: None,
                    hook: String::new(),
                })
            }
            Shell::PowerShell => {
                let script = dir.join("snipshot.ps1");
                let startup = home()?
                    .join(".config")
                    .join("powershell")
                    .join("Microsoft.PowerShell_profile.ps1");
                let hook = format!(". {}", quote(&script));
                Ok(Self {
                    script,
                    startup: Some(startup),
                    hook,
                })
            }
            _ => {
                Err("这个 shell 没法自动接入，请用 `snipshot completion <shell>` 自行 eval".into())
            }
        }
    }
}

fn install(shell: Option<Shell>, dry_run: bool) -> ExitCode {
    let shell = match shell.map_or_else(detect_shell, Ok) {
        Ok(shell) => shell,
        Err(err) => return fail(&err),
    };
    let plan = match Plan::build(shell) {
        Ok(plan) => plan,
        Err(err) => return fail(&err),
    };

    let verb = if dry_run { "将写入" } else { "已写入" };
    if !dry_run {
        if let Some(parent) = plan.script.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            return fail(&format!("创建 {} 失败：{err}", parent.display()));
        }
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut command(), BIN, &mut buf);
        if let Err(err) = fs::write(&plan.script, buf) {
            return fail(&format!("写入 {} 失败：{err}", plan.script.display()));
        }
    }
    println!("{verb}补全脚本  {}", plan.script.display());

    if let Some(startup) = &plan.startup {
        match apply_hook(startup, &plan.hook, dry_run) {
            Ok(Change::Added) => println!("{verb}接入块    {}（新增）", startup.display()),
            Ok(Change::Updated) => println!("{verb}接入块    {}（更新）", startup.display()),
            Ok(Change::Unchanged) => println!("接入块已是    {}（无需改动）", startup.display()),
            Err(err) => return fail(&err),
        }
    } else {
        println!("无需改启动文件  {} 会自动加载该目录", shell);
    }

    println!();
    if dry_run {
        println!("以上为演练，去掉 --dry-run 才会真的写。");
    } else {
        println!("生效：重开终端，或 `exec {shell}`");
        println!("试试：`{BIN} co<Tab>` → completion");
        println!("撤销：`{BIN} completion uninstall`");
    }
    ExitCode::SUCCESS
}

fn uninstall(shell: Option<Shell>, dry_run: bool) -> ExitCode {
    let shell = match shell.map_or_else(detect_shell, Ok) {
        Ok(shell) => shell,
        Err(err) => return fail(&err),
    };
    let plan = match Plan::build(shell) {
        Ok(plan) => plan,
        Err(err) => return fail(&err),
    };

    let mut touched = false;
    if let Some(startup) = &plan.startup {
        match remove_hook(startup, dry_run) {
            Ok(true) => {
                touched = true;
                println!(
                    "{}接入块    {}",
                    if dry_run { "将移除" } else { "已移除" },
                    startup.display()
                );
            }
            Ok(false) => println!("启动文件里没有接管块  {}", startup.display()),
            Err(err) => return fail(&err),
        }
    }
    if plan.script.exists() || dry_run {
        if !dry_run {
            if let Err(err) = fs::remove_file(&plan.script) {
                return fail(&format!("删除 {} 失败：{err}", plan.script.display()));
            }
        }
        touched = true;
        println!(
            "{}补全脚本  {}",
            if dry_run { "将删除" } else { "已删除" },
            plan.script.display()
        );
    }

    if !touched {
        println!("没有发现需要清理的内容。");
    } else if !dry_run {
        println!("\n重开终端后补全即失效（已开着的 shell 还会记得旧的补全函数）。");
    }
    ExitCode::SUCCESS
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    Added,
    Updated,
    Unchanged,
}

fn apply_hook(startup: &Path, hook: &str, dry_run: bool) -> Result<Change, String> {
    let text = read_or_empty(startup)?;
    let block = format!("{BEGIN}\n{}\n{END}\n", hook.trim_end());
    let existed = region(&text).is_some();
    let next = upsert(&text, &block);

    let change = if next == text {
        Change::Unchanged
    } else if existed {
        Change::Updated
    } else {
        Change::Added
    };
    if change != Change::Unchanged && !dry_run {
        write_text(startup, &next)?;
    }
    Ok(change)
}

fn remove_hook(startup: &Path, dry_run: bool) -> Result<bool, String> {
    let text = read_or_empty(startup)?;
    let Some(next) = strip(&text) else {
        return Ok(false);
    };
    if !dry_run {
        write_text(startup, &next)?;
    }
    Ok(true)
}

/// 标记块的字节区间：从 `BEGIN` 那行行首，到 `END` 那行的换行之后。
fn region(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BEGIN)?;
    let end_at = text[start..].find(END)? + start;
    let end = match text[end_at..].find('\n') {
        Some(offset) => end_at + offset + 1,
        None => text.len(),
    };
    Some((start, end))
}

fn upsert(text: &str, block: &str) -> String {
    match region(text) {
        Some((start, end)) => format!("{}{}{}", &text[..start], block, &text[end..]),
        None => {
            let mut out = text.to_string();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(block);
            out
        }
    }
}

fn strip(text: &str) -> Option<String> {
    let (start, end) = region(text)?;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    // 追加时补的那个空行也跟着收掉，反复装卸不会越堆越空。
    while out.ends_with("\n\n") {
        out.pop();
    }
    Some(out)
}

fn read_or_empty(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("读取 {} 失败：{err}", path.display())),
    }
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        return Err(format!("创建 {} 失败：{err}", parent.display()));
    }
    fs::write(path, text).map_err(|err| format!("写入 {} 失败：{err}", path.display()))
}

fn detect_shell() -> Result<Shell, String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let name = Path::new(&shell)
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let matched = if name.contains("zsh") {
        Some(Shell::Zsh)
    } else if name.contains("bash") {
        Some(Shell::Bash)
    } else if name.contains("fish") {
        Some(Shell::Fish)
    } else if name.contains("pwsh") || name.contains("powershell") {
        Some(Shell::PowerShell)
    } else if name.contains("elvish") {
        Some(Shell::Elvish)
    } else {
        None
    };
    matched.ok_or_else(|| {
        format!("认不出 $SHELL=`{shell}`，请显式指定：`snipshot completion install --shell zsh`")
    })
}

fn home() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "找不到家目录".to_string())
}

fn completions_dir() -> PathBuf {
    crate::config::support_dir().join("completions")
}

/// macOS 的 Terminal / iTerm 默认起的是 login shell，读 `.bash_profile`；
/// 已经存在的那个就是用户实际在用的，优先它。
fn bash_startup() -> Result<PathBuf, String> {
    let home = home()?;
    for name in [".bash_profile", ".bashrc"] {
        let candidate = home.join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Ok(home.join(".bash_profile"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_is_appended_once_and_replaced_in_place() {
        let original = "export PATH=/usr/bin:$PATH\n";
        let block = format!("{BEGIN}\nold\n{END}\n");
        let once = upsert(original, &block);
        assert!(once.starts_with("export PATH"), "不该动用户的原有内容");
        assert_eq!(once.matches(BEGIN).count(), 1);

        // 再装一次：就地替换，不会追加第二块
        let block2 = format!("{BEGIN}\nnew\n{END}\n");
        let twice = upsert(&once, &block2);
        assert_eq!(twice.matches(BEGIN).count(), 1);
        assert!(twice.contains("new"));
        assert!(!twice.contains("old"));
        assert_eq!(twice, upsert(&twice, &block2), "重复执行应当幂等");
    }

    #[test]
    fn strip_removes_only_our_block() {
        let original = "alias ll='ls -l'\n";
        let block = format!("\n{BEGIN}\nsource x\n{END}\n");
        let combined = upsert(original, &block);
        assert!(combined.contains(BEGIN));

        let restored = strip(&combined).unwrap();
        assert_eq!(restored.trim_end(), original.trim_end());
        assert!(!restored.contains(BEGIN));

        // 没有块时返回 None，调用方据此报"没有需要清理的内容"
        assert!(strip(original).is_none());
        // 只有 BEGIN 没有 END（手改坏了）也不能乱删
        assert!(strip(&format!("{original}\n{BEGIN}\n")).is_none());
    }

    #[test]
    fn every_supported_shell_has_a_plan() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
            let plan = Plan::build(shell).unwrap_or_else(|err| panic!("{shell}: {err}"));
            assert!(!plan.script.as_os_str().is_empty());
            if shell == Shell::Fish {
                assert!(plan.startup.is_none(), "fish 自动加载，不该去改 rc");
            } else {
                assert!(plan.startup.is_some());
                assert!(!plan.hook.is_empty());
            }
        }
        // elvish 没有约定俗成的自动加载点，明确拒绝而不是装个半成品
        assert!(Plan::build(Shell::Elvish).is_err());
    }

    #[test]
    fn generated_scripts_are_non_empty_for_every_shell() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut command(), BIN, &mut buf);
            let text = String::from_utf8(buf).unwrap();
            assert!(text.contains("snipshot"), "{shell} 的脚本里没有命令名");
        }
    }

    #[test]
    fn completion_subcommands_parse() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrapper {
            #[command(subcommand)]
            action: CompletionAction,
        }

        assert!(matches!(
            Wrapper::try_parse_from(["snipshot", "install"])
                .unwrap()
                .action,
            CompletionAction::Install { .. }
        ));
        assert!(matches!(
            Wrapper::try_parse_from(["snipshot", "uninstall"])
                .unwrap()
                .action,
            CompletionAction::Uninstall { .. }
        ));
        assert!(
            Wrapper::try_parse_from(["snipshot", "install", "--shell", "zsh"]).is_ok(),
            "--shell 必须能收下 zsh"
        );
    }
}
