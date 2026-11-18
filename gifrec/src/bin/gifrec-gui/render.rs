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

#[derive(Clone, Debug)]
pub enum Kind {
    Mosaic { block: u32 },
    Blur { radius: u32 },
    Text { text: String, size: f32, color: [u8; 4], bg: bool },
    Image { name: String, img: Arc<RgbaImage>, opacity: f32 },
    /// 拉近：這段影格把畫面平滑放大到這個範圍
    Zoom,
}

impl PartialEq for Kind {
    fn eq(&self, o: &Self) -> bool {
        use Kind::*;
        match (self, o) {
            (Mosaic { block: a }, Mosaic { block: b }) => a == b,
            (Blur { radius: a }, Blur { radius: b }) => a == b,
            (Text { text: t1, size: s1, color: c1, bg: b1 }, Text { text: t2, size: s2, color: c2, bg: b2 }) => {
                t1 == t2 && s1 == s2 && c1 == c2 && b1 == b2
            }
            (Image { name: n1, img: i1, opacity: o1 }, Image { name: n2, img: i2, opacity: o2 }) => {
                n1 == n2 && Arc::ptr_eq(i1, i2) && o1 == o2
            }
            (Zoom, Zoom) => true,
            _ => false,
        }
    }
}

impl Kind {
    /// 時間軸色條的顏色
    pub fn color(&self) -> [u8; 3] {
        match self {
            Kind::Mosaic { .. } => [235, 140, 50],
            Kind::Blur { .. } => [150, 120, 230],
            Kind::Text { .. } => [70, 170, 240],
            Kind::Image { .. } => [80, 190, 120],
            Kind::Zoom => [230, 80, 120],
        }
    }

    /// 大小是否鎖定比例（圖片、拉近）
    pub fn keeps_aspect(&self) -> bool {
        matches!(self, Kind::Image { .. } | Kind::Zoom)
    }

    pub fn label(&self) -> String {
        match self {
            Kind::Mosaic { .. } => "馬賽克".into(),
            Kind::Blur { .. } => "模糊".into(),
            Kind::Zoom => "拉近".into(),
            Kind::Text { text, .. } => {
                let t: String = text.lines().next().unwrap_or("").chars().take(10).collect();
                format!("文字「{t}」")
            }
            Kind::Image { name, .. } => format!("圖片 {name}"),
        }
    }
}

/// 一個編修效果。座標單位是 GIF 的像素。
#[derive(Clone, Debug, PartialEq)]
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
    Blur { x: u32, y: u32, w: u32, h: u32, radius: u32 },
    Bitmap { x: i32, y: i32, img: RgbaImage, opacity: f32 },
    None,
}

/// 效果範圍和畫面取交集（像素整數）
fn clip(item: &Item, fw: u32, fh: u32) -> Option<(u32, u32, u32, u32)> {
    let x = item.x.max(0.0) as u32;
    let y = item.y.max(0.0) as u32;
    let x2 = ((item.x + item.w).max(0.0).round() as u32).min(fw);
    let y2 = ((item.y + item.h).max(0.0).round() as u32).min(fh);
    (x < x2 && y < y2).then(|| (x, y, x2 - x, y2 - y))
}

pub fn prepare(item: &mut Item, font: Option<&FontVec>, fw: u32, fh: u32) -> Prepared {
    match &item.kind {
        Kind::Mosaic { block } => match clip(item, fw, fh) {
            Some((x, y, w, h)) => Prepared::Mosaic { x, y, w, h, block: (*block).max(2) },
            None => Prepared::None,
        },
        Kind::Blur { radius } => match clip(item, fw, fh) {
            Some((x, y, w, h)) => Prepared::Blur { x, y, w, h, radius: (*radius).max(1) },
            None => Prepared::None,
        },
        Kind::Zoom => Prepared::None,
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
            Prepared::Blur { x, y, w, h, radius } => blur(frame, *x, *y, *w, *h, *radius),
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

/// 區域模糊：三次方框模糊（近似高斯）。取樣會延伸到區域外一點，邊緣比較自然，
/// 但只寫回區域內的像素。
fn blur(img: &mut RgbaImage, x0: u32, y0: u32, w: u32, h: u32, radius: u32) {
    let (iw, ih) = img.dimensions();
    let m = radius * 2;
    let (ax, ay) = (x0.saturating_sub(m), y0.saturating_sub(m));
    let (bx, by) = ((x0 + w + m).min(iw), (y0 + h + m).min(ih));
    let (cw, ch) = ((bx - ax) as usize, (by - ay) as usize);
    let mut buf: Vec<[f32; 3]> = Vec::with_capacity(cw * ch);
    for y in ay..by {
        for x in ax..bx {
            let p = img.get_pixel(x, y).0;
            buf.push([p[0] as f32, p[1] as f32, p[2] as f32]);
        }
    }
    let r = (radius as usize).max(1);
    let mut tmp = buf.clone();
    for _ in 0..3 {
        box_pass(&buf, &mut tmp, cw, ch, r, true);
        box_pass(&tmp, &mut buf, cw, ch, r, false);
    }
    for y in y0..(y0 + h).min(ih) {
        for x in x0..(x0 + w).min(iw) {
            let v = buf[(y - ay) as usize * cw + (x - ax) as usize];
            let p = img.get_pixel_mut(x, y);
            p.0[0] = v[0].round() as u8;
            p.0[1] = v[1].round() as u8;
            p.0[2] = v[2].round() as u8;
        }
    }
}

/// 一維滑動平均（邊界用最近的像素補齊）
fn box_pass(src: &[[f32; 3]], dst: &mut [[f32; 3]], w: usize, h: usize, r: usize, horizontal: bool) {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let n = (2 * r + 1) as f32;
    for line in 0..lines {
        let get = |i: isize| src[at(line, i.clamp(0, len as isize - 1) as usize)];
        let mut acc = [0f32; 3];
        for i in -(r as isize)..=(r as isize) {
            let v = get(i);
            for c in 0..3 {
                acc[c] += v[c];
            }
        }
        for i in 0..len {
            dst[at(line, i)] = [acc[0] / n, acc[1] / n, acc[2] / n];
            let add = get(i as isize + r as isize + 1);
            let sub = get(i as isize - r as isize);
            for c in 0..3 {
                acc[c] += add[c] - sub[c];
            }
        }
    }
}

// ───────────────────── 畫面裁切、拉近、輸出尺寸 ─────────────────────

/// 一份編輯內容（可以整份複製，用於復原 / 重做與專案存檔）
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub items: Vec<Item>,
    /// 保留的影格範圍（含頭含尾）
    pub trim: (usize, usize),
    /// 輸出的畫面範圍 x, y, w, h（原始 GIF 像素）
    pub crop: [u32; 4],
    /// 輸出寬度，0 = 與裁切範圍相同
    pub out_w: u32,
}

impl Doc {
    pub fn new(w: u32, h: u32, frames: usize) -> Self {
        Doc { items: Vec::new(), trim: (0, frames.saturating_sub(1)), crop: [0, 0, w, h], out_w: 0 }
    }

    pub fn out_size(&self) -> (u32, u32) {
        let [_, _, cw, ch] = self.crop;
        if self.out_w == 0 || self.out_w >= cw {
            (cw, ch)
        } else {
            let w = self.out_w.max(16);
            (w, ((ch as u64 * w as u64) / cw as u64).max(1) as u32)
        }
    }
}

/// 把矩形調成指定長寬比（以中心為準往外擴），並盡量放進畫面內
pub fn fit_aspect(r: [f32; 4], aspect: f32, fw: f32, fh: f32) -> [f32; 4] {
    let [x, y, w, h] = r;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (mut nw, mut nh) = if w / h.max(1.0) > aspect { (w, w / aspect) } else { (h * aspect, h) };
    if nw > fw {
        nw = fw;
        nh = fw / aspect;
    }
    if nh > fh {
        nh = fh;
        nw = fh * aspect;
    }
    let nx = (cx - nw / 2.0).clamp(0.0, (fw - nw).max(0.0));
    let ny = (cy - nh / 2.0).clamp(0.0, (fh - nh).max(0.0));
    [nx, ny, nw, nh]
}

/// 第 idx 格要輸出的畫面範圍：平常是裁切範圍；遇到「拉近」就在前後幾格平滑過渡
pub fn view_rect(doc: &Doc, idx: usize, fw: u32, fh: u32) -> [f32; 4] {
    let [cx, cy, cw, ch] = doc.crop;
    let base = [cx as f32, cy as f32, cw as f32, ch as f32];
    let Some(z) = doc.items.iter().find(|it| it.kind == Kind::Zoom && it.visible(idx)) else {
        return base;
    };
    let target = fit_aspect([z.x, z.y, z.w, z.h], cw as f32 / ch as f32, fw as f32, fh as f32);
    let len = z.end - z.start + 1;
    let ramp = (len / 3).clamp(1, 5) as f32; // 拉近 / 拉遠各用最多 5 格
    let k = ((idx - z.start) as f32 + 1.0).min((z.end - idx) as f32 + 1.0);
    let t = (k / ramp).min(1.0);
    let t = t * t * (3.0 - 2.0 * t); // smoothstep
    let mut out = [0.0; 4];
    for i in 0..4 {
        out[i] = base[i] + (target[i] - base[i]) * t;
    }
    out
}

/// 完整輸出一格：套效果 → 取畫面範圍（裁切 / 拉近）→ 縮放到輸出尺寸
pub fn compose(frame: &RgbaImage, doc: &Doc, preps: &[Prepared], idx: usize) -> RgbaImage {
    let mut img = frame.clone();
    apply(&mut img, &doc.items, preps, idx);
    let (fw, fh) = img.dimensions();
    let (ow, oh) = doc.out_size();
    let v = view_rect(doc, idx, fw, fh);
    sample(&img, v, ow, oh)
}

/// 從浮點範圍取樣成 ow×oh。整數範圍且不縮放時直接裁切；
/// 否則先用 Triangle 縮放（縮小時不會鋸齒），再以雙線性補上小數位移（拉近時不會抖動）。
pub fn sample(src: &RgbaImage, v: [f32; 4], ow: u32, oh: u32) -> RgbaImage {
    let (sw, sh) = src.dimensions();
    let [vx, vy, vw, vh] = v;
    let integral = v.iter().all(|f| (f - f.round()).abs() < 1e-3);
    if integral && vw.round() as u32 == ow && vh.round() as u32 == oh {
        return imageops::crop_imm(src, vx.round() as u32, vy.round() as u32, ow, oh).to_image();
    }
    let bx0 = vx.floor().max(0.0) as u32;
    let by0 = vy.floor().max(0.0) as u32;
    let bx1 = ((vx + vw).ceil() as u32).clamp(bx0 + 1, sw);
    let by1 = ((vy + vh).ceil() as u32).clamp(by0 + 1, sh);
    let sx = ow as f32 / vw; // 輸出像素 / 原始像素
    let sy = oh as f32 / vh;
    let rw = (((bx1 - bx0) as f32 * sx).round() as u32).max(1);
    let rh = (((by1 - by0) as f32 * sy).round() as u32).max(1);
    let sub = imageops::crop_imm(src, bx0, by0, bx1 - bx0, by1 - by0).to_image();
    let big = if (rw, rh) == sub.dimensions() { sub } else { imageops::resize(&sub, rw, rh, imageops::FilterType::Triangle) };
    let fx = rw as f32 / (bx1 - bx0) as f32;
    let fy = rh as f32 / (by1 - by0) as f32;
    let (ox, oy) = ((vx - bx0 as f32) * fx, (vy - by0 as f32) * fy);
    let (bw, bh) = (big.width() as i32, big.height() as i32);
    let raw = big.as_raw();
    let px = |x: i32, y: i32| {
        let i = ((y.clamp(0, bh - 1) * bw + x.clamp(0, bw - 1)) * 4) as usize;
        [raw[i] as f32, raw[i + 1] as f32, raw[i + 2] as f32, raw[i + 3] as f32]
    };
    RgbaImage::from_fn(ow, oh, |x, y| {
        let gx = ox + x as f32 * (vw * fx / ow as f32) + 0.0;
        let gy = oy + y as f32 * (vh * fy / oh as f32) + 0.0;
        let (x0, y0) = (gx.floor(), gy.floor());
        let (tx, ty) = (gx - x0, gy - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let (a, b, c, d) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
        let mut o = [0u8; 4];
        for i in 0..4 {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bot = c[i] + (d[i] - c[i]) * tx;
            o[i] = (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8;
        }
        Rgba(o)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| if (x / 8 + y / 8) % 2 == 0 { Rgba([255, 255, 255, 255]) } else { Rgba([0, 0, 0, 255]) })
    }

    #[test]
    fn crop_is_exact_and_resize_keeps_aspect() {
        let img = grid(200, 100);
        let mut doc = Doc::new(200, 100, 10);
        doc.crop = [16, 8, 80, 40];
        let out = compose(&img, &doc, &[], 0);
        assert_eq!(out.dimensions(), (80, 40));
        assert_eq!(out.get_pixel(0, 0), img.get_pixel(16, 8));
        doc.out_w = 40;
        assert_eq!(doc.out_size(), (40, 20));
        assert_eq!(compose(&img, &doc, &[], 0).dimensions(), (40, 20));
    }

    #[test]
    fn zoom_eases_in_and_out() {
        let mut doc = Doc::new(200, 100, 30);
        doc.items.push(Item { kind: Kind::Zoom, x: 50.0, y: 25.0, w: 50.0, h: 25.0, start: 10, end: 20 });
        let w = |i| view_rect(&doc, i, 200, 100)[2];
        assert_eq!(w(5), 200.0); // 範圍外：完整畫面
        assert!(w(10) < 200.0 && w(10) > 50.0); // 開始過渡
        assert!((w(15) - 50.0).abs() < 0.01); // 中段：完全拉近
        assert!(w(20) > 50.0); // 結束過渡
        // 輸出尺寸不變
        assert_eq!(compose(&grid(200, 100), &doc, &[], 15).dimensions(), (200, 100));
    }

    #[test]
    fn blur_only_touches_its_rect() {
        let mut img = grid(64, 64);
        let before = img.clone();
        blur(&mut img, 16, 16, 32, 32, 4);
        assert_eq!(img.get_pixel(5, 5), before.get_pixel(5, 5));
        let p = img.get_pixel(32, 32).0[0];
        assert!(p > 40 && p < 215, "應被模糊成灰色，實際 {p}");
    }
}
