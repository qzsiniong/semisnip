//! 菜单栏图标（NSStatusItem）。
//!
//! daemon 常驻但**不进 Dock**（见 `lib.rs` 里的 `ActivationPolicy::Accessory`），
//! 用户能看见它、能操作它的唯一界面就是这个图标。所以这里不做"点一下直接开
//! 面板"：面板的正路是 `;;`，菜单栏是管理入口（重建、看配置、看日志、退出）。
//!
//! 图标用**模板图**（`template(true)`）：macOS 只取 alpha 当遮罩再按当前外观
//! 上色，浅色菜单栏上自动变黑、深色上自动变白，不用我们自己判断外观。

use std::process::Command;
use std::sync::mpsc::Sender;

use gpui_kit::{App, MenuItem};

use crate::app::event::Event;
use crate::ipc::proto::Origin;

gpui_kit::actions!(
    snipshot_tray,
    [OpenPanel, ReloadIndex, OpenConfig, OpenMatchFile, OpenLog, Quit]
);

/// 保住 `Tray` 的句柄。丢了它 NSStatusItem 会被移除，图标当场消失。
/// 字段从不读取，`_tray` 的下划线就是这个意思——它的价值在于"仍然存在"。
struct GlobalTray {
    _tray: gpui_tray::Tray,
}

impl gpui_kit::Global for GlobalTray {}

pub fn install(cx: &mut App, tx: Sender<Event>) -> anyhow::Result<()> {
    register_actions(cx, tx);

    let tray = gpui_tray::Tray::builder()
        .icon(icon()?)
        .tooltip("snipshot — 输入 ;; 选片段")
        .menu(menu)
        .build(cx)?;

    cx.set_global(GlobalTray { _tray: tray });
    log::info!("菜单栏图标已就绪");
    Ok(())
}

fn menu(_cx: &mut App) -> Vec<MenuItem> {
    menu_items()
}

fn menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem::action("打开片段面板", OpenPanel),
        MenuItem::separator(),
        MenuItem::action("重建索引", ReloadIndex),
        MenuItem::separator(),
        MenuItem::action("打开配置…", OpenConfig),
        MenuItem::action("打开 snipshot.yml…", OpenMatchFile),
        MenuItem::action("打开日志…", OpenLog),
        MenuItem::separator(),
        MenuItem::action("退出 snipshot", Quit),
    ]
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

    let reload = tx.clone();
    cx.on_action(move |_: &ReloadIndex, _: &mut App| {
        let _ = reload.send(Event::Reload);
    });

    let quit = tx;
    cx.on_action(move |_: &Quit, _: &mut App| {
        // 走 Shutdown 而不是直接 quit：scheduler 会先关面板、清掉 tap 状态。
        let _ = quit.send(Event::Shutdown);
    });

    // 这三个只是"用系统默认程序打开文件"，不值得绕一圈事件循环。
    cx.on_action(|_: &OpenConfig, _: &mut App| {
        reveal(crate::config::config_path(), crate::config::support_dir());
    });
    cx.on_action(|_: &OpenMatchFile, _: &mut App| {
        reveal(crate::espanso::paths::match_file(), crate::espanso::paths::match_dir());
    });
    cx.on_action(|_: &OpenLog, _: &mut App| {
        reveal(crate::config::log_path(), crate::config::support_dir());
    });
}

/// 文件不存在时退而打开所在目录：菜单点下去毫无反应是最难受的。
fn reveal(target: std::path::PathBuf, fallback_dir: std::path::PathBuf) {
    let path = if target.exists() {
        target
    } else {
        log::info!("{} 不存在，改开目录 {}", target.display(), fallback_dir.display());
        fallback_dir
    };
    if let Err(err) = Command::new("open").arg(&path).spawn() {
        log::error!("打开 {} 失败：{err}", path.display());
    }
}

/// 菜单栏里占的高度（pt）与 retina 倍数。宽度不写死，按标记的宽高比推。
const HEIGHT: u32 = 15;
const SCALE: u32 = 2;

/// 菜单栏图标：和 Dock 里那个是同一个标记（`src/mark.rs` 是唯一来源），
/// 只是更小、且走模板图。
///
/// 之前这里优先用系统符号 `text.cursor`，现在不用了：那是「光标」而不是
/// 「snipshot」。应用图标已经变成「I;;」，菜单栏必须跟着变，否则同一个程序
/// 在两处长得像两个东西。
fn icon() -> anyhow::Result<gpui_tray::Icon> {
    let w = (HEIGHT as f32 * crate::mark::aspect()).round() as u32 * SCALE;
    let h = HEIGHT * SCALE;
    Ok(
        gpui_tray::Icon::from_rgba(crate::mark::template_rgba(w, h, 0.02), w, h)
            .scale_factor(f64::from(SCALE))
            .template(true)
            .build()?,
    )
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
        let items = menu_items();
        assert_eq!(items.len(), 9);
        assert!(matches!(items[0], MenuItem::Action { .. }));
        // 分组线：打开 | 重建 | 三个"打开…" | 退出
        for index in [1, 3, 7] {
            assert!(matches!(items[index], MenuItem::Separator), "第 {index} 项应是分隔线");
        }
        assert!(matches!(items[8], MenuItem::Action { .. }));
    }
}
