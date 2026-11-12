//! GIF encoding: merges unchanged frames, encodes only the changed region of each
//! frame, and quantizes colors with quantette (MIT OR Apache-2.0).

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::time::Instant;

use gif::{DisposalMethod, Encoder, Frame, Repeat};
use quantette::deps::palette::Srgb;
use quantette::{ImageBuf, PaletteSize, Pipeline};

/// One captured frame, already scaled to the output size.
pub struct RgbFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGB pixels (3 bytes per pixel).
    pub rgb: Vec<u8>,
    /// Milliseconds since the recording started.
    pub t_ms: u64,
}

pub struct EncodeOptions {
    pub dither: bool,
    /// Total recording length; the last frame is held until this point.
    pub end_ms: u64,
}

#[derive(Debug, Default)]
pub struct EncodeStats {
    pub input_frames: usize,
    pub written_frames: usize,
    pub encode_secs: f64,
    pub file_bytes: u64,
}

/// A rectangle of changed pixels: x, y, width, height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Bounding box of pixels that differ between two same-sized RGB frames.
/// Returns `None` when the frames are identical.
pub fn diff_rect(prev: &[u8], next: &[u8], width: u32, height: u32) -> Option<Rect> {
    let (w, h) = (width as usize, height as usize);
    let row = w * 3;
    let mut min_x = w;
    let mut max_x = 0usize;
    let mut min_y = h;
    let mut max_y = 0usize;
    for y in 0..h {
        let a = &prev[y * row..(y + 1) * row];
        let b = &next[y * row..(y + 1) * row];
        if a == b {
            continue;
        }
        let first = (0..w).find(|&x| a[x * 3..x * 3 + 3] != b[x * 3..x * 3 + 3]);
        let last = (0..w).rev().find(|&x| a[x * 3..x * 3 + 3] != b[x * 3..x * 3 + 3]);
        if let (Some(f), Some(l)) = (first, last) {
            min_x = min_x.min(f);
            max_x = max_x.max(l);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    if min_y > max_y {
        return None;
    }
    Some(Rect {
        x: min_x as u32,
        y: min_y as u32,
        w: (max_x - min_x + 1) as u32,
        h: (max_y - min_y + 1) as u32,
    })
}

fn crop(rgb: &[u8], width: u32, r: Rect) -> Vec<Srgb<u8>> {
    let mut out = Vec::with_capacity((r.w * r.h) as usize);
    for y in r.y..r.y + r.h {
        let start = ((y * width + r.x) * 3) as usize;
        let end = start + (r.w * 3) as usize;
        out.extend(rgb[start..end].chunks_exact(3).map(|c| Srgb::new(c[0], c[1], c[2])));
    }
    out
}

/// Encodes frames to a GIF file at `path`.
pub fn encode_gif(frames: &[RgbFrame], path: &Path, opts: &EncodeOptions) -> Result<EncodeStats, String> {
    let first = frames.first().ok_or("沒有任何影格可輸出")?;
    let (width, height) = (first.width, first.height);
    if width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(format!("尺寸過大：{width}x{height}"));
    }
    let started = Instant::now();

    // 1. Decide which frames to write and which region of each changed.
    let mut parts: Vec<(usize, Rect)> = vec![(0, Rect { x: 0, y: 0, w: width, h: height })];
    let mut last_written = 0usize;
    for i in 1..frames.len() {
        let f = &frames[i];
        if f.width != width || f.height != height {
            return Err("影格尺寸不一致".into());
        }
        if let Some(r) = diff_rect(&frames[last_written].rgb, &f.rgb, width, height) {
            parts.push((i, r));
            last_written = i;
        }
    }

    // 2. Write them, each held until the next written frame starts.
    let file = File::create(path).map_err(|e| format!("無法建立檔案：{e}"))?;
    let mut enc = Encoder::new(BufWriter::new(file), width as u16, height as u16, &[])
        .map_err(|e| e.to_string())?;
    enc.set_repeat(Repeat::Infinite).map_err(|e| e.to_string())?;

    let pipeline = Pipeline::new()
        .palette_size(PaletteSize::MAX)
        .ditherer(if opts.dither { Some(quantette::dither::FloydSteinberg::new()) } else { None })
        .parallel(true);

    let cs = |ms: u64| (ms + 5) / 10; // milliseconds -> centiseconds, rounded
    for (n, &(idx, r)) in parts.iter().enumerate() {
        let start = frames[idx].t_ms;
        let next = parts.get(n + 1).map_or(opts.end_ms.max(start + 100), |&(j, _)| frames[j].t_ms);
        let delay = cs(next).saturating_sub(cs(start)).clamp(2, u16::MAX as u64) as u16;

        let pixels = crop(&frames[idx].rgb, width, r);
        let img = ImageBuf::new(r.w, r.h, pixels).map_err(|_| "影格資料長度錯誤".to_string())?;
        let indexed = pipeline.clone().input_image(img.as_ref()).output_srgb8_indexed_image();
        let palette: Vec<u8> = indexed.palette().iter().flat_map(|c| [c.red, c.green, c.blue]).collect();

        let mut frame = Frame::from_palette_pixels(r.w as u16, r.h as u16, indexed.indices().to_vec(), palette, None);
        frame.left = r.x as u16;
        frame.top = r.y as u16;
        frame.delay = delay;
        frame.dispose = DisposalMethod::Keep;
        enc.write_frame(&frame).map_err(|e| e.to_string())?;
    }
    drop(enc.into_inner().map_err(|e| e.to_string())?);

    Ok(EncodeStats {
        input_frames: frames.len(),
        written_frames: parts.len(),
        encode_secs: started.elapsed().as_secs_f64(),
        file_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_frames_have_no_diff() {
        let a = vec![7u8; 4 * 3 * 3];
        assert_eq!(diff_rect(&a, &a, 4, 3), None);
    }

    #[test]
    fn diff_finds_changed_box() {
        let a = vec![0u8; 5 * 4 * 3];
        let mut b = a.clone();
        for (x, y) in [(1, 1), (3, 2)] {
            b[(y * 5 + x) * 3] = 255;
        }
        assert_eq!(diff_rect(&a, &b, 5, 4), Some(Rect { x: 1, y: 1, w: 3, h: 2 }));
    }
}
