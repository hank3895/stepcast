//! GIF 編修畫面：馬賽克、文字、圖片、頭尾裁剪、播放預覽、匯出

use crate::render::{self, Frame, Item, Kind, Prepared};
use ab_glyph::FontVec;
use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle};
use gifrec::gifenc::{self, GifWriter};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

pub enum Action {
    None,
    Back,
}

enum Drag {
    Move { grab: egui::Vec2 },
    Resize,
}

struct Export {
    done: Arc<AtomicUsize>,
    total: usize,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<Result<(PathBuf, u64), String>>,
}

pub struct Editor {
    pub path: PathBuf,
    frames: Arc<Vec<Frame>>,
    w: u32,
    h: u32,
    cur: usize,
    playing: bool,
    play_acc: f32,
    trim: (usize, usize),
    items: Vec<Item>,
    preps: Vec<Prepared>,
    selected: Option<usize>,
    dirty: bool,
    tex: Option<TextureHandle>,
    shown: Option<(usize, u64)>,
    version: u64,
    drag: Option<Drag>,
    quality: u8,
    export: Option<Export>,
    message: Option<(String, bool)>, // (文字, 是否為錯誤)
    last_export: Option<PathBuf>,
    font: Option<Arc<FontVec>>,
}

impl Editor {
    pub fn open(path: &Path, font: Option<Arc<FontVec>>) -> Result<Self, String> {
        let frames = render::load_gif(path)?;
        let (w, h) = frames[0].rgba.dimensions();
        let n = frames.len();
        Ok(Editor {
            path: path.to_path_buf(),
            frames: Arc::new(frames),
            w,
            h,
            cur: 0,
            playing: false,
            play_acc: 0.0,
            trim: (0, n - 1),
            items: Vec::new(),
            preps: Vec::new(),
            selected: None,
            dirty: true,
            tex: None,
            shown: None,
            version: 0,
            drag: None,
            quality: 90,
            export: None,
            message: None,
            last_export: None,
            font,
        })
    }

    fn n(&self) -> usize {
        self.frames.len()
    }

    fn time_at(&self, idx: usize) -> f32 {
        self.frames[..idx].iter().map(|f| f.delay_ms).sum::<u32>() as f32 / 1000.0
    }

    fn touch(&mut self) {
        self.dirty = true;
    }

    fn add(&mut self, kind: Kind, x: f32, y: f32, w: f32, h: f32) {
        self.items.push(Item { kind, x, y, w, h, start: self.trim.0, end: self.trim.1 });
        self.selected = Some(self.items.len() - 1);
        self.touch();
    }

    fn add_mosaic(&mut self) {
        let (w, h) = (self.w as f32, self.h as f32);
        self.add(Kind::Mosaic { block: ((w / 60.0) as u32).clamp(6, 24) }, w * 0.375, h * 0.375, w * 0.25, h * 0.25);
    }

    fn add_text(&mut self) {
        let (w, h) = (self.w as f32, self.h as f32);
        let size = (h / 12.0).clamp(14.0, 72.0);
        self.add(
            Kind::Text { text: "輸入文字".into(), size, color: [255, 255, 255, 255], bg: true },
            w * 0.05,
            h * 0.78,
            0.0,
            0.0,
        );
    }

    fn add_image_from(&mut self, p: &Path) {
        match image::open(p) {
            Ok(img) => {
                let img = img.to_rgba8();
                let (iw, ih) = (img.width() as f32, img.height() as f32);
                // 預設縮到 GIF 寬度的 1/4 以內
                let s = (self.w as f32 * 0.25 / iw).min(self.h as f32 * 0.4 / ih).min(1.0);
                let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let (dw, dh) = (iw * s, ih * s);
                self.add(
                    Kind::Image { name, img: Arc::new(img), opacity: 1.0 },
                    self.w as f32 - dw - self.w as f32 * 0.03,
                    self.h as f32 * 0.03,
                    dw,
                    dh,
                );
            }
            Err(e) => self.message = Some((format!("無法讀取圖片：{e}"), true)),
        }
    }

    fn rebuild(&mut self) {
        if !self.dirty {
            return;
        }
        let font = self.font.clone();
        self.preps = self
            .items
            .iter_mut()
            .map(|it| render::prepare(it, font.as_deref(), self.w, self.h))
            .collect();
        self.dirty = false;
        self.version += 1;
    }

    fn refresh_texture(&mut self, ctx: &egui::Context) {
        self.rebuild();
        let key = (self.cur, self.version);
        if self.shown == Some(key) && self.tex.is_some() {
            return;
        }
        let mut img = self.frames[self.cur].rgba.clone();
        render::apply(&mut img, &self.items, &self.preps, self.cur);
        let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
        match &mut self.tex {
            Some(t) => t.set(ci, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("gif-frame", ci, egui::TextureOptions::LINEAR)),
        }
        self.shown = Some(key);
    }

    fn start_export(&mut self, out: PathBuf) {
        let frames = self.frames.clone();
        let mut items = self.items.clone();
        let font = self.font.clone();
        let (w, h) = (self.w, self.h);
        let (a, b) = self.trim;
        let quality = self.quality;
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (d2, c2) = (done.clone(), cancel.clone());

        let handle = thread::spawn(move || -> Result<(PathBuf, u64), String> {
            let preps: Vec<Prepared> =
                items.iter_mut().map(|it| render::prepare(it, font.as_deref(), w, h)).collect();
            let enc = GifWriter::create(
                &out,
                gifenc::Settings { max_width: 0, quality, progress: Some(d2) },
            )
            .map_err(|e| format!("{e:#}"))?;

            let mut pts = 0.0f64;
            for idx in a..=b {
                if c2.load(Ordering::Relaxed) {
                    enc.abort();
                    let _ = std::fs::remove_file(&out);
                    return Err("已取消匯出".into());
                }
                let mut img = frames[idx].rgba.clone();
                render::apply(&mut img, &items, &preps, idx);
                enc.add(img, pts).map_err(|e| format!("{e:#}"))?;
                pts += frames[idx].delay_ms as f64 / 1000.0;
            }
            enc.finish(pts).map_err(|e| format!("{e:#}"))?;
            let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            Ok((out, size))
        });
        self.export = Some(Export { done, total: b - a + 1, cancel, handle });
        self.message = None;
    }

    fn poll_export(&mut self) {
        if self.export.as_ref().is_some_and(|e| e.handle.is_finished()) {
            let ex = self.export.take().unwrap();
            self.message = Some(match ex.handle.join() {
                Ok(Ok((p, size))) => {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    self.last_export = Some(p);
                    (format!("已匯出 {name}（{:.2} MB）", size as f64 / 1_048_576.0), false)
                }
                Ok(Err(e)) => (e, true),
                Err(_) => ("匯出時發生錯誤".into(), true),
            });
        }
    }

    // ───────────────────────── 介面 ─────────────────────────

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Action {
        let ctx = ui.ctx().clone();
        self.poll_export();
        let mut action = Action::None;

        // 拖進來的檔案：GIF 換檔、圖片則加成圖片效果
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for p in dropped {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext == "gif" {
                match Editor::open(&p, self.font.clone()) {
                    Ok(ed) => {
                        *self = ed;
                        return Action::None;
                    }
                    Err(e) => self.message = Some((e, true)),
                }
            } else if ["png", "jpg", "jpeg"].contains(&ext.as_str()) {
                self.add_image_from(&p);
            }
        }

        egui::Panel::top("ed_top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("← 返回錄影").clicked() {
                    action = Action::Back;
                }
                if ui.button("開啟 GIF…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("GIF", &["gif"]).pick_file() {
                        match Editor::open(&p, self.font.clone()) {
                            Ok(ed) => *self = ed,
                            Err(e) => self.message = Some((e, true)),
                        }
                    }
                }
                ui.separator();
                ui.label(RichText::new(self.path.file_name().unwrap_or_default().to_string_lossy()).strong());
                ui.label(format!("{}x{}，{} 格，{:.1} 秒", self.w, self.h, self.n(), self.time_at(self.n())));
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("ed_bottom").show(ui, |ui| self.timeline_ui(ui));
        egui::Panel::right("ed_side").exact_size(270.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.side_ui(ui));
        });
        egui::CentralPanel::default().show(ui, |ui| self.preview_ui(ui));

        if self.playing || self.export.is_some() {
            ctx.request_repaint();
        }
        action
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let n = self.n();
        ui.horizontal(|ui| {
            let label = if self.playing { "■ 暫停" } else { "▶ 播放" };
            if ui.add(egui::Button::new(label).min_size(vec2(70.0, 0.0))).clicked() {
                self.playing = !self.playing;
                if self.playing && (self.cur < self.trim.0 || self.cur >= self.trim.1) {
                    self.cur = self.trim.0;
                }
                self.play_acc = 0.0;
            }
            if ui.button("◀").on_hover_text("上一格").clicked() && self.cur > 0 {
                self.cur -= 1;
                self.playing = false;
            }
            if ui.button("▶").on_hover_text("下一格").clicked() && self.cur + 1 < n {
                self.cur += 1;
                self.playing = false;
            }
            ui.spacing_mut().slider_width = (ui.available_width() - 150.0).max(100.0);
            if ui.add(egui::Slider::new(&mut self.cur, 0..=n - 1).show_value(false)).changed() {
                self.playing = false;
            }
            ui.label(format!("{} / {}　{:.2}s", self.cur + 1, n, self.time_at(self.cur)));
        });

        // 播放：照每格的延遲前進，只在裁剪範圍內循環
        if self.playing {
            self.play_acc += ui.input(|i| i.stable_dt).min(0.2);
            loop {
                let d = self.frames[self.cur].delay_ms as f32 / 1000.0;
                if self.play_acc < d {
                    break;
                }
                self.play_acc -= d;
                self.cur = if self.cur >= self.trim.1 { self.trim.0 } else { self.cur + 1 };
            }
        }

        ui.horizontal(|ui| {
            ui.label("裁剪：");
            if ui.button("設為起點").clicked() {
                self.trim.0 = self.cur.min(self.trim.1);
            }
            if ui.button("設為終點").clicked() {
                self.trim.1 = self.cur.max(self.trim.0);
            }
            if ui.button("重設").clicked() {
                self.trim = (0, n - 1);
            }
            let (a, b) = self.trim;
            let kept = self.time_at(b + 1) - self.time_at(a);
            ui.label(format!("保留第 {}–{} 格（{:.1} 秒）", a + 1, b + 1, kept));
        });
        ui.add_space(6.0);
    }

    fn side_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("新增效果").strong());
        ui.horizontal(|ui| {
            if ui.button("▦ 馬賽克").clicked() {
                self.add_mosaic();
            }
            if ui.button("T 文字").clicked() {
                self.add_text();
            }
            if ui.button("▣ 圖片").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("圖片", &["png", "jpg", "jpeg"]).pick_file() {
                    self.add_image_from(&p);
                }
            }
        });
        ui.label(RichText::new("在預覽上拖曳移動，拉右下角縮放；圖片也可以直接拖進視窗").small().weak());

        ui.add_space(8.0);
        ui.label(RichText::new("效果清單").strong());
        if self.items.is_empty() {
            ui.label(RichText::new("（尚未加入）").weak());
        }
        for i in 0..self.items.len() {
            let it = &self.items[i];
            let txt = format!("{}　第 {}–{} 格", it.kind.label(), it.start + 1, it.end + 1);
            if ui.selectable_label(self.selected == Some(i), txt).clicked() {
                self.selected = Some(i);
                // 跳到看得到這個效果的位置
                let it = &self.items[i];
                if !it.visible(self.cur) {
                    self.cur = it.start;
                }
            }
        }

        if let Some(i) = self.selected.filter(|&i| i < self.items.len()) {
            ui.add_space(8.0);
            ui.separator();
            self.props_ui(ui, i);
        }

        ui.add_space(12.0);
        ui.separator();
        self.export_ui(ui);
    }

    fn props_ui(&mut self, ui: &mut egui::Ui, i: usize) {
        let n = self.n();
        let cur = self.cur;
        let mut changed = false;
        let mut delete = false;
        let mut layer: i32 = 0;
        let it = &mut self.items[i];

        ui.label(RichText::new(format!("編輯：{}", it.kind.label())).strong());
        ui.horizontal(|ui| {
            ui.label("出現");
            changed |= ui.add(egui::DragValue::new(&mut it.start).range(0..=it.end).custom_formatter(|v, _| format!("{}", v as usize + 1)).custom_parser(|s| s.parse::<f64>().ok().map(|v| v - 1.0))).changed();
            ui.label("到");
            changed |= ui.add(egui::DragValue::new(&mut it.end).range(it.start..=n - 1).custom_formatter(|v, _| format!("{}", v as usize + 1)).custom_parser(|s| s.parse::<f64>().ok().map(|v| v - 1.0))).changed();
            ui.label("格");
        });
        ui.horizontal(|ui| {
            if ui.small_button("從目前格開始").clicked() {
                it.start = cur.min(it.end);
                changed = true;
            }
            if ui.small_button("到目前格結束").clicked() {
                it.end = cur.max(it.start);
                changed = true;
            }
            if ui.small_button("全部").clicked() {
                it.start = 0;
                it.end = n - 1;
                changed = true;
            }
        });

        match &mut it.kind {
            Kind::Mosaic { block } => {
                changed |= ui.add(egui::Slider::new(block, 3..=48).text("格子大小")).changed();
            }
            Kind::Text { text, size, color, bg } => {
                changed |= ui
                    .add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY))
                    .changed();
                changed |= ui.add(egui::Slider::new(size, 10.0..=160.0).text("字級")).changed();
                ui.horizontal(|ui| {
                    ui.label("顏色");
                    changed |= ui.color_edit_button_srgba_unmultiplied(color).changed();
                    for c in [[255, 255, 255, 255], [255, 220, 0, 255], [255, 70, 60, 255], [0, 0, 0, 255]] {
                        let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                        ui.painter().rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
                        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.0, Color32::GRAY), StrokeKind::Inside);
                        if resp.clicked() {
                            *color = c;
                            changed = true;
                        }
                    }
                });
                changed |= ui.checkbox(bg, "半透明黑底").changed();
            }
            Kind::Image { img, opacity, .. } => {
                changed |= ui.add(egui::Slider::new(opacity, 0.1..=1.0).text("不透明度")).changed();
                let aspect = img.height() as f32 / img.width() as f32;
                let mut wv = it.w;
                if ui.add(egui::DragValue::new(&mut wv).range(8.0..=4000.0).suffix(" px 寬")).changed() {
                    it.w = wv;
                    it.h = wv * aspect;
                    changed = true;
                }
                if ui.small_button("原始大小").clicked() {
                    it.w = img.width() as f32;
                    it.h = img.height() as f32;
                    changed = true;
                }
            }
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("上移一層").clicked() {
                layer = 1;
            }
            if ui.button("下移一層").clicked() {
                layer = -1;
            }
            if ui.button(RichText::new("刪除").color(Color32::from_rgb(220, 70, 60))).clicked() {
                delete = true;
            }
        });

        if delete {
            self.items.remove(i);
            self.selected = None;
            changed = true;
        } else if layer == 1 && i + 1 < self.items.len() {
            self.items.swap(i, i + 1);
            self.selected = Some(i + 1);
            changed = true;
        } else if layer == -1 && i > 0 {
            self.items.swap(i, i - 1);
            self.selected = Some(i - 1);
            changed = true;
        }
        if changed {
            self.touch();
        }
    }

    fn export_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("匯出").strong());
        ui.add_enabled(self.export.is_none(), egui::Slider::new(&mut self.quality, 30..=100).text("畫質"));
        if let Some(ex) = &self.export {
            let done = ex.done.load(Ordering::Relaxed).min(ex.total);
            ui.add(egui::ProgressBar::new(done as f32 / ex.total as f32).text(format!("匯出中 {done}/{}", ex.total)));
            if ui.button("取消匯出").clicked() {
                ex.cancel.store(true, Ordering::Relaxed);
            }
        } else if ui.add(egui::Button::new(RichText::new("匯出 GIF…").size(16.0)).min_size(vec2(140.0, 30.0))).clicked() {
            let stem = self.path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
            if let Some(out) = rfd::FileDialog::new()
                .add_filter("GIF", &["gif"])
                .set_directory(&dir)
                .set_file_name(format!("{stem}-edited.gif"))
                .save_file()
            {
                let out = if out.extension().is_none() { out.with_extension("gif") } else { out };
                if out == self.path {
                    self.message = Some(("請另存新檔，不要覆蓋正在編輯的原始檔".into(), true));
                } else {
                    self.start_export(out);
                }
            }
        }
        if let Some((m, err)) = &self.message {
            let c = if *err { Color32::from_rgb(220, 70, 60) } else { Color32::from_rgb(40, 160, 70) };
            ui.label(RichText::new(m).color(c));
        }
        if let (Some(p), None) = (&self.last_export, &self.export) {
            let p = p.clone();
            ui.horizontal(|ui| {
                if ui.button("開啟").clicked() {
                    crate::open_path(&p);
                }
                if ui.button("開啟資料夾").clicked() {
                    crate::reveal(&p);
                }
            });
        }
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.refresh_texture(&ctx);
        let avail = ui.available_rect_before_wrap();
        let s = (avail.width() / self.w as f32).min(avail.height() / self.h as f32).min(4.0);
        let size = vec2(self.w as f32 * s, self.h as f32 * s);
        let img_rect = Rect::from_center_size(avail.center(), size);
        let resp = ui.allocate_rect(img_rect, Sense::click_and_drag());
        let painter = ui.painter_at(avail);

        // 棋盤格底，看得出透明區域
        painter.rect_filled(img_rect, 0.0, Color32::from_gray(60));
        if let Some(t) = &self.tex {
            painter.image(t.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // 裁剪範圍外的影格加上暗色提示
        if self.cur < self.trim.0 || self.cur > self.trim.1 {
            painter.rect_filled(img_rect, 0.0, Color32::from_black_alpha(140));
            painter.text(img_rect.center(), egui::Align2::CENTER_CENTER, "此格在裁剪範圍外，不會匯出", egui::FontId::proportional(18.0), Color32::WHITE);
        }

        let to_screen = |x: f32, y: f32| img_rect.min + vec2(x, y) * s;
        let to_img = |p: Pos2| ((p - img_rect.min) / s).to_pos2();

        // 外框與把手
        let handle_size = 10.0;
        let grab_r = 14.0; // 把手的感應範圍比畫出來的大一些，比較好抓
        for (i, it) in self.items.iter().enumerate() {
            if !it.visible(self.cur) {
                continue;
            }
            let r = Rect::from_min_max(to_screen(it.x, it.y), to_screen(it.x + it.w, it.y + it.h));
            if self.selected == Some(i) {
                painter.rect_stroke(r, 0.0, Stroke::new(2.0, Color32::from_rgb(60, 160, 255)), StrokeKind::Outside);
                if !matches!(it.kind, Kind::Text { .. }) {
                    let h = Rect::from_center_size(r.max, vec2(handle_size, handle_size));
                    painter.rect_filled(h, 2.0, Color32::from_rgb(60, 160, 255));
                }
            } else {
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_white_alpha(90)), StrokeKind::Outside);
            }
        }

        // 滑鼠游標提示
        if let Some(hp) = resp.hover_pos() {
            if let Some(i) = self.selected {
                let it = &self.items[i];
                if it.visible(self.cur) && !matches!(it.kind, Kind::Text { .. }) && hp.distance(to_screen(it.x + it.w, it.y + it.h)) < grab_r {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                }
            }
        }

        if resp.drag_started() {
            if let Some(pp) = resp.interact_pointer_pos() {
                let p = to_img(pp);
                self.drag = None;
                if let Some(i) = self.selected {
                    let it = &self.items[i];
                    if it.visible(self.cur) && !matches!(it.kind, Kind::Text { .. }) && pp.distance(to_screen(it.x + it.w, it.y + it.h)) < grab_r {
                        self.drag = Some(Drag::Resize);
                    }
                }
                if self.drag.is_none() {
                    let hit = self.items.iter().enumerate().rev().find(|(_, it)| {
                        it.visible(self.cur) && p.x >= it.x && p.x <= it.x + it.w && p.y >= it.y && p.y <= it.y + it.h
                    });
                    match hit {
                        Some((i, it)) => {
                            self.drag = Some(Drag::Move { grab: vec2(p.x - it.x, p.y - it.y) });
                            self.selected = Some(i);
                        }
                        None => self.selected = None,
                    }
                }
                self.playing = false;
            }
        } else if resp.dragged() {
            if let (Some(d), Some(i), Some(pp)) = (&self.drag, self.selected, resp.interact_pointer_pos()) {
                let p = to_img(pp);
                let (fw, fh) = (self.w as f32, self.h as f32);
                let it = &mut self.items[i];
                match d {
                    Drag::Move { grab } => {
                        it.x = (p.x - grab.x).clamp(-it.w + 8.0, fw - 8.0).round();
                        it.y = (p.y - grab.y).clamp(-it.h + 8.0, fh - 8.0).round();
                    }
                    Drag::Resize => {
                        let w = (p.x - it.x).max(8.0).round();
                        match &it.kind {
                            Kind::Image { img, .. } => {
                                it.w = w;
                                it.h = (w * img.height() as f32 / img.width() as f32).round();
                            }
                            _ => {
                                it.w = w;
                                it.h = (p.y - it.y).max(8.0).round();
                            }
                        }
                    }
                }
                self.dirty = true;
            }
        } else if resp.drag_stopped() {
            self.drag = None;
        } else if resp.clicked() {
            // 單擊：選取（不拖曳）
            if let Some(pp) = resp.interact_pointer_pos() {
                let p = to_img(pp);
                self.selected = self
                    .items
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, it)| it.visible(self.cur) && p.x >= it.x && p.x <= it.x + it.w && p.y >= it.y && p.y <= it.y + it.h)
                    .map(|(i, _)| i);
            }
        }

        // Delete 鍵刪除選取的效果
        if let Some(i) = self.selected {
            let typing = ctx.memory(|m| m.focused().is_some());
            if !typing && ctx.input(|inp| inp.key_pressed(egui::Key::Delete)) {
                self.items.remove(i);
                self.selected = None;
                self.dirty = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 以範例 GIF 加上三種效果、裁剪後匯出，確認輸出正確
    #[test]
    fn export_with_effects() {
        let src = Path::new("/home/claude/sample.gif");
        if !src.exists() {
            return;
        }
        let font = render::load_font_bytes().and_then(|b| render::parse_font(&b)).map(Arc::new);
        let mut ed = Editor::open(src, font).unwrap();
        ed.add_mosaic();
        ed.items[0].x = 150.0;
        ed.items[0].y = 40.0;
        ed.items[0].w = 200.0;
        ed.items[0].h = 60.0;
        ed.add_text();
        if let Kind::Text { text, .. } = &mut ed.items[1].kind {
            *text = "示範：登入流程".into();
        }
        ed.items[1].start = 5;
        ed.add_image_from(Path::new("/home/claude/logo.png"));
        if let Kind::Image { opacity, .. } = &mut ed.items[2].kind {
            *opacity = 0.8;
        }
        ed.trim = (2, 27);
        ed.rebuild();
        let out = PathBuf::from("/tmp/edited.gif");
        ed.start_export(out.clone());
        while ed.export.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(100));
            ed.poll_export();
        }
        let (msg, err) = ed.message.clone().unwrap();
        assert!(!err, "{msg}");
        let back = render::load_gif(&out).unwrap();
        assert_eq!(back.len(), 26);
        println!("{msg}");
    }
}
