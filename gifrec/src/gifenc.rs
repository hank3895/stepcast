//! GIF 編碼器（MIT 授權相依：gif + color_quant）
//!
//! 為了讓檔案小，每張影格只寫「和上一張不同」的部分：
//! 1. 找出變動區域的外框，只寫那一塊（裁切）
//! 2. 外框內沒變的像素設成透明，LZW 壓縮率更好
//! 3. 每格有自己的調色盤；顏色 ≤ 255 種時用精確色（操作畫面常見，文字更清楚），
//!    超過才用 NeuQuant 量化。第 255 號色固定留給透明。
//!
//! 編碼在背景執行緒進行，呼叫端只需要 add() 送影格。

use anyhow::{anyhow, bail, Context, Result};
use color_quant::NeuQuant;
use gif::{DisposalMethod, Encoder, Frame, Repeat};
use image::{imageops, RgbaImage};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::File,
    io::BufWriter,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
};

const TRANSPARENT: u8 = 255;

#[derive(Clone)]
pub struct Settings {
    /// 輸出寬度上限（等比縮小），0 = 原尺寸
    pub max_width: u32,
    /// 1–100，越高越好但越慢越大
    pub quality: u8,
    /// 每寫完一格就 +1，可用來顯示進度
    pub progress: Option<Arc<AtomicUsize>>,
}

enum Msg {
    Frame(RgbaImage, f64),
    Finish(f64),
}

pub struct GifWriter {
    tx: Option<SyncSender<Msg>>,
    handle: Option<JoinHandle<Result<()>>>,
}

impl GifWriter {
    pub fn create(path: &Path, settings: Settings) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("無法建立 {}", path.display()))?;
        // 佇列最多 8 張，避免截圖比編碼快時把記憶體吃光
        let (tx, rx) = sync_channel::<Msg>(8);
        let handle = thread::spawn(move || -> Result<()> {
            let mut w = Worker::new(BufWriter::new(file), settings);
            for msg in rx {
                match msg {
                    Msg::Frame(img, pts) => w.push(img, pts)?,
                    Msg::Finish(end) => return w.finish(end),
                }
            }
            bail!("編碼被中斷")
        });
        Ok(GifWriter { tx: Some(tx), handle: Some(handle) })
    }

    /// 加入一張影格；pts = 這張開始顯示的時間（秒）
    pub fn add(&self, img: RgbaImage, pts: f64) -> Result<()> {
        let tx = self.tx.as_ref().unwrap();
        if tx.send(Msg::Frame(img, pts)).is_err() {
            // 背景執行緒已經出錯結束，錯誤訊息在 finish() 時回報
            bail!("編碼執行緒已停止");
        }
        Ok(())
    }

    /// 寫完檔案；end = 最後一張結束顯示的時間（秒）
    pub fn finish(mut self, end: f64) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Finish(end));
        }
        self.join()
    }

    fn join(&mut self) -> Result<()> {
        match self.handle.take() {
            Some(h) => h.join().map_err(|_| anyhow!("編碼執行緒崩潰"))?,
            None => Ok(()),
        }
    }

    /// 放棄編碼（例如使用者取消），不寫結尾
    pub fn abort(mut self) {
        self.tx.take();
        let _ = self.join();
    }
}

impl Drop for GifWriter {
    fn drop(&mut self) {
        self.tx.take();
        let _ = self.join();
    }
}

struct Worker {
    out: Option<BufWriter<File>>,
    enc: Option<Encoder<BufWriter<File>>>,
    settings: Settings,
    size: (u32, u32),
    /// 目前畫面上「看起來」的內容（未量化前的來源像素），用來判斷哪裡變了
    reference: Option<RgbaImage>,
    /// 延後一格寫入：要知道下一張的時間才能算出這張的停留時間
    pending: Option<(RgbaImage, f64)>,
    /// 已寫入的時間（百分之一秒），避免四捨五入誤差累積
    written_cs: i64,
    speed: i32,
    tolerance: i32,
}

impl Worker {
    fn new(out: BufWriter<File>, settings: Settings) -> Self {
        let q = settings.quality.clamp(1, 100) as i32;
        Worker {
            out: Some(out),
            enc: None,
            size: (0, 0),
            reference: None,
            pending: None,
            written_cs: 0,
            // 畫質 → NeuQuant 取樣速度（1 最好、30 最快）
            speed: (31 - q * 30 / 100).clamp(1, 30),
            // 畫質越低，越小的差異就當作沒變（檔案更小）
            tolerance: ((100 - q) / 8).clamp(0, 12),
            settings,
        }
    }

    fn push(&mut self, img: RgbaImage, pts: f64) -> Result<()> {
        let img = self.fit(img);
        if let Some((prev, prev_pts)) = self.pending.take() {
            self.write(prev, prev_pts, pts)?;
        }
        self.pending = Some((img, pts));
        Ok(())
    }

    fn finish(&mut self, end: f64) -> Result<()> {
        if let Some((prev, prev_pts)) = self.pending.take() {
            self.write(prev, prev_pts, end.max(prev_pts + 0.02))?;
        }
        if self.enc.is_none() {
            bail!("沒有任何影格");
        }
        // Encoder 被 drop 時會寫入 GIF 結尾
        drop(self.enc.take());
        Ok(())
    }

    /// 縮放到輸出尺寸；第一張決定整個 GIF 的大小
    fn fit(&mut self, img: RgbaImage) -> RgbaImage {
        if self.size == (0, 0) {
            let (w, h) = img.dimensions();
            let mw = self.settings.max_width;
            self.size = if mw > 0 && w > mw {
                (mw, ((h as u64 * mw as u64) / w as u64).max(1) as u32)
            } else {
                (w, h)
            };
            // GIF 尺寸上限 65535
            self.size = (self.size.0.min(65535), self.size.1.min(65535));
        }
        if img.dimensions() == self.size {
            img
        } else {
            imageops::resize(&img, self.size.0, self.size.1, imageops::FilterType::Triangle)
        }
    }

    fn write(&mut self, img: RgbaImage, pts: f64, next: f64) -> Result<()> {
        let (w, h) = self.size;
        if self.enc.is_none() {
            let mut enc = Encoder::new(self.out.take().unwrap(), w as u16, h as u16, &[])?;
            enc.set_repeat(Repeat::Infinite)?;
            self.enc = Some(enc);
            self.written_cs = (pts * 100.0).round() as i64;
        }
        let end_cs = (next * 100.0).round() as i64;
        let delay = (end_cs - self.written_cs).clamp(2, u16::MAX as i64) as u16;
        self.written_cs += delay as i64;

        let frame = match &mut self.reference {
            None => {
                // 第一張：完整畫面
                let mut px = img.as_raw().clone();
                px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
                let f = quantize(w as u16, h as u16, &px, self.speed);
                self.reference = Some(img);
                f
            }
            Some(reference) => diff_frame(reference, &img, self.tolerance, self.speed),
        };
        let mut frame = frame;
        frame.delay = delay;
        frame.dispose = DisposalMethod::Keep;
        self.enc.as_mut().unwrap().write_frame(&frame)?;
        if let Some(p) = &self.settings.progress {
            p.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// 只輸出和 reference 不同的區域；同時把 reference 更新成新畫面
fn diff_frame(reference: &mut RgbaImage, img: &RgbaImage, tol: i32, speed: i32) -> Frame<'static> {
    let (w, h) = img.dimensions();
    let changed = |a: &[u8], b: &[u8]| {
        (a[0] as i32 - b[0] as i32).abs() > tol
            || (a[1] as i32 - b[1] as i32).abs() > tol
            || (a[2] as i32 - b[2] as i32).abs() > tol
    };
    let (src, re) = (img.as_raw(), reference.as_raw());
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for y in 0..h {
        let row = (y * w * 4) as usize;
        for x in 0..w {
            let i = row + (x * 4) as usize;
            if changed(&src[i..i + 4], &re[i..i + 4]) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 {
        // 完全沒變：寫一個 1x1 的透明格，只用來延長停留時間
        return Frame {
            width: 1,
            height: 1,
            buffer: Cow::Owned(vec![0]),
            palette: Some(vec![0, 0, 0]),
            transparent: Some(0),
            ..Frame::default()
        };
    }

    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut px = Vec::with_capacity((cw * ch * 4) as usize);
    let rw = reference.width();
    let refbuf: &mut [u8] = reference;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = ((y * w + x) * 4) as usize;
            let s = &src[i..i + 4];
            if changed(s, &refbuf[i..i + 4]) {
                px.extend_from_slice(&[s[0], s[1], s[2], 255]);
                let j = ((y * rw + x) * 4) as usize;
                refbuf[j..j + 4].copy_from_slice(s);
            } else {
                px.extend_from_slice(&[0, 0, 0, 0]); // 透明：沿用上一張
            }
        }
    }
    let mut f = quantize(cw as u16, ch as u16, &px, speed);
    f.left = x0 as u16;
    f.top = y0 as u16;
    f
}

/// RGBA（alpha 只有 0 / 255）→ 帶調色盤的 GIF 影格。第 255 號色保留給透明。
fn quantize(w: u16, h: u16, px: &[u8], speed: i32) -> Frame<'static> {
    let has_transparent = px.chunks_exact(4).any(|p| p[3] == 0);

    // 顏色夠少就用精確調色盤
    let mut exact: HashMap<[u8; 3], u8> = HashMap::new();
    let mut overflow = false;
    for p in px.chunks_exact(4) {
        if p[3] == 0 {
            continue;
        }
        let c = [p[0], p[1], p[2]];
        if !exact.contains_key(&c) {
            if exact.len() >= 255 {
                overflow = true;
                break;
            }
            let idx = exact.len() as u8;
            exact.insert(c, idx);
        }
    }

    let (palette, buffer) = if !overflow {
        let mut pal = vec![0u8; 256 * 3];
        for (c, &i) in &exact {
            pal[i as usize * 3..i as usize * 3 + 3].copy_from_slice(c);
        }
        let buf = px
            .chunks_exact(4)
            .map(|p| if p[3] == 0 { TRANSPARENT } else { exact[&[p[0], p[1], p[2]]] })
            .collect();
        (pal, buf)
    } else {
        // 只拿不透明的像素訓練 NeuQuant，避免透明區干擾配色
        let train: Vec<u8> = px.chunks_exact(4).filter(|p| p[3] != 0).flatten().copied().collect();
        let nq = NeuQuant::new(speed, 255, &train);
        let mut pal = nq.color_map_rgb();
        pal.resize(256 * 3, 0);
        let buf = px
            .chunks_exact(4)
            .map(|p| if p[3] == 0 { TRANSPARENT } else { nq.index_of(p) as u8 })
            .collect();
        (pal, buf)
    };

    Frame {
        width: w,
        height: h,
        buffer: Cow::Owned(buffer),
        palette: Some(palette),
        transparent: has_transparent.then_some(TRANSPARENT),
        ..Frame::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{codecs::gif::GifDecoder, AnimationDecoder, Rgba};

    /// 造一組影格：漸層背景（>255 色，走 NeuQuant）+ 移動方塊 + 靜止段
    fn frames() -> Vec<RgbaImage> {
        (0..20)
            .map(|i| {
                let mut im = RgbaImage::from_fn(320, 180, |x, y| Rgba([(x * 255 / 320) as u8, (y * 255 / 180) as u8, 120, 255]));
                let k = if i < 12 { i } else { 12 }; // 後 8 張完全不動
                for y in 60..100 {
                    for x in (10 + k * 20)..(50 + k * 20) {
                        im.put_pixel(x, y, Rgba([255, 255, 255, 255]));
                    }
                }
                im
            })
            .collect()
    }

    fn roundtrip(quality: u8, src: &[RgbaImage]) -> (Vec<RgbaImage>, u32, u64) {
        let out = std::env::temp_dir().join(format!("gifenc-test-{quality}.gif"));
        let enc = GifWriter::create(&out, Settings { max_width: 0, quality, progress: None }).unwrap();
        for (i, f) in src.iter().enumerate() {
            enc.add(f.clone(), i as f64 * 0.1).unwrap();
        }
        enc.finish(src.len() as f64 * 0.1).unwrap();
        let size = std::fs::metadata(&out).unwrap().len();
        let dec = GifDecoder::new(std::io::BufReader::new(File::open(&out).unwrap())).unwrap();
        let fr = dec.into_frames().collect_frames().unwrap();
        let total_ms: u32 = fr.iter().map(|f| { let (n, d) = f.delay().numer_denom_ms(); n / d }).sum();
        (fr.into_iter().map(|f| f.into_buffer()).collect(), total_ms, size)
    }

    fn mean_err(a: &RgbaImage, b: &RgbaImage) -> f64 {
        let s: u64 = a.as_raw().iter().zip(b.as_raw()).enumerate()
            .filter(|(i, _)| i % 4 != 3)
            .map(|(_, (x, y))| (*x as i64 - *y as i64).unsigned_abs())
            .sum();
        s as f64 / (a.width() * a.height() * 3) as f64
    }

    #[test]
    fn roundtrip_quality_and_timing() {
        let src = frames();
        for q in [100u8, 80, 40] {
            let (back, ms, size) = roundtrip(q, &src);
            assert_eq!(back.len(), src.len(), "影格數");
            assert_eq!(ms, 2000, "總時間");
            let worst = src.iter().zip(&back).map(|(a, b)| mean_err(a, b)).fold(0.0, f64::max);
            println!("quality {q}: {size} bytes, 最差平均誤差 {worst:.2}");
            assert!(worst < 8.0, "畫質 {q} 誤差過大：{worst}");
            // 透明像素不能「誤傷」到有內容的地方：白色方塊要完整
            let last = back.last().unwrap();
            assert_eq!(last.get_pixel(10 + 12 * 20 + 5, 70).0[..3], [255, 255, 255]);
        }
    }

    #[test]
    fn exact_palette_is_lossless() {
        // 操作畫面類：顏色少於 255 種 → 應該完全無損
        let src: Vec<RgbaImage> = (0..5)
            .map(|i| RgbaImage::from_fn(100, 60, |x, y| if (x / 10 + y / 10 + i) % 3 == 0 { Rgba([20, 20, 20, 255]) } else { Rgba([240, 240, 235, 255]) }))
            .collect();
        let (back, _, _) = roundtrip(80, &src);
        for (a, b) in src.iter().zip(&back) {
            assert_eq!(mean_err(a, b), 0.0);
        }
    }
}
