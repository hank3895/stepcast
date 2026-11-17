//! 錄影工作執行緒：截圖 → 去重 → GIF 編碼

use anyhow::{bail, Context, Result};
use gifrec::gifenc::{self, GifWriter};
use xcap::image::RgbaImage;
use std::{
    path::Path,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use xcap::Monitor;

/// 錄影目標。只存座標，真正的 Monitor 在工作執行緒裡再取得
/// （Windows 上 Monitor 帶有 HMONITOR，不保證能跨執行緒傳遞）。
#[derive(Clone, Copy, Debug)]
pub enum Target {
    /// 整個螢幕；(mx, my) 為該螢幕左上角的桌面座標
    Full { mx: i32, my: i32 },
    /// 螢幕內的一塊區域；x,y 相對於該螢幕左上角
    Region { mx: i32, my: i32, x: u32, y: u32, w: u32, h: u32 },
}

impl Target {
    pub fn monitor(&self) -> Result<Monitor> {
        let (mx, my) = match *self {
            Target::Full { mx, my } | Target::Region { mx, my, .. } => (mx, my),
        };
        Monitor::from_point(mx, my).context("找不到要錄影的螢幕")
    }

    pub fn describe(&self) -> String {
        match *self {
            Target::Full { .. } => match self.monitor() {
                Ok(m) => format!(
                    "全螢幕 {} ({}x{})",
                    m.name().unwrap_or_default(),
                    m.width().unwrap_or(0),
                    m.height().unwrap_or(0)
                ),
                Err(_) => "全螢幕".into(),
            },
            Target::Region { mx, my, x, y, w, h } => {
                format!("區域 {w}x{h}（桌面座標 {},{}）", mx + x as i32, my + y as i32)
            }
        }
    }
}

pub struct Options {
    pub fps: u32,
    pub width: u32,
    pub quality: u8,
    pub max_secs: u64,
    pub out: String,
}

pub struct Summary {
    pub secs: f64,
    pub frames: usize,
    pub bytes: u64,
}

/// 開始錄影直到 stop 變成 true（或達到 max_secs）。在呼叫端的執行緒上阻塞執行。
pub fn record(target: Target, opt: &Options, stop: Arc<AtomicBool>) -> Result<Summary> {
    let mon = target.monitor()?;
    let capture = |m: &Monitor| -> Result<xcap::image::RgbaImage> {
        Ok(match target {
            Target::Full { .. } => m.capture_image()?,
            Target::Region { x, y, w, h, .. } => m.capture_region(x, y, w, h)?,
        })
    };
    // 先試一張，權限或座標有問題能早點知道
    capture(&mon).context("截圖失敗")?;

    let enc = GifWriter::create(
        Path::new(&opt.out),
        gifenc::Settings { max_width: opt.width, quality: opt.quality, progress: None },
    )?;

    let interval = Duration::from_secs_f64(1.0 / opt.fps as f64);
    let max = (opt.max_secs > 0).then(|| Duration::from_secs(opt.max_secs));
    let start = Instant::now();
    let mut next_tick = start;
    let mut idx = 0usize;
    let mut last: Option<RgbaImage> = None;

    while !stop.load(Ordering::SeqCst) && max.map_or(true, |m| start.elapsed() < m) {
        let img = capture(&mon)?;
        let pts = start.elapsed().as_secs_f64();

        // 畫面完全沒變就跳過，上一張的停留時間會自動延長
        if last.as_ref() != Some(&img) {
            enc.add(img.clone(), pts)?;
            idx += 1;
            last = Some(img);
        }

        print!("\r● 錄影中 {:>5.1}s  {idx} 張   ", pts);
        let _ = io::stdout().flush();

        next_tick += interval;
        match next_tick.checked_duration_since(Instant::now()) {
            Some(d) => thread::sleep(d),
            None => next_tick = Instant::now(), // 截圖太慢就不追趕
        }
    }
    if idx == 0 {
        enc.abort();
        let _ = std::fs::remove_file(&opt.out);
        bail!("沒有錄到任何畫面");
    }

    let secs = start.elapsed().as_secs_f64();
    println!("\n編碼中…");
    enc.finish(secs)?;
    let bytes = std::fs::metadata(&opt.out)?.len();
    Ok(Summary { secs, frames: idx, bytes })
}
