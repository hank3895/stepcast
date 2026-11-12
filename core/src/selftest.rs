//! Synthetic recording that needs no screen, so CI can verify the encoder.
//! It imitates a UI: a static window, a box that moves, and a pause with no change.

use crate::encode::RgbFrame;

pub fn synthetic_frames(width: u32, height: u32, fps: u32, seconds: u32) -> Vec<RgbFrame> {
    let total = (fps * seconds) as usize;
    let mut frames = Vec::with_capacity(total);
    for i in 0..total {
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        for y in 0..height {
            for x in 0..width {
                let p = ((y * width + x) * 3) as usize;
                let (r, g, b) = if y < 32 {
                    (40, 60, 110) // title bar
                } else if x < 160 {
                    (230, 232, 236) // sidebar
                } else {
                    // soft gradient content area, to exercise the palette
                    (250, (240 - y * 40 / height) as u8, (255 - x * 60 / width) as u8)
                };
                rgb[p..p + 3].copy_from_slice(&[r, g, b]);
            }
        }
        // The box moves during the first half, then stays still (idle period).
        let step = i.min(total / 2) as u32;
        let bx = 180 + step * 8 % (width.saturating_sub(260).max(1));
        let by = height / 2;
        for y in by..(by + 40).min(height) {
            for x in bx..(bx + 60).min(width) {
                let p = ((y * width + x) * 3) as usize;
                rgb[p..p + 3].copy_from_slice(&[220, 70, 60]);
            }
        }
        frames.push(RgbFrame { width, height, rgb, t_ms: (i as u64 * 1000) / fps as u64 });
    }
    frames
}
