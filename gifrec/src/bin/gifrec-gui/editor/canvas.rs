//! 預覽畫面上的互動：用工具直接框出效果、移動、八方向控制點縮放、裁切畫面

use super::{Editor, Tool, View};
use crate::render::{fit_aspect, Kind};
use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};

/// 八個控制點
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

impl Handle {
    const ALL: [Handle; 8] = [Handle::NW, Handle::N, Handle::NE, Handle::E, Handle::SE, Handle::S, Handle::SW, Handle::W];
    const CORNERS: [Handle; 4] = [Handle::NW, Handle::NE, Handle::SE, Handle::SW];

    fn pos(self, r: Rect) -> Pos2 {
        let (l, t, rr, b, cx, cy) = (r.left(), r.top(), r.right(), r.bottom(), r.center().x, r.center().y);
        match self {
            Handle::NW => pos2(l, t),
            Handle::N => pos2(cx, t),
            Handle::NE => pos2(rr, t),
            Handle::E => pos2(rr, cy),
            Handle::SE => pos2(rr, b),
            Handle::S => pos2(cx, b),
            Handle::SW => pos2(l, b),
            Handle::W => pos2(l, cy),
        }
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
            Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
            Handle::N | Handle::S => CursorIcon::ResizeVertical,
            Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
        }
    }

    fn west(self) -> bool {
        matches!(self, Handle::NW | Handle::W | Handle::SW)
    }
    fn east(self) -> bool {
        matches!(self, Handle::NE | Handle::E | Handle::SE)
    }
    fn north(self) -> bool {
        matches!(self, Handle::NW | Handle::N | Handle::NE)
    }
    fn south(self) -> bool {
        matches!(self, Handle::SW | Handle::S | Handle::SE)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Target {
    Item(usize),
    Crop,
}

#[derive(Clone, Debug)]
pub enum Drag {
    Move { target: Target, grab: Vec2, orig: [f32; 4] },
    Resize { target: Target, handle: Handle, orig: [f32; 4], font: f32 },
}

/// 依控制點移動後的新矩形；aspect = 寬/高（鎖定比例時）
fn resize_rect(o: [f32; 4], h: Handle, p: Pos2, aspect: Option<f32>, min: f32) -> [f32; 4] {
    let (mut l, mut t, mut r, mut b) = (o[0], o[1], o[0] + o[2], o[1] + o[3]);
    if h.west() {
        l = p.x.min(r - min);
    }
    if h.east() {
        r = p.x.max(l + min);
    }
    if h.north() {
        t = p.y.min(b - min);
    }
    if h.south() {
        b = p.y.max(t + min);
    }
    if let Some(a) = aspect {
        let w = r - l;
        let nh = w / a;
        if h.north() {
            t = b - nh;
        } else {
            b = t + nh;
        }
    }
    [l.round(), t.round(), (r - l).round(), (b - t).round()]
}

impl Editor {
    fn crop_f(&self) -> [f32; 4] {
        let c = self.doc.crop;
        [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]
    }

    fn set_crop(&mut self, r: [f32; 4]) {
        let (w, h) = (self.w as f32, self.h as f32);
        let x = r[0].clamp(0.0, w - 16.0);
        let y = r[1].clamp(0.0, h - 16.0);
        let cw = r[2].clamp(16.0, w - x);
        let ch = r[3].clamp(16.0, h - y);
        self.doc.crop = [x.round() as u32, y.round() as u32, cw.round() as u32, ch.round() as u32];
    }

    fn rect_of(&self, t: Target) -> [f32; 4] {
        match t {
            Target::Crop => self.crop_f(),
            Target::Item(i) => {
                let it = &self.doc.items[i];
                [it.x, it.y, it.w, it.h]
            }
        }
    }

    /// 目標可以用哪些控制點
    fn handles_of(&self, t: Target) -> &'static [Handle] {
        match t {
            Target::Crop => &Handle::ALL,
            Target::Item(i) => match &self.doc.items[i].kind {
                Kind::Text { .. } => &[Handle::SE],
                k if k.keeps_aspect() => &Handle::CORNERS,
                _ => &Handle::ALL,
            },
        }
    }

    fn crop_aspect(&self) -> f32 {
        self.doc.crop[2] as f32 / self.doc.crop[3].max(1) as f32
    }

    pub(super) fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.refresh_texture(&ctx);
        let avail = ui.available_rect_before_wrap();
        let painter = ui.painter_at(avail);

        // 輸出預覽：只顯示結果
        if self.view == View::Output {
            let (ow, oh) = self.doc.out_size();
            let s = (avail.width() / ow as f32).min(avail.height() / oh as f32).min(4.0);
            let r = Rect::from_center_size(avail.center(), vec2(ow as f32 * s, oh as f32 * s));
            ui.allocate_rect(avail, Sense::hover());
            if let Some(t) = &self.tex {
                painter.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            painter.text(avail.left_top() + vec2(8.0, 8.0), Align2::LEFT_TOP, format!("輸出預覽 {ow}x{oh}（不能在這裡編輯，切回「編輯畫面」）"), FontId::proportional(13.0), Color32::from_white_alpha(170));
            return;
        }

        let s = (avail.width() / self.w as f32).min(avail.height() / self.h as f32).min(4.0);
        let img_rect = Rect::from_center_size(avail.center(), vec2(self.w as f32 * s, self.h as f32 * s));
        let resp = ui.allocate_rect(avail, Sense::click_and_drag());
        let to_screen = |x: f32, y: f32| img_rect.min + vec2(x, y) * s;
        let to_img = |p: Pos2| ((p - img_rect.min) / s).to_pos2();
        let srect = |r: [f32; 4]| Rect::from_min_max(to_screen(r[0], r[1]), to_screen(r[0] + r[2], r[1] + r[3]));

        painter.rect_filled(img_rect, 0.0, Color32::from_gray(60));
        if let Some(t) = &self.tex {
            painter.image(t.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }

        // 裁切範圍外變暗
        let crop_r = srect(self.crop_f());
        if crop_r != img_rect {
            let dim = Color32::from_black_alpha(if self.tool == Tool::Crop { 170 } else { 130 });
            for r in [
                Rect::from_min_max(img_rect.min, pos2(img_rect.max.x, crop_r.min.y)),
                Rect::from_min_max(pos2(img_rect.min.x, crop_r.max.y), img_rect.max),
                Rect::from_min_max(pos2(img_rect.min.x, crop_r.min.y), pos2(crop_r.min.x, crop_r.max.y)),
                Rect::from_min_max(pos2(crop_r.max.x, crop_r.min.y), pos2(img_rect.max.x, crop_r.max.y)),
            ] {
                painter.rect_filled(r, 0.0, dim);
            }
            painter.rect_stroke(crop_r, 0.0, Stroke::new(1.0, Color32::from_white_alpha(200)), StrokeKind::Outside);
        }

        if self.cur < self.doc.trim.0 || self.cur > self.doc.trim.1 {
            painter.rect_filled(img_rect, 0.0, Color32::from_black_alpha(140));
            painter.text(img_rect.center(), Align2::CENTER_CENTER, "此格在頭尾裁剪範圍外，不會匯出", FontId::proportional(18.0), Color32::WHITE);
        }

        // 效果外框
        let accent = Color32::from_rgb(60, 160, 255);
        for (i, it) in self.doc.items.iter().enumerate() {
            if !it.visible(self.cur) {
                continue;
            }
            let mut r = [it.x, it.y, it.w, it.h];
            if it.kind == Kind::Zoom {
                r = fit_aspect(r, self.crop_aspect(), self.w as f32, self.h as f32);
            }
            let sr = srect(r);
            let c = it.kind.color();
            let col = Color32::from_rgb(c[0], c[1], c[2]);
            if self.selected == Some(i) && self.tool == Tool::Select {
                painter.rect_stroke(sr, 0.0, Stroke::new(2.0, accent), StrokeKind::Outside);
            } else if it.kind == Kind::Zoom {
                dashed_rect(&painter, sr, Stroke::new(1.5, col));
            } else {
                painter.rect_stroke(sr, 0.0, Stroke::new(1.0, col.gamma_multiply(0.8)), StrokeKind::Outside);
            }
            if matches!(it.kind, Kind::Zoom | Kind::Mosaic { .. } | Kind::Blur { .. }) {
                painter.text(sr.left_top() + vec2(3.0, 2.0), Align2::LEFT_TOP, it.kind.label(), FontId::proportional(11.0), col);
            }
        }

        // 控制點：選取工具 → 選中的效果；裁切工具 → 裁切框
        let active = match self.tool {
            Tool::Crop => Some(Target::Crop),
            Tool::Select => self.selected.filter(|&i| i < self.doc.items.len() && self.doc.items[i].visible(self.cur)).map(Target::Item),
            _ => None,
        };
        let hs = 8.0;
        if let Some(t) = active {
            let sr = srect(self.rect_of(t));
            if t == Target::Crop {
                painter.rect_stroke(sr, 0.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
            }
            for h in self.handles_of(t) {
                let hr = Rect::from_center_size(h.pos(sr), vec2(hs, hs));
                painter.rect_filled(hr, 1.5, Color32::WHITE);
                painter.rect_stroke(hr, 1.5, Stroke::new(1.5, accent), StrokeKind::Inside);
            }
        }

        // 找出游標下的控制點
        let handle_at = |p: Pos2| -> Option<Handle> {
            let t = active?;
            let sr = srect(self.rect_of(t));
            self.handles_of(t).iter().copied().find(|h| h.pos(sr).distance(p) <= hs)
        };
        let item_at = |p: Pos2| -> Option<usize> {
            self.doc.items.iter().enumerate().rev().find_map(|(i, it)| {
                let mut r = [it.x, it.y, it.w, it.h];
                if it.kind == Kind::Zoom {
                    r = fit_aspect(r, self.crop_aspect(), self.w as f32, self.h as f32);
                }
                (it.visible(self.cur) && srect(r).expand(2.0).contains(p)).then_some(i)
            })
        };

        // 先算好游標 / 按下位置的命中結果（之後才會修改 self）
        let hover = resp.hover_pos().filter(|p| img_rect.expand(hs).contains(*p));
        let hover_handle = hover.and_then(handle_at);
        let hover_item = hover.and_then(item_at);
        // 用「按下時」的位置判斷，不是 egui 判定開始拖曳時（已經移動了幾像素）的位置
        let press = ctx.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        let press_handle = press.and_then(handle_at);
        let press_item = press.and_then(item_at);

        // 游標樣式
        if let Some(hp) = hover {
            let icon = match (&self.drag, self.tool) {
                (Some(Drag::Resize { handle, .. }), _) => handle.cursor(),
                (Some(Drag::Move { .. }), _) => CursorIcon::Grabbing,
                (None, Tool::Select) => match hover_handle {
                    Some(h) => h.cursor(),
                    None if hover_item.is_some() => CursorIcon::Grab,
                    None => CursorIcon::Default,
                },
                (None, Tool::Crop) => match hover_handle {
                    Some(h) => h.cursor(),
                    None if crop_r.contains(hp) && crop_r != img_rect => CursorIcon::Grab,
                    None => CursorIcon::Crosshair,
                },
                (None, Tool::Text) => CursorIcon::Text,
                _ => CursorIcon::Crosshair,
            };
            ctx.set_cursor_icon(icon);
        }

        // ── 開始拖曳 ──
        if resp.drag_started() {
            if let Some(pp) = press {
                let p = to_img(pp);
                self.playing = false;
                self.drag = None;
                match self.tool {
                    Tool::Select => {
                        if let (Some(h), Some(t)) = (press_handle, active) {
                            self.drag = Some(self.start_resize(t, h));
                        } else if let Some(i) = press_item {
                            self.selected = Some(i);
                            let r = self.rect_of(Target::Item(i));
                            self.drag = Some(Drag::Move { target: Target::Item(i), grab: vec2(p.x - r[0], p.y - r[1]), orig: r });
                        } else {
                            self.selected = None;
                        }
                    }
                    Tool::Crop => {
                        if let Some(h) = press_handle {
                            self.drag = Some(self.start_resize(Target::Crop, h));
                        } else if crop_r.contains(pp) && crop_r != img_rect {
                            let r = self.crop_f();
                            self.drag = Some(Drag::Move { target: Target::Crop, grab: vec2(p.x - r[0], p.y - r[1]), orig: r });
                        } else {
                            // 在外面拖 = 重新框一個裁切範圍
                            let o = [p.x.round(), p.y.round(), 0.0, 0.0];
                            self.drag = Some(Drag::Resize { target: Target::Crop, handle: Handle::SE, orig: o, font: 0.0 });
                        }
                    }
                    Tool::Mosaic | Tool::Blur | Tool::Zoom => {
                        let kind = match self.tool {
                            Tool::Mosaic => Kind::Mosaic { block: ((self.w as f32 / 60.0) as u32).clamp(6, 24) },
                            Tool::Blur => Kind::Blur { radius: 8 },
                            _ => Kind::Zoom,
                        };
                        let it = self.new_item(kind, p.x.round(), p.y.round(), 1.0, 1.0);
                        let i = self.push_item(it);
                        self.drag = Some(Drag::Resize { target: Target::Item(i), handle: Handle::SE, orig: [p.x.round(), p.y.round(), 0.0, 0.0], font: 0.0 });
                    }
                    Tool::Text => self.place_text(p),
                }
            }
        } else if resp.dragged() {
            if let (Some(d), Some(pp)) = (self.drag.clone(), resp.interact_pointer_pos()) {
                let p = to_img(pp);
                let (fw, fh) = (self.w as f32, self.h as f32);
                match d {
                    Drag::Move { target, grab, orig } => {
                        let (nx, ny) = (p.x - grab.x, p.y - grab.y);
                        match target {
                            Target::Crop => self.set_crop([nx.clamp(0.0, fw - orig[2]), ny.clamp(0.0, fh - orig[3]), orig[2], orig[3]]),
                            Target::Item(i) => {
                                let it = &mut self.doc.items[i];
                                it.x = nx.clamp(-it.w + 8.0, fw - 8.0).round();
                                it.y = ny.clamp(-it.h + 8.0, fh - 8.0).round();
                            }
                        }
                    }
                    Drag::Resize { target, handle, orig, font } => {
                        let shift = ctx.input(|i| i.modifiers.shift);
                        match target {
                            Target::Crop => {
                                let aspect = shift.then(|| orig[2] / orig[3].max(1.0)).filter(|a| a.is_finite() && *a > 0.0);
                                let pc = pos2(p.x.clamp(0.0, fw), p.y.clamp(0.0, fh));
                                self.set_crop(resize_rect(orig, handle, pc, aspect, 16.0));
                            }
                            Target::Item(i) => {
                                let ca = self.crop_aspect();
                                let it = &mut self.doc.items[i];
                                match &mut it.kind {
                                    Kind::Text { size, .. } => {
                                        // 拉角落 = 改字級
                                        let nw = (p.x - it.x).max(10.0);
                                        *size = (font * nw / orig[2].max(1.0)).clamp(8.0, 300.0).round();
                                    }
                                    k => {
                                        let aspect = match k {
                                            Kind::Image { img, .. } => Some(img.width() as f32 / img.height() as f32),
                                            Kind::Zoom => Some(ca),
                                            _ => shift.then(|| orig[2] / orig[3].max(1.0)).filter(|a| a.is_finite() && *a > 0.0),
                                        };
                                        let r = resize_rect(orig, handle, p, aspect, 4.0);
                                        it.x = r[0];
                                        it.y = r[1];
                                        it.w = r[2];
                                        it.h = r[3];
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else if resp.drag_stopped() {
            // 剛框好的效果如果太小（等於只是點一下），給一個預設大小
            if let Some(Drag::Resize { target: Target::Item(i), orig, .. }) = self.drag.clone() {
                if orig[2] == 0.0 && i < self.doc.items.len() {
                    let (fw, fh) = (self.w as f32, self.h as f32);
                    let it = &mut self.doc.items[i];
                    if it.w < 12.0 || it.h < 12.0 {
                        let (w, h) = if it.kind == Kind::Zoom { (fw / 2.0, fh / 2.0) } else { (fw / 5.0, fh / 8.0) };
                        it.x = (it.x - w / 2.0).clamp(0.0, fw - w).round();
                        it.y = (it.y - h / 2.0).clamp(0.0, fh - h).round();
                        it.w = w.round();
                        it.h = h.round();
                    }
                    self.tool = Tool::Select;
                }
            }
            self.drag = None;
        } else if resp.clicked() {
            if let Some(pp) = resp.interact_pointer_pos() {
                let p = to_img(pp);
                match self.tool {
                    Tool::Select => self.selected = press_item,
                    Tool::Text => self.place_text(p),
                    Tool::Mosaic | Tool::Blur | Tool::Zoom if img_rect.contains(pp) => {
                        // 單擊也能放一個預設大小的
                        let (fw, fh) = (self.w as f32, self.h as f32);
                        let (kind, w, h) = match self.tool {
                            Tool::Mosaic => (Kind::Mosaic { block: ((fw / 60.0) as u32).clamp(6, 24) }, fw / 5.0, fh / 8.0),
                            Tool::Blur => (Kind::Blur { radius: 8 }, fw / 5.0, fh / 8.0),
                            _ => (Kind::Zoom, fw / 2.0, fh / 2.0),
                        };
                        let it = self.new_item(kind, (p.x - w / 2.0).clamp(0.0, fw - w).round(), (p.y - h / 2.0).clamp(0.0, fh - h).round(), w.round(), h.round());
                        self.push_item(it);
                        self.tool = Tool::Select;
                    }
                    _ => {}
                }
            }
        }

        // 雙擊文字 → 直接編輯內容
        if resp.double_clicked() && self.tool == Tool::Select {
            if let Some(i) = press_item {
                if matches!(self.doc.items[i].kind, Kind::Text { .. }) {
                    self.selected = Some(i);
                    self.focus_text = true;
                }
            }
        }
    }

    fn start_resize(&self, t: Target, h: Handle) -> Drag {
        let orig = self.rect_of(t);
        let font = match t {
            Target::Item(i) => match &self.doc.items[i].kind {
                Kind::Text { size, .. } => *size,
                _ => 0.0,
            },
            Target::Crop => 0.0,
        };
        Drag::Resize { target: t, handle: h, orig, font }
    }

    fn place_text(&mut self, p: Pos2) {
        let it = self.new_item(self.default_text(), p.x.round(), p.y.round(), 0.0, 0.0);
        self.push_item(it);
        self.tool = Tool::Select;
        self.focus_text = true;
    }
}

fn dashed_rect(p: &egui::Painter, r: Rect, s: Stroke) {
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    p.extend(Shape::dashed_line(&pts, s, 6.0, 4.0));
}
