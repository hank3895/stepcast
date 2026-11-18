//! 縮圖時間軸：點縮圖跳格；下方色條是每個效果出現的範圍，拖兩端改起訖、拖中間整段移動

use super::Editor;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Rect, RichText, Sense, Stroke, StrokeKind};
use image::imageops;

const THUMB_H: f32 = 48.0;
const BAR_H: f32 = 14.0;
const EDGE: f32 = 6.0;

#[derive(Clone, Copy, Debug)]
pub enum BarMode {
    Start,
    End,
    Move { grab: usize, len: usize },
}

#[derive(Clone, Copy, Debug)]
pub struct BarDrag {
    item: usize,
    mode: BarMode,
}

impl Editor {
    fn thumb_w(&self) -> f32 {
        (THUMB_H * self.w as f32 / self.h as f32).clamp(36.0, 110.0).round()
    }

    fn thumb(&mut self, ctx: &egui::Context, i: usize) -> Option<egui::TextureId> {
        if self.thumbs[i].is_none() {
            let tw = self.thumb_w();
            let img = imageops::thumbnail(&self.frames[i].rgba, (tw * 1.5) as u32, (THUMB_H * 1.5) as u32);
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
            self.thumbs[i] = Some(ctx.load_texture(format!("thumb{i}"), ci, egui::TextureOptions::LINEAR));
        }
        self.thumbs[i].as_ref().map(|t| t.id())
    }

    pub(super) fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let n = self.n();
        ui.add_space(6.0);

        // 播放（只在頭尾裁剪範圍內循環）
        if self.playing {
            self.play_acc += ui.input(|i| i.stable_dt).min(0.2);
            loop {
                let d = self.frames[self.cur].delay_ms as f32 / 1000.0;
                if self.play_acc < d {
                    break;
                }
                self.play_acc -= d;
                self.cur = if self.cur >= self.doc.trim.1 { self.doc.trim.0 } else { self.cur + 1 };
                self.scroll_to_cur = true;
            }
        }

        ui.horizontal(|ui| {
            let label = if self.playing { "■ 暫停" } else { "▶ 播放" };
            if ui.add(egui::Button::new(label).min_size(vec2(70.0, 0.0))).on_hover_text("空白鍵").clicked() {
                self.toggle_play();
            }
            if ui.button("◀").on_hover_text("上一格（, 或 ←）").clicked() {
                self.seek(self.cur.saturating_sub(1));
            }
            if ui.button("▶").on_hover_text("下一格（. 或 →）").clicked() {
                self.seek((self.cur + 1).min(n - 1));
            }
            ui.label(format!("第 {} / {} 格　{:.2}s", self.cur + 1, n, self.time_at(self.cur)));
            ui.separator();
            ui.label("頭尾裁剪");
            if ui.button("設為起點").clicked() {
                self.doc.trim.0 = self.cur.min(self.doc.trim.1);
            }
            if ui.button("設為終點").clicked() {
                self.doc.trim.1 = self.cur.max(self.doc.trim.0);
            }
            if ui.button("重設").clicked() {
                self.doc.trim = (0, n - 1);
            }
            let (a, b) = self.doc.trim;
            let kept = self.time_at(b + 1) - self.time_at(a);
            ui.label(RichText::new(format!("保留 {}–{} 格，{:.1} 秒", a + 1, b + 1, kept)).weak());
        });
        ui.add_space(4.0);

        let tw = self.thumb_w();
        let rows = self.doc.items.len().max(1);
        let total = vec2(tw * n as f32, THUMB_H + 6.0 + rows as f32 * (BAR_H + 3.0));
        let scroll_to = std::mem::take(&mut self.scroll_to_cur);

        egui::ScrollArea::horizontal().id_salt("timeline").max_height(total.y + 14.0).show_viewport(ui, |ui, viewport| {
            let (area, _) = ui.allocate_exact_size(total, Sense::hover());
            let painter = ui.painter_at(area);
            let x0 = area.min.x;
            let frame_at = |x: f32| (((x - x0) / tw).floor().max(0.0) as usize).min(n - 1);

            // ── 縮圖列 ──
            let first = ((viewport.min.x / tw).floor().max(0.0) as usize).min(n - 1);
            let last = ((viewport.max.x / tw).ceil() as usize).min(n - 1);
            let strip = Rect::from_min_size(area.min, vec2(total.x, THUMB_H));
            let strip_resp = ui.interact(strip, egui::Id::unique("timeline-strip"), Sense::click_and_drag());
            for i in first..=last {
                let r = Rect::from_min_size(pos2(x0 + i as f32 * tw, area.min.y), vec2(tw - 2.0, THUMB_H));
                if let Some(id) = self.thumb(&ctx, i) {
                    painter.image(id, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                if i < self.doc.trim.0 || i > self.doc.trim.1 {
                    painter.rect_filled(r, 0.0, Color32::from_black_alpha(170));
                }
                if i == self.cur {
                    painter.rect_stroke(r, 2.0, Stroke::new(3.0, Color32::from_rgb(60, 160, 255)), StrokeKind::Inside);
                }
                if i % 5 == 0 || i == n - 1 {
                    painter.text(r.left_bottom() + vec2(3.0, -2.0), Align2::LEFT_BOTTOM, format!("{}", i + 1), FontId::proportional(10.0), Color32::from_white_alpha(220));
                }
            }
            // 拖過縮圖 = 快速瀏覽
            if strip_resp.clicked() || strip_resp.dragged() {
                if let Some(p) = strip_resp.interact_pointer_pos() {
                    self.cur = frame_at(p.x);
                    self.playing = false;
                }
            }
            // 頭尾裁剪的標記線
            for (f, txt) in [(self.doc.trim.0, "起"), (self.doc.trim.1 + 1, "終")] {
                let x = x0 + f as f32 * tw - 1.0;
                painter.line_segment([pos2(x, area.min.y), pos2(x, area.max.y)], Stroke::new(2.0, Color32::from_rgb(255, 200, 0)));
                painter.text(pos2(x, area.min.y), if txt == "起" { Align2::LEFT_TOP } else { Align2::RIGHT_TOP }, txt, FontId::proportional(11.0), Color32::from_rgb(255, 200, 0));
            }

            // ── 效果色條（新加入的在上面，和右側清單順序一致）──
            let bars_top = area.min.y + THUMB_H + 6.0;
            let count = self.doc.items.len();
            for row in 0..count {
                let i = count - 1 - row;
                let it = &self.doc.items[i];
                let y = bars_top + row as f32 * (BAR_H + 3.0);
                let r = Rect::from_min_max(pos2(x0 + it.start as f32 * tw, y), pos2(x0 + (it.end + 1) as f32 * tw - 2.0, y + BAR_H));
                let c = it.kind.color();
                let selected = self.selected == Some(i);
                let fill = Color32::from_rgb(c[0], c[1], c[2]).gamma_multiply(if selected { 1.0 } else { 0.6 });
                painter.rect_filled(r, 3.0, fill);
                if selected {
                    painter.rect_stroke(r, 3.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
                }
                // 兩端把手
                for ex in [r.left() + 2.0, r.right() - 2.0] {
                    painter.line_segment([pos2(ex, r.top() + 3.0), pos2(ex, r.bottom() - 3.0)], Stroke::new(2.0, Color32::from_black_alpha(140)));
                }
                if r.width() > 50.0 {
                    let label_x = r.left().max(viewport.min.x + x0) + 8.0;
                    painter.text(pos2(label_x, r.center().y), Align2::LEFT_CENTER, it.kind.label(), FontId::proportional(10.5), Color32::BLACK);
                }

                let resp = ui.interact(r.expand2(vec2(EDGE / 2.0, 0.0)), egui::Id::unique(("timeline-bar", i)), Sense::click_and_drag());
                if let Some(hp) = resp.hover_pos() {
                    if (hp.x - r.left()).abs() <= EDGE || (hp.x - r.right()).abs() <= EDGE {
                        ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                    } else {
                        ctx.set_cursor_icon(CursorIcon::Grab);
                    }
                }
                if resp.drag_started() {
                    if let Some(p) = ctx.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
                        let mode = if (p.x - r.left()).abs() <= EDGE {
                            BarMode::Start
                        } else if (p.x - r.right()).abs() <= EDGE {
                            BarMode::End
                        } else {
                            BarMode::Move { grab: frame_at(p.x).saturating_sub(it.start), len: it.end - it.start }
                        };
                        self.bar_drag = Some(BarDrag { item: i, mode });
                        self.selected = Some(i);
                    }
                }
                if resp.clicked() {
                    self.selected = Some(i);
                    if !self.doc.items[i].visible(self.cur) {
                        self.cur = self.doc.items[i].start;
                    }
                }
            }

            // 色條拖曳中
            if let Some(bd) = self.bar_drag {
                let pointer = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_down()));
                match pointer {
                    (Some(p), true) if bd.item < self.doc.items.len() => {
                        let f = frame_at(p.x);
                        let it = &mut self.doc.items[bd.item];
                        match bd.mode {
                            BarMode::Start => it.start = f.min(it.end),
                            BarMode::End => it.end = f.max(it.start),
                            BarMode::Move { grab, len } => {
                                let s = f.saturating_sub(grab).min(n - 1 - len);
                                it.start = s;
                                it.end = s + len;
                            }
                        }
                        // 拖的時候畫面跟著跳到那一格，方便對準
                        self.cur = match bd.mode {
                            BarMode::End => it.end,
                            _ => it.start,
                        };
                    }
                    _ => self.bar_drag = None,
                }
            }

            if scroll_to {
                let r = Rect::from_min_size(pos2(x0 + self.cur as f32 * tw, area.min.y), vec2(tw, THUMB_H));
                ui.scroll_to_rect(r, None);
            }
        });
        ui.add_space(4.0);
    }
}
