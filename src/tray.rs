//! 菜单栏图标（NSStatusItem）。
//!
//! daemon 常驻但**不进 Dock**（见 `lib.rs` 里的 `ActivationPolicy::Accessory`），
//! 用户能看见它、能操作它的唯一界面就是这个图标。所以这里不做"点一下直接开
//! 面板"：面板的正路是 `;;`，菜单栏是管理入口（暂停、重启、看配置、看日志、退出）。
//!
//! 图标用**模板图**（`template(true)`）：macOS 只取 alpha 当遮罩再按当前外观
//! 上色，浅色菜单栏上自动变黑、深色上自动变白，不用我们自己判断外观。
//!
//! 暂停触发时会换成另一个图标（[`crate::mark::paused_rgba`]：标记压淡 + 一道
//! 斜杠）——和 espanso 一样，状态得能一眼看见，不能只藏在菜单里。

use std::cell::Cell;
use std::sync::mpsc::Sender;

use gpui_kit::{App, MenuItem};

use crate::app::event::{self, Event};
use crate::config::Config;
use crate::ipc::proto::Origin;
use crate::paths;

gpui_kit::actions!(
    semisnip_tray,
    [
        OpenPanel,
        ClosePanel,
        ToggleEnabled,
        ReloadIndex,
        Restart,
        OpenSettings,
        OpenGuide,
        OpenConfig,
        OpenMatchFile,
        OpenLog,
        Quit
    ]
);

/// 保住 `Tray` 的句柄。丢了它 NSStatusItem 会被移除，图标当场消失。
/// 除了「别被回收」，它还是刷新菜单的入口：[`refresh`]。
struct GlobalTray {
    tray: gpui_tray::Tray,
    /// 当前图标是不是「暂停」版式。
    ///
    /// 缓存这一位只为了**别在每次开合面板时都重传一张位图**（`refresh` 会被
    /// 面板开合触发，而图标只在暂停状态变化时才需要换）。`Cell` 够用：
    /// 全局只在 UI 线程访问。
    paused: Cell<bool>,
}

impl gpui_kit::Global for GlobalTray {}

/// 重建菜单。面板开合之后必须调一次，否则菜单第一项会停在旧文案上
/// （用户看着「打开片段面板」，点下去却发现面板已经开着）。
///
/// 菜单项文案依赖面板状态，所以菜单是「按需重建」而不是一次定死。
pub fn refresh(cx: &mut App) {
    let Some(tray) = cx.try_global::<GlobalTray>().map(|g| g.tray.clone()) else {
        return;
    };
    if let Err(err) = tray.refresh_menu(cx) {
        log::warn!("刷新菜单栏菜单失败：{err}");
    }
}

/// 把菜单栏的「状态外观」对齐到当前配置：暂停时换图标、改 tooltip、改菜单文案。
///
/// 只在 `enabled` 变了之后才有实际动作（图标是位图，换一次就要重建一张
/// NSImage；`refresh` 会被面板开合频繁触发，不该每帧都重传）。调用点只有
/// 配置变更这一处——装了图标之后的第一次由 [`install`] 负责。
pub fn sync(cx: &mut App) {
    let Some((tray, paused)) = cx
        .try_global::<GlobalTray>()
        .map(|global| (global.tray.clone(), global.paused.get()))
    else {
        return;
    };
    let enabled = enabled(cx);
    if paused == !enabled {
        refresh(cx);
        return;
    }

    match icon(!enabled) {
        Ok(image) => {
            if let Err(err) = tray.set_icon(image, cx) {
                log::warn!("切换菜单栏图标失败：{err}");
            }
        }
        Err(err) => log::error!("生成菜单栏图标失败：{err}"),
    }
    if let Err(err) = tray.set_tooltip(Some(tooltip(enabled, &trigger(cx))), cx) {
        log::warn!("更新菜单栏提示失败：{err}");
    }
    if let Some(global) = cx.try_global::<GlobalTray>() {
        global.paused.set(!enabled);
    }
    log::info!(
        "菜单栏图标已切换为{}",
        if enabled { "正常" } else { "暂停" }
    );
    refresh(cx);
}

/// 装菜单栏图标：注册动作 → 建 `NSStatusItem` → 把句柄塞进全局。
///
/// 顺序不能反：`register_actions` 必须在 `build` 之前，因为菜单项的点击
/// 是同步派发 GPUI action 的，handle 没注册就会"点了没反应"且没有任何报错。
///
/// 失败（图标画不出来、系统拒绝创建 status item）时返回 `Err` 让调用方决定：
/// `lib.rs` 只记日志不中止——面板靠 `;;` 仍然可用，菜单栏只是备用入口。
pub fn install(cx: &mut App, tx: Sender<Event>) -> anyhow::Result<()> {
    register_actions(cx, tx);

    // 直接读盘而不是要一份配置传进来：`lib.rs` 刚刚用同一份文件装过 tap，
    // 这里再读一次得到的是同一个值，代价只是一次小文件读取。
    let config = Config::load();
    let paused = !config.enabled;
    let tray = gpui_tray::Tray::builder()
        .icon(icon(paused)?)
        .tooltip(tooltip(config.enabled, &config.trigger))
        .menu(menu)
        .build(cx)?;

    cx.set_global(GlobalTray {
        tray,
        paused: Cell::new(paused),
    });
    log::info!("菜单栏图标已就绪{}", if paused { "（暂停中）" } else { "" });
    Ok(())
}

fn menu(cx: &mut App) -> Vec<MenuItem> {
    menu_items(panel_open(cx), enabled(cx), &trigger(cx))
}

/// 触发串此刻是否生效。
///
/// 读 daemon 内存里的那份配置：菜单栏与设置窗口改完都会走
/// `ConfigChanged → AppState::reload_config`，所以它比磁盘更新。
/// 拿不到全局（单测、极端装配错误）就退回磁盘——菜单宁可慢半拍，
/// 也不要画出一个和实际状态相反的开关键。
fn enabled(cx: &App) -> bool {
    cx.try_global::<crate::app::state::GlobalApp>()
        .map(|global| global.0.read(cx).config.enabled)
        .unwrap_or_else(|| Config::load().enabled)
}

/// 当前触发串。菜单文案要带它（「暂停触发 `;;`」），否则用户不知道暂停的是什么。
fn trigger(cx: &App) -> String {
    cx.try_global::<crate::app::state::GlobalApp>()
        .map(|global| global.0.read(cx).config.trigger.clone())
        .unwrap_or_else(|| Config::load().trigger)
}

/// 面板此刻是否开着。菜单文案要用，但拿不到 `Entity` 就按「没开」处理——
/// 菜单栏比面板更早存在（install 时面板一定是关的）。
///
/// 判据是**窗口句柄**而不是 `state.open`：用户看见的是窗口，菜单跟着窗口走
/// 才不会出现「写着打开、点下去没反应」。句柄在 `window::open` 里先落、
/// 在 `window::close` 里先摘，所以两处刷新时读到的都是最终状态。
fn panel_open(cx: &App) -> bool {
    cx.try_global::<crate::app::state::GlobalApp>()
        .map(|global| global.0.read(cx).handle.is_some())
        .unwrap_or(false)
}

/// 菜单内容。第一项随面板状态切换：**这是没有键盘时的正规出口**。
///
/// 面板是 `NonactivatingPanel`，只要键盘一路（CGEventTap）断了——没给
/// 「辅助功能」权限、或重签之后授权失效——面板就收不到 `Esc`。这时从菜单栏
/// 原路点「关闭片段面板」是唯一不依赖权限的收尾方式，所以它必须存在。
///
/// 「暂停 / 恢复触发」排在窗口项之前：它是这个程序自己的总开关，改完立刻
/// 想看到效果（图标变样），不该藏在"打开日志"后面。
fn menu_items(panel_open: bool, enabled: bool, trigger: &str) -> Vec<MenuItem> {
    let first = if panel_open {
        MenuItem::action("关闭片段面板", ClosePanel)
    } else {
        MenuItem::action("打开片段面板", OpenPanel)
    };
    // 文案直接写状态，不用勾选标记：菜单栏图标已经在表达"暂停/正常"，
    // 文案再写一遍「暂停触发」是给"正在找它的人"看的，动词比勾选更直白。
    let toggle = if enabled {
        MenuItem::action(format!("暂停触发 {trigger}"), ToggleEnabled)
    } else {
        MenuItem::action(format!("恢复触发 {trigger}"), ToggleEnabled)
    };
    vec![
        first,
        MenuItem::separator(),
        toggle,
        MenuItem::action("偏好设置…", OpenSettings),
        MenuItem::action("使用引导…", OpenGuide),
        MenuItem::separator(),
        MenuItem::action("重建索引", ReloadIndex),
        MenuItem::separator(),
        MenuItem::action("打开配置…", OpenConfig),
        MenuItem::action("打开 semisnip.yml…", OpenMatchFile),
        MenuItem::action("打开日志…", OpenLog),
        MenuItem::separator(),
        // 重启和退出都是"进程生命周期"的事，放在同一组、挨着退出：
        // 一个重来、一个停掉，用户的预期在同一处。
        MenuItem::action("重启 semisnip", Restart),
        MenuItem::action("退出 semisnip", Quit),
    ]
}

/// 悬停在菜单栏图标上时显示的提示。
fn tooltip(enabled: bool, trigger: &str) -> String {
    if enabled {
        format!("semisnip — 输入 {trigger} 选片段")
    } else {
        format!("semisnip — 已暂停（{trigger} 不触发），菜单里可恢复")
    }
}

/// 暂停 / 恢复触发：写盘 → 投事件。
///
/// 不在这里直接改 tap：设置窗口里的同一个开关走的是 `ConfigChanged`，
/// 两个入口各改一次的话"谁是真相"迟早分叉——磁盘上的 `config.yml` 才是。
/// scheduler 收到事件后会 `reload_config`（顺带通知 tap）再回过头来
/// [`sync`] 这个菜单栏，所以图标与文案也在这里一起变。
fn toggle_enabled(cx: &mut App) {
    let mut config = Config::load();
    config.enabled = !config.enabled;
    if let Err(err) = config.save() {
        log::error!("切换「启用触发」失败：{err}");
        return;
    }
    log::info!("触发已{}", if config.enabled { "恢复" } else { "暂停" });
    match event::tx(cx) {
        Some(tx) => {
            let _ = tx.send(Event::ConfigChanged);
        }
        // 没有事件通道理论上只出现在单测里：至少把菜单和图标对齐到新状态。
        None => sync(cx),
    }
}

/// 重启 daemon。
///
/// 走 `launchctl kickstart -k` 而不是自己 `cx.quit()` 再拉起：
///
/// - plist 里 `KeepAlive.SuccessfulExit = false`——正常退出（0）launchd **不会**
///   重启，自己退就等于"点了重启，进程没了"；
/// - `kickstart -k` 由 launchd 先给我们发信号、再按 plist 原样拉起新实例，
///   argv、环境变量、KeepAlive 策略与开机启动完全一致。
///
/// 先投一条 `Cancel` 把面板收起来：新进程是干净的，不该在屏幕上留一个旧的
/// 吞键窗口。`launchctl` 是个子进程，等它返回的这段时间足够 scheduler 走完
/// 一轮事件循环。
pub(crate) fn restart(cx: &mut App) {
    if let Some(tx) = event::tx(cx) {
        let _ = tx.send(Event::Cancel);
    }
    match kickstart() {
        // 成功之后什么都不做：接下来 launchd 会终止本进程（`-k` 的含义），
        // 再由它拉起新实例。这时候再调 `cx.quit()` 反而会和"新实例启动"抢
        // socket——我们退出得越晚，新实例撞上单实例检查的概率越大。
        Ok(()) => log::info!("已请求 launchd 重启 daemon"),
        Err(err) => {
            // 不是 launchd 拉起的（`cargo run`、终端里的 `semisnip daemon`），
            // kickstart 找不到服务。这时只能自己退出，让一个子进程等 socket
            // 释放之后再把自己 exec 回来。
            log::warn!("launchctl 重启不可用（{err}），改为退出后重新拉起");
            if let Err(err) = respawn_after_exit() {
                log::error!("重新拉起失败：{err}");
                return;
            }
            if let Some(tx) = event::tx(cx) {
                let _ = tx.send(Event::Shutdown);
            }
        }
    }
}

/// `launchctl kickstart -k gui/<uid>/<label>`。
///
/// 必须带 `-k`：不带的话"已经在跑"就什么都不做，用户点了重启毫无反应。
fn kickstart() -> anyhow::Result<()> {
    let target = format!("gui/{}/{}", uid()?, paths::LABEL);
    let out = std::process::Command::new("/bin/launchctl")
        .args(["kickstart", "-k", &target])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "launchctl kickstart 失败（{}）：{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// 当前用户的 uid（launchd 的 `gui/<uid>/` 域）。
///
/// 为了一个 `getuid()` 引入 `libc` 不划算：重启是手动、低频的操作，
/// 起一个 `id -u` 子进程（几毫秒）完全可以接受。
fn uid() -> anyhow::Result<String> {
    let out = std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()?;
    if !out.status.success() {
        anyhow::bail!("id -u 退出码 {}", out.status);
    }
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        anyhow::bail!("id -u 没有输出");
    }
    Ok(uid)
}

/// 非 launchd 场景的兜底重启：起一个 `sh`，等我们退出、socket 放掉之后
/// 再 `exec` 自己。
///
/// 为什么要等：单实例是靠 UDS `bind` 抢的，新进程早一步启动会撞上
/// `AlreadyRunning` 直接以 0 退出——用户看到的是"点了重启，daemon 就没了"。
/// 子进程在我们退出之后会被 launchd 接管（reparent），不会跟着一起死，
/// 所以这里不需要 `nohup` / `setsid`。
fn respawn_after_exit() -> anyhow::Result<()> {
    // 解开软链再交给子进程：`current_exe` 可能是 `/usr/local/bin/semisnip`，
    // 软链哪天被删，拉起来的就还是它自己（和 `cli::write_match_file` 同一条理由）。
    let exe = std::env::current_exe()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    std::process::Command::new("/bin/sh")
        // `$0` 是下面那个额外参数（exe 路径）——`sh -c script name` 的约定。
        // 不重定向 stdout/stderr：这条路径多半是开发时 `cargo run` 起的，
        // 让新进程继续往终端/日志里写，比丢进 /dev/null 有用。
        .arg("-c")
        .arg(r#"sleep 0.8; exec "$0" daemon"#)
        .arg(exe)
        .spawn()?;
    Ok(())
}

fn register_actions(cx: &mut App, tx: Sender<Event>) {
    // 面板与索引都归 scheduler 管，这里只投事件，不直接碰 AppState——
    // 免得和 tap / espanso 两条路径抢同一个状态。
    let open = tx.clone();
    cx.on_action(move |_: &OpenPanel, _: &mut App| {
        let _ = open.send(Event::Open {
            origin: Origin::Manual,
            erase: 0,
        });
    });

    // 与按 Esc 完全同一条路：Cancel 会让 scheduler 放开吞键、删窗口、
    // 把触发词补回文档。这里绝不能自己关窗，否则收尾逻辑会分叉。
    let close = tx.clone();
    cx.on_action(move |_: &ClosePanel, _: &mut App| {
        let _ = close.send(Event::Cancel);
    });

    let reload = tx.clone();
    cx.on_action(move |_: &ReloadIndex, _: &mut App| {
        let _ = reload.send(Event::Reload);
    });

    // 暂停/恢复与重启都直接在本线程处理：前者只改配置（再投 ConfigChanged），
    // 后者要起 `launchctl` 子进程，都和面板的开合状态无关。
    cx.on_action(|_: &ToggleEnabled, cx: &mut App| toggle_enabled(cx));
    cx.on_action(|_: &Restart, cx: &mut App| restart(cx));

    let quit = tx;
    cx.on_action(move |_: &Quit, _: &mut App| {
        // 走 Shutdown 而不是直接 quit：scheduler 会先关面板、清掉 tap 状态。
        let _ = quit.send(Event::Shutdown);
    });

    // 两个窗口直接开，不走事件通道：它们的生命周期与面板无关，
    // 也不需要 tap 的 `panel_open` 状态。
    cx.on_action(|_: &OpenSettings, cx: &mut App| {
        if let Err(err) = crate::ui::settings::open(cx) {
            log::error!("打开设置失败：{err}");
        }
    });
    cx.on_action(|_: &OpenGuide, cx: &mut App| {
        if let Err(err) = crate::ui::onboarding::open(cx) {
            log::error!("打开引导失败：{err}");
        }
    });

    // 这三个只是"用系统默认程序打开文件"，不值得绕一圈事件循环。
    cx.on_action(|_: &OpenConfig, _: &mut App| {
        crate::ui::reveal(crate::paths::config_path(), crate::paths::config_dir());
    });
    cx.on_action(|_: &OpenMatchFile, _: &mut App| {
        crate::ui::reveal(
            crate::paths::espanso_match_file(),
            crate::paths::espanso_match_dir(),
        );
    });
    cx.on_action(|_: &OpenLog, _: &mut App| {
        crate::ui::reveal(crate::paths::log_path(), crate::paths::config_dir());
    });
}

/// 菜单栏里占的高度（pt）与 retina 倍数。宽度不写死，按标记的宽高比推。
const HEIGHT: u32 = 15;
const SCALE: u32 = 2;

/// 菜单栏图标：和 Dock 里那个是同一个标记（`src/mark.rs` 是唯一来源），
/// 只是更小、且走模板图。
///
/// 之前这里优先用系统符号 `text.cursor`，现在不用了：那是「光标」而不是
/// 「semisnip」。应用图标已经变成「I;;」，菜单栏必须跟着变，否则同一个程序
/// 在两处长得像两个东西。
///
/// `paused` 为真时换成「压淡 + 划一道斜杠」的版式（见 `mark::paused_rgba`）：
/// 模板图只有 alpha，颜色由菜单栏明暗决定，所以状态只能靠"变淡/划掉"表达。
fn icon(paused: bool) -> anyhow::Result<gpui_tray::Icon> {
    let w = (HEIGHT as f32 * crate::mark::aspect()).round() as u32 * SCALE;
    let h = HEIGHT * SCALE;
    let rgba = if paused {
        crate::mark::paused_rgba(w, h, 0.02)
    } else {
        crate::mark::template_rgba(w, h, 0.02)
    };
    Ok(gpui_tray::Icon::from_rgba(rgba, w, h)
        .scale_factor(f64::from(SCALE))
        .template(true)
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_bitmap_covers_the_mark_but_not_the_corners() {
        // 尺寸按 `icon()` 的算法推，保证测的就是真正塞给 NSStatusItem 的那张图
        let w = (HEIGHT as f32 * crate::mark::aspect()).round() as u32 * SCALE;
        let h = HEIGHT * SCALE;
        let rgba = crate::mark::template_rgba(w, h, 0.02);
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        let alpha_at = |x: u32, y: u32| rgba[((y * w + x) * 4 + 3) as usize];

        // 四角必须透明，否则菜单栏上会多出一块若有若无的底
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(alpha_at(x, y), 0, "角 ({x},{y}) 不透明");
        }

        // 斜体的竖杠是唯一跨越大半高度的笔画：找最实的那一列，它必须够长。
        // 不能用 w/2 —— 标记的中心正好落在「I」和分号之间的空隙里。
        let tallest = (0..w)
            .map(|x| (0..h).filter(|&y| alpha_at(x, y) > 200).count())
            .max()
            .unwrap();
        assert!(
            tallest as u32 > h / 2,
            "没有一列够实心，竖杠可能没画出来：最高 {tallest}/{h}"
        );
    }

    #[test]
    fn menu_entries_are_well_formed() {
        // 菜单里每一项都得是已定义的动作，否则点下去没反应
        let items = menu_items(false, true, ";;");
        assert_eq!(items.len(), 14);
        assert!(matches!(items[0], MenuItem::Action { .. }));
        // 分组线：打开 | 开关 + 两个窗口 | 重建 | 三个"打开…" | 重启 + 退出
        for index in [1, 5, 7, 11] {
            assert!(
                matches!(items[index], MenuItem::Separator),
                "第 {index} 项应是分隔线"
            );
        }
        assert!(matches!(items[13], MenuItem::Action { .. }));
    }

    #[test]
    fn first_item_toggles_with_the_panel() {
        // 面板开着时第一项必须是「关闭」：键盘坏了（没授权 / 重签后授权失效）
        // 的时候，菜单是唯一能把面板收起来的地方，文案错了就等于没有出口。
        let name = |items: Vec<MenuItem>| match &items[0] {
            MenuItem::Action { name, .. } => name.to_string(),
            _ => panic!("第一项应是动作"),
        };
        assert_eq!(name(menu_items(false, true, ";;")), "打开片段面板");
        assert_eq!(name(menu_items(true, true, ";;")), "关闭片段面板");
        // 除第一项外，两种状态下菜单结构完全一致
        assert_eq!(
            menu_items(false, true, ";;").len(),
            menu_items(true, true, ";;").len()
        );
    }

    #[test]
    fn pause_entry_follows_the_switch_and_names_the_trigger() {
        // 文案里必须带触发串：用户改过 `trigger` 之后，菜单还写 `;;` 会让人
        // 以为暂停的是另一个东西。
        let label = |items: Vec<MenuItem>| match &items[2] {
            MenuItem::Action { name, .. } => name.to_string(),
            _ => panic!("第三项应是「暂停/恢复」动作"),
        };
        assert_eq!(label(menu_items(false, true, ";;")), "暂停触发 ;;");
        assert_eq!(label(menu_items(false, false, ";;")), "恢复触发 ;;");
        assert_eq!(label(menu_items(false, true, ",,")), "暂停触发 ,,");
    }

    #[test]
    fn tooltip_says_whether_it_is_paused() {
        assert!(tooltip(true, ";;").contains(";;"));
        assert!(!tooltip(true, ";;").contains("暂停"));
        // 暂停时用户最容易怀疑"程序是不是挂了"，提示里要给出恢复路径
        let paused = tooltip(false, ";;");
        assert!(paused.contains("暂停"), "{paused}");
        assert!(paused.contains("恢复"), "{paused}");
    }
}
