//! 子命令入口（clap）。
//!
//! 只有 `daemon` 会初始化 gpui；其余子命令必须是"秒退、不打印 stdout"的
//! 轻量进程——`trigger` 会被 espanso 的 shell 变量同步调用。
//!
//! 不带子命令时分两种情况，靠"谁把进程拉起来的"区分（见 [`launched_from_bundle`]）：
//!
//! - **终端里裸敲 `semisnip`**：只打印帮助。一个会常驻、会抢键盘的进程不该是
//!   一条裸命令的默认后果。
//! - **Finder 双击 / `open -a Semisnip`**：这就是"我要用这个 app"，所以起
//!   daemon（已有实例时把面板叫出来）。否则双击的观感是"图标闪一下，什么都没发生"。

mod completion;
mod config_cmd;
mod log_cmd;
mod match_cmd;
mod uninstall;

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

use crate::config::Config;
use crate::espanso::cli::EspansoCli;
use crate::index::loader::{SnippetSource, YamlSource};
use crate::ipc::{client, proto::Origin};
use crate::paths;

#[derive(Debug, Parser)]
#[command(
    name = "semisnip",
    version,
    about = "espanso 的片段检索层：非激活面板负责选，espanso 负责展",
    subcommand_required = true,
    arg_required_else_help = true,
    max_term_width = 92,
    after_help = "常用：
  semisnip install               接入 espanso（写入降级触发的 match 定义）
  semisnip match edit            看/改写进 espanso 的 semisnip.yml（退出时校验）
  semisnip completion            给当前 shell 装上补全（再也不用记子命令）
  semisnip doctor                体检：espanso / 权限 / 索引 / daemon
  semisnip config set trigger ,, 把触发串改成 `,,`
  semisnip log -n 100 -g 触发     回看最近的日志"
)]
/// 顶层参数。`subcommand_required = true` + `arg_required_else_help`：
/// 不带子命令时打印帮助并以 0 退出，而不是默默起一个常驻 daemon。
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 启动 daemon：常驻进程，负责 IPC、键盘 tap 与面板
    Daemon,
    /// 请求打开片段面板（espanso 的 shell 变量会同步调用它）
    Trigger {
        /// 谁唤起的；只影响日志与降级判断
        #[arg(long, value_enum, default_value_t = OriginArg::Manual, value_name = "KIND")]
        origin: OriginArg,
    },
    /// 重建片段索引（daemon 会顺带重新读一遍 YAML）
    Reload,
    /// 接入 espanso：写入触发定义，并给出后续步骤
    Install,
    /// 查看/编辑写进 espanso 的 semisnip.yml
    #[command(alias = "yml")]
    Match {
        #[command(subcommand)]
        action: Option<match_cmd::MatchCmd>,
    },
    /// 卸载 semisnip 自己装进去的东西
    Uninstall(uninstall::UninstallArgs),
    /// 查看与修改配置
    Config {
        #[command(subcommand)]
        action: Option<config_cmd::ConfigCmd>,
    },
    /// 查看 daemon 日志
    Log(log_cmd::LogArgs),
    /// 生成或安装 shell 补全
    Completion(completion::CompletionArgs),
    /// 体检：espanso / 权限 / 索引 / daemon
    Doctor,
    /// 调试：合成字符输入
    #[command(hide = true)]
    Type {
        /// 要键入的文本（多个参数会用空格拼接）
        #[arg(value_name = "TEXT", num_args = 0.., trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// 调试：合成命名按键
    #[command(hide = true)]
    Key {
        /// esc / enter / up / down 等
        #[arg(value_name = "KEY", required = true)]
        keys: Vec<String>,
    },
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
enum OriginArg {
    /// 键盘 tap 命中（正常路径）
    Tap,
    /// espanso 的 shell 变量唤醒（无辅助功能权限时的降级路径）
    #[value(name = "espanso", alias = "espanso-shell", alias = "espanso_shell")]
    Espanso,
    /// 手动在终端执行
    #[default]
    Manual,
}

impl From<OriginArg> for Origin {
    fn from(value: OriginArg) -> Self {
        match value {
            OriginArg::Tap => Origin::Tap,
            OriginArg::Espanso => Origin::EspansoShell,
            OriginArg::Manual => Origin::Manual,
        }
    }
}

/// 解析参数并分派子命令。返回的退出码直接决定进程怎么做收尾。
///
/// 每条子命令的"输出纪律"不一样，改的时候要看清：
/// - 除 `daemon` 外的命令都是秒退的短进程；
/// - `trigger` / `type` / `key` **绝不写 stdout**（espanso 会把 stdout 当展开内容）；
/// - 其余命令写 stdout 是给人看的，错误写 stderr。
pub fn run() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // 必须抢在 clap 前面：`arg_required_else_help` 会把"没有参数"当成"想看看
    // 怎么用"，而 Finder 双击恰恰也是"没有参数"。两者的区别不在参数，在启动来源。
    if launched_from_bundle(args.len(), bundle_id_env().as_deref()) {
        return launch_from_bundle();
    }
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => return report(err),
    };
    // 只有 daemon 会进 gpui 事件循环，所以它必须最后一步再分派。
    match cli.command {
        Command::Daemon => {
            crate::run_daemon();
            ExitCode::SUCCESS
        }
        Command::Trigger { origin } => trigger(origin.into()),
        Command::Reload => match client::reload() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(&format!("reload 失败：{err}")),
        },
        Command::Install => install(),
        Command::Match { action } => match_cmd::run(action),
        Command::Uninstall(args) => uninstall::run(args),
        Command::Config { action } => config_cmd::run(action),
        Command::Log(args) => log_cmd::run(args),
        Command::Completion(args) => completion::run(args),
        Command::Doctor => doctor(),
        // 调试用：合成按键（从 CLI 进程发出，source pid 与 daemon 不同，
        // 因此 tap 会当作真实输入）。没有辅助功能权限时也能冒烟测交互。
        Command::Type { text } => {
            crate::handoff::inject::type_text(&text.join(" "));
            ExitCode::SUCCESS
        }
        Command::Key { keys } => {
            for name in &keys {
                crate::handoff::inject::press_named(name);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            ExitCode::SUCCESS
        }
    }
}

/// 读 LaunchServices 注入的 bundle id。
///
/// `__CFBundleIdentifier` 是 launchd 给"以某个 app 身份运行"的进程打的标记：
/// 从 Finder 双击我们的包时是 `com.semisnip.app`；从终端裸跑时是终端自己的 id
/// （iTerm2 是 `com.googlecode.iterm2`，等等），于是两种来源自然分开了。
/// 拿不到（老系统、非 LS 启动）就当 CLI 处理——宁可打印帮助，也不要意外起一个常驻进程。
fn bundle_id_env() -> Option<String> {
    std::env::var("__CFBundleIdentifier").ok()
}

/// 判断"这次是双击 app 启动的"，而不是终端里敲的命令。
///
/// 参数必须为空：带子命令时永远是 CLI（LaunchAgent 的 `daemon`、espanso 的
/// `trigger` 都是这条路）。bundle id 必须是我们自己：终端里跑的进程会继承
/// 终端的 id。
///
/// 抽成纯函数只为了能测：真实环境里改不了自己的父进程，也改不了 launchd 注入
/// 的变量。
fn launched_from_bundle(argc: usize, bundle_id: Option<&str>) -> bool {
    argc <= 1 && bundle_id == Some(paths::BUNDLE_ID)
}

/// 双击启动时的默认动作。
///
/// 分成两种情况，因为用户要的东西不一样：
///
/// - daemon 没在跑：自己当 daemon（菜单栏出现图标；首次运行还会弹引导页）。
///   这条路不经过 launchd，所以**不会开机自启**——那是 `--install` 里
///   LaunchAgent 的职责。
/// - daemon 已经在跑：把面板叫出来。这是唯一能让人看见"它活着"的反馈；
///   默默退出会和"双击没反应"一模一样。
fn launch_from_bundle() -> ExitCode {
    // `ping` 而不是 `status`：只关心有没有活着的实例，不关心它的 tap 状态。
    if client::ping().is_ok() {
        let trigger = Config::load().trigger;
        match client::open(Origin::Manual, &trigger) {
            Ok(()) => return ExitCode::SUCCESS,
            // 叫不出面板就退化成"自己起一个"：那会撞上单实例检查并以 0 退出，
            // 至少不会留下半启动的状态。
            Err(err) => log::debug!("已有 daemon 但叫不出面板：{err}"),
        }
    }
    crate::run_daemon();
    ExitCode::SUCCESS
}

/// 把 clap 的错误转成退出码。
///
/// 关键区分：`semisnip`（无参数）走的是 `DisplayHelpOnMissingArgumentOrSubcommand`，
/// 那是"想看看怎么用"，不是错误，退出码必须是 0。
fn report(err: clap::Error) -> ExitCode {
    let code = classify(err.kind());
    // clap 自己决定走 stdout 还是 stderr（help → stdout，用法错误 → stderr）。
    let _ = err.print();
    code
}

fn classify(kind: clap::error::ErrorKind) -> ExitCode {
    use clap::error::ErrorKind;

    match kind {
        // `semisnip`（无参数）会落到第二项：那是"想看看怎么用"，不是错误。
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        | ErrorKind::DisplayVersion => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// 统一错误出口：打到 stderr，返回失败退出码。
///
/// 单独抽出来是为了让"错误文本前缀"只有一处：`semisnip: ` 这个格式在脚本里
/// 被 grep 过，各处手写迟早会漏掉前缀。
pub(crate) fn fail(message: &str) -> ExitCode {
    eprintln!("semisnip: {message}");
    ExitCode::FAILURE
}

/// 供 `completion` 生成补全脚本时复用同一份命令定义。
pub(crate) fn command() -> clap::Command {
    Cli::command()
}

fn trigger(origin: Origin) -> ExitCode {
    match client::open(origin, ";;") {
        // 关键：成功时绝不能往 stdout 写任何东西——espanso 会把 stdout
        // 当作展开内容注入，多一个字节就会污染用户文档。
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(&format!("{err}")),
    }
}

/// 接入 espanso：写配置、写降级触发定义、打印后续步骤。
///
/// 为什么还要 `config.save()`：首次安装时配置文件并不存在，而用户接着打开
/// 设置窗口 / 手改 YAML 都需要一个带抬头和 schema 提示的起点。这里主动写一份，
/// 比让用户面对"文件不存在"友好。
///
/// 输出全部走 stdout：这是给人看的命令，不是被 espanso 同步调用的路径。
fn install() -> ExitCode {
    paths::ensure_dirs();
    let config = Config::load();
    let _ = config.save();

    let target = match write_match_file() {
        Ok(target) => target,
        Err(err) => return fail(&format!("{err}")),
    };
    println!("已写入 {}", target.display());
    println!();

    let exe = std::env::current_exe()
        .map(|exe| exe.display().to_string())
        .unwrap_or_else(|_| "<semisnip>".into());
    println!("下一步：");
    println!("  1. espanso restart        （让新 match 生效）");
    println!("  2. 把 Semisnip.app 加进「系统设置 → 隐私与安全性 → 辅助功能」");
    println!(
        "     —— 没有这项权限时，`{}` 会退化成 espanso shell 触发",
        config.trigger
    );
    println!("  3. 跑 `semisnip doctor` 确认每项都打勾");
    // 二进制藏在 .app 里，没软链的话 shell 找不到命令，这一步经常被漏掉。
    if !paths::cli_link().exists() {
        println!();
        println!("提示：`semisnip` 还不在 PATH 上，补一条软链后命令才可用：");
        println!("  sudo ln -sf {exe} /usr/local/bin/semisnip");
    }
    ExitCode::SUCCESS
}

/// 把 match 定义写进 espanso 的 match 目录，已存在则先备份。
///
/// `install` 和 `semisnip match regenerate` 走同一条路径：两边各写一份迟早
/// 会写到不一致，而这份文件错了的表现是「`;;` 时好时坏」，极难查。
pub(crate) fn write_match_file() -> anyhow::Result<std::path::PathBuf> {
    // current_exe 返回的是 argv[0] 那条路径，可能只是 /usr/local/bin 的软链。
    // 写进 yml 的必须是真身：软链哪天被删，降级触发就会静默失效（espanso 只
    // 记日志，用户什么也看不到）。
    let exe = resolve_exe(std::env::current_exe()?);
    let trigger = Config::load().trigger;

    let match_dir = paths::espanso_match_dir();
    std::fs::create_dir_all(&match_dir)?;

    let target = paths::espanso_match_file();
    if target.exists() {
        let backup = match_dir.join(format!("{}.bak", paths::ESPANSO_MATCH_FILE));
        std::fs::copy(&target, &backup)?;
        println!("已备份原文件到 {}", backup.display());
    }

    std::fs::write(
        &target,
        render_match_file(&exe.display().to_string(), &trigger),
    )?;

    Ok(target)
}

/// 把 exe 路径上的软链解开；解不开就原样返回（比如二进制刚被删掉）。
fn resolve_exe(exe: std::path::PathBuf) -> std::path::PathBuf {
    exe.canonicalize().unwrap_or(exe)
}

/// 生成降级路径的 match 定义：tap 不可用时由 espanso 唤醒 semisnip。
pub fn render_match_file(exe: &str, trigger: &str) -> String {
    format!(
        r#"# 由 `semisnip install` 生成。
#
# 作用：当 semisnip 的键盘 tap 不可用（缺辅助功能权限）时，
# 由 espanso 的 shell 扩展唤醒面板。tap 正常工作时，`{trigger}` 的最后一个键
# 已被 semisnip 吞掉，espanso 根本看不到这个 trigger，本文件不会生效。
#
# 注意：shell 变量的 stdout 会被原样注入文档，所以命令必须保持静默。
# yaml-language-server: $schema=https://raw.githubusercontent.com/espanso/espanso/dev/schemas/match.schema.json
matches:
  - trigger: "{trigger}"
    replace: "{{{{output}}}}"
    label: "semisnip · 打开片段面板（降级触发）"
    vars:
      - name: output
        type: shell
        params:
          cmd: "'{exe}' trigger --origin espanso 2>>/dev/null"
          shell: sh
          trim: true
"#
    )
}

/// 体检。逐项打印结果，最后给一句总结；退出码只看"有没有硬问题"。
///
/// 两个细节值得注意：
/// - 索引是**现读**的（`YamlSource::load`），不用 daemon 的快照——体检要回答
///   "现在磁盘上有什么"，而不是"daemon 记得什么"；
/// - 辅助功能那一项**问 daemon**（`client::status`），不是查自己：TCC 按进程
///   授权，从终端跑的 doctor 继承的是终端的权限，自己查只会得到误导性的绿勾。
fn doctor() -> ExitCode {
    let mut problems = 0;

    println!("semisnip {}", env!("CARGO_PKG_VERSION"));

    let cli = EspansoCli::discover();
    match cli.version() {
        Some(version) => println!("  espanso      ✅ {version} ({})", cli.bin().display()),
        None => {
            problems += 1;
            println!("  espanso      ❌ 找不到可执行的 espanso");
        }
    }

    let root = paths::espanso_root();
    println!(
        "  配置目录     {} {}",
        if root.exists() { "✅" } else { "⚠️ " },
        root.display()
    );

    let snapshot = YamlSource::new(root.clone()).load();
    match snapshot {
        Ok(snapshot) => {
            println!(
                "  片段         ✅ {} 条（espanso 校验 {}）",
                snapshot.len(),
                if snapshot.espanso_checked {
                    format!("{} 个 trigger", snapshot.espanso_known.len())
                } else {
                    "未执行".to_string()
                }
            );
            for diagnostic in snapshot.diagnostics.iter().take(5) {
                println!("               ⚠️  {}", diagnostic.message());
            }
            if snapshot.is_empty() {
                problems += 1;
            }
        }
        Err(err) => {
            problems += 1;
            println!("  片段         ❌ 索引失败：{err}");
        }
    }

    let socket = paths::socket_path();
    let alive = client::ping().is_ok();
    println!(
        "  daemon       {} {}",
        if alive { "✅" } else { "❌" },
        if alive {
            format!("运行中（{}）", socket.display())
        } else {
            "未运行：执行 `semisnip daemon` 或让 LaunchAgent 拉起".to_string()
        }
    );
    if !alive {
        problems += 1;
    }

    // TCC 授权是按进程判的：从终端跑 `doctor` 会继承终端的授权，
    // 而 launchd 起的 daemon 是另一份。所以这里以 daemon 自报的状态为准。
    let daemon_tap = if alive { client::status().ok() } else { None };
    let local_tap = crate::trigger::tap::accessibility_granted();
    let mut degraded = false;
    match daemon_tap {
        Some(true) => println!("  辅助功能     ✅ 已授权（tap 路径可用）"),
        Some(false) => {
            degraded = true;
            println!("  辅助功能     ⚠️  未授权：daemon 正在走 espanso 降级触发");
            if local_tap {
                // 最迷惑人的一种：终端里测着是好的，daemon 却没有权限。
                println!("               （当前进程有授权，但 daemon 没有——两者是独立条目）");
            }
            println!("              打开：系统设置 → 隐私与安全性 → 辅助功能");
        }
        None => {
            if local_tap {
                println!("  辅助功能     ✅ 本进程已授权（daemon 未运行，无法确认它的状态）");
            } else {
                degraded = true;
                println!("  辅助功能     ⚠️  未授权：只能用 espanso shell 降级触发");
                println!("              打开：系统设置 → 隐私与安全性 → 辅助功能");
            }
        }
    }

    let config = Config::load();
    println!(
        "  配置         trigger=`{}` focus={:?} max={}",
        config.trigger, config.focus_mode, config.max_results
    );

    if problems == 0 && !degraded {
        println!("\n全部正常。");
        ExitCode::SUCCESS
    } else if problems == 0 {
        // 能跑，但不是最佳路径：`;;` 要靠 espanso 的 shell 变量绕一圈，
        // 多一次进程启动，也没有「吞键」带来的零残留。
        println!(
            "\n可用，但处于降级模式：授权辅助功能后 `{}` 才由 tap 直接接管。",
            Config::load().trigger
        );
        ExitCode::SUCCESS
    } else {
        println!("\n有 {problems} 项需要处理。");
        if degraded {
            println!("另有 1 项降级（辅助功能未授权）。");
        }
        ExitCode::FAILURE
    }
}

/// 同时写 stderr 与日志文件：以 .app 启动时没有终端，出问题只能靠文件回溯。
struct Tee(std::io::Stderr, Option<std::fs::File>);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(file) = self.1.as_mut() {
            let _ = file.write_all(buf);
        }
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = self.1.as_mut() {
            let _ = file.flush();
        }
        self.0.flush()
    }
}

/// 初始化日志：写文件，**同时**复制一份到 stderr。
///
/// stderr 这一路不需要任何"是否在终端"的判断：daemon 由 launchd 启动时标准流
/// 已被重定向到 /dev/null，等于静默；在终端里跑 `semisnip daemon` 时又能直接
/// 看见。两种场景用同一份代码就对。
///
/// **绝不写 stdout**：`trigger` 的 stdout 会被 espanso 当成展开内容，
/// 一条日志污染的就是用户文档。
///
/// 配置文件打不开（目录不可写）时 `file` 为 `None`，Tee 会退化成只写 stderr——
/// 拿不到日志总比起不来好，最多是排查时少一份证据。
pub fn init_logging() {
    paths::ensure_dirs();
    let path = paths::log_path();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(&path))
        .ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(Tee(
            std::io::stderr(),
            file,
        ))))
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_file_is_silent_by_contract() {
        let yaml = render_match_file("/Applications/Semisnip.app/Contents/MacOS/semisnip", ";;");
        assert!(
            yaml.contains("2>>/dev/null"),
            "命令必须吞掉 stdout 之外的噪声"
        );
        assert!(yaml.contains("{{output}}"));
        assert!(yaml.contains("trigger: \";;\""));
        // 能被 espanso 的 match schema 解析成合法 YAML
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert!(parsed.get("matches").is_some());
    }

    #[test]
    fn bare_invocation_asks_for_help_instead_of_daemon() {
        // 终端里裸敲：默认动作必须是"解释自己"，而不是偷偷起一个常驻进程。
        let err = Cli::try_parse_from(["semisnip"]).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert_eq!(classify(err.kind()), ExitCode::SUCCESS);
    }

    #[test]
    fn finder_launch_is_told_apart_from_a_bare_terminal_command() {
        // 唯一的区别是 LaunchServices 注入的 bundle id：双击 app 时是我们自己，
        // 终端里跑时是终端自己的。
        assert!(launched_from_bundle(1, Some(paths::BUNDLE_ID)));
        assert!(!launched_from_bundle(1, Some("com.googlecode.iterm2")));
        assert!(!launched_from_bundle(1, None));
        // 带子命令的永远是 CLI：LaunchAgent 的 `daemon`、espanso 的 `trigger`。
        assert!(!launched_from_bundle(2, Some(paths::BUNDLE_ID)));
    }

    #[test]
    fn help_and_version_are_success() {
        for args in [["semisnip", "--help"], ["semisnip", "--version"]] {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(classify(err.kind()), ExitCode::SUCCESS, "{args:?}");
        }
        let err = Cli::try_parse_from(["semisnip", "nope"]).unwrap_err();
        assert_eq!(classify(err.kind()), ExitCode::FAILURE);
    }

    #[test]
    fn trigger_defaults_to_manual_and_accepts_both_spellings() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["semisnip", "trigger"];
            args.extend_from_slice(extra);
            match Cli::try_parse_from(args).unwrap().command {
                Command::Trigger { origin } => origin,
                other => panic!("解析成了 {other:?}"),
            }
        };
        assert_eq!(parse(&[]), OriginArg::Manual);
        assert_eq!(parse(&["--origin", "espanso"]), OriginArg::Espanso);
        // 老脚本里写的是下划线版本，必须继续可用
        assert_eq!(parse(&["--origin", "espanso_shell"]), OriginArg::Espanso);
        assert_eq!(parse(&["--origin", "tap"]), OriginArg::Tap);
    }

    #[test]
    fn command_tree_is_usable_by_completion_generator() {
        // clap_complete 要求定义自洽（名字唯一、没有必填冲突），这里提前暴露
        command().debug_assert();
    }

    #[test]
    fn symlinked_exe_resolves_to_the_real_binary() {
        // 从 /usr/local/bin/semisnip（软链）运行时，写进 yml 的必须是 .app 里的
        // 真身，否则删掉软链降级触发就悄悄失效了。
        let dir = std::env::temp_dir().join("semisnip-resolve-exe");
        let _ = std::fs::create_dir_all(&dir);
        let real = dir.join("real-bin");
        std::fs::write(&real, b"#!/bin/sh\n").unwrap();

        let link = dir.join("link-bin");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            resolve_exe(link.clone()),
            real.canonicalize().unwrap(),
            "软链要解到真身"
        );
        // 不存在的路径原样返回，不 panic
        let ghost = dir.join("ghost");
        assert_eq!(resolve_exe(ghost.clone()), ghost);

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&real);
        let _ = std::fs::remove_dir(&dir);
    }
}
