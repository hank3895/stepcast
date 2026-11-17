//! 框選覆蓋層：凍結目前畫面、變暗，拖曳滑鼠選出要錄的區域。
//!
//! 操作：左鍵拖曳 = 框選；放開 = 確定；單擊不拖 = 整個螢幕；Esc / 右鍵 = 取消

use anyhow::{Context as _, Result};
use softbuffer::{Context, Surface};
use std::{num::NonZeroU32, sync::Arc};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::{CursorIcon, Fullscreen, Window, WindowId, WindowLevel},
};
use xcap::Monitor;

use crate::recorder::Target;

pub enum Outcome {
    Pending,
    Selected(Target),
    Cancelled,
}

pub struct Overlay {
    window: Arc<Window>,
    surface: Surface<Arc<Window>, Arc<Window>>,
    _ctx: Context<Arc<Window>>,
    /// 凍結的螢幕截圖（RGBA，螢幕實際像素）
    shot: xcap::image::RgbaImage,
    mon_pos: (i32, i32),
    /// 依視窗大小預先算好的亮 / 暗兩份畫面（0x00RRGGBB）
    bright: Vec<u32>,
    dim: Vec<u32>,
    buf_size: (u32, u32),
    cursor: PhysicalPosition<f64>,
    drag_from: Option<PhysicalPosition<f64>>,
}

impl Overlay {
    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// 在指定螢幕（xcap Monitor）上開一個全螢幕覆蓋視窗
    pub fn open(el: &ActiveEventLoop, mon: &Monitor) -> Result<Self> {
        let mon_pos = (mon.x()?, mon.y()?);
        let shot = mon.capture_image().context("無法截取螢幕畫面")?;

        // 找到 winit 中對應同一個螢幕的 handle（用左上角座標比對）
        let handle = el
            .available_monitors()
            .find(|m| {
                let p = m.position();
                (p.x, p.y) == mon_pos
            })
            .or_else(|| el.primary_monitor());

        let attrs = Window::default_attributes()
            .with_title("gifrec — 拖曳選取錄影區域，Esc 取消")
            .with_decorations(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(PhysicalPosition::new(mon_pos.0, mon_pos.1))
            .with_inner_size(PhysicalSize::new(shot.width(), shot.height()))
            .with_fullscreen(Some(Fullscreen::Borderless(handle)));
        let window = Arc::new(el.create_window(attrs)?);
        window.set_cursor(CursorIcon::Crosshair);

        let ctx = Context::new(window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let surface = Surface::new(&ctx, window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;

        let ov = Overlay {
            window,
            surface,
            _ctx: ctx,
            shot,
            mon_pos,
            bright: Vec::new(),
            dim: Vec::new(),
            buf_size: (0, 0),
            cursor: PhysicalPosition::new(0.0, 0.0),
            drag_from: None,
        };
        ov.window.focus_window();
        ov.window.request_redraw();
        Ok(ov)
    }

    /// 視窗大小改變時重新縮放截圖（大小相同時就是 1:1 複製）
    fn rebuild(&mut self, w: u32, h: u32) {
        if self.buf_size == (w, h) {
            return;
        }
        let (sw, sh) = self.shot.dimensions();
        let src = self.shot.as_raw();
        let n = (w * h) as usize;
        self.bright = Vec::with_capacity(n);
        self.dim = Vec::with_capacity(n);
        for y in 0..h {
            let sy = (y as u64 * sh as u64 / h as u64) as usize;
            for x in 0..w {
                let sx = (x as u64 * sw as u64 / w as u64) as usize;
                let i = (sy * sw as usize + sx) * 4;
                let (r, g, b) = (src[i] as u32, src[i + 1] as u32, src[i + 2] as u32);
                self.bright.push(r << 16 | g << 8 | b);
                self.dim.push((r * 2 / 5) << 16 | (g * 2 / 5) << 8 | (b * 2 / 5));
            }
        }
        self.buf_size = (w, h);
    }

    /// 目前拖曳的矩形（視窗像素座標）：x, y, w, h
    fn rect(&self) -> Option<(u32, u32, u32, u32)> {
        let a = self.drag_from?;
        let b = self.cursor;
        let (bw, bh) = (self.buf_size.0 as f64, self.buf_size.1 as f64);
        let x0 = a.x.min(b.x).clamp(0.0, bw);
        let y0 = a.y.min(b.y).clamp(0.0, bh);
        let x1 = a.x.max(b.x).clamp(0.0, bw);
        let y1 = a.y.max(b.y).clamp(0.0, bh);
        Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    fn redraw(&mut self) -> Result<()> {
        let size = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return Ok(());
        };
        self.rebuild(size.width, size.height);
        self.surface.resize(w, h).map_err(|e| anyhow::anyhow!("{e}"))?;
        let rect = self.rect();
        let mut buf = self.surface.buffer_mut().map_err(|e| anyhow::anyhow!("{e}"))?;
        buf.copy_from_slice(&self.dim);

        let ww = size.width as usize;
        if let Some((x, y, rw, rh)) = rect {
            let (x, y, rw, rh) = (x as usize, y as usize, rw as usize, rh as usize);
            // 選取區域內顯示原本亮度
            for row in y..y + rh {
                let s = row * ww + x;
                buf[s..s + rw].copy_from_slice(&self.bright[s..s + rw]);
            }
            // 紅色外框（2px，畫在區域外側，不會被錄進去）
            const RED: u32 = 0xFF3B30;
            let hh = size.height as usize;
            let left = x.saturating_sub(2);
            let right = (x + rw + 2).min(ww);
            for t in 1..=2 {
                if y >= t {
                    buf[(y - t) * ww + left..(y - t) * ww + right].fill(RED);
                }
                if y + rh + t - 1 < hh {
                    let r = y + rh + t - 1;
                    buf[r * ww + left..r * ww + right].fill(RED);
                }
            }
            for row in y.saturating_sub(2)..(y + rh + 2).min(hh) {
                for t in 1..=2 {
                    if x >= t {
                        buf[row * ww + x - t] = RED;
                    }
                    if x + rw + t - 1 < ww {
                        buf[row * ww + x + rw + t - 1] = RED;
                    }
                }
            }
        }
        buf.present().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    }

    /// 把視窗座標的矩形換算成螢幕實際像素
    fn to_target(&self, r: (u32, u32, u32, u32)) -> Target {
        let (sw, sh) = self.shot.dimensions();
        let (bw, bh) = self.buf_size;
        let sx = |v: u32| (v as u64 * sw as u64 / bw.max(1) as u64) as u32;
        let sy = |v: u32| (v as u64 * sh as u64 / bh.max(1) as u64) as u32;
        let (x, y) = (sx(r.0), sy(r.1));
        let w = sx(r.2).clamp(1, sw - x);
        let h = sy(r.3).clamp(1, sh - y);
        Target::Region { mx: self.mon_pos.0, my: self.mon_pos.1, x, y, w, h }
    }

    pub fn handle(&mut self, ev: WindowEvent) -> Outcome {
        match ev {
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.redraw() {
                    eprintln!("繪製失敗：{e}");
                }
            }
            WindowEvent::Resized(_) => self.window.request_redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                if self.drag_from.is_some() {
                    if let Some((_, _, w, h)) = self.rect() {
                        print!("\r選取中：{w} x {h}      ");
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    self.drag_from = Some(self.cursor);
                }
                ElementState::Released => {
                    let r = self.rect();
                    self.drag_from = None;
                    println!();
                    return match r {
                        // 幾乎沒拖動 = 單擊 → 錄整個螢幕
                        Some((_, _, w, h)) if w < 4 || h < 4 => Outcome::Selected(Target::Full {
                            mx: self.mon_pos.0,
                            my: self.mon_pos.1,
                        }),
                        Some(r) => Outcome::Selected(self.to_target(r)),
                        None => Outcome::Pending,
                    };
                }
            },
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. }
            | WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. },
                ..
            }
            | WindowEvent::CloseRequested => return Outcome::Cancelled,
            _ => {}
        }
        Outcome::Pending
    }
}
