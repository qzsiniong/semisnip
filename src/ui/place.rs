//! 面板定位：把「插入点在哪儿」翻译成一个窗口矩形。
//!
//! 两件事分开做，因为它们的可靠程度差得远：
//!
//! - **选哪块屏**：问 [`crate::platform`] 拿全局几何，用插入点（自动）或前台
//!   窗口中心（居中）去命中。多屏时这一步最关键——选错屏，面板就跑到另一块
//!   显示器上去了，用户会以为"没打开"。
//! - **屏内摆哪儿**：纯算术，[`place_auto`] / [`place_center`] 两个函数，
//!   不碰 gpui，可以单测。
//!
//! 坐标系有两套，换算只发生在这里：AX / CGDisplayBounds 给的是**全局**坐标
//! （主屏左上角为原点、y 向下、单位点），而 gpui 的 `WindowBounds::Windowed`
//! 要的是**选定屏幕的本地**坐标（该屏左上角为原点）。两者只差一个屏幕原点，
//! 但这个原点在副屏上不是 (0,0)，忘了减就正好偏出去一整块屏。
//!
//! 参考了 `~/code/labs/snippets` 的 `place()`：那边是输入法式贴靠（左缘对齐
//! 插入点、下方放不下翻上方、按可用高度收缩行数）。这里不需要收缩行数
//! （面板高度是固定配置），但保留了"选屏 → 水平对齐光标 → 垂直贴靠 → 夹进
//! 可视区"的骨架。

use gpui_kit::{App, Bounds, DisplayId, Pixels, Size, WindowBounds, point, px, size};

use crate::config::{Config, Position};
use crate::platform::{self, CaretAnchor, ScreenRect};

/// 面板与插入点之间的垂直间距。
///
/// 6 点是"明显不粘在一起、又不显得断开"的经验值：再小会和下一行文字的
/// 上沿打架，再大就离光标太远、看不出是从这儿弹出来的。
const GAP: f32 = 6.0;

/// 面板与屏幕可视区边缘（菜单栏 / 程序坞之外）的最小距离。
const MARGIN: f32 = 8.0;

/// 定位结果。
pub struct Placement {
    /// 交给 `WindowOptions.window_bounds`，坐标是**屏幕本地**的。
    pub window_bounds: WindowBounds,
    /// 交给 `WindowOptions.display_id`：决定窗口出现在哪块屏上。
    pub display_id: Option<DisplayId>,
    /// 选定屏幕的全局原点。
    ///
    /// gpui 的 `Window::bounds()` 返回屏幕本地坐标，而 `trigger::tap` 判"点面板
    /// 里面还是外面"用的是全局坐标；把原点一起带出来，才能在
    /// `ui::window` 里把两者对上。
    pub screen_origin: (f32, f32),
    /// 一句话说明这次怎么定的位，写进日志。
    pub note: String,
}

/// 屏幕可视区（屏幕本地坐标，y 向下）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Area {
    fn right(&self) -> f32 {
        self.x + self.w
    }

    fn bottom(&self) -> f32 {
        self.y + self.h
    }

    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// 计算面板该出现在哪儿。
///
/// 任何一步拿不到信息都只降级、不失败：拿不到屏幕列表 → 交回 gpui 的
/// `centered`；拿不到插入点 → 居中；屏幕本地尺寸异常 → 夹进可视区。
pub fn compute(cx: &App, config: &Config, panel: Size<Pixels>) -> Placement {
    let (width, height) = (panel.width.as_f32(), panel.height.as_f32());
    let screens = platform::screens();
    let anchor = platform::caret_anchor();
    let window = platform::focused_window();
    let mouse = platform::mouse_location();

    // 选屏的锚点：两种模式问的不是同一个东西。
    // - 自动：插入点（拿不到就用鼠标，因为"用户正在哪儿打字"已经无从判断了）；
    // - 居中：前台窗口中心，其次鼠标。用户可能把鼠标停在 A 屏、却在 B 屏打字，
    //   按窗口选才和"当前激活窗口所在屏幕"这个说法一致。
    let probe = match config.position {
        Position::Auto => anchor
            .map(|anchor| (anchor.x, anchor.bottom))
            .or(mouse)
            .or(window.map(center_of)),
        Position::Center => window
            .map(center_of)
            .or(mouse)
            .or(anchor.map(|a| (a.x, a.bottom))),
    };
    let screen = probe.and_then(|(x, y)| platform::screen_at(&screens, x, y));

    let Some(display) = screen
        .and_then(|screen| cx.find_display(DisplayId::new(screen.id)))
        .or_else(|| cx.primary_display())
    else {
        // 连显示器都问不到（极早期启动）：交给 gpui 按主屏居中，别让面板开不出来。
        return Placement {
            window_bounds: WindowBounds::centered(panel, cx),
            display_id: None,
            screen_origin: (0.0, 0.0),
            note: "没有显示器信息，退回主屏居中".into(),
        };
    };

    let visible = display.visible_bounds();
    let area = Area {
        x: visible.origin.x.as_f32(),
        y: visible.origin.y.as_f32(),
        w: visible.size.width.as_f32(),
        h: visible.size.height.as_f32(),
    };
    let origin = screen.map_or((0.0, 0.0), |screen| (screen.x as f32, screen.y as f32));
    // 插入点是全局坐标，先换算到这块屏的本地坐标再算位置。
    let local_anchor = anchor.map(|anchor| shift(anchor, origin));

    let (x, y) = match config.position {
        Position::Auto => place_auto(area, local_anchor, (width, height)),
        Position::Center => place_center(area, (width, height)),
    };

    let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
    Placement {
        window_bounds: WindowBounds::Windowed(bounds),
        display_id: Some(display.id()),
        screen_origin: origin,
        note: describe(config.position, anchor, screen, (x, y), display.id()),
    }
}

/// 窗口矩形的中心点（选屏用）。
fn center_of(rect: ScreenRect) -> (f64, f64) {
    rect.center()
}

/// 全局坐标 → 屏幕本地坐标。
fn shift(anchor: CaretAnchor, origin: (f32, f32)) -> (f32, f32, f32) {
    (
        anchor.x as f32 - origin.0,
        anchor.top as f32 - origin.1,
        anchor.bottom as f32 - origin.1,
    )
}

/// 自动：左缘对齐光标、垂直贴插入点下方；下方放不下就翻到上方。
///
/// 没有插入点时退化成居中——"跟随"没有目标可跟，居中至少是可预期的。
/// 左缘对齐是 macOS 输入法式贴靠：面板从光标位置向右展开，就像
/// 拼音候选窗那样，而不是把面板中心对着光标。右边溢出时夹进可视区。
fn place_auto(visible: Area, anchor: Option<(f32, f32, f32)>, panel: (f32, f32)) -> (f32, f32) {
    let (width, height) = panel;
    let Some((anchor_x, top, bottom)) = anchor else {
        return (center_x(visible, width), place_center(visible, panel).1);
    };
    // 左缘对齐光标：x 直接取光标 x，右边溢出就夹住。
    let x = clamp(
        anchor_x,
        visible.x + MARGIN,
        visible.right() - MARGIN - width,
    );

    let below = bottom + GAP;
    let above = top - GAP - height;
    let y = if below + height <= visible.bottom() - MARGIN {
        below
    } else if above >= visible.y + MARGIN {
        above
    } else {
        // 上下都放不下（面板比可视区还高，或光标正在屏幕正中且面板很高）：
        // 守在下方再夹进可视区，宁可盖住屏幕下半部分，也不要把插入点盖住。
        clamp(
            below,
            visible.y + MARGIN,
            visible.bottom() - MARGIN - height,
        )
    };

    (x, y)
}

/// 居中：可视区正中（`WindowBounds::centered` 的语义，只是换成了指定的屏）。
fn place_center(visible: Area, panel: (f32, f32)) -> (f32, f32) {
    let (width, height) = panel;
    let (_, cy) = visible.center();
    (
        center_x(visible, width),
        clamp(
            cy - height / 2.0,
            visible.y + MARGIN,
            visible.bottom() - MARGIN - height,
        ),
    )
}

/// 水平居中，并保证整块面板落在可视区内。
fn center_x(visible: Area, width: f32) -> f32 {
    let (center, _) = visible.center();
    clamp(
        center - width / 2.0,
        visible.x + MARGIN,
        visible.right() - MARGIN - width,
    )
}

/// `clamp` 的安全版：面板比可视区还宽时 `high < low`，`f32::clamp` 会 panic。
///
/// 这种时候守左边界：面板右边溢出屏幕总比左边被切掉好认（标题和 `✕` 在左边）。
fn clamp(value: f32, low: f32, high: f32) -> f32 {
    if high <= low {
        low
    } else {
        value.clamp(low, high)
    }
}

/// 拼一句人话进日志：定位出问题时，第一个要回答的就是"它以为我在哪块屏、按什么算的"。
fn describe(
    position: Position,
    anchor: Option<CaretAnchor>,
    screen: Option<ScreenRect>,
    at: (f32, f32),
    display: DisplayId,
) -> String {
    let mode = match position {
        Position::Auto => "跟随插入点",
        Position::Center => "居中",
    };
    let source = anchor.map_or("无插入点", |anchor| anchor.source.label());
    let screen = screen.map_or("未知屏幕".to_string(), |screen| {
        format!(
            "屏 {} {}x{} @ ({:.0},{:.0})",
            screen.id, screen.width, screen.height, screen.x, screen.y
        )
    });
    format!(
        "{mode}（{source}）· {screen} · 本地 ({:.0},{:.0}) · {display:?}",
        at.0, at.1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一块 1800x1169 的屏，去掉菜单栏后可视区从 y=25 起（贴近真实 MacBook）。
    fn visible() -> Area {
        Area {
            x: 0.0,
            y: 25.0,
            w: 1800.0,
            h: 1144.0,
        }
    }

    #[test]
    fn auto_left_aligns_to_caret_like_macos_ime() {
        // 光标在 x=200：面板左缘也应该在 200
        let (x, y) = place_auto(visible(), Some((200.0, 500.0, 520.0)), (720.0, 420.0));
        assert_eq!(x, 200.0, "面板左缘 = 光标 x");
        assert_eq!(y, 526.0, "插入点下沿 520 + 6 的间距");

        // 光标太靠右、面板放不下：夹住右边缘
        let (x, _) = place_auto(visible(), Some((1750.0, 500.0, 520.0)), (720.0, 420.0));
        assert_eq!(
            x + 720.0,
            visible().right() - MARGIN,
            "面板整体不超出可视区右边缘"
        );

        // 光标太靠左、负溢出：夹住左边缘
        let (x, _) = place_auto(visible(), Some((-50.0, 500.0, 520.0)), (720.0, 420.0));
        assert_eq!(x, MARGIN, "负溢出时夹到左边界");
    }

    #[test]
    fn auto_flips_above_when_there_is_no_room_below() {
        // 插入点贴着屏幕底部：下方只剩 1169-1020=149，放不下 420
        let (_, y) = place_auto(visible(), Some((900.0, 1000.0, 1020.0)), (720.0, 420.0));
        assert_eq!(y, 574.0, "1000 - 6 - 420，翻到插入点上方");
    }

    #[test]
    fn auto_keeps_the_caret_visible_when_neither_side_fits() {
        // 面板 1000 高，插入点在正中间：上下都放不下，只能夹进可视区
        let (_, y) = place_auto(visible(), Some((900.0, 600.0, 620.0)), (720.0, 1000.0));
        assert!(
            y >= visible().y + MARGIN && y + 1000.0 <= visible().bottom() - MARGIN,
            "面板整体必须留在可视区内，实际 y={y}"
        );
    }

    #[test]
    fn auto_without_anchor_behaves_like_center() {
        let auto = place_auto(visible(), None, (720.0, 420.0));
        assert_eq!(auto, place_center(visible(), (720.0, 420.0)));
    }

    #[test]
    fn center_is_the_middle_of_the_visible_area() {
        let (x, y) = place_center(visible(), (720.0, 420.0));
        assert_eq!(x, 540.0);
        // 可视区 25..1169，中心 597，面板 420 高 → 387
        assert_eq!(y, 387.0);
    }

    #[test]
    fn oversized_panel_is_pinned_to_the_top_left_of_the_visible_area() {
        // 面板比屏幕还宽：不能 panic，也不能算出负数
        let (x, _) = place_center(visible(), (2000.0, 420.0));
        assert_eq!(x, MARGIN);
    }

    #[test]
    fn global_coordinates_become_screen_local_ones() {
        // 副屏在主屏右侧、原点 (1800,0)：插入点全局 x=2700 → 本地 900
        let anchor = CaretAnchor {
            x: 2700.0,
            top: 500.0,
            bottom: 520.0,
            source: platform::AnchorSource::Caret,
        };
        assert_eq!(shift(anchor, (1800.0, 0.0)), (900.0, 500.0, 520.0));
    }
}
