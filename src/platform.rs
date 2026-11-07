//! 屏幕几何与「插入点」查询：面板定位的输入。
//!
//! 面板要贴在用户正在打字的地方，而不是主显示器正中。这需要两样东西：
//!
//! 1. **每块屏在全局坐标里的矩形**（[`screens`]）。`CGDisplayBounds` 用的是
//!    「主屏左上角为原点、y 向下、单位点」的全局坐标，与 AX / CGEvent 同源；
//! 2. **插入点在哪儿**（[`caret_anchor`]）。macOS 没有"全局光标位置"这种 API，
//!    只能问 AX：焦点元素的 `AXSelectedTextRange` → `AXBoundsForRange`。
//!
//! 坐标系只有一套（全局、点、y 向下），换算成 gpui 的「屏幕本地坐标」是
//! `ui::place` 的活，这里不做。这样 `trigger::tap` 里那份面板矩形（用来判断
//! "点里面还是点外面"）可以直接用全局坐标，多屏时也不会错位。
//!
//! 取不到锚点是常态而不是异常：终端、部分 Electron 应用、无辅助功能授权时
//! 都拿不到。调用方必须接受 `None` 并退回鼠标位置或屏幕居中——面板位置差一点
//! 是可以接受的，打不开面板不是。

use std::ffi::c_void;

use core_foundation::base::{CFRelease, TCFType};
use core_foundation::string::CFString;
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

/// 锚点的来源。定位不准时先看日志里这一项：它说明我们信的是谁。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorSource {
    /// AX 真实插入点矩形（最可信）。
    Caret,
    /// 单行输入控件的整体边界（搜索框、地址栏这类拿不到插入点的情况）。
    Element,
    /// 当前鼠标位置（最终兜底）。
    Mouse,
}

impl AnchorSource {
    /// 日志里的中文名。
    pub fn label(self) -> &'static str {
        match self {
            AnchorSource::Caret => "AX 插入点",
            AnchorSource::Element => "AX 控件边界",
            AnchorSource::Mouse => "鼠标位置",
        }
    }
}

/// 插入点锚点（全局坐标，单位点，y 向下）。
///
/// 带上下沿而不只是 y：面板要么贴在 `bottom` 下面，要么贴在 `top` 上面，
/// 只知道一个点的话，"翻到上方"就得多算一次行高——而每个应用的行高都不同。
#[derive(Clone, Copy, Debug)]
pub struct CaretAnchor {
    /// 插入点左边（面板水平居中用它所在屏的中心，这里只用于选屏）。
    pub x: f64,
    /// 插入点所在行的上沿。
    pub top: f64,
    /// 插入点所在行的下沿。
    pub bottom: f64,
    pub source: AnchorSource,
}

/// 一块屏幕在**全局坐标**里的矩形（单位点，y 向下）。
///
/// `id` 就是 `CGDirectDisplayID`，与 gpui 的 `DisplayId` 是同一个数字
/// （见 `gpui-pre-macos` 的 `MacDisplay::id`），所以两边能对上。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScreenRect {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ScreenRect {
    /// 点是否落在这块屏里。
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// 点到这块屏矩形的最短距离平方。
    ///
    /// 锚点可能落在两块屏之间的空隙（上下错位排列的屏幕），这时"包含"判定
    /// 一个都不中，靠它选最近的一块。
    pub fn distance_sq(&self, x: f64, y: f64) -> f64 {
        let dx = (self.x - x).max(0.0).max(x - (self.x + self.width));
        let dy = (self.y - y).max(0.0).max(y - (self.y + self.height));
        dx * dx + dy * dy
    }

    /// 矩形中心点，用于"居中于这块屏"。
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// 从中挑出包含该点的屏；都不包含时取最近的一块。
pub fn screen_at(screens: &[ScreenRect], x: f64, y: f64) -> Option<ScreenRect> {
    screens
        .iter()
        .find(|screen| screen.contains(x, y))
        .copied()
        .or_else(|| {
            screens
                .iter()
                .min_by(|a, b| a.distance_sq(x, y).total_cmp(&b.distance_sq(x, y)))
                .copied()
        })
}

/// 所有活动屏幕的全局几何。
///
/// 顺序不保证，调用方不要依赖"第 0 个是主屏"。拿不到时返回空表——那时
/// 定位会退回 `WindowBounds::centered`（gpui 自己按主屏居中）。
pub fn screens() -> Vec<ScreenRect> {
    let Ok(ids) = core_graphics::display::CGDisplay::active_displays() else {
        return Vec::new();
    };
    ids.into_iter()
        .filter_map(|id| {
            let bounds = core_graphics::display::CGDisplay::new(id).bounds();
            if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
                return None;
            }
            Some(ScreenRect {
                id: u64::from(id),
                x: bounds.origin.x,
                y: bounds.origin.y,
                width: bounds.size.width,
                height: bounds.size.height,
            })
        })
        .collect()
}

/// 当前鼠标位置（全局坐标）。取不到就是 `None`（没有鼠标的会话）。
pub fn mouse_location() -> Option<(f64, f64)> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let event = CGEvent::new(source).ok()?;
    let point = event.location();
    Some((point.x, point.y))
}

/// 前台窗口（焦点窗口）的全局矩形。
///
/// 「居中」模式靠它选屏：用户可能把鼠标放在 A 屏、但正在 B 屏的窗口里打字，
/// 那时的"当前屏幕"应该是 B 屏——与插入点走同一套判断才不自相矛盾。
pub fn focused_window() -> Option<ScreenRect> {
    unsafe {
        let window = ax_focused_window_element()?;
        let frame = ax_frame(window);
        CFRelease(window);
        let frame = frame?;
        (frame.size.width > 0.0 && frame.size.height > 0.0).then_some(ScreenRect {
            id: 0,
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        })
    }
}

/// 插入点锚点：先问 AX 插入点，再退到单行控件边界，最后退到鼠标。
///
/// 为什么接受 `None` 的反面：没有辅助功能授权时 AX 调用全部失败，但鼠标位置
/// 不受授权限制，所以正常情况这里**总能**给出一个位置；只有连鼠标都查不到
/// （无图形会话）才会返回 `None`。
pub fn caret_anchor() -> Option<CaretAnchor> {
    if let Some(anchor) = ax_anchor() {
        return Some(anchor);
    }
    let (x, y) = mouse_location()?;
    // 鼠标热点在箭头尖端，按 16px 行高把它当成"光标所在行"的底部。
    Some(CaretAnchor {
        x,
        top: y - 16.0,
        bottom: y,
        source: AnchorSource::Mouse,
    })
}

// ---- AX ----

/// AX 里能拿到的最合适的锚点。
///
/// 所有权提醒：`AXUIElementCopyAttributeValue` 返回的是 +1 引用，每一条返回
/// 路径都必须 `CFRelease`——包括"找到了，提前返回"那几条。这里把真正的判定
/// 收进 [`scan_elements`]，释放统一放在外层，省得以后加分支时漏掉一个。
fn ax_anchor() -> Option<CaretAnchor> {
    unsafe {
        let window = ax_focused_window_element();
        let anchor = scan_elements(window);
        if let Some(window) = window {
            CFRelease(window);
        }
        anchor
    }
}

unsafe fn scan_elements(window: Option<*mut c_void>) -> Option<CaretAnchor> {
    unsafe {
        let window_frame = window.and_then(|window| ax_frame(window));

        // 候选元素：焦点元素优先；有些 App 只在窗口上实现 AXFocusedUIElement。
        let mut candidates: Vec<*mut c_void> = Vec::new();
        if let Some(element) = ax_focused_element() {
            candidates.push(element);
        }
        if let Some(window) = window {
            if let Ok(child) = ax_attr_raw(window, "AXFocusedUIElement") {
                candidates.push(child);
            }
        }

        let mut found = None;
        for element in &candidates {
            // 目标 App 无响应时最多等这么久，别把 UI 线程挂住。
            AXUIElementSetMessagingTimeout(*element, 0.12);
            let element_frame = ax_frame(*element).or(window_frame);

            if let Some(rect) = ax_caret_rect(*element) {
                // 插入点必须落在自己的元素/窗口里，否则是过期或串台的数据。
                if plausible(&rect) && element_frame.is_none_or(|frame| within(&frame, &rect, 8.0))
                {
                    found = Some(CaretAnchor {
                        x: rect.origin.x,
                        top: rect.origin.y,
                        bottom: rect.origin.y + rect.size.height,
                        source: AnchorSource::Caret,
                    });
                    break;
                }
                log::debug!(
                    "插入点不可信：rect=({:.0},{:.0}) {:.0}x{:.0}",
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height
                );
            }

            // 单行输入控件（搜索框 / 地址栏 / 表单）：整体边界仍然可用。
            // 大型文本区不适用——它的左上角跟光标位置毫无关系。
            if let (Some(origin), Some(size)) =
                (ax_point(*element, "AXPosition"), ax_size(*element))
            {
                if (8.0..=64.0).contains(&size.height) && (24.0..=1600.0).contains(&size.width) {
                    found = Some(CaretAnchor {
                        x: origin.x,
                        top: origin.y,
                        bottom: origin.y + size.height,
                        source: AnchorSource::Element,
                    });
                    break;
                }
            }
        }

        release_all(&candidates);
        found
    }
}

/// 释放候选元素（AX 的所有权是 CFRetain/Create 规则，漏了就是泄漏）。
fn release_all(elements: &[*mut c_void]) {
    for element in elements {
        unsafe { CFRelease(*element) };
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AxPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AxSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AxRect {
    origin: AxPoint,
    size: AxSize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AxRange {
    location: isize,
    length: isize,
}

/// `AXValueType` 里我们用到的那四种。
const AX_VALUE_CG_POINT: u32 = 1;
const AX_VALUE_CG_SIZE: u32 = 2;
const AX_VALUE_CG_RECT: u32 = 3;
const AX_VALUE_CF_RANGE: u32 = 4;

/// 插入点矩形是否可信。
///
/// 行高 4–120 点覆盖了从 9pt 小字到放大后的编辑器；宽度上限放得宽是因为
/// 有些应用把"插入点"报成整个字符框，而中文字符可以很宽。
fn plausible(rect: &AxRect) -> bool {
    (4.0..=120.0).contains(&rect.size.height)
        && (0.0..=400.0).contains(&rect.size.width)
        && rect.origin.x.is_finite()
        && rect.origin.y.is_finite()
}

/// `rect` 是否落在 `frame` 内（`slack` 容忍各 App 的取整误差）。
fn within(frame: &AxRect, rect: &AxRect, slack: f64) -> bool {
    rect.origin.x >= frame.origin.x - slack
        && rect.origin.x <= frame.origin.x + frame.size.width + slack
        && rect.origin.y >= frame.origin.y - slack
        && rect.origin.y + rect.size.height <= frame.origin.y + frame.size.height + slack
}

unsafe fn ax_attr_raw(element: *mut c_void, name: &str) -> Result<*mut c_void, i32> {
    unsafe {
        let attr = CFString::new(name);
        let mut value: *mut c_void = std::ptr::null_mut();
        let err = AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef() as *const c_void,
            &mut value,
        );
        if err == 0 && !value.is_null() {
            Ok(value)
        } else {
            Err(err)
        }
    }
}

/// system-wide 的焦点元素；没有焦点元素时是 `None`。
unsafe fn ax_focused_element() -> Option<*mut c_void> {
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(system, 0.12);
        let result = ax_attr_raw(system, "AXFocusedUIElement").ok();
        CFRelease(system);
        result
    }
}

/// 前台应用的焦点窗口。
unsafe fn ax_focused_window_element() -> Option<*mut c_void> {
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(system, 0.12);
        let app = ax_attr_raw(system, "AXFocusedApplication").ok();
        CFRelease(system);

        let app = app?;
        AXUIElementSetMessagingTimeout(app, 0.12);
        let window = ax_attr_raw(app, "AXFocusedWindow").ok();
        CFRelease(app);
        window
    }
}

/// 元素的 `AXPosition`。
unsafe fn ax_point(element: *mut c_void, name: &str) -> Option<AxPoint> {
    unsafe {
        let value = ax_attr_raw(element, name).ok()?;
        let mut point = AxPoint { x: 0.0, y: 0.0 };
        let ok = AXValueGetValue(
            value,
            AX_VALUE_CG_POINT,
            &mut point as *mut AxPoint as *mut c_void,
        );
        CFRelease(value);
        ok.then_some(point)
    }
}

/// 元素的 `AXSize`。
unsafe fn ax_size(element: *mut c_void) -> Option<AxSize> {
    unsafe {
        let value = ax_attr_raw(element, "AXSize").ok()?;
        let mut size = AxSize {
            width: 0.0,
            height: 0.0,
        };
        let ok = AXValueGetValue(
            value,
            AX_VALUE_CG_SIZE,
            &mut size as *mut AxSize as *mut c_void,
        );
        CFRelease(value);
        ok.then_some(size)
    }
}

/// 元素的 frame（`AXPosition` + `AXSize`）。
unsafe fn ax_frame(element: *mut c_void) -> Option<AxRect> {
    unsafe {
        let origin = ax_point(element, "AXPosition")?;
        let size = ax_size(element)?;
        (size.width > 0.0 && size.height > 0.0).then_some(AxRect { origin, size })
    }
}

/// `AXBoundsForRange`：把字符范围换成屏幕矩形。
unsafe fn ax_bounds_for_range(element: *mut c_void, range: AxRange) -> Option<AxRect> {
    unsafe {
        let param = AXValueCreate(AX_VALUE_CF_RANGE, &range as *const AxRange as *const c_void);
        if param.is_null() {
            return None;
        }
        let attr = CFString::new("AXBoundsForRange");
        let mut raw: *mut c_void = std::ptr::null_mut();
        let err = AXUIElementCopyParameterizedAttributeValue(
            element,
            attr.as_concrete_TypeRef() as *const c_void,
            param,
            &mut raw,
        );
        CFRelease(param);
        if err != 0 || raw.is_null() {
            return None;
        }
        let mut rect = AxRect {
            origin: AxPoint { x: 0.0, y: 0.0 },
            size: AxSize {
                width: 0.0,
                height: 0.0,
            },
        };
        let ok = AXValueGetValue(
            raw,
            AX_VALUE_CG_RECT,
            &mut rect as *mut AxRect as *mut c_void,
        );
        CFRelease(raw);
        ok.then_some(rect)
    }
}

/// 插入点矩形：`AXSelectedTextRange` 的起点，`length = 0`。
///
/// 很多应用对空范围返回零矩形（Terminal、Telegram），所以再试一次"取一个字符"：
/// 有光标时那一个字符的矩形正好覆盖光标所在行，没有时（文末）退回前一个字符。
unsafe fn ax_caret_rect(element: *mut c_void) -> Option<AxRect> {
    unsafe {
        let range = {
            let value = ax_attr_raw(element, "AXSelectedTextRange").ok()?;
            let mut range = AxRange {
                location: 0,
                length: 0,
            };
            let ok = AXValueGetValue(
                value,
                AX_VALUE_CF_RANGE,
                &mut range as *mut AxRange as *mut c_void,
            );
            CFRelease(value);
            ok.then_some(range)?
        };
        let location = range.location;

        // 有选区时用选区起点，长度归零：面板该贴的是"选区的开头"，不是整段。
        let rect = ax_bounds_for_range(
            element,
            AxRange {
                location,
                length: 0,
            },
        )?;
        if rect.size.height > 0.0 {
            return Some(rect);
        }

        for probe in [location, location - 1] {
            if probe < 0 {
                continue;
            }
            if let Some(rect) = ax_bounds_for_range(
                element,
                AxRange {
                    location: probe,
                    length: 1,
                },
            ) {
                if rect.size.height > 0.0 {
                    return Some(rect);
                }
            }
        }
        None
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> *mut c_void;
    fn AXUIElementCopyAttributeValue(
        element: *mut c_void,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: *mut c_void,
        attribute: *const c_void,
        parameter: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: *mut c_void, timeout: f64) -> i32;
    fn AXValueCreate(the_type: u32, value: *const c_void) -> *mut c_void;
    fn AXValueGetValue(value: *const c_void, the_type: u32, value: *mut c_void) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(id: u64, x: f64, y: f64, width: f64, height: f64) -> ScreenRect {
        ScreenRect {
            id,
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn point_is_matched_to_its_own_screen() {
        let screens = [
            screen(1, 0.0, 0.0, 1800.0, 1169.0),
            screen(2, 1800.0, 0.0, 2560.0, 1440.0),
        ];
        assert_eq!(screen_at(&screens, 100.0, 100.0).map(|s| s.id), Some(1));
        assert_eq!(screen_at(&screens, 2000.0, 100.0).map(|s| s.id), Some(2));
    }

    #[test]
    fn gap_between_screens_falls_back_to_the_nearest_one() {
        // 副屏在主屏右上方：两块屏之间有一段 y 上的错位，全局坐标会落进"空隙"
        let screens = [
            screen(1, 0.0, 400.0, 1800.0, 800.0),
            screen(2, 1800.0, 0.0, 1280.0, 800.0),
        ];
        // (1900, 100) 在副屏里
        assert_eq!(screen_at(&screens, 1900.0, 100.0).map(|s| s.id), Some(2));
        // (1800, 850) 不在任何屏内（主屏 y 到 1200 才有，副屏 x 从 1800 起但 y 到 800）
        let picked = screen_at(&screens, 1700.0, 1300.0).map(|s| s.id);
        assert_eq!(picked, Some(1), "应退到距离最近的主屏");
    }

    #[test]
    fn distance_inside_a_screen_is_zero() {
        let s = screen(1, 0.0, 0.0, 100.0, 100.0);
        assert_eq!(s.distance_sq(50.0, 50.0), 0.0);
        assert_eq!(s.distance_sq(110.0, 50.0), 100.0);
    }

    #[test]
    fn empty_screen_list_yields_none() {
        assert!(screen_at(&[], 10.0, 10.0).is_none());
    }
}
