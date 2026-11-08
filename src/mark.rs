//! 「I;;」标记的几何定义——图标和菜单栏共用的唯一一份。
//!
//! 应用图标由 `build.rs` 在构建期渲染成 PNG/ICNS，菜单栏图标由 `tray.rs` 在
//! 运行期渲染成 RGBA 位图；两边都从这里取形状。几何必须只有一份，否则哪天
//! 调了斜度，Dock 里的图标和菜单栏里的图标就会长成两个东西。
//!
//! 坐标系是 100×100、y 向下的设计格子，与输出像素无关：调用方把自己的像素
//! 网格映射进来，按 4×4 子样本算覆盖率，于是 16px 和 1024px 都是同一套线条。
//!
//! 形状语义：斜体大写 `I` 加两个分号。`;;` 是 semisnip 的触发词，斜体 `I`
//! 是光标——「在光标处敲 `;;`」，整个产品就这一句话。
//!
//! 粗细是刻意的**不对称**：`I` 细（笔画 6–9），`;;` 重（逗号碗直径 13.6）。
//! 字母是内容，分号是身份；两者一样粗的时候，远看只是一串竖条。
//!
//! 分号内部的配比也有讲究：**点小、逗号胖**。反过来（大点配细尾）就是字母 i
//! 的结构，两个并排立刻读成「ii」——这条踩过坑，见 `punctuation_outweighs_the_letter`。

/// 斜切量：设计格里每升高 1 个单位，笔画右移 0.28（约 15.6°，接近常见斜体）。
const SLANT: f32 = 0.28;
/// 斜切的支点纵坐标。上下以它为轴反向偏移，字形才不会整体歪向一边。
const PIVOT: f32 = 50.0;

/// `I` 的三条边：上下横杠各 30 宽 6 厚，竖杠 9 宽，总高 54。
/// 比 `;;` 轻一档是有意的——细笔画才衬得出旁边那两个逗号的重量。
const BAR_TOP: (f32, f32) = (23.0, 29.0);
const BAR_BOTTOM: (f32, f32) = (71.0, 77.0);
const BAR_X: (f32, f32) = (25.0, 55.0);
const STEM_X: (f32, f32) = (35.5, 44.5);

/// 两个分号的中心横坐标。间距 21 是按真实字体量的（分号间距约为字高的一半），
/// 而且要留出两根逗号之间的空当，否则缩到 32px 就连成一根短横。
const SEMI_X: [f32; 2] = [70.5, 91.5];
/// 圆点：圆心纵坐标与半径。**比逗号的碗小一圈**——真实分号里，逗号才是
/// 胖的那一半。这一点做反（大点配细尾巴）就成了字母 i，`;;` 立刻读作「ii」。
const SEMI_DOT: (f32, f32) = (45.0, 4.2);
/// 逗号的碗：一个圆心与半径。上沿 56.2，与圆点之间留 7.0 的空当。
const SEMI_HEAD: (f32, f32) = (63.0, 6.8);
/// 颈部（尾巴的起笔）相对碗心向右错开的距离。
///
/// 错开是为了让尾巴挂在碗的**右半边**：左边于是空出一道凹口，那才是逗号的
/// 「腰」。居中起笔（不错开）得到的是一颗水滴，不是逗号。
const NECK_OFFSET: f32 = 1.5;
/// 尾巴中心线的起点：碗心 + 颈部错开，且颈部半径 = 碗半径 - 错开量，
/// 于是起笔那个圆与碗**内切**——多一分就会在右侧顶出一根刺（踩过）。
const TAIL_R0: f32 = SEMI_HEAD.1 - NECK_OFFSET;
/// 尾巴中心线的终点 `(相对碗心的横偏移, 纵坐标)`：往左甩 6，比 `I` 的基线
/// （77）略低——分号底下那一点本来就是往下的，也是 `BOUNDS` 的下边界。
const TAIL_END: (f32, f32) = (-6.0, 77.6);
/// 中心线的控制点纵坐标。比中点略高，尾巴才是先直后弯，不是一路圆弧。
const TAIL_CTRL_Y: f32 = 70.5;
/// 尾巴末端的半宽。**收尖但不收成针**：真实逗号的尾巴是一条粗带子，
/// 一路收到 2.3（约碗半径的三分之一）就被切掉了。收成 0 会变成气球挂针。
const TAIL_TIP_HALF: f32 = 2.3;
/// 尾巴由粗到细的幂次。`1 - t²` 的底数保证起笔处斜率为零：颈部的侧壁是
/// 竖直的，才接得上碗在三点钟方向的竖直切线，接缝处不会出现折角。
const TAPER: f32 = 1.35;

/// 内容的包围盒 `(x0, y0, x1, y1)`，由上面的数字推出来，不是拍的：
/// 左边界是下横杠在最低处被斜切的结果，右边界是第二个分号的碗缘，
/// 下边界是尾巴末端的圆头（比 `I` 的底低，分号本来就该探出去一点）。
pub const BOUNDS: (f32, f32, f32, f32) = (
    BAR_X.0 + (PIVOT - BAR_BOTTOM.1) * SLANT,
    BAR_TOP.0,
    SEMI_X[1] + SEMI_HEAD.1,
    TAIL_END.1 + TAIL_TIP_HALF,
);

/// 一笔墨归谁：应用图标要分色画，所以光知道"这里有没有墨"不够。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// 斜体 `I`——文字本身。
    Letter,
    /// 两个分号——触发符，也是唯一需要被记住的符号。
    Punct,
}

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
    let (mw, mh) = ((BOUNDS.2 - BOUNDS.0) * scale, (BOUNDS.3 - BOUNDS.1) * scale);
    ((w - mw) / 2.0, (h - mh) / 2.0, mw, mh)
}

/// 像素坐标 `(px, py)` 落在哪个部件上；`bx` 是 [`fit`] 给出的框。
///
/// 框太窄时会自动省掉分号：16px 的图标里圆点只有 1.5 像素，画出来不是「;」
/// 而是一团灰，把 `I` 的轮廓一起拖脏。宁可少一笔，也不要糊一片。
pub fn sample(bx: (f32, f32, f32, f32), px: f32, py: f32) -> Option<Part> {
    let x = BOUNDS.0 + (px - bx.0) / bx.2 * (BOUNDS.2 - BOUNDS.0);
    let y = BOUNDS.1 + (py - bx.1) / bx.3 * (BOUNDS.3 - BOUNDS.1);
    let w = weight(bx.2);
    if italic_i(x, y, w) {
        return Some(Part::Letter);
    }
    if has_detail(bx.2) && semicolons(x, y, w) {
        return Some(Part::Punct);
    }
    None
}

/// 只问"有没有墨"。菜单栏的模板图只认 alpha，不关心分色。
pub fn hit(bx: (f32, f32, f32, f32), px: f32, py: f32) -> bool {
    sample(bx, px, py).is_some()
}

/// 小尺寸的光学补偿：把笔画向外扩 `w` 个设计单位。
///
/// `I` 只有 6 个单位厚，缩到 16px 时不足 1 像素，纯采样会淡成一条灰边。
/// 24px 以下逐步补粗，最多 2.2 个单位；正常尺寸（>= 24px）完全不受影响。
fn weight(box_width_px: f32) -> f32 {
    ((24.0 - box_width_px) / 24.0 * 2.2).clamp(0.0, 2.2)
}

/// 分号的圆点在当前框宽下能画出几个像素。
fn has_detail(box_width_px: f32) -> bool {
    let dot_px = 2.0 * SEMI_DOT.1 / (BOUNDS.2 - BOUNDS.0) * box_width_px;
    dot_px >= 1.8
}

/// 斜体 `I`：把采样点反向斜切回竖直坐标系，再按三个矩形判断。
///
/// 反着做（判点而不是造多边形）是因为竖杠与横杠要拼成一整块，任何一条边
/// 都不能出现接缝——按矩形判、再取并集，接缝自然消失。
fn italic_i(x: f32, y: f32, w: f32) -> bool {
    let ux = x + (y - PIVOT) * SLANT;
    let in_x = |band: (f32, f32)| ux >= band.0 - w && ux < band.1 + w;
    let in_y = |band: (f32, f32)| y >= band.0 - w && y < band.1 + w;

    (in_y(BAR_TOP) || in_y(BAR_BOTTOM)) && in_x(BAR_X)
        || (y >= BAR_TOP.0 - w && y < BAR_BOTTOM.1 + w && in_x(STEM_X))
}

/// 两个分号：一个圆点，加一个由「圆脑袋 + 收尖的尾巴」拼成的逗号。
///
/// 逗号不能用梯形或三角形糊弄——两种都试过，在 512px 下分别读作「ii」和
/// 「两块三角」。真实的逗号是圆的头和一条钩出去的尾，缺了圆头就只是个楔子。
fn semicolons(x: f32, y: f32, w: f32) -> bool {
    SEMI_X.iter().any(|&cx| {
        let (dx, dy) = (x - cx, y - SEMI_DOT.0);
        let r = SEMI_DOT.1 + w;
        dx * dx + dy * dy <= r * r || comma(cx, x, y, w)
    })
}

/// 逗号：一个碗（圆）并上一条逐渐左拐、逐渐收细的尾巴。
fn comma(cx: f32, x: f32, y: f32, w: f32) -> bool {
    let (dx, dy) = (x - cx, y - SEMI_HEAD.0);
    let r = SEMI_HEAD.1 + w;
    dx * dx + dy * dy <= r * r || tail(cx, x, y, w)
}

/// 尾巴：中心线是一条先竖直、后左拐的二次贝塞尔，沿途每一点一颗逐渐变小
/// 的圆，取并集。
///
/// 早先这里用的是**逐行的水平切片**（拿 x 到中心线的横向距离比半宽）。两种
/// 画法在小尺寸上看不出差别，但切片的起笔是一道水平口子，与碗的竖直切线正
/// 交——交界处于是留下一个直角，右侧看起来就是一根刺。改成沿中心线铺圆之后，
/// 起笔那个圆与碗内切，接缝是光滑的。
fn tail(cx: f32, x: f32, y: f32, w: f32) -> bool {
    // 先框掉不可能有墨的地方：尾巴只占图标里很小一块，25 颗圆的距离计算
    // 不该摊到每一像素的每个子样本上。
    if y < SEMI_HEAD.0 || y > TAIL_END.1 + TAIL_TIP_HALF + w {
        return false;
    }
    if x < cx + TAIL_END.0 - TAIL_TIP_HALF - w || x > cx + SEMI_HEAD.1 + w {
        return false;
    }
    for i in 0..=TAIL_STEPS {
        let t = i as f32 / TAIL_STEPS as f32;
        let (px, py) = tail_at(t, cx);
        let r = tail_half(t) + w;
        let (dx, dy) = (x - px, y - py);
        if dx * dx + dy * dy <= r * r {
            return true;
        }
    }
    false
}

/// 中心线上均匀取多少颗圆。纵坐标对 `t` 几乎线性（二次项只有 -0.4），所以
/// 均匀的 `t` 也就是均匀的纵向间距；20 颗时相邻圆的缺口约 0.03 个单位，
/// 1024px 下不足半个像素。
const TAIL_STEPS: u32 = 20;

/// 尾巴中心线在参数 `t` 处的点。横坐标用 `t²` 插值，起点斜率为零（竖直起笔），
/// 越往下越往左，正是逗号的走势。
fn tail_at(t: f32, cx: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let x = cx + NECK_OFFSET * u * u + TAIL_END.0 * t * t;
    let y = u * u * SEMI_HEAD.0 + 2.0 * u * t * TAIL_CTRL_Y + t * t * TAIL_END.1;
    (x, y)
}

/// 尾巴在参数 `t` 处的半宽：从颈部（与碗内切）一路收到末端。
fn tail_half(t: f32) -> f32 {
    TAIL_TIP_HALF + (TAIL_R0 - TAIL_TIP_HALF) * (1.0 - t * t).powf(TAPER)
}

/// 每个像素打多少个样本。4×4 足够：斜率都是直线段，再细看不出差别，
/// 但 16px 的图标只有十几个像素宽，不采样就全是锯齿。
pub const SUB: u32 = 4;

/// 渲染成 `w×h` 的黑白模板图 RGBA：RGB 全 0，alpha 是覆盖率。
///
/// 模板图（template image）只认 alpha，颜色由 AppKit 按菜单栏明暗决定；
/// 自己也画一遍黑白反而会在深浅切换时露馅。所以菜单栏这个标记**永远单色**，
/// 双色只出现在应用图标里（那条路由 `build.rs` 走）。
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

/// 「已暂停」时标记保留的不透明度。
///
/// 0.45 是试出来的档位：再高就分不出"淡"和"正常"，再低在 15pt 上几乎看不见
/// 笔画，只剩下一道斜杠漂在菜单栏里。
const PAUSED_DIM: f32 = 0.45;

/// 渲染成「已暂停」的模板图 RGBA：标记压淡，再斜着划一道。
///
/// 状态必须画出来，不能只写进菜单文案：菜单栏图标是这个程序唯一常驻的可见物，
/// 用户按下「暂停」之后要能一眼确认它还活着、还抢不抢 `;;`。模板图只有 alpha
/// （颜色由菜单栏明暗决定），所以只能用两个 alpha 层面的手段——变淡 + 划掉：
///
/// - 只变淡：在 15pt 上看起来像图标没画好；
/// - 只划掉：标记还是满黑，远看像正常运行。
///
/// 斜杠从左上划到右下（`\`），与「禁止」符号同向，落到这个图标上还能顺带
/// 从视觉上"划掉"那对分号。
pub fn paused_rgba(w: u32, h: u32, inset: f32) -> Vec<u8> {
    let bx = fit(w as f32, h as f32, inset);
    // 端点按输出尺寸取比例而不是设计格子：斜杠要跨满整个图标框，
    // 而设计格子只有 54 单位的字高，按格子算会剩一圈空白。
    let from = (w as f32 * 0.16, h as f32 * 0.10);
    let to = (w as f32 * 0.84, h as f32 * 0.90);
    let thickness = (h as f32 * 0.05).max(1.0);

    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0f32;
            for sy in 0..SUB {
                for sx in 0..SUB {
                    let px = x as f32 + (sx as f32 + 0.5) / SUB as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SUB as f32;
                    // 斜杠不透明、标记半透明；两者重叠仍按不透明算（斜杠在上）。
                    sum += if segment_distance(px, py, from, to) <= thickness {
                        1.0
                    } else if hit(bx, px, py) {
                        PAUSED_DIM
                    } else {
                        0.0
                    };
                }
            }
            let alpha = (sum / (SUB * SUB) as f32 * 255.0).round().clamp(0.0, 255.0) as u8;
            rgba.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    rgba
}

/// 点到线段的距离。斜杠的笔宽只有一两个像素，用"距离 <= 半宽"判定比
/// 逐行切片稳——后者在折点处会漏出缺口。
fn segment_distance(px: f32, py: f32, from: (f32, f32), to: (f32, f32)) -> f32 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return ((px - from.0).powi(2) + (py - from.1).powi(2)).sqrt();
    }
    // 投影参数夹到 [0,1]：超出两端时距离按端点算（圆头）。
    let t = (((px - from.0) * dx + (py - from.1) * dy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (from.0 + t * dx, from.1 + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 设计坐标 → 像素：用 100×100 的框跟设计格子 1:1，断言才好读。
    fn ink_at(x: f32, y: f32) -> bool {
        hit(
            (BOUNDS.0, BOUNDS.1, BOUNDS.2 - BOUNDS.0, BOUNDS.3 - BOUNDS.1),
            x,
            y,
        )
    }

    /// 应用图标里标记框的宽度：画布留白 20%，所以是 `size * (1 - 2 * 0.2)`。
    fn icon_box(size: u32) -> f32 {
        size as f32 * 0.6
    }

    #[test]
    fn strokes_are_where_they_should_be() {
        // 竖杠中线（斜切后仍在中线附近）、上下横杠、两个分号的点与尾巴
        assert!(ink_at(40.0, 50.0), "竖杠");
        assert!(ink_at(40.0, 26.0), "上横杠");
        assert!(ink_at(40.0, 74.0), "下横杠");
        assert!(ink_at(SEMI_X[0], SEMI_DOT.0), "第一个分号的点");
        assert!(ink_at(SEMI_X[1], SEMI_DOT.0), "第二个分号的点");
        assert!(ink_at(SEMI_X[1] - 2.6, 72.0), "第二个分号的尾巴");
    }

    #[test]
    fn gaps_stay_empty() {
        // 字母与分号之间、两个分号之间、圆点与尾巴之间、字形外圈都必须是空的
        assert!(!ink_at(56.0, 47.0), "I 与分号之间");
        assert!(!ink_at((SEMI_X[0] + SEMI_X[1]) / 2.0, 47.0), "两个分号之间");
        assert!(!ink_at(SEMI_X[0], 53.0), "圆点与尾巴之间");
        assert!(!ink_at(10.0, 50.0), "字形左侧");
        assert!(!ink_at(40.0, 12.0), "字形上方");
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
        assert!(top > bottom, "顶部左缘 {top} 应比底部左缘 {bottom} 更靠右");
    }

    #[test]
    fn bounds_are_tight() {
        // 包围盒要真的贴住笔画：右缘贴碗，上缘贴横杠，下缘贴尾巴末端的圆头
        let (x0, y0, x1, y1) = BOUNDS;
        assert!(ink_at(x1 - 0.5, SEMI_HEAD.0), "右边界应贴住碗");
        assert!(ink_at(40.0, y0 + 0.5), "上边界应贴住横杠");
        assert!(ink_at(x0 + 0.5, 76.0), "左边界应贴住下横杠");
        assert!(
            ink_at(SEMI_X[1] + TAIL_END.0, y1 - 0.5),
            "下边界应贴住尾巴末端"
        );
    }

    #[test]
    fn comma_tail_hugs_the_bowl() {
        // 颈部的圆与碗内切：错开量 + 颈部半径 == 碗半径。多一分右侧就长刺，
        // 少一分碗与尾巴之间就裂出一道缝。
        assert!(
            (NECK_OFFSET + TAIL_R0 - SEMI_HEAD.1).abs() < 1e-6,
            "颈部没有和碗内切：{:?}",
            NECK_OFFSET + TAIL_R0
        );
        // 起笔处（碗心那一行）的最右端必须正好是碗缘，不能再往外凸。
        assert!(ink_at(SEMI_X[1] + SEMI_HEAD.1 - 0.4, SEMI_HEAD.0), "碗缘");
        assert!(
            !ink_at(SEMI_X[1] + SEMI_HEAD.1 + 0.4, SEMI_HEAD.0),
            "碗缘右侧不该有墨——那是交界处顶出来的毛刺"
        );
    }

    #[test]
    fn punctuation_outweighs_the_letter() {
        // 「I 细、;; 重」是可测量的，不靠肉眼；但重的那一半是**逗号**，不是圆点。
        let stem = STEM_X.1 - STEM_X.0;
        let head = 2.0 * SEMI_HEAD.1;
        assert!(head >= stem * 1.3, "逗号脑袋 {head} 相对竖杠 {stem} 不够粗");

        // 圆点必须明显小于逗号脑袋。反过来（大点配细尾）就是字母 i 的结构，
        // 两个并排会读成「ii」——这条断言专门防这个回退。
        let dot = 2.0 * SEMI_DOT.1;
        assert!(
            dot < head * 0.8,
            "圆点 {dot} 太接近逗号脑袋 {head}，会读成 ii"
        );

        let count = |f: &dyn Fn(f32, f32) -> bool| {
            (0..100)
                .flat_map(|y| (0..100).map(move |x| (x, y)))
                .filter(|&(x, y)| f(x as f32 + 0.5, y as f32 + 0.5))
                .count()
        };
        let letter = count(&|x, y| italic_i(x, y, 0.0));
        let punct = count(&|x, y| semicolons(x, y, 0.0));
        let ratio = punct as f32 / letter as f32;
        assert!(ratio >= 0.6, "分号墨量只有字母的 {ratio:.2} 倍，压不住");
    }

    #[test]
    fn detail_is_dropped_when_too_small() {
        // 16px 的圆点只有 1.5 像素，画出来是一团灰；32px 有 3 像素，够用
        assert!(!has_detail(icon_box(16)), "16px 应当只画 I");
        assert!(has_detail(icon_box(32)), "32px 应当画出 ;;");
    }

    #[test]
    fn small_size_still_has_solid_pixels() {
        // 16px 只画 I，但光学加粗必须把它撑住——整片半透明等于没有图标
        let rgba = template_rgba(16, 16, 0.20);
        let solid = rgba.chunks_exact(4).filter(|p| p[3] > 200).count();
        assert!(solid >= 6, "16px 的实心像素只有 {solid} 个，太淡");
    }

    #[test]
    fn paused_icon_is_faded_and_slashed() {
        // 尺寸按 `tray::icon()` 的算法推（15pt × 2 倍图），测的就是真正塞给
        // NSStatusItem 那张图。
        let (w, h) = (40, 30);
        let normal = template_rgba(w, h, 0.02);
        let paused = paused_rgba(w, h, 0.02);
        assert_eq!(normal.len(), paused.len());

        let alpha = |rgba: &[u8], x: u32, y: u32| rgba[((y * w + x) * 4 + 3) as usize];

        // 四角仍然透明：斜杠两端都留了余量，不该顶到角上
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(alpha(&paused, x, y), 0, "角 ({x},{y}) 不透明");
        }

        // 斜杠中点必须实心。这个点本来就有墨，所以它同时验证了"斜杠盖在标记上"：
        // 如果斜杠没画出来，这里会继承压淡后的 alpha。
        let (mx, my) = (w / 2, h / 2);
        assert!(
            alpha(&paused, mx, my) > 240,
            "斜杠中点不够实：{}",
            alpha(&paused, mx, my)
        );

        // 标记整体要变淡：找"正常版很实、暂停版还在但明显更淡"的像素。
        // 少于十个就说明 PAUSED_DIM 没生效（或者被斜杠糊满了）。
        let faded = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                alpha(&normal, x, y) > 220 && (40..180).contains(&alpha(&paused, x, y))
            })
            .count();
        assert!(faded > 10, "标记没有被压淡（只有 {faded} 个像素变淡）");
    }

    #[test]
    fn distance_to_a_segment_handles_both_ends() {
        let from = (0.0, 0.0);
        let to = (10.0, 0.0);
        // 正中间：垂直距离
        assert!((segment_distance(5.0, 3.0, from, to) - 3.0).abs() < 1e-5);
        // 两端之外：按端点算，不按直线延长线算（否则斜杠两头会拖出尖角）
        assert!((segment_distance(-4.0, 0.0, from, to) - 4.0).abs() < 1e-5);
        assert!((segment_distance(14.0, 0.0, from, to) - 4.0).abs() < 1e-5);
        // 退化线段（两个端点重合）不能除零
        assert!(segment_distance(3.0, 4.0, from, from).is_finite());
    }

    #[test]
    fn corners_are_transparent() {
        // 菜单栏上不能出现一块若隐若现的底
        let w = 40;
        let h = 30;
        let rgba = template_rgba(w, h, 0.02);
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(
                rgba[((y * w + x) * 4 + 3) as usize],
                0,
                "角 ({x},{y}) 不透明"
            );
        }
    }
}
