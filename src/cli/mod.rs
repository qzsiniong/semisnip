//! 子命令入口。
//!
//! 只有 `daemon` 会初始化 gpui；其余子命令必须是"秒退、不打印 stdout"的
//! 轻量进程——`trigger` 会被 espanso 的 shell 变量同步调用。

mod config_cmd;
mod log_cmd;
mod uninstall;

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use crate::config::{self, Config};
use crate::espanso::cli::EspansoCli;
use crate::espanso::paths;
use crate::index::loader::{SnippetSource, YamlSource};
use crate::ipc::{client, proto::Origin};

const USAGE: &str = "\
snipshot — espanso 的片段检索层

用法:
  snipshot                       启动 daemon（默认）
  snipshot daemon                同上
  snipshot trigger [--origin O]  请求打开面板（espanso 的 shell 变量会调它）
  snipshot reload                重建索引
  snipshot install               写入 espanso 触发定义并给出后续步骤
  snipshot uninstall [--yes]     卸载：演练删什么，--yes 真删，--purge 连配置一起
  snipshot config [子命令]        查看/修改配置（path get set unset reset keys edit）
  snipshot log [选项]            看日志（-n 行数  -f 跟随  --grep 关键词  --level 级别）
  snipshot doctor                体检：espanso / 权限 / 索引 / daemon
  snipshot type <text>           调试：合成字符输入
  snipshot key <esc|enter|up|down>  调试：合成命名按键

子命令的详细用法：snipshot <子命令> --help
";

pub fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("daemon");

    match command {
        "daemon" | "" => {
            crate::run_daemon();
            ExitCode::SUCCESS
        }
        "trigger" => trigger(&args[1..]),
        "reload" => match client::reload() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(&format!("reload 失败：{err}")),
        },
        // 调试用：合成按键（从 CLI 进程发出，source pid 与 daemon 不同，
        // 因此 tap 会当作真实输入）。没有辅助功能权限时也能冒烟测交互。
        "type" => {
            crate::handoff::inject::type_text(&args[1..].join(" "));
            ExitCode::SUCCESS
        }
        "key" => {
            for name in &args[1..] {
                crate::handoff::inject::press_named(name);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            ExitCode::SUCCESS
        }
        "install" => install(),
        "uninstall" => uninstall::run(&args[1..]),
        "config" => config_cmd::run(&args[1..]),
        "log" => log_cmd::run(&args[1..]),
        "doctor" => doctor(),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "--version" | "version" => {
            println!("snipshot {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("未知子命令：{other}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("snipshot: {message}");
    ExitCode::FAILURE
}

fn trigger(args: &[String]) -> ExitCode {
    let origin = match args {
        [] => Origin::Manual,
        [flag, value] if flag == "--origin" => match value.as_str() {
            "espanso" | "espanso_shell" => Origin::EspansoShell,
            "tap" => Origin::Tap,
            _ => Origin::Manual,
        },
        _ => Origin::Manual,
    };

    match client::open(origin, ";;") {
        // 关键：成功时绝不能往 stdout 写任何东西——espanso 会把 stdout
        // 当作展开内容注入，多一个字节就会污染用户文档。
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(&format!("{err}")),
    }
}

fn install() -> ExitCode {
    config::ensure_dirs();
    let config = Config::load();
    let _ = config.save();

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return fail(&format!("无法定位自身路径：{err}")),
    };

    let match_dir = paths::match_dir();
    if let Err(err) = std::fs::create_dir_all(&match_dir) {
        return fail(&format!("创建 {} 失败：{err}", match_dir.display()));
    }

    let target = match_dir.join("snipshot.yml");
    if target.exists() {
        let backup = match_dir.join("snipshot.yml.bak");
        if let Err(err) = std::fs::copy(&target, &backup) {
            return fail(&format!("备份已有 snipshot.yml 失败：{err}"));
        }
        println!("已备份原文件到 {}", backup.display());
    }

    let body = render_match_file(&exe.display().to_string(), &config.trigger);
    if let Err(err) = std::fs::write(&target, body) {
        return fail(&format!("写入 {} 失败：{err}", target.display()));
    }

    println!("已写入 {}", target.display());
    println!();
    println!("下一步：");
    println!("  1. espanso restart        （让新 match 生效）");
    println!("  2. 把 Snipshot.app 加进「系统设置 → 隐私与安全性 → 辅助功能」");
    println!(
        "     —— 没有这项权限时，`{}` 会退化成 espanso shell 触发",
        config.trigger
    );
    println!("  3. 跑 `snipshot doctor` 确认四态正常");
    // 二进制藏在 .app 里，没软链的话 shell 找不到命令，这一步经常被漏掉。
    if !std::path::Path::new("/usr/local/bin/snipshot").exists() {
        println!();
        println!("提示：`snipshot` 还不在 PATH 上，补一条软链后命令才可用：");
        println!("  sudo ln -sf {} /usr/local/bin/snipshot", exe.display());
    }
    ExitCode::SUCCESS
}

/// 生成降级路径的 match 定义：tap 不可用时由 espanso 唤醒 snipshot。
pub fn render_match_file(exe: &str, trigger: &str) -> String {
    format!(
        r#"# 由 `snipshot install` 生成。
#
# 作用：当 snipshot 的键盘 tap 不可用（缺辅助功能权限）时，
# 由 espanso 的 shell 扩展唤醒面板。tap 正常工作时，`{trigger}` 的最后一个键
# 已被 snipshot 吞掉，espanso 根本看不到这个 trigger，本文件不会生效。
#
# 注意：shell 变量的 stdout 会被原样注入文档，所以命令必须保持静默。
matches:
  - trigger: "{trigger}"
    replace: "{{{{output}}}}"
    label: "snipshot · 打开片段面板（降级触发）"
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

fn doctor() -> ExitCode {
    let mut problems = 0;

    println!("snipshot {}", env!("CARGO_PKG_VERSION"));

    let cli = EspansoCli::discover();
    match cli.version() {
        Some(version) => println!("  espanso      ✅ {version} ({})", cli.bin().display()),
        None => {
            problems += 1;
            println!("  espanso      ❌ 找不到可执行的 espanso");
        }
    }

    let root = paths::config_root();
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

    let socket = config::socket_path();
    let alive = client::ping().is_ok();
    println!(
        "  daemon       {} {}",
        if alive { "✅" } else { "❌" },
        if alive {
            format!("运行中（{}）", socket.display())
        } else {
            "未运行：执行 `snipshot daemon` 或让 LaunchAgent 拉起".to_string()
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

pub fn init_logging() {
    config::ensure_dirs();
    let path = config::log_path();
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
        let yaml = render_match_file("/Applications/Snipshot.app/Contents/MacOS/snipshot", ";;");
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
}
