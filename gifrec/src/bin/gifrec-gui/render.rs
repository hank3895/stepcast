//! 編修效果的實際像素處理：預覽與匯出共用同一套，畫面所見即所得。

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use image::{codecs::gif::GifDecoder, imageops, AnimationDecoder, Rgba, RgbaImage};
use std::{fs::File, io::BufReader, path::Path, sync::Arc};

pub struct Frame {
    pub rgba: RgbaImage,
    pub delay_ms: u32,
}

/// 讀入 GIF 並展開成完整的 RGBA 影格（已處理 GIF 的疊加 / 清除規則）
pub fn load_gif(path: &Path) -> Result<Vec<Frame>, String> {
    let f = File::open(path).map_err(|e| format!("無法開啟檔案：{e}"))?;
    let dec = GifDecoder::new(BufReader::new(f)).map_err(|e| format!("不是有效的 GIF：{e}"))?;
    let frames = dec.into_frames().collect_frames().map_err(|e| format!("解碼失敗：{e}"))?;
    if frames.is_empty() {
        return Err("GIF 裡沒有任何影格".into());
    }
    Ok(frames
        .into_iter()
        .map(|fr| {
            let (n, d) = fr.delay().numer_denom_ms();
            let ms = if d == 0 { 100 } else { n / d };
            // 瀏覽器會把 <20ms 當成 100ms，這裡比照
            let delay_ms = if ms < 20 { 100 } else { ms };
            Frame { rgba: fr.into_buffer(), delay_ms }
        })
        .collect())
}

#[derive(Clone)]
pub enum Kind {
    Mosaic { block: u32 },
    Text { text: String, size: f32, color: [u8; 4], bg: bool },
    Image { name: String, img: Arc<RgbaImage>, opacity: f32 },
}

impl Kind {
    pub fn label(&self) -> String {
        match self {
            Kind::Mosaic { .. } => "馬賽克".into(),
            Kind::Text { text, .. } => {
                let t: String = text.lines().next().unwrap_or("").chars().take(10).collect();
                format!("文字「{t}」")
            }
            Kind::Image { name, .. } => format!("圖片 {name}"),
        }
    }
}

/// 一個編修效果。座標單位是 GIF 的像素。
#[derive(Clone)]
pub struct Item {
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 出現的影格範圍（含頭含尾）
    pub start: usize,
    pub end: usize,
}

impl Item {
    pub fn visible(&self, idx: usize) -> bool {
        idx >= self.start && idx <= self.end
    }
}

/// 事先算好的效果：文字先畫成小圖、圖片先縮放好，每格只需要貼上
pub enum Prepared {
    Mosaic { x: u32, y: u32, w: u32, h: u32, block: u32 },
    Bitmap { x: i32, y: i32, img: RgbaImage, opacity: f32 },
    None,
}

pub fn prepare(item: &mut Item, font: Option<&FontVec>, fw: u32, fh: u32) -> Prepared {
    match &item.kind {
        Kind::Mosaic { block } => {
            let x = item.x.max(0.0) as u32;
            let y = item.y.max(0.0) as u32;
            let x2 = ((item.x + item.w).max(0.0) as u32).min(fw);
            let y2 = ((item.y + item.h).max(0.0) as u32).min(fh);
            if x >= x2 || y >= y2 {
                return Prepared::None;
            }
            Prepared::Mosaic { x, y, w: x2 - x, h: y2 - y, block: (*block).max(2) }
        }
        Kind::Text { text, size, color, bg } => {
            let Some(font) = font else { return Prepared::None };
            let img = render_text(text, *size, *color, *bg, font);
            // 文字的大小由內容與字級決定，回寫給拖曳判定用
            item.w = img.width() as f32;
            item.h = img.height() as f32;
            Prepared::Bitmap { x: item.x as i32, y: item.y as i32, img, opacity: 1.0 }
        }
        Kind::Image { img, opacity, .. } => {
            let w = item.w.round().max(1.0) as u32;
            let h = item.h.round().max(1.0) as u32;
            let scaled = if (w, h) == img.dimensions() {
                (**img).clone()
            } else {
                imageops::resize(&**img, w, h, imageops::FilterType::Triangle)
            };
            Prepared::Bitmap { x: item.x as i32, y: item.y as i32, img: scaled, opacity: *opacity }
        }
    }
}

/// 把效果依序套到一張影格上
pub fn apply(frame: &mut RgbaImage, items: &[Item], preps: &[Prepared], idx: usize) {
    for (it, p) in items.iter().zip(preps) {
        if !it.visible(idx) {
            continue;
        }
        match p {
            Prepared::Mosaic { x, y, w, h, block } => mosaic(frame, *x, *y, *w, *h, *block),
            Prepared::Bitmap { x, y, img, opacity } => blit(frame, img, *x, *y, *opacity),
            Prepared::None => {}
        }
    }
}

fn mosaic(img: &mut RgbaImage, x0: u32, y0: u32, w: u32, h: u32, block: u32) {
    let (x1, y1) = ((x0 + w).min(img.width()), (y0 + h).min(img.height()));
    let mut by = y0;
    while by < y1 {
        let bh = block.min(y1 - by);
        let mut bx = x0;
        while bx < x1 {
            let bw = block.min(x1 - bx);
            let mut sum = [0u64; 4];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let p = img.get_pixel(x, y).0;
                    for c in 0..4 {
                        sum[c] += p[c] as u64;
                    }
                }
            }
            let n = (bw * bh) as u64;
            let avg = Rgba([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8]);
            for y in by..by + bh {
                for x in bx..bx + bw {
                    img.put_pixel(x, y, avg);
                }
            }
            bx += bw;
        }
        by += bh;
    }
}

/// 以 alpha 混合把 src 貼到 dst 的 (x, y)
fn blit(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32, opacity: f32) {
    let (dw, dh) = (dst.width() as i32, dst.height() as i32);
    for sy in 0..src.height() as i32 {
        let dy = y + sy;
        if dy < 0 || dy >= dh {
            continue;
        }
        for sx in 0..src.width() as i32 {
            let dx = x + sx;
            if dx < 0 || dx >= dw {
                continue;
            }
            let s = src.get_pixel(sx as u32, sy as u32).0;
            let a = s[3] as f32 / 255.0 * opacity;
            if a <= 0.0 {
                continue;
            }
            let d = dst.get_pixel_mut(dx as u32, dy as u32);
            over(&mut d.0, [s[0], s[1], s[2]], a);
        }
    }
}

/// 標準的 source-over 混合（非預乘）
fn over(d: &mut [u8; 4], s: [u8; 3], sa: f32) {
    let da = d[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return;
    }
    for c in 0..3 {
        let v = (s[c] as f32 * sa + d[c] as f32 * da * (1.0 - sa)) / oa;
        d[c] = v.round().clamp(0.0, 255.0) as u8;
    }
    d[3] = (oa * 255.0).round() as u8;
}

/// 把文字畫成一張透明底（或半透明黑底）的小圖，支援多行
pub fn render_text(text: &str, size: f32, color: [u8; 4], bg: bool, font: &FontVec) -> RgbaImage {
    let scale = PxScale::from(size.max(6.0));
    let sf = font.as_scaled(scale);
    let lines: Vec<&str> = if text.is_empty() { vec![" "] } else { text.lines().collect() };
    let line_h = sf.height() + sf.line_gap();
    let pad = if bg { (size * 0.35).ceil() } else { (size * 0.08).ceil() + 2.0 };

    let width_of = |line: &str| {
        let mut w = 0.0;
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                w += sf.kern(p, id);
            }
            w += sf.h_advance(id);
            prev = Some(id);
        }
        w
    };
    let max_w = lines.iter().map(|l| width_of(l)).fold(0.0f32, f32::max);
    let (w, h) = ((max_w + pad * 2.0).ceil() as u32, (line_h * lines.len() as f32 + pad * 2.0).ceil() as u32);
    let mut img = RgbaImage::from_pixel(w.max(1), h.max(1), Rgba(if bg { [0, 0, 0, 160] } else { [0, 0, 0, 0] }));

    // 沒有底色時加一層淡陰影，淺色背景上也看得清楚
    let shadow = (size / 18.0).ceil().max(1.0);
    let passes: &[(f32, [u8; 3], f32)] = if bg {
        &[(0.0, [0, 0, 0], 0.0)]
    } else {
        &[(shadow, [0, 0, 0], 0.65), (0.0, [0, 0, 0], 0.0)]
    };
    for &(off, shade, shade_a) in passes {
        let is_shadow = shade_a > 0.0;
        for (i, line) in lines.iter().enumerate() {
            let baseline = pad + sf.ascent() + line_h * i as f32;
            let mut caret = pad;
            let mut prev = None;
            for ch in line.chars() {
                let id = font.glyph_id(ch);
                if let Some(p) = prev {
                    caret += sf.kern(p, id);
                }
                let g = id.with_scale_and_position(scale, point(caret + off, baseline + off));
                if let Some(og) = font.outline_glyph(g) {
                    let b = og.px_bounds();
                    og.draw(|gx, gy, cov| {
                        let px = b.min.x as i32 + gx as i32;
                        let py = b.min.y as i32 + gy as i32;
                        if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                            return;
                        }
                        let d = img.get_pixel_mut(px as u32, py as u32);
                        if is_shadow {
                            over(&mut d.0, shade, cov * shade_a * color[3] as f32 / 255.0);
                        } else {
                            over(&mut d.0, [color[0], color[1], color[2]], cov * color[3] as f32 / 255.0);
                        }
                    });
                }
                caret += sf.h_advance(id);
                prev = Some(id);
            }
        }
    }
    img
}

/// 從系統讀一套支援中文的字型（.ttc 取第一套）
pub fn load_font_bytes() -> Option<Vec<u8>> {
    [
        r"C:\Windows\Fonts\msjh.ttc", // 微軟正黑體
        r"C:\Windows\Fonts\msyh.ttc", // 微軟雅黑
        r"C:\Windows\Fonts\mingliu.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ]
    .iter()
    .find_map(|p| std::fs::read(p).ok())
}

pub fn parse_font(bytes: &[u8]) -> Option<FontVec> {
    FontVec::try_from_vec_and_index(bytes.to_vec(), 0).ok()
}
