//! 「I;;」标记的几何定义——图标和菜单栏共用的唯一一份。
//!
//! 应用图标由 `build.rs` 在构建期渲染成 PNG/ICNS，菜单栏图标由 `tray.rs` 在
//! 运行期渲染成 RGBA 位图；两边都从这里取形状。几何必须只有一份，否则哪天
//! 调了斜度，Dock 里的图标和菜单栏里的图标就会长成两个东西。
//!
//! 坐标系是 100×100、y 向下的设计格子，与输出像素无关：调用方把自己的像素
//! 网格映射进来，按 4×4 子样本算覆盖率，于是 16px 和 1024px 都是同一套线条。
//!
//! 形状语义：斜体大写 `I`（衬线：上下横杠 + 竖杠）加两个分号。`;;` 是 snipshot
//! 的触发词，斜体 `I` 是光标——「在光标处敲 `;;`」，整个产品就这一句话。

/// 斜切量：设计格里每升高 1 个单位，笔画右移 0.28（约 15.6°，接近常见斜体）。
const SLANT: f32 = 0.28;
/// 斜切的支点纵坐标。上下以它为轴反向偏移，字形才不会整体歪向一边。
const PIVOT: f32 = 50.0;

/// `I` 的三条边：上下横杠各 34 宽 9 厚，竖杠 13 宽，总高 56。
const BAR_TOP: (f32, f32) = (22.0, 31.0);
const BAR_BOTTOM: (f32, f32) = (69.0, 78.0);
const BAR_X: (f32, f32) = (26.0, 60.0);
const STEM_X: (f32, f32) = (36.0, 49.0);

/// 两个分号的中心横坐标；每个分号 = 方点 + 向下收窄的尾巴。
///
/// 分号比 `I` 细一档是有意的：它是标点，本该比字母轻；如果跟竖杠一样粗，
/// 两个分号会连成一块，远看像「H」的右半边。
const SEMI_X: [f32; 2] = [73.0, 86.5];
const SEMI_DOT: (f32, f32) = (51.5, 4.4);
/// 尾巴：上宽 5.2、下宽 2.6，并且整体往左倒一点——分号的尾巴本来就是个左撇。
const SEMI_TAIL: (f32, f32) = (60.0, 74.5);

/// 内容的包围盒 `(x0, y0, x1, y1)`，由上面的数字推出来，不是拍的：
/// 左边界是下横杠在 y = 78 处的斜切结果，右边界是第二个分号的右缘。
pub const BOUNDS: (f32, f32, f32, f32) = (
    BAR_X.0 + (PIVOT - BAR_BOTTOM.1) * SLANT,
    BAR_TOP.0,
    SEMI_X[1] + SEMI_DOT.1,
    BAR_BOTTOM.1,
);

/// 标记的宽高比，调用方据此在自己的框里摆放而不变形。
pub fn aspect() -> f32 {
    (BOUNDS.2 - BOUNDS.0) / (BOUNDS.3 - BOUNDS.1)
}

/// 在 `w×h` 的区域里居中放一个等比缩放的标记框，返回 `(x0, y0, w, h)`。
///
/// `inset` 是四周留白占区域的比例（0..0.45）：应用图标要留呼吸，菜单栏要贴满。
pub fn fit(w: f32, h: f32, inset: f32) -> (f32, f32, f32, f32) {
    let scale = ((w * (1.0 - 2.0 * inset)) / (BOUNDS.2 - BOUNDS.0))
        .min((h * (1.0 - 2.0 * inset)) / (BOUNDS.3 - BOUNDS.1));
    let (mw, mh) = (
        (BOUNDS.2 - BOUNDS.0) * scale,
        (BOUNDS.3 - BOUNDS.1) * scale,
    );
    ((w - mw) / 2.0, (h - mh) / 2.0, mw, mh)
}

/// 像素坐标 `(px, py)` 是否落在标记上；`bx` 是 [`fit`] 给出的框。
///
/// 框太窄时会自动省掉分号：16px 的图标里分号的圆点只有 1 像素出头，画出来
/// 不是「;」而是一团灰，把 `I` 的轮廓一起拖脏。宁可少一笔，也不要糊一片。
pub fn hit(bx: (f32, f32, f32, f32), px: f32, py: f32) -> bool {
    let x = BOUNDS.0 + (px - bx.0) / bx.2 * (BOUNDS.2 - BOUNDS.0);
    let y = BOUNDS.1 + (py - bx.1) / bx.3 * (BOUNDS.3 - BOUNDS.1);
    ink(x, y) || (has_detail(bx.2) && semicolons(x, y))
}

/// 分号的圆点在当前框宽下能画出几个像素。
fn has_detail(box_width_px: f32) -> bool {
    let dot_px = 2.0 * SEMI_DOT.1 / (BOUNDS.2 - BOUNDS.0) * box_width_px;
    dot_px >= 1.5
}

/// 设计坐标处的判断，只问 `I` 本身。
pub fn ink(x: f32, y: f32) -> bool {
    italic_i(x, y)
}

/// 斜体 `I`：把采样点反向斜切回竖直坐标系，再按三个矩形判断。
///
/// 反着做（判点而不是造多边形）是因为竖杠与横杠要拼成一整块，任何一条边
/// 都不能出现接缝——按矩形判、再取并集，接缝自然消失。
fn italic_i(x: f32, y: f32) -> bool {
    let ux = x + (y - PIVOT) * SLANT;
    let in_bar_y = |y: f32, band: (f32, f32)| y >= band.0 && y < band.1;

    (in_bar_y(y, BAR_TOP) || in_bar_y(y, BAR_BOTTOM)) && ux >= BAR_X.0 && ux < BAR_X.1
        || (y >= BAR_TOP.0 && y < BAR_BOTTOM.1 && ux >= STEM_X.0 && ux < STEM_X.1)
}

/// 两个分号：圆点 + 左倾的细尾巴。
///
/// 用圆点而不是方点，是为了和方头方脑的衬线 `I` 拉开对比——分号是标点，
/// 比字母轻、也比字母软。尾巴比圆点窄，中间必须留出一道缝，否则点与尾巴
/// 会糊成一整块，远看像「⊞」。
fn semicolons(x: f32, y: f32) -> bool {
    SEMI_X.iter().any(|&cx| {
        let (dx, dy) = (x - cx, y - SEMI_DOT.0);
        dx * dx + dy * dy <= SEMI_DOT.1 * SEMI_DOT.1
            || in_poly(
                x,
                y,
                &[
                    (cx - 2.6, SEMI_TAIL.0),
                    (cx + 2.6, SEMI_TAIL.0),
                    (cx - 0.7, SEMI_TAIL.1),
                    (cx - 3.3, SEMI_TAIL.1),
                ],
            )
    })
}

/// 凸多边形内外测试：所有边与采样点的叉积同号即在内部。
fn in_poly(x: f32, y: f32, pts: &[(f32, f32)]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..pts.len() {
        let (ax, ay) = pts[i];
        let (bx, by) = pts[(i + 1) % pts.len()];
        let cross = (bx - ax) * (y - ay) - (by - ay) * (x - ax);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// 每个像素打多少个样本。4×4 足够：斜率都是直线段，再细看不出差别，
/// 但 16px 的图标只有十几个像素宽，不采样就全是锯齿。
pub const SUB: u32 = 4;

/// 渲染成 `w×h` 的黑白模板图 RGBA：RGB 全 0，alpha 是覆盖率。
///
/// 模板图（template image）只认 alpha，颜色由 AppKit 按菜单栏明暗决定；
/// 自己也画一遍黑白反而会在深浅切换时露馅。
pub fn template_rgba(w: u32, h: u32, inset: f32) -> Vec<u8> {
    let bx = fit(w as f32, h as f32, inset);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let mut hits = 0u32;
            for sy in 0..SUB {
                for sx in 0..SUB {
                    let px = x as f32 + (sx as f32 + 0.5) / SUB as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SUB as f32;
                    if hit(bx, px, py) {
                        hits += 1;
                    }
                }
            }
            rgba.extend_from_slice(&[0, 0, 0, (hits * 255 / (SUB * SUB)) as u8]);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 设计坐标 → 像素：用 100×100 的框跟设计格子 1:1，断言才好读。
    fn ink_at(x: f32, y: f32) -> bool {
        hit((BOUNDS.0, BOUNDS.1, BOUNDS.2 - BOUNDS.0, BOUNDS.3 - BOUNDS.1), x, y)
    }

    #[test]
    fn strokes_are_where_they_should_be() {
        // 竖杠中线（斜切后仍在中线附近）、上下横杠、两个分号的点与尾巴
        assert!(ink_at(42.5, 50.0), "竖杠");
        assert!(ink_at(42.5, 26.0), "上横杠");
        assert!(ink_at(42.5, 73.0), "下横杠");
        assert!(ink_at(SEMI_X[0], SEMI_DOT.0), "第一个分号的点");
        assert!(ink_at(SEMI_X[1], SEMI_DOT.0), "第二个分号的点");
        assert!(ink_at(SEMI_X[1] - 2.0, 70.0), "第二个分号的尾巴");
    }

    #[test]
    fn gaps_stay_empty() {
        // 横杠与分号之间、两个分号之间、字形左侧都必须是空的
        assert!(!ink_at(SEMI_X[0] - SEMI_DOT.1 - 1.0, SEMI_DOT.0), "I 与分号之间");
        assert!(!ink_at((SEMI_X[0] + SEMI_X[1]) / 2.0, 45.0), "两个分号之间");
        assert!(!ink_at(20.0, 50.0), "字形左侧");
        assert!(!ink_at(50.0, 12.0), "字形上方");
    }

    #[test]
    fn italic_leans_right() {
        // 竖杠段（避开横杠）：同一个纵坐标扫过去，靠上的那个左缘必须更靠右，
        // 这就是「斜体」的可测量定义。
        let left_edge = |y: f32| {
            (25..60)
                .map(|i| i as f32)
                .find(|&x| ink_at(x, y))
                .unwrap_or(f32::NAN)
        };
        let (top, bottom) = (left_edge(40.0), left_edge(60.0));
        assert!(
            top > bottom,
            "顶部左缘 {top} 应比底部左缘 {bottom} 更靠右"
        );
    }

    #[test]
    fn bounds_are_tight() {
        // 包围盒要真的贴住笔画：右缘贴分号，上缘贴横杠，下缘贴横杠与尾巴
        let (x0, y0, x1, y1) = BOUNDS;
        assert!(ink_at(x1 - 0.5, SEMI_DOT.0), "右边界应贴住分号");
        assert!(ink_at(48.0, y0 + 0.5), "上边界应贴住横杠");
        assert!(ink_at(30.0, y1 - 0.5), "下边界应贴住下横杠");
        assert!(ink_at(x0 + 0.5, y1 - 0.5), "左边界应贴住下横杠的左端");
    }

    #[test]
    fn coverage_is_stable_across_sizes() {
        // 16px 与 128px 的实心占比应当接近：说明是按尺寸缩放画的，
        // 不是拿一张大图硬缩。差太多就说明小尺寸被抗锯齿吃掉了。
        let ratio = |size: u32| {
            let (w, h) = (size, (size as f32 / aspect()) as u32);
            let rgba = template_rgba(w, h, 0.0);
            let solid = rgba.chunks_exact(4).filter(|p| p[3] > 128).count();
            solid as f32 / (w * h) as f32
        };
        let (small, large) = (ratio(16), ratio(128));
        assert!(
            (small - large).abs() < 0.05,
            "16px 覆盖率 {small:.3} 与 128px {large:.3} 差太多"
        );
    }

    #[test]
    fn corners_are_transparent() {
        // 菜单栏上不能出现一块若隐若现的底
        let w = 40;
        let h = 30;
        let rgba = template_rgba(w, h, 0.02);
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(rgba[((y * w + x) * 4 + 3) as usize], 0, "角 ({x},{y}) 不透明");
        }
    }
}
