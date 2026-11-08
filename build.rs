//! 构建脚本：把「I;;」标记渲成应用图标，顺便出两张预览图。
//!
//! 图标是**生成**的，仓库里不放二进制素材：图形在 `src/mark.rs` 里是一组
//! 解析式矩形，构建期按需渲染成任意尺寸，想要多大就有多大，也不会出现
//! 「源文件是 1024 但某处贴了张 256」这种漂移。
//!
//! 配色是**双色**的：斜体 `I` 近白，两个分号琥珀。菜单栏图标反过来必须
//! 单色（模板图由系统按外观上色），所以分色只发生在这一层——`tray.rs` 那边
//! 调的仍然是 `mark::template_rgba`，只有 alpha。
//!
//! 产出（都在 `packaging/macos/`，内容没变时不重写，免得刷 mtime）：
//! - `AppIcon.icns`：10 个槽位（16…1024，含 @2x），PNG 负载，手写的 ICNS 容器
//! - `icon-preview.png`：512px 预览，README 里贴的就是它
//! - `tray-preview.png`：菜单栏图标在深/浅底色上的对照图（正常 / 暂停两行）
//!
//! 为什么自己拼 ICNS 而不调 `iconutil`：iconutil 只在 macOS 上有，而且得先
//! 落一盘 `.iconset` 目录；ICNS 容器就是「魔数 + 一串 (类型, 长度, 数据)」，
//! 二十行就写完了，还能顺带让 Linux 上的 `cargo build` 也不至于中断。

use std::path::{Path, PathBuf};

#[path = "src/mark.rs"]
#[allow(dead_code)]
mod mark;

/// 图标按 1024 的逻辑格设计，其他尺寸都由它缩放而来。
const CANVAS: u32 = 1024;
/// 圆角方块四周留 100：macOS 的应用图标栅格就是 824×824 居中。
const INSET: f32 = 100.0;
const RADIUS: f32 = 185.0;

type Rgb = (u8, u8, u8);

const TOP: Rgb = (43, 48, 59); // #2B303B
const BOTTOM: Rgb = (18, 21, 26); // #12151A
/// 字母：近白而非纯白——纯白在浅色壁纸上会发飘。
const LETTER: Rgb = (247, 248, 250); // #F7F8FA
/// 分号：琥珀。深色底上这个色最"跳"，而且和白色字母并排不会互相吃掉。
const PUNCT: Rgb = (255, 158, 100); // #FF9E64

/// ICNS 的槽位：类型标签 + 该标签要求的像素边长。同一个尺寸出现两次是正常的
/// ——`icp5` 是 32×32@1x，`ic11` 是 16×16@2x，系统按屏幕倍率挑。
const SLOTS: [(&[u8; 4], u32); 10] = [
    (b"icp4", 16),
    (b"ic11", 32),
    (b"icp5", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

fn main() {
    // 标记改了图标就得重画；除此之外没有别的输入。
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/mark.rs");

    let out_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("packaging")
        .join("macos");

    let mut chunks = Vec::new();
    for (tag, size) in SLOTS {
        chunks.push((tag, encode_png(&render(size), size, size)));
    }
    write_if_changed(&out_dir.join("AppIcon.icns"), &icns(&chunks));

    let preview = render(512);
    write_if_changed(
        &out_dir.join("icon-preview.png"),
        &encode_png(&preview, 512, 512),
    );

    let (tray, tw, th) = tray_preview();
    write_if_changed(
        &out_dir.join("tray-preview.png"),
        &encode_png(&tray, tw, th),
    );
}

/// 渲染一张 `size×size` 的双色图标。
///
/// 每个尺寸都从零开始采样，不做「渲大图再缩」：16px 的图标只有十几像素宽，
/// 缩下来的笔画会糊成一团灰，而解析式渲染在这个尺寸上还能保持两三像素的实心。
fn render(size: u32) -> Vec<u8> {
    let s = size as f32;
    let inset = INSET * s / CANVAS as f32;
    let (lo, hi) = (inset, s - inset);
    let radius = RADIUS * s / CANVAS as f32;
    let bx = mark::fit(s, s, 0.20);

    let mut buf = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut body, mut letter, mut punct) = (0u32, 0u32, 0u32);
            for sy in 0..mark::SUB {
                for sx in 0..mark::SUB {
                    let px = x as f32 + (sx as f32 + 0.5) / mark::SUB as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / mark::SUB as f32;
                    if in_squircle(px, py, lo, hi, radius) {
                        body += 1;
                    }
                    match mark::sample(bx, px, py) {
                        Some(mark::Part::Letter) => letter += 1,
                        Some(mark::Part::Punct) => punct += 1,
                        None => {}
                    }
                }
            }
            let n = mark::SUB * mark::SUB;
            let i = ((y * size + x) * 4) as usize;
            if body == 0 {
                continue;
            }
            // 底色沿 y 方向从 TOP 渐变到 BOTTOM，像被一盏顶灯照着。
            let t = ((y as f32 - lo) / (hi - lo)).clamp(0.0, 1.0);
            let bg = mix(TOP, BOTTOM, t);
            let fg = blot(bg, letter, punct, n);
            buf[i] = fg.0;
            buf[i + 1] = fg.1;
            buf[i + 2] = fg.2;
            buf[i + 3] = (body * 255 / n) as u8;
        }
    }
    buf
}

/// 把两种墨合成一层再铺到渐变底上。
///
/// 先按各自的样本数求出这一像素的"平均墨色"，再整体乘上总覆盖率——不是
/// 分别两次混合。两次混合会在字母与分号的交界处叠出第三种颜色（两边的
/// 抗锯齿都是半透明，叠起来比谁都深），边缘会有一圈脏线。
fn blot(bg: Rgb, letter: u32, punct: u32, n: u32) -> Rgb {
    let ink = letter + punct;
    if ink == 0 {
        return bg;
    }
    let k = 1.0 / ink as f32;
    let mixed = (
        (LETTER.0 as f32 * letter as f32 + PUNCT.0 as f32 * punct as f32) * k,
        (LETTER.1 as f32 * letter as f32 + PUNCT.1 as f32 * punct as f32) * k,
        (LETTER.2 as f32 * letter as f32 + PUNCT.2 as f32 * punct as f32) * k,
    );
    mix_f(bg, mixed, ink as f32 / n as f32)
}

/// 圆角方块：把点夹到「内矩形」上再比距离，矩形内部距离为 0，四角自然圆掉。
fn in_squircle(x: f32, y: f32, lo: f32, hi: f32, r: f32) -> bool {
    let (cx, cy) = (x.clamp(lo + r, hi - r), y.clamp(lo + r, hi - r));
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

fn mix_f(a: Rgb, b: (f32, f32, f32), t: f32) -> Rgb {
    let f = |a: u8, b: f32| (a as f32 + (b - a as f32) * t).round().clamp(0.0, 255.0) as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

/// 菜单栏图标在深/浅底色上的样子（放大 8 倍），用来确认 15pt 下还认得出来。
///
/// 两行两列：上排「正常」、下排「暂停」，左右分别是深色与浅色菜单栏。
/// 暂停版式不是装饰——它是这张对照图存在的另一半理由：状态切换后必须还能
/// 认出是同一个标记被划掉了，而不是变成一团糊。
fn tray_preview() -> (Vec<u8>, u32, u32) {
    // 与 `tray.rs` 里完全相同的调用：同一份几何、同一个尺寸。
    let (w, h) = (40u32, 30u32);
    let variants = [
        mark::template_rgba(w, h, 0.02),
        mark::paused_rgba(w, h, 0.02),
    ];
    // 深色菜单栏画白、浅色画黑——模板图由系统反色，这里把两种都画出来。
    let backgrounds = [((40u8, 44u8, 52u8), 255u8), ((242, 242, 245), 28)];

    let zoom = 8;
    let pad = 14;
    let gap = 20;
    let (box_w, box_h) = (pad * 2 + w * zoom, pad * 2 + h * zoom);
    let (total_w, total_h) = (box_w * 2 + gap, box_h * 2 + gap);
    let mut buf = vec![0u8; (total_w * total_h * 4) as usize];

    for (row, alpha) in variants.iter().enumerate() {
        let oy = row as u32 * (box_h + gap);
        for (col, (bg, ink)) in backgrounds.into_iter().enumerate() {
            let ox = col as u32 * (box_w + gap);
            for y in 0..box_h {
                for x in 0..box_w {
                    let j = (((oy + y) * total_w + ox + x) * 4) as usize;
                    buf[j..j + 4].copy_from_slice(&[bg.0, bg.1, bg.2, 255]);
                }
            }
            for y in 0..h {
                for x in 0..w {
                    let a = alpha[((y * w + x) * 4 + 3) as usize] as u32;
                    if a == 0 {
                        continue;
                    }
                    for dy in 0..zoom {
                        for dx in 0..zoom {
                            let px = ox + pad + x * zoom + dx;
                            let py = oy + pad + y * zoom + dy;
                            let j = ((py * total_w + px) * 4) as usize;
                            // 覆盖率当不透明度铺在底色上，边缘才有过渡
                            for (k, base) in [bg.0, bg.1, bg.2].into_iter().enumerate() {
                                buf[j + k] =
                                    ((base as u32 * (255 - a) + ink as u32 * a) / 255) as u8;
                            }
                        }
                    }
                }
            }
        }
    }
    (buf, total_w, total_h)
}

/// 把若干 (标签, PNG) 拼成 ICNS：魔数 + 总长 + 每块的「标签 + 长度 + 数据」。
fn icns(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (tag, png) in chunks {
        body.extend_from_slice(*tag);
        body.extend_from_slice(&((png.len() + 8) as u32).to_be_bytes());
        body.extend_from_slice(png);
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn encode_png(buf: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(std::io::Cursor::new(&mut out), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(buf).unwrap();
    }
    out
}

/// 内容一致时不动 mtime：build.rs 每次构建都会跑，无脑写会让下游的
/// 「文件变了就重建」逻辑空转。
fn write_if_changed(path: &Path, bytes: &[u8]) {
    if let Ok(existing) = std::fs::read(path) {
        if existing == bytes {
            return;
        }
    }
    std::fs::write(path, bytes).unwrap();
}
