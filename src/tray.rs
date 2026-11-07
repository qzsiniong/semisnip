//! 菜单栏图标（NSStatusItem）。
//!
//! daemon 常驻但**不进 Dock**（见 `lib.rs` 里的 `ActivationPolicy::Accessory`），
//! 用户能看见它、能操作它的唯一界面就是这个图标。所以这里不做"点一下直接开
//! 面板"：面板的正路是 `;;`，菜单栏是管理入口（重建、看配置、看日志、退出）。
//!
//! 图标用**模板图**（`template(true)`）：macOS 只取 alpha 当遮罩再按当前外观
//! 上色，浅色菜单栏上自动变黑、深色上自动变白，不用我们自己判断外观。

use std::sync::mpsc::Sender;

use gpui_kit::{App, MenuItem};

use crate::app::event::Event;
use crate::ipc::proto::Origin;

gpui_kit::actions!(
    semisnip_tray,
    [
        OpenPanel,
        ClosePanel,
        ReloadIndex,
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

pub fn install(cx: &mut App, tx: Sender<Event>) -> anyhow::Result<()> {
    register_actions(cx, tx);

    let tray = gpui_tray::Tray::builder()
        .icon(icon()?)
        .tooltip("semisnip — 输入 ;; 选片段")
        .menu(menu)
        .build(cx)?;

    cx.set_global(GlobalTray { tray });
    log::info!("菜单栏图标已就绪");
    Ok(())
}

fn menu(cx: &mut App) -> Vec<MenuItem> {
    menu_items(panel_open(cx))
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
fn menu_items(panel_open: bool) -> Vec<MenuItem> {
    let first = if panel_open {
        MenuItem::action("关闭片段面板", ClosePanel)
    } else {
        MenuItem::action("打开片段面板", OpenPanel)
    };
    vec![
        first,
        MenuItem::separator(),
        MenuItem::action("偏好设置…", OpenSettings),
        MenuItem::action("使用引导…", OpenGuide),
        MenuItem::separator(),
        MenuItem::action("重建索引", ReloadIndex),
        MenuItem::separator(),
        MenuItem::action("打开配置…", OpenConfig),
        MenuItem::action("打开 semisnip.yml…", OpenMatchFile),
        MenuItem::action("打开日志…", OpenLog),
        MenuItem::separator(),
        MenuItem::action("退出 semisnip", Quit),
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

    // 与按 Esc 完全同一条路：Cancel 会让 scheduler 放开吞键、删窗口、
    // 必要时回删触发符。这里绝不能自己关窗，否则收尾逻辑会分叉。
    let close = tx.clone();
    cx.on_action(move |_: &ClosePanel, _: &mut App| {
        let _ = close.send(Event::Cancel);
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
        let items = menu_items(false);
        assert_eq!(items.len(), 12);
        assert!(matches!(items[0], MenuItem::Action { .. }));
        // 分组线：打开 | 两个窗口 | 重建 | 三个"打开…" | 退出
        for index in [1, 4, 6, 10] {
            assert!(
                matches!(items[index], MenuItem::Separator),
                "第 {index} 项应是分隔线"
            );
        }
        assert!(matches!(items[11], MenuItem::Action { .. }));
    }

    #[test]
    fn first_item_toggles_with_the_panel() {
        // 面板开着时第一项必须是「关闭」：键盘坏了（没授权 / 重签后授权失效）
        // 的时候，菜单是唯一能把面板收起来的地方，文案错了就等于没有出口。
        let name = |items: Vec<MenuItem>| match &items[0] {
            MenuItem::Action { name, .. } => name.to_string(),
            _ => panic!("第一项应是动作"),
        };
        assert_eq!(name(menu_items(false)), "打开片段面板");
        assert_eq!(name(menu_items(true)), "关闭片段面板");
        // 除第一项外，两种状态下菜单结构完全一致
        assert_eq!(menu_items(false).len(), menu_items(true).len());
    }
}
