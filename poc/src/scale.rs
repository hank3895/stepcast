//! Area-average downscaling from RGBA to RGB.

/// Output size that fits `max_width` while keeping the aspect ratio.
pub fn fit(src_w: u32, src_h: u32, max_width: u32) -> (u32, u32) {
    if src_w <= max_width || max_width == 0 {
        return (src_w, src_h);
    }
    let h = ((src_h as u64 * max_width as u64 + src_w as u64 / 2) / src_w as u64).max(1) as u32;
    (max_width, h)
}

/// Downscales tightly packed RGBA (4 bytes per pixel) to RGB at `dst_w` x `dst_h`.
pub fn rgba_to_rgb_scaled(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (src_w as usize, src_h as usize, dst_w as usize, dst_h as usize);
    let mut out = Vec::with_capacity(dw * dh * 3);
    if sw == dw && sh == dh {
        for px in src.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
        return out;
    }
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1);
            let mut acc = [0u32; 3];
            for y in y0..y1 {
                let row = &src[(y * sw + x0) * 4..(y * sw + x1) * 4];
                for px in row.chunks_exact(4) {
                    acc[0] += px[0] as u32;
                    acc[1] += px[1] as u32;
                    acc[2] += px[2] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(acc.iter().map(|&c| ((c + n / 2) / n) as u8));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_ratio() {
        assert_eq!(fit(1920, 1080, 960), (960, 540));
        assert_eq!(fit(800, 600, 960), (800, 600));
    }

    #[test]
    fn halves_by_averaging() {
        // 2x1 RGBA -> 1x1 RGB
        let src = [0, 0, 0, 255, 200, 100, 50, 255];
        assert_eq!(rgba_to_rgb_scaled(&src, 2, 1, 1, 1), vec![100, 50, 25]);
    }
}
