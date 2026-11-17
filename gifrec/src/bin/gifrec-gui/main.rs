//! gifrec-gui — gifrec 的簡單圖形介面
//!
//! 設定好後按「開始錄影」，GUI 會在背景啟動同資料夾的 gifrec(.exe)
//! 做實際錄影（框選、熱鍵、編碼都由它處理），GUI 只負責設定與顯示狀態。

#![cfg_attr(windows, windows_subsystem = "windows")] // Windows 雙擊時不跳出黑色主控台

mod editor;
mod render;

use eframe::egui::{self, Color32, RichText};
use ab_glyph::FontVec;
use std::{
    sync::Arc,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const WIDTHS: [(&str, u32); 5] = [("原尺寸", 0), ("1280", 1280), ("960", 960), ("720", 720), ("480", 480)];

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Full,
    Select,
}

#[derive(PartialEq, Clone, Copy)]
enum StartBy {
    Countdown,
    Hotkey,
}

#[derive(Clone)]
enum Status {
    Idle,
    Starting,
    Selecting,
    Countdown(String),
    Waiting,
    Recording(String),
    Encoding,
    Done(String),
    Failed(String),
    Cancelled,
}

impl Status {
    fn running(&self) -> bool {
        !matches!(self, Status::Idle | Status::Done(_) | Status::Failed(_) | Status::Cancelled)
    }
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
}

struct Gui {
    monitors: Vec<String>,
    monitor: usize,
    mode: Mode,
    start_by: StartBy,
    delay: u64,
    fps: u32,
    width_idx: usize,
    quality: u8,
    hotkey: String,
    folder: String,
    minimize: bool,

    status: Status,
    warning: Option<String>,
    run: Option<Running>,
    spawn_at: Option<Instant>,
    out_path: Option<PathBuf>,
    hidden: bool,

    editor: Option<editor::Editor>,
    font: Option<Arc<FontVec>>,
    open_error: Option<String>,
}

impl Gui {
    fn new(font: Option<Arc<FontVec>>) -> Self {
        let (monitors, primary) = list_monitors();
        Gui {
            monitors,
            monitor: primary,
            mode: Mode::Full,
            start_by: StartBy::Countdown,
            delay: 3,
            fps: 10,
            width_idx: 2,
            quality: 80,
            hotkey: "ctrl+shift+F9".into(),
            folder: default_folder().display().to_string(),
            minimize: true,
            status: Status::Idle,
            warning: None,
            run: None,
            spawn_at: None,
            out_path: None,
            hidden: false,
            editor: None,
            font,
            open_error: None,
        }
    }

    fn open_editor(&mut self, ctx: &egui::Context, p: &Path) {
        match editor::Editor::open(p, self.font.clone()) {
            Ok(ed) => {
                self.editor = Some(ed);
                self.open_error = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1150.0, 760.0)));
            }
            Err(e) => self.open_error = Some(e),
        }
    }

    fn recorder_exe() -> PathBuf {
        let name = if cfg!(windows) { "gifrec.exe" } else { "gifrec" };
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(name)))
            .unwrap_or_else(|| PathBuf::from(name))
    }

    /// 按下「開始錄影」：先縮小視窗，稍等一下再啟動錄影程式，避免把 GUI 自己錄進去
    fn begin(&mut self, ctx: &egui::Context) {
        let folder = PathBuf::from(self.folder.trim());
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.status = Status::Failed(format!("無法建立資料夾：{e}"));
            return;
        }
        let name = chrono::Local::now().format("gifrec-%Y%m%d-%H%M%S.gif").to_string();
        self.out_path = Some(folder.join(name));
        self.warning = None;
        self.status = Status::Starting;
        if self.minimize {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            self.hidden = true;
            self.spawn_at = Some(Instant::now() + Duration::from_millis(500));
        } else {
            self.spawn_at = Some(Instant::now());
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn spawn(&mut self, ctx: &egui::Context) {
        let out = self.out_path.clone().unwrap();
        let mut cmd = Command::new(Self::recorder_exe());
        cmd.arg("--fps").arg(self.fps.to_string())
            .arg("--width").arg(WIDTHS[self.width_idx].1.to_string())
            .arg("--quality").arg(self.quality.to_string())
            .arg("--monitor").arg(self.monitor.to_string())
            .arg("--hotkey").arg(self.hotkey.trim())
            .arg("--delay").arg(self.delay.to_string())
            .arg("-o").arg(&out);
        if self.mode == Mode::Select {
            cmd.arg("--select");
        }
        if self.start_by == StartBy::Hotkey {
            cmd.arg("--wait");
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }

        match cmd.spawn() {
            Ok(mut child) => {
                let (tx, rx) = mpsc::channel();
                for pipe in [
                    child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
                    child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
                ]
                .into_iter()
                .flatten()
                {
                    let tx = tx.clone();
                    let ctx = ctx.clone();
                    thread::spawn(move || read_lines(pipe, tx, ctx));
                }
                let stdin = child.stdin.take().unwrap();
                self.run = Some(Running { child, stdin, rx });
            }
            Err(e) => {
                self.restore(ctx);
                self.status = Status::Failed(format!(
                    "找不到錄影程式 {}：{e}\n請把 gifrec 和 gifrec-gui 放在同一個資料夾",
                    Self::recorder_exe().display()
                ));
            }
        }
    }

    /// 送一個 Enter 給錄影程式：待命→開始、倒數→立即開始、錄影→停止
    fn toggle(&mut self) {
        if let Some(r) = &mut self.run {
            let _ = r.stdin.write_all(b"\n");
            let _ = r.stdin.flush();
        }
    }

    fn cancel(&mut self) {
        if let Some(r) = &mut self.run {
            let _ = r.child.kill();
        }
    }

    fn restore(&mut self, ctx: &egui::Context) {
        if self.hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.hidden = false;
        }
    }

    fn on_line(&mut self, line: &str) {
        let l = line.trim().trim_start_matches('●').trim();
        self.status = if l.starts_with("拖曳") {
            Status::Selecting
        } else if l.contains("後開始") {
            Status::Countdown(l.to_string())
        } else if l.starts_with("待命中") {
            Status::Waiting
        } else if let Some(rest) = l.strip_prefix("錄影中") {
            Status::Recording(rest.trim().to_string())
        } else if l.starts_with("開始錄影") {
            Status::Recording("0.0s".into())
        } else if l.starts_with("編碼中") {
            Status::Encoding
        } else if let Some(rest) = l.strip_prefix("完成：") {
            // 完整路徑太長，只顯示檔名
            let short = match &self.out_path {
                Some(p) => rest.replace(&p.display().to_string(), &p.file_name().unwrap_or_default().to_string_lossy()),
                None => rest.to_string(),
            };
            Status::Done(short)
        } else if let Some(rest) = l.strip_prefix("錯誤：") {
            Status::Failed(rest.to_string())
        } else if l.starts_with("已取消") {
            Status::Cancelled
        } else {
            if l.starts_with("（熱鍵") {
                self.warning = Some(l.trim_matches(['（', '）']).to_string());
            }
            return;
        };
    }
}

/// 讀取子行程輸出，以 \r 或 \n 斷行（錄影進度用 \r 原地更新）
fn read_lines(mut r: Box<dyn Read + Send>, tx: mpsc::Sender<String>, ctx: egui::Context) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &chunk[..n] {
            if b == b'\n' || b == b'\r' {
                if !buf.is_empty() {
                    let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
                    buf.clear();
                }
            } else {
                buf.push(b);
            }
        }
        ctx.request_repaint();
    }
    if !buf.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    }
    ctx.request_repaint();
}

impl eframe::App for Gui {
    /// 視窗縮小時也會被呼叫：在這裡處理子行程的狀態
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(t) = self.spawn_at {
            if Instant::now() >= t {
                self.spawn_at = None;
                self.spawn(ctx);
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }

        let mut lines = Vec::new();
        let mut exited = None;
        if let Some(r) = &mut self.run {
            while let Ok(l) = r.rx.try_recv() {
                lines.push(l);
            }
            if let Ok(Some(st)) = r.child.try_wait() {
                exited = Some(st);
                // 子行程結束後把剩下的輸出讀完
                thread::sleep(Duration::from_millis(30));
                while let Ok(l) = r.rx.try_recv() {
                    lines.push(l);
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(200));
            }
        }
        for l in lines {
            self.on_line(&l);
        }
        if let Some(st) = exited {
            self.run = None;
            if self.status.running() {
                self.status = if st.success() {
                    Status::Cancelled
                } else {
                    match st.code() {
                        Some(130) | None => Status::Cancelled,
                        Some(c) => Status::Failed(format!("錄影程式異常結束（代碼 {c}）")),
                    }
                };
            }
            self.restore(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if let Some(ed) = &mut self.editor {
            if let editor::Action::Back = ed.ui(ui) {
                self.editor = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(420.0, 440.0)));
            }
            return;
        }

        // 把 GIF 拖進視窗 → 直接開啟編輯
        let dropped: Option<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("gif"))
            })
        });
        if let Some(p) = dropped {
            if !self.status.running() {
                self.open_editor(&ctx, &p);
                return;
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let busy = self.status.running();

            ui.add_enabled_ui(!busy, |ui| {
                egui::Grid::new("settings").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("錄影範圍");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.mode, Mode::Full, "全螢幕");
                        ui.radio_value(&mut self.mode, Mode::Select, "框選區域");
                    });
                    ui.end_row();

                    ui.label("螢幕");
                    let cur = self.monitors.get(self.monitor).cloned().unwrap_or_default();
                    egui::ComboBox::from_id_salt("mon").selected_text(cur).width(220.0).show_ui(ui, |ui| {
                        for (i, m) in self.monitors.iter().enumerate() {
                            ui.selectable_value(&mut self.monitor, i, m);
                        }
                    });
                    ui.end_row();

                    ui.label("開始方式");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.start_by, StartBy::Countdown, "倒數");
                        ui.add_enabled(
                            self.start_by == StartBy::Countdown,
                            egui::DragValue::new(&mut self.delay).range(0..=10).suffix(" 秒"),
                        );
                        ui.radio_value(&mut self.start_by, StartBy::Hotkey, "按熱鍵開始");
                    });
                    ui.end_row();

                    ui.label("熱鍵");
                    ui.add(egui::TextEdit::singleline(&mut self.hotkey).desired_width(140.0))
                        .on_hover_text("錄影中按一下停止。例如 ctrl+shift+F9、alt+F10");
                    ui.end_row();

                    ui.label("每秒張數");
                    ui.add(egui::Slider::new(&mut self.fps, 5..=30));
                    ui.end_row();

                    ui.label("最大寬度");
                    egui::ComboBox::from_id_salt("w").selected_text(WIDTHS[self.width_idx].0).show_ui(ui, |ui| {
                        for (i, (n, _)) in WIDTHS.iter().enumerate() {
                            ui.selectable_value(&mut self.width_idx, i, *n);
                        }
                    });
                    ui.end_row();

                    ui.label("畫質");
                    ui.add(egui::Slider::new(&mut self.quality, 30..=100));
                    ui.end_row();

                    ui.label("儲存位置");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.folder).desired_width(220.0));
                        if ui.button("瀏覽…").clicked() {
                            if let Some(d) = rfd::FileDialog::new().set_directory(self.folder.trim()).pick_folder() {
                                self.folder = d.display().to_string();
                            }
                        }
                    });
                    ui.end_row();
                });
                ui.checkbox(&mut self.minimize, "錄影時自動縮小這個視窗");
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            // 主要按鈕
            ui.horizontal(|ui| {
                let big = |t: &str| egui::Button::new(RichText::new(t).size(18.0)).min_size([150.0, 36.0].into());
                match &self.status {
                    Status::Waiting | Status::Countdown(_) => {
                        if ui.add(big("● 立即開始")).clicked() {
                            self.toggle();
                        }
                        if ui.button("取消").clicked() {
                            self.cancel();
                        }
                    }
                    Status::Recording(_) => {
                        if ui.add(big("■ 停止錄影")).clicked() {
                            self.toggle();
                        }
                    }
                    Status::Starting | Status::Selecting => {
                        if ui.button("取消").clicked() {
                            self.cancel();
                        }
                    }
                    Status::Encoding => {
                        ui.spinner();
                    }
                    _ => {
                        if ui.add(big("● 開始錄影")).clicked() {
                            self.begin(&ctx);
                        }
                        if ui.button("開啟 GIF 編輯…").on_hover_text("也可以直接把 GIF 拖進這個視窗").clicked() {
                            if let Some(p) = rfd::FileDialog::new().add_filter("GIF", &["gif"]).set_directory(self.folder.trim()).pick_file() {
                                self.open_editor(&ctx, &p);
                            }
                        }
                    }
                }
            });

            ui.add_space(6.0);
            let (text, color) = match &self.status {
                Status::Idle => ("準備就緒".to_string(), ui.visuals().weak_text_color()),
                Status::Starting => ("啟動中…".into(), ui.visuals().text_color()),
                Status::Selecting => ("請在畫面上拖曳框選（單擊 = 整個螢幕，Esc 取消）".into(), ui.visuals().text_color()),
                Status::Countdown(s) => (s.clone(), ui.visuals().text_color()),
                Status::Waiting => (format!("待命中，按 {} 開始", self.hotkey), ui.visuals().text_color()),
                Status::Recording(s) => (format!("● 錄影中 {s}　按 {} 停止", self.hotkey), Color32::from_rgb(230, 60, 50)),
                Status::Encoding => ("編碼中…".into(), ui.visuals().text_color()),
                Status::Done(s) => (format!("完成：{s}"), Color32::from_rgb(40, 160, 70)),
                Status::Failed(s) => (format!("失敗：{s}"), Color32::from_rgb(230, 60, 50)),
                Status::Cancelled => ("已取消".into(), ui.visuals().weak_text_color()),
            };
            ui.label(RichText::new(text).color(color));
            if let Some(w) = &self.warning {
                ui.label(RichText::new(w).small().color(Color32::from_rgb(200, 140, 0)));
            }

            if let Some(e) = &self.open_error {
                ui.label(RichText::new(e).color(Color32::from_rgb(230, 60, 50)));
            }
            if let (Status::Done(_), Some(p)) = (&self.status, self.out_path.clone()) {
                let p = &p;
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("編輯這個 GIF").strong()).clicked() {
                        self.open_editor(&ctx, p);
                    }
                    if ui.button("開啟 GIF").clicked() {
                        open_path(p);
                    }
                    if ui.button("開啟資料夾").clicked() {
                        reveal(p);
                    }
                });
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cancel();
    }
}

fn list_monitors() -> (Vec<String>, usize) {
    let mut primary = 0;
    let list = xcap::Monitor::all()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let p = m.is_primary().unwrap_or(false);
            if p {
                primary = i;
            }
            format!(
                "{}：{}x{}{}",
                i + 1,
                m.width().unwrap_or(0),
                m.height().unwrap_or(0),
                if p { "（主螢幕）" } else { "" }
            )
        })
        .collect::<Vec<_>>();
    if list.is_empty() {
        (vec!["（偵測不到螢幕）".into()], 0)
    } else {
        (list, primary)
    }
}

/// 預設存到桌面（含 OneDrive 桌面），找不到就用目前資料夾
fn default_folder() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    if let Some(h) = home {
        for d in [h.join("Desktop"), h.join("OneDrive").join("Desktop"), h.join("Pictures")] {
            if d.is_dir() {
                return d;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn open_path(p: &Path) {
    #[cfg(windows)]
    let _ = Command::new("cmd").args(["/C", "start", ""]).arg(p).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(p).spawn();
}

fn reveal(p: &Path) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(p.parent().unwrap_or(Path::new("."))).spawn();
}

/// egui 內建字型沒有中文，從系統載入一個；同一套字型也給編輯器畫文字用
fn setup_fonts(ctx: &egui::Context) -> Option<Arc<FontVec>> {
    let bytes = render::load_font_bytes()?;
    let font = render::parse_font(&bytes).map(Arc::new);
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(fam).or_default().push("cjk".into());
    }
    ctx.set_fonts(fonts);
    font
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("gifrec 螢幕錄影")
            .with_inner_size([420.0, 440.0])
            .with_min_inner_size([400.0, 380.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "gifrec",
        opts,
        Box::new(|cc| {
            let font = setup_fonts(&cc.egui_ctx);
            let mut gui = Gui::new(font);
            // gifrec-gui 某檔.gif（或把 GIF 拖到 exe 圖示上）→ 直接進編輯器
            if let Some(p) = std::env::args_os().nth(1).map(PathBuf::from) {
                gui.open_editor(&cc.egui_ctx, &p);
            }
            Ok(Box::new(gui))
        }),
    )
}
