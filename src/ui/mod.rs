//! 界面层：面板、偏好设置、首次引导。
//!
//! 三个窗口的"性格"不一样，别混着改：
//!
//! | 窗口 | 类型 | 抢焦点 | 谁打开 |
//! |---|---|---|---|
//! | 面板 `window.rs` / `view.rs` | `PopUp` + 非激活 | 否 | 触发键、菜单、CLI |
//! | 设置 `settings.rs` | `Normal` + 标题栏 | 是 | 菜单栏 |
//! | 引导 `onboarding.rs` | `Normal` + 标题栏 | 是 | 首次启动、菜单栏 |
//!
//! 面板不抢焦点是产品前提（回车之后不用还原前台）；设置和引导反过来——
//! 用户是特意来操作的，抢焦点才符合直觉。

pub mod onboarding;
pub mod settings;
pub mod view;
pub mod window;

use std::path::PathBuf;
use std::process::Command;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{App, Window, WindowAppearance};

/// 把组件库的主题对齐到系统外观。
///
/// gpui-component 默认停在 `ThemeMode::Light`，它**不会**跟着 macOS 的
/// 深色模式走。面板可以不管（浮层本来就压在半透明底上，颜色由
/// `theme.colors.popover` 决定），但设置和引导是用户会停留的普通窗口：
/// 深色系统里弹一个纯白窗口太刺眼，所以打开前同步一次。
///
/// 主题是全局的，同步之后面板下次打开也会跟着换色——这正好是想要的效果，
/// 不需要再单独处理。
pub fn sync_theme(window: &Window, cx: &mut App) {
    let mode = match window.appearance() {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
        // `VibrantLight` 也归到浅色：组件库没有"半透明浅色"这套 token，
        // 按浅色画比按深色画更接近系统观感。
        _ => ThemeMode::Light,
    };
    // 每次 `Theme::change` 都会重绘所有窗口，值没变就别白跑一趟。
    if cx.theme().mode != mode {
        Theme::change(mode, None, cx);
    }
}

/// 用系统默认程序打开文件；文件不存在时退而打开它所在的目录。
///
/// 菜单项和设置页的「打开配置 / 打开日志」共用这一份。点下去毫无反应是
/// 最难受的失败方式——比如用户还没跑过 `install`，`config.yml` 并不存在，
/// 这时打开配置目录至少能让他看见"这里确实是空的"。
pub fn reveal(target: PathBuf, fallback_dir: PathBuf) {
    let path = if target.exists() {
        target
    } else {
        log::info!(
            "{} 不存在，改开目录 {}",
            target.display(),
            fallback_dir.display()
        );
        fallback_dir
    };
    if let Err(err) = Command::new("open").arg(&path).spawn() {
        log::error!("打开 {} 失败：{err}", path.display());
    }
}

/// 打开「系统设置 → 隐私与安全性 → 辅助功能」。
///
/// 引导页和设置页都提供这个入口。没有辅助功能权限时 semisnip 会退化成
/// espanso shell 触发（能用，但每次都要等 espanso 启动），用户很少知道
/// 该去哪一页勾选，所以直接把那一页调出来。
pub fn open_accessibility_settings() {
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
    if let Err(err) = Command::new("open").arg(url).spawn() {
        log::error!("打开辅助功能设置失败：{err}");
    }
}
