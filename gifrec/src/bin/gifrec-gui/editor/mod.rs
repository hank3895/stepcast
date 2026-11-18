//! GIF 編修畫面
//!
//! - mod.rs      狀態、面板、快捷鍵、復原 / 重做、專案、匯出
//! - canvas.rs   預覽畫面上的框選、拖曳、八方向縮放、裁切
//! - timeline.rs 縮圖時間軸、效果出現範圍色條、播放與頭尾裁剪

mod canvas;
mod timeline;

use crate::project;
use crate::render::{self, Doc, Frame, Item, Kind, Prepared};
use ab_glyph::FontVec;
use eframe::egui::{self, vec2, Color32, Key, Modifiers, RichText, TextureHandle};
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

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    Select,
    Mosaic,
    Blur,
    Text,
    Zoom,
    Crop,
}

impl Tool {
    const ALL: [(Tool, &'static str, &'static str, &'static str); 6] = [
        (Tool::Select, "選取", "V", "點選、拖曳移動、拉控制點縮放"),
        (Tool::Mosaic, "馬賽克", "M", "在畫面上拖曳框出要遮的範圍"),
        (Tool::Blur, "模糊", "B", "在畫面上拖曳框出要模糊的範圍"),
        (Tool::Text, "文字", "T", "在畫面上點一下放文字"),
        (Tool::Zoom, "拉近", "Z", "框出要放大的範圍，這段會平滑拉近再拉遠"),
        (Tool::Crop, "裁切", "C", "拖曳框出要保留的畫面，拉控制點調整"),
    ];
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Edit,
    Output,
}

struct Export {
    done: Arc<AtomicUsize>,
    total: usize,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<Result<(PathBuf, u64), String>>,
}

pub struct Editor {
    pub path: PathBuf,
    project_path: Option<PathBuf>,
    frames: Arc<Vec<Frame>>,
    w: u32,
    h: u32,

    // 編輯內容與歷史
    doc: Doc,
    committed: Doc,
    saved: Doc,
    undo: Vec<Doc>,
    redo: Vec<Doc>,

    // 預先算好的效果
    preps: Vec<Prepared>,
    prepped: Option<Vec<Item>>,

    // 播放
    cur: usize,
    playing: bool,
    play_acc: f32,
    scroll_to_cur: bool,

    // 互動
    tool: Tool,
    view: View,
    selected: Option<usize>,
    drag: Option<canvas::Drag>,
    bar_drag: Option<timeline::BarDrag>,
    clipboard: Option<Item>,
    focus_text: bool,
    typing: bool,
    v_pressed: bool,

    // 顯示
    tex: Option<TextureHandle>,
    tex_state: Option<(usize, View, Doc)>,
    thumbs: Vec<Option<TextureHandle>>,

    // 匯出與訊息
    quality: u8,
    export: Option<Export>,
    message: Option<(String, bool)>,
    last_export: Option<PathBuf>,
    font: Option<Arc<FontVec>>,
}

const HISTORY_LIMIT: usize = 200;

impl Editor {
    /// 開啟 GIF 或 .gifrec 專案檔
    pub fn open(path: &Path, font: Option<Arc<FontVec>>) -> Result<Self, String> {
        let is_project = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(project::EXT));
        let (gif, loaded) = if is_project {
            let l = project::load(path)?;
            (l.source.clone(), Some(l))
        } else {
            (path.to_path_buf(), None)
        };
        let frames = render::load_gif(&gif)?;
        let (w, h) = frames[0].rgba.dimensions();
        let n = frames.len();
        let mut doc = Doc::new(w, h, n);
        let mut message = None;
        if let Some(l) = loaded {
            if l.frames != n {
                message = Some((format!("注意：來源 GIF 的影格數（{n}）和存專案時（{}）不同，效果位置可能對不上", l.frames), true));
            }
            doc = l.doc;
            // 防呆：範圍超出時夾回來
            let last = n - 1;
            doc.trim = (doc.trim.0.min(last), doc.trim.1.min(last).max(doc.trim.0.min(last)));
            for it in &mut doc.items {
                it.end = it.end.min(last);
                it.start = it.start.min(it.end);
            }
            let [cx, cy, cw, ch] = doc.crop;
            if cx + cw > w || cy + ch > h || cw == 0 || ch == 0 {
                doc.crop = [0, 0, w, h];
            }
        }
        Ok(Editor {
            path: gif,
            project_path: is_project.then(|| path.to_path_buf()),
            frames: Arc::new(frames),
            w,
            h,
            committed: doc.clone(),
            saved: doc.clone(),
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            preps: Vec::new(),
            prepped: None,
            cur: 0,
            playing: false,
            play_acc: 0.0,
            scroll_to_cur: false,
            tool: Tool::Select,
            view: View::Edit,
            selected: None,
            drag: None,
            bar_drag: None,
            clipboard: None,
            focus_text: false,
            typing: false,
            v_pressed: false,
            tex: None,
            tex_state: None,
            thumbs: (0..n).map(|_| None).collect(),
            quality: 90,
            export: None,
            message,
            last_export: None,
            font,
        })
    }

    fn n(&self) -> usize {
        self.frames.len()
    }

    fn time_at(&self, idx: usize) -> f32 {
        self.frames[..idx.min(self.n())].iter().map(|f| f.delay_ms).sum::<u32>() as f32 / 1000.0
    }

    /// 從 idx 起算大約 secs 秒是第幾格
    fn frames_for(&self, idx: usize, secs: f32) -> usize {
        let mut t = 0.0;
        let mut i = idx;
        while i < self.doc.trim.1 && t < secs {
            t += self.frames[i].delay_ms as f32 / 1000.0;
            i += 1;
        }
        i
    }

    fn dirty(&self) -> bool {
        self.doc != self.saved
    }

    // ───────────────────────── 編輯內容 ─────────────────────────

    fn new_item(&self, kind: Kind, x: f32, y: f32, w: f32, h: f32) -> Item {
        let (start, end) = match kind {
            // 拉近預設只用在目前這附近約 2 秒
            Kind::Zoom => (self.cur, self.frames_for(self.cur, 2.0)),
            _ => self.doc.trim,
        };
        Item { kind, x, y, w, h, start, end }
    }

    fn push_item(&mut self, it: Item) -> usize {
        self.doc.items.push(it);
        let i = self.doc.items.len() - 1;
        self.selected = Some(i);
        i
    }

    fn default_text(&self) -> Kind {
        Kind::Text { text: "輸入文字".into(), size: (self.h as f32 / 12.0).clamp(14.0, 72.0), color: [255, 255, 255, 255], bg: true }
    }

    fn add_image_from(&mut self, p: &Path) {
        match image::open(p) {
            Ok(img) => {
                let img = img.to_rgba8();
                let (iw, ih) = (img.width() as f32, img.height() as f32);
                let s = (self.w as f32 * 0.25 / iw).min(self.h as f32 * 0.4 / ih).min(1.0);
                let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let (dw, dh) = ((iw * s).round(), (ih * s).round());
                let it = self.new_item(
                    Kind::Image { name, img: Arc::new(img), opacity: 1.0 },
                    (self.w as f32 - dw - self.w as f32 * 0.03).round(),
                    (self.h as f32 * 0.03).round(),
                    dw,
                    dh,
                );
                self.push_item(it);
                self.tool = Tool::Select;
            }
            Err(e) => self.message = Some((format!("無法讀取圖片：{e}"), true)),
        }
    }

    fn pick_image(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("圖片", &["png", "jpg", "jpeg"]).pick_file() {
            self.add_image_from(&p);
        }
    }

    fn delete_selected(&mut self) {
        if let Some(i) = self.selected.take() {
            if i < self.doc.items.len() {
                self.doc.items.remove(i);
            }
        }
    }

    fn duplicate(&mut self, src: Item) {
        let mut it = src;
        it.x += 12.0;
        it.y += 12.0;
        self.push_item(it);
    }

    fn copy_selected(&mut self) {
        if let Some(i) = self.selected {
            self.clipboard = self.doc.items.get(i).cloned();
        }
    }

    fn paste(&mut self) {
        if let Some(it) = self.clipboard.clone() {
            self.duplicate(it.clone());
            // 下次貼上再往右下偏移，不會疊在同一個位置
            if let Some(c) = &mut self.clipboard {
                c.x += 12.0;
                c.y += 12.0;
            }
        }
    }

    fn nudge(&mut self, dx: f32, dy: f32) {
        if let Some(it) = self.selected.and_then(|i| self.doc.items.get_mut(i)) {
            it.x += dx;
            it.y += dy;
        }
    }

    fn clamp_selection(&mut self) {
        if self.selected.is_some_and(|i| i >= self.doc.items.len()) {
            self.selected = None;
        }
    }

    // ───────────────────────── 復原 / 重做 ─────────────────────────

    /// 互動結束（放開滑鼠、離開文字框）才記一筆，拖曳一次只算一步
    fn commit_if_idle(&mut self, ctx: &egui::Context) {
        let busy = ctx.input(|i| i.pointer.any_down()) || self.typing || self.drag.is_some() || self.bar_drag.is_some();
        if !busy {
            self.commit_now();
        }
    }

    fn commit_now(&mut self) {
        if self.doc != self.committed {
            self.undo.push(std::mem::replace(&mut self.committed, self.doc.clone()));
            if self.undo.len() > HISTORY_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    fn undo(&mut self) {
        self.commit_now();
        if let Some(d) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.doc, d));
            self.committed = self.doc.clone();
            self.clamp_selection();
        }
    }

    fn redo(&mut self) {
        self.commit_now();
        if let Some(d) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.doc, d));
            self.committed = self.doc.clone();
            self.clamp_selection();
        }
    }

    // ───────────────────────── 專案 ─────────────────────────

    fn save_project(&mut self, save_as: bool) {
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => {
                let stem = self.path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
                rfd::FileDialog::new()
                    .add_filter("gifrec 專案", &[project::EXT])
                    .set_directory(&dir)
                    .set_file_name(format!("{stem}.{}", project::EXT))
                    .save_file()
                    .map(|p| if p.extension().is_none() { p.with_extension(project::EXT) } else { p })
            }
        };
        let Some(path) = path else { return };
        self.commit_now();
        match project::save(&path, &self.path, &self.doc, self.n()) {
            Ok(()) => {
                self.saved = self.doc.clone();
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                self.project_path = Some(path);
                self.message = Some((format!("已儲存專案 {name}"), false));
            }
            Err(e) => self.message = Some((e, true)),
        }
    }

    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("GIF 或專案", &["gif", project::EXT])
            .set_directory(self.path.parent().unwrap_or(Path::new(".")))
            .pick_file()
        {
            self.replace_with(&p);
        }
    }

    fn replace_with(&mut self, p: &Path) {
        match Editor::open(p, self.font.clone()) {
            Ok(ed) => *self = ed,
            Err(e) => self.message = Some((e, true)),
        }
    }

    // ───────────────────────── 繪製 ─────────────────────────

    fn rebuild(&mut self) {
        if self.prepped.as_ref() == Some(&self.doc.items) {
            return;
        }
        let font = self.font.clone();
        let (w, h) = (self.w, self.h);
        self.preps = self.doc.items.iter_mut().map(|it| render::prepare(it, font.as_deref(), w, h)).collect();
        // 文字的大小在 prepare 時決定，同步回「已記錄」的狀態，免得多出一筆復原
        if self.committed.items.len() == self.doc.items.len() {
            for (c, d) in self.committed.items.iter_mut().zip(&self.doc.items) {
                if c.kind == d.kind && c.x == d.x && c.y == d.y {
                    c.w = d.w;
                    c.h = d.h;
                }
            }
        }
        self.prepped = Some(self.doc.items.clone());
    }

    fn refresh_texture(&mut self, ctx: &egui::Context) {
        self.rebuild();
        if self.tex.is_some() && self.tex_state.as_ref().is_some_and(|(c, v, d)| *c == self.cur && *v == self.view && *d == self.doc) {
            return;
        }
        let src = &self.frames[self.cur].rgba;
        let img = match self.view {
            View::Edit => {
                let mut img = src.clone();
                render::apply(&mut img, &self.doc.items, &self.preps, self.cur);
                img
            }
            View::Output => render::compose(src, &self.doc, &self.preps, self.cur),
        };
        let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
        match &mut self.tex {
            Some(t) => t.set(ci, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("gif-frame", ci, egui::TextureOptions::LINEAR)),
        }
        self.tex_state = Some((self.cur, self.view, self.doc.clone()));
    }

    // ───────────────────────── 匯出 ─────────────────────────

    fn start_export(&mut self, out: PathBuf) {
        self.commit_now();
        let frames = self.frames.clone();
        let mut doc = self.doc.clone();
        let font = self.font.clone();
        let (w, h) = (self.w, self.h);
        let (a, b) = doc.trim;
        let quality = self.quality;
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (d2, c2) = (done.clone(), cancel.clone());

        let handle = thread::spawn(move || -> Result<(PathBuf, u64), String> {
            let preps: Vec<Prepared> = doc.items.iter_mut().map(|it| render::prepare(it, font.as_deref(), w, h)).collect();
            let enc = GifWriter::create(&out, gifenc::Settings { max_width: 0, quality, progress: Some(d2) })
                .map_err(|e| format!("{e:#}"))?;
            let mut pts = 0.0f64;
            for idx in a..=b {
                if c2.load(Ordering::Relaxed) {
                    enc.abort();
                    let _ = std::fs::remove_file(&out);
                    return Err("已取消匯出".into());
                }
                let img = render::compose(&frames[idx].rgba, &doc, &preps, idx);
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

    fn export_dialog(&mut self) {
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

    // ───────────────────────── 快捷鍵 ─────────────────────────

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // 正在輸入文字或數字時，不攔截按鍵
        if self.typing {
            return;
        }
        let cmd = Modifiers::COMMAND;
        let mut act: Vec<&str> = Vec::new();
        let v_seen = &mut self.v_pressed;
        ctx.input_mut(|i| {
            if i.consume_key(cmd | Modifiers::SHIFT, Key::Z) || i.consume_key(cmd, Key::Y) {
                act.push("redo");
            }
            if i.consume_key(cmd, Key::Z) {
                act.push("undo");
            }
            if i.consume_key(cmd | Modifiers::SHIFT, Key::S) {
                act.push("saveas");
            }
            if i.consume_key(cmd, Key::S) {
                act.push("save");
            }
            if i.consume_key(cmd, Key::D) {
                act.push("dup");
            }
            // Ctrl+C / X 在 egui 會變成 Copy / Cut 事件。
            // Ctrl+V 的「按下」會被 egui 拿去讀系統剪貼簿（剪貼簿沒文字時就整個吞掉），
            // 只剩「放開 V」。所以：放開 V 之前沒看到按下 V → 那就是 Ctrl+V。
            // （不靠放開時的 Ctrl 狀態，因為使用者可能先放開 Ctrl。）
            for e in &i.events {
                match e {
                    egui::Event::Copy => act.push("copy"),
                    egui::Event::Cut => act.push("cut"),
                    egui::Event::Key { key: Key::V, pressed: true, .. } => *v_seen = true,
                    egui::Event::Key { key: Key::V, pressed: false, .. } => {
                        if !std::mem::take(v_seen) {
                            act.push("paste");
                        }
                    }
                    _ => {}
                }
            }
            if i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace) {
                act.push("delete");
            }
            if i.consume_key(Modifiers::NONE, Key::Space) {
                act.push("play");
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                act.push("esc");
            }
            if i.consume_key(Modifiers::NONE, Key::Home) {
                act.push("home");
            }
            if i.consume_key(Modifiers::NONE, Key::End) {
                act.push("end");
            }
            if i.consume_key(Modifiers::NONE, Key::Comma) {
                act.push("prev");
            }
            if i.consume_key(Modifiers::NONE, Key::Period) {
                act.push("next");
            }
            for k in [Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown] {
                // 先判斷 Shift：egui 的 consume_key(NONE, …) 也會吃掉按著 Shift 的按鍵
                for (m, mul) in [(Modifiers::SHIFT, 10.0), (Modifiers::NONE, 1.0)] {
                    if i.consume_key(m, k) {
                        let tag: &'static str = match (k, mul > 1.0) {
                            (Key::ArrowLeft, false) => "l1",
                            (Key::ArrowLeft, true) => "l10",
                            (Key::ArrowRight, false) => "r1",
                            (Key::ArrowRight, true) => "r10",
                            (Key::ArrowUp, false) => "u1",
                            (Key::ArrowUp, true) => "u10",
                            (Key::ArrowDown, false) => "d1",
                            _ => "d10",
                        };
                        act.push(tag);
                    }
                }
            }
            for (tool, _, key, _) in Tool::ALL {
                let k = Key::from_name(key).unwrap();
                if i.consume_key(Modifiers::NONE, k) {
                    act.push(match tool {
                        Tool::Select => "tsel",
                        Tool::Mosaic => "tmos",
                        Tool::Blur => "tblur",
                        Tool::Text => "ttext",
                        Tool::Zoom => "tzoom",
                        Tool::Crop => "tcrop",
                    });
                }
            }
        });

        let n = self.n();
        for a in act {
            match a {
                "undo" => self.undo(),
                "redo" => self.redo(),
                "save" => self.save_project(false),
                "saveas" => self.save_project(true),
                "dup" => {
                    if let Some(it) = self.selected.and_then(|i| self.doc.items.get(i)).cloned() {
                        self.duplicate(it);
                    }
                }
                "copy" => self.copy_selected(),
                "cut" => {
                    self.copy_selected();
                    self.delete_selected();
                }
                "paste" => self.paste(),
                "delete" => self.delete_selected(),
                "play" => self.toggle_play(),
                "esc" => {
                    self.tool = Tool::Select;
                    self.selected = None;
                }
                "home" => self.seek(self.doc.trim.0),
                "end" => self.seek(self.doc.trim.1),
                "prev" => self.seek(self.cur.saturating_sub(1)),
                "next" => self.seek((self.cur + 1).min(n - 1)),
                "tsel" => self.tool = Tool::Select,
                "tmos" => self.tool = Tool::Mosaic,
                "tblur" => self.tool = Tool::Blur,
                "ttext" => self.tool = Tool::Text,
                "tzoom" => self.tool = Tool::Zoom,
                "tcrop" => self.tool = Tool::Crop,
                arrow => {
                    let mul = if arrow.ends_with("10") { 10.0 } else { 1.0 };
                    let dir = &arrow[..1];
                    if self.selected.is_some() {
                        match dir {
                            "l" => self.nudge(-mul, 0.0),
                            "r" => self.nudge(mul, 0.0),
                            "u" => self.nudge(0.0, -mul),
                            _ => self.nudge(0.0, mul),
                        }
                    } else {
                        // 沒選取效果時，左右鍵 = 前後一格（Shift = 10 格）
                        let step = mul as usize;
                        match dir {
                            "l" => self.seek(self.cur.saturating_sub(step)),
                            "r" => self.seek((self.cur + step).min(n - 1)),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    fn seek(&mut self, idx: usize) {
        self.cur = idx.min(self.n() - 1);
        self.playing = false;
        self.scroll_to_cur = true;
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing;
        let (a, b) = self.doc.trim;
        if self.playing && (self.cur < a || self.cur >= b) {
            self.cur = a;
        }
        self.play_acc = 0.0;
    }

    // ───────────────────────── 介面 ─────────────────────────

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Action {
        let ctx = ui.ctx().clone();
        self.poll_export();
        self.shortcuts(&ctx);
        let mut action = Action::None;

        // 拖進來的檔案：GIF / 專案換檔，圖片加成圖片效果
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for p in dropped {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext == "gif" || ext == project::EXT {
                self.replace_with(&p);
                return Action::None;
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
                if ui.button("開啟…").on_hover_text("開啟 GIF 或 .gifrec 專案").clicked() {
                    self.open_dialog();
                }
                if ui.button("儲存專案").on_hover_text("Ctrl+S").clicked() {
                    self.save_project(false);
                }
                if ui.button("另存專案…").on_hover_text("Ctrl+Shift+S").clicked() {
                    self.save_project(true);
                }
                ui.separator();
                let can_undo = !self.undo.is_empty() || self.doc != self.committed;
                if ui.add_enabled(can_undo, egui::Button::new("復原")).on_hover_text("Ctrl+Z").clicked() {
                    self.undo();
                }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("重做")).on_hover_text("Ctrl+Y").clicked() {
                    self.redo();
                }
                ui.separator();
                ui.selectable_value(&mut self.view, View::Edit, "編輯畫面");
                ui.selectable_value(&mut self.view, View::Output, "輸出預覽")
                    .on_hover_text("套用裁切、拉近、輸出尺寸後的實際樣子");
                ui.separator();
                let name = self.project_path.as_ref().unwrap_or(&self.path).file_name().unwrap_or_default().to_string_lossy().into_owned();
                let mark = if self.dirty() { " ●" } else { "" };
                ui.label(RichText::new(format!("{name}{mark}")).strong()).on_hover_text(if self.dirty() { "有未儲存的變更" } else { "" });
                let (ow, oh) = self.doc.out_size();
                ui.label(RichText::new(format!("{}x{} → 輸出 {ow}x{oh}", self.w, self.h)).weak());
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("ed_bottom").show(ui, |ui| self.timeline_ui(ui));
        egui::Panel::right("ed_side").exact_size(290.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.side_ui(ui));
        });
        egui::CentralPanel::default().show(ui, |ui| self.canvas_ui(ui));

        self.typing = ctx.memory(|m| m.focused().is_some());
        self.commit_if_idle(&ctx);
        if self.playing || self.export.is_some() {
            ctx.request_repaint();
        }
        action
    }

    fn side_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("工具").strong());
        egui::Grid::new("tools").num_columns(3).spacing([4.0, 4.0]).show(ui, |ui| {
            for (k, (tool, name, key, tip)) in Tool::ALL.iter().enumerate() {
                let btn = egui::Button::selectable(self.tool == *tool, format!("{name} {key}")).min_size(vec2(84.0, 26.0));
                if ui.add(btn).on_hover_text(*tip).clicked() {
                    self.tool = *tool;
                }
                if k % 3 == 2 {
                    ui.end_row();
                }
            }
        });
        if ui.add(egui::Button::new("＋ 加入圖片…").min_size(vec2(260.0, 24.0))).on_hover_text("也可以把 PNG / JPG 直接拖進視窗").clicked() {
            self.pick_image();
        }
        let tip = Tool::ALL.iter().find(|t| t.0 == self.tool).map(|t| t.3).unwrap_or("");
        ui.label(RichText::new(tip).small().weak());

        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new("效果清單").strong());
        if self.doc.items.is_empty() {
            ui.label(RichText::new("（尚未加入）").weak());
        }
        let mut jump = None;
        for i in (0..self.doc.items.len()).rev() {
            let it = &self.doc.items[i];
            let c = it.kind.color();
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                let txt = format!("{}　{}–{} 格", it.kind.label(), it.start + 1, it.end + 1);
                if ui.selectable_label(self.selected == Some(i), txt).clicked() {
                    self.selected = Some(i);
                    self.tool = Tool::Select;
                    if !it.visible(self.cur) {
                        jump = Some(it.start);
                    }
                }
            });
        }
        if let Some(j) = jump {
            self.seek(j);
        }

        if let Some(i) = self.selected.filter(|&i| i < self.doc.items.len()) {
            ui.add_space(6.0);
            ui.separator();
            self.props_ui(ui, i);
        }

        ui.add_space(8.0);
        ui.separator();
        self.frame_ui(ui);

        ui.add_space(8.0);
        ui.separator();
        self.export_ui(ui);
        ui.add_space(6.0);
        ui.label(
            RichText::new("快捷鍵：Ctrl+Z 復原、Ctrl+Y 重做、Ctrl+C/V 複製貼上、Ctrl+D 複製一份、Delete 刪除、方向鍵微調（Shift ×10）、空白鍵播放、Ctrl+S 存專案")
                .small()
                .weak(),
        );
    }

    fn props_ui(&mut self, ui: &mut egui::Ui, i: usize) {
        let n = self.n();
        let cur = self.cur;
        let (fw, fh) = (self.w as f32, self.h as f32);
        let focus_text = std::mem::take(&mut self.focus_text);
        let mut delete = false;
        let mut layer: i32 = 0;
        let mut dup = false;
        let it = &mut self.doc.items[i];

        ui.label(RichText::new(format!("編輯：{}", it.kind.label())).strong());
        ui.horizontal(|ui| {
            ui.label("出現");
            let fmt = |v: f64, _: std::ops::RangeInclusive<usize>| format!("{}", v as usize + 1);
            let parse = |s: &str| s.parse::<f64>().ok().map(|v| v - 1.0);
            ui.add(egui::DragValue::new(&mut it.start).range(0..=it.end).custom_formatter(fmt).custom_parser(parse));
            ui.label("到");
            ui.add(egui::DragValue::new(&mut it.end).range(it.start..=n - 1).custom_formatter(fmt).custom_parser(parse));
            ui.label("格");
        });
        ui.horizontal(|ui| {
            if ui.small_button("從這格開始").clicked() {
                it.start = cur.min(it.end);
            }
            if ui.small_button("到這格結束").clicked() {
                it.end = cur.max(it.start);
            }
            if ui.small_button("全部").clicked() {
                it.start = 0;
                it.end = n - 1;
            }
        });

        match &mut it.kind {
            Kind::Mosaic { block } => {
                ui.add(egui::Slider::new(block, 3..=48).text("格子大小"));
                if ui.small_button("改成模糊").clicked() {
                    it.kind = Kind::Blur { radius: 8 };
                }
            }
            Kind::Blur { radius } => {
                ui.add(egui::Slider::new(radius, 1..=30).text("模糊程度"));
                if ui.small_button("改成馬賽克").clicked() {
                    it.kind = Kind::Mosaic { block: 12 };
                }
            }
            Kind::Text { text, size, color, bg } => {
                let r = ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY));
                if focus_text {
                    r.request_focus();
                }
                ui.add(egui::Slider::new(size, 10.0..=160.0).text("字級"));
                ui.horizontal(|ui| {
                    ui.label("顏色");
                    ui.color_edit_button_srgba_unmultiplied(color);
                    for c in [[255, 255, 255, 255], [255, 220, 0, 255], [255, 70, 60, 255], [0, 0, 0, 255]] {
                        let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::click());
                        ui.painter().rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
                        ui.painter().rect_stroke(r, 3.0, egui::Stroke::new(1.0, Color32::GRAY), egui::StrokeKind::Inside);
                        if resp.clicked() {
                            *color = c;
                        }
                    }
                });
                ui.checkbox(bg, "半透明黑底");
            }
            Kind::Image { img, opacity, .. } => {
                ui.add(egui::Slider::new(opacity, 0.1..=1.0).text("不透明度"));
                let aspect = img.height() as f32 / img.width() as f32;
                let mut wv = it.w;
                if ui.add(egui::DragValue::new(&mut wv).range(8.0..=4000.0).suffix(" px 寬")).changed() {
                    it.w = wv.round();
                    it.h = (wv * aspect).round();
                }
                if ui.small_button("原始大小").clicked() {
                    it.w = img.width() as f32;
                    it.h = img.height() as f32;
                }
            }
            Kind::Zoom => {
                let len = it.end - it.start + 1;
                ui.label(RichText::new(format!("放大約 {:.1} 倍，共 {len} 格（前後各約 {} 格平滑過渡）", fw / it.w.max(1.0), (len / 3).clamp(1, 5))).small());
                if ui.small_button("切到「輸出預覽」看效果").clicked() {
                    self.view = View::Output;
                }
            }
        }

        // 位置與大小（可直接輸入）
        let it = &mut self.doc.items[i];
        if !matches!(it.kind, Kind::Text { .. }) {
            ui.horizontal(|ui| {
                ui.label("位置");
                ui.add(egui::DragValue::new(&mut it.x).range(-fw..=fw).speed(1.0));
                ui.add(egui::DragValue::new(&mut it.y).range(-fh..=fh).speed(1.0));
                ui.label("大小");
                let aspect = it.h / it.w.max(1.0);
                let keep = it.kind.keeps_aspect();
                if ui.add(egui::DragValue::new(&mut it.w).range(4.0..=fw * 4.0).speed(1.0)).changed() && keep {
                    it.h = (it.w * aspect).round();
                }
                if ui.add_enabled(!keep, egui::DragValue::new(&mut it.h).range(4.0..=fh * 4.0).speed(1.0)).changed() {}
            });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("上移一層").clicked() {
                layer = 1;
            }
            if ui.button("下移一層").clicked() {
                layer = -1;
            }
            if ui.button("複製").on_hover_text("Ctrl+D").clicked() {
                dup = true;
            }
            if ui.button(RichText::new("刪除").color(Color32::from_rgb(220, 70, 60))).clicked() {
                delete = true;
            }
        });

        let len = self.doc.items.len();
        if delete {
            self.doc.items.remove(i);
            self.selected = None;
        } else if dup {
            let it = self.doc.items[i].clone();
            self.duplicate(it);
        } else if layer == 1 && i + 1 < len {
            self.doc.items.swap(i, i + 1);
            self.selected = Some(i + 1);
        } else if layer == -1 && i > 0 {
            self.doc.items.swap(i, i - 1);
            self.selected = Some(i - 1);
        }
    }

    /// 畫面：裁切與輸出尺寸
    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("畫面").strong());
        let (w, h) = (self.w, self.h);
        let full = self.doc.crop == [0, 0, w, h];
        ui.horizontal(|ui| {
            ui.label("裁切");
            if ui.selectable_label(self.tool == Tool::Crop, "用滑鼠框選").clicked() {
                self.tool = Tool::Crop;
            }
            if ui.add_enabled(!full, egui::Button::new("不裁切")).clicked() {
                self.doc.crop = [0, 0, w, h];
            }
        });
        let c = &mut self.doc.crop;
        ui.horizontal(|ui| {
            ui.label("x");
            ui.add(egui::DragValue::new(&mut c[0]).range(0..=w - 16));
            ui.label("y");
            ui.add(egui::DragValue::new(&mut c[1]).range(0..=h - 16));
            ui.label("寬");
            let (max_w, max_h) = (w - c[0], h - c[1]);
            ui.add(egui::DragValue::new(&mut c[2]).range(16..=max_w));
            ui.label("高");
            ui.add(egui::DragValue::new(&mut c[3]).range(16..=max_h));
        });
        c[2] = c[2].min(w - c[0]).max(16);
        c[3] = c[3].min(h - c[1]).max(16);

        let cw = self.doc.crop[2];
        ui.horizontal(|ui| {
            ui.label("輸出寬度");
            let label = if self.doc.out_w == 0 || self.doc.out_w >= cw { "原尺寸".to_string() } else { format!("{} px", self.doc.out_w) };
            egui::ComboBox::from_id_salt("outw").selected_text(label).width(90.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.doc.out_w, 0, "原尺寸");
                for v in [1280u32, 960, 800, 640, 480, 320] {
                    if v < cw {
                        ui.selectable_value(&mut self.doc.out_w, v, format!("{v} px"));
                    }
                }
            });
            let mut pct = if self.doc.out_w == 0 { 100 } else { (self.doc.out_w * 100 / cw.max(1)).min(100) };
            if ui.add(egui::DragValue::new(&mut pct).range(10..=100).suffix(" %")).changed() {
                self.doc.out_w = if pct >= 100 { 0 } else { cw * pct / 100 };
            }
        });
        let (ow, oh) = self.doc.out_size();
        ui.label(RichText::new(format!("輸出 {ow} x {oh}")).small().weak());
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
            self.export_dialog();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Option<Editor> {
        let src = Path::new("/home/claude/sample.gif");
        src.exists().then(|| {
            let font = render::load_font_bytes().and_then(|b| render::parse_font(&b)).map(Arc::new);
            Editor::open(src, font).unwrap()
        })
    }

    #[test]
    fn undo_redo_groups_changes() {
        let Some(mut ed) = sample() else { return };
        let it = ed.new_item(Kind::Mosaic { block: 10 }, 10.0, 10.0, 50.0, 30.0);
        ed.push_item(it);
        ed.commit_now();
        ed.doc.items[0].x = 40.0;
        ed.doc.items[0].x = 80.0; // 同一次拖曳中的多次變動
        ed.commit_now();
        assert_eq!(ed.undo.len(), 2);
        ed.undo();
        assert_eq!(ed.doc.items[0].x, 10.0);
        ed.undo();
        assert!(ed.doc.items.is_empty());
        ed.redo();
        ed.redo();
        assert_eq!(ed.doc.items[0].x, 80.0);
        // 新的變動會清掉重做
        ed.undo();
        ed.doc.crop = [0, 0, 100, 100];
        ed.commit_now();
        assert!(ed.redo.is_empty());
    }

    #[test]
    fn copy_paste_offsets() {
        let Some(mut ed) = sample() else { return };
        let it = ed.new_item(Kind::Blur { radius: 5 }, 10.0, 10.0, 50.0, 30.0);
        ed.push_item(it);
        ed.copy_selected();
        ed.paste();
        ed.paste();
        assert_eq!(ed.doc.items.len(), 3);
        assert_eq!(ed.doc.items[1].x, 22.0);
        assert_eq!(ed.doc.items[2].x, 34.0);
    }

    /// 範例 GIF：馬賽克、模糊、文字、圖片、拉近、裁切、縮小、頭尾裁剪，全部匯出
    #[test]
    fn export_everything() {
        let Some(mut ed) = sample() else { return };
        let m = ed.new_item(Kind::Mosaic { block: 10 }, 150.0, 40.0, 200.0, 60.0);
        ed.push_item(m);
        let b = ed.new_item(Kind::Blur { radius: 6 }, 380.0, 40.0, 200.0, 60.0);
        ed.push_item(b);
        let mut t = ed.new_item(ed.default_text(), 40.0, 300.0, 0.0, 0.0);
        t.start = 5;
        ed.push_item(t);
        ed.add_image_from(Path::new("/home/claude/logo.png"));
        ed.cur = 10;
        let z = ed.new_item(Kind::Zoom, 300.0, 150.0, 160.0, 90.0);
        ed.push_item(z);
        ed.doc.crop = [20, 20, 600, 320];
        ed.doc.out_w = 480;
        ed.doc.trim = (2, 27);
        let out = PathBuf::from("/tmp/edited-all.gif");
        ed.start_export(out.clone());
        while ed.export.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            ed.poll_export();
        }
        let (msg, err) = ed.message.clone().unwrap();
        assert!(!err, "{msg}");
        let back = render::load_gif(&out).unwrap();
        assert_eq!(back.len(), 26);
        assert_eq!(back[0].rgba.dimensions(), (480, 256));
        println!("{msg}");

        // 專案存檔再開啟，內容一樣
        let p = PathBuf::from("/tmp/sample-test.gifrec");
        project::save(&p, &ed.path, &ed.doc, ed.n()).unwrap();
        let ed2 = Editor::open(&p, None).unwrap();
        assert_eq!(ed2.doc.items.len(), ed.doc.items.len());
        assert_eq!(ed2.doc.crop, ed.doc.crop);
        assert_eq!(ed2.doc.out_w, 480);
    }
}
