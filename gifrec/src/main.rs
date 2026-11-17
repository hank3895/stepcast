//! gifrec — 輕量螢幕錄影轉 GIF
//!
//!   gifrec                              全螢幕（主螢幕）
//!   gifrec --monitor 1                  全螢幕（第 2 個螢幕）
//!   gifrec --region 100,200,800,600     指定區域 x,y,寬,高（桌面座標）
//!   gifrec --select                     用滑鼠拖曳框選區域（單擊 = 整個螢幕）
//!   gifrec --wait                       先待命，按熱鍵才開始
//!   gifrec --list                       列出所有螢幕
//!
//! 全域熱鍵（預設 Ctrl+Shift+F9）：待命時按 = 開始；錄影中按 = 停止。
//! 主控台裡按 Enter 效果相同；Ctrl+C 在錄影中 = 停止存檔，其他時候 = 取消。

mod overlay;
mod recorder;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use recorder::{Options, Summary, Target};
use std::{
    io::{self, BufRead, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::WindowId,
};
use xcap::Monitor;

#[derive(Parser)]
#[command(version, about = "輕量螢幕錄影，輸出 GIF")]
struct Args {
    /// 全螢幕錄影（預設就是全螢幕，可省略）
    #[arg(long, conflicts_with_all = ["region", "select"])]
    full: bool,

    /// 區域錄影：x,y,寬,高（桌面座標，實際像素）
    #[arg(long, value_parser = parse_region, conflicts_with = "select")]
    region: Option<(i32, i32, u32, u32)>,

    /// 用滑鼠拖曳框選錄影區域
    #[arg(long, short)]
    select: bool,

    /// 螢幕編號（見 --list），預設為主螢幕。用於全螢幕與 --select
    #[arg(long)]
    monitor: Option<usize>,

    /// 列出螢幕後結束
    #[arg(long)]
    list: bool,

    /// 不倒數，待命直到按下熱鍵（或 Enter）才開始
    #[arg(long, short)]
    wait: bool,

    /// 全域熱鍵，例如 ctrl+shift+F9、alt+F10
    #[arg(long, default_value = "ctrl+shift+F9")]
    hotkey: String,

    /// 不註冊全域熱鍵
    #[arg(long)]
    no_hotkey: bool,

    /// 每秒張數
    #[arg(long, default_value_t = 10)]
    fps: u32,

    /// 輸出寬度上限（等比縮小），0 = 不縮
    #[arg(long, default_value_t = 960)]
    width: u32,

    /// 畫質 1–100
    #[arg(long, default_value_t = 80)]
    quality: u8,

    /// 最長錄影秒數，0 = 不限
    #[arg(long, default_value_t = 0)]
    max_secs: u64,

    /// 開始前倒數秒數（--wait 時不使用）
    #[arg(long, default_value_t = 3)]
    delay: u64,

    /// 輸出檔名
    #[arg(short, long, default_value = "out.gif")]
    out: String,
}

fn parse_region(s: &str) -> Result<(i32, i32, u32, u32), String> {
    let v: Vec<&str> = s.split(',').map(str::trim).collect();
    if v.len() != 4 {
        return Err("格式應為 x,y,寬,高，例如 100,200,800,600".into());
    }
    let p = |i: usize| v[i].parse::<i64>().map_err(|_| format!("無法解析數字：{}", v[i]));
    let (x, y, w, h) = (p(0)?, p(1)?, p(2)?, p(3)?);
    if w <= 0 || h <= 0 {
        return Err("寬、高必須大於 0".into());
    }
    Ok((x as i32, y as i32, w as u32, h as u32))
}

fn pick_monitor(index: Option<usize>) -> Result<Monitor> {
    let monitors = Monitor::all().context("無法取得螢幕清單")?;
    if monitors.is_empty() {
        bail!("找不到任何螢幕");
    }
    let idx = match index {
        Some(i) if i < monitors.len() => i,
        Some(i) => bail!("沒有編號 {i} 的螢幕，請用 --list 查看"),
        None => monitors.iter().position(|m| m.is_primary().unwrap_or(false)).unwrap_or(0),
    };
    Ok(monitors.into_iter().nth(idx).unwrap())
}

fn region_target(gx: i32, gy: i32, w: u32, h: u32) -> Result<Target> {
    let mon = Monitor::from_point(gx, gy).context("區域起點不在任何螢幕上")?;
    let (mx, my) = (mon.x()?, mon.y()?);
    let (mw, mh) = (mon.width()?, mon.height()?);
    let (x, y) = ((gx - mx) as u32, (gy - my) as u32);
    if x + w > mw || y + h > mh {
        bail!("區域超出螢幕範圍（該螢幕 {mw}x{mh}，起點 {mx},{my}）；暫不支援跨螢幕區域");
    }
    Ok(Target::Region { mx, my, x, y, w, h })
}

fn list_monitors() -> Result<()> {
    for (i, m) in Monitor::all()?.iter().enumerate() {
        println!(
            "[{i}] {} {}x{} @ ({},{}){}",
            m.name().unwrap_or_default(),
            m.width()?,
            m.height()?,
            m.x()?,
            m.y()?,
            if m.is_primary().unwrap_or(false) { "  ← 主螢幕" } else { "" }
        );
    }
    Ok(())
}

// ───────────────────────── 事件迴圈 ─────────────────────────

enum Ev {
    /// 熱鍵或 Enter：待命→開始、倒數→立刻開始、錄影→停止
    Toggle,
    /// Ctrl+C：錄影中→停止存檔，其他→取消
    Interrupt,
    /// 錄影執行緒結束
    Done(Result<Summary>),
}

enum Phase {
    Init,
    Selecting(overlay::Overlay),
    Waiting(Target),
    Countdown { target: Target, until: Instant },
    Recording(Arc<AtomicBool>),
    Finished,
}

struct App {
    args: Args,
    hotkey_label: Option<String>,
    proxy: EventLoopProxy<Ev>,
    phase: Phase,
    exit_code: i32,
}

impl App {
    fn fail(&mut self, el: &ActiveEventLoop, e: anyhow::Error) {
        eprintln!("\n錯誤：{e:#}");
        self.exit_code = 1;
        self.phase = Phase::Finished;
        el.exit();
    }

    fn cancel(&mut self, el: &ActiveEventLoop) {
        println!("\n已取消");
        self.exit_code = 130;
        self.phase = Phase::Finished;
        el.exit();
    }

    /// 目標決定後：待命或倒數
    fn arm(&mut self, target: Target, min_delay: Duration) {
        println!("模式：{}", target.describe());
        if self.args.wait {
            match &self.hotkey_label {
                Some(k) => println!("待命中：按 {k}（或在此按 Enter）開始錄影"),
                None => println!("待命中：在此按 Enter 開始錄影"),
            }
            self.phase = Phase::Waiting(target);
        } else {
            let d = Duration::from_secs(self.args.delay).max(min_delay);
            self.phase = Phase::Countdown { target, until: Instant::now() + d };
        }
    }

    fn start_recording(&mut self, target: Target) {
        let stop = Arc::new(AtomicBool::new(false));
        let opt = Options {
            fps: self.args.fps,
            width: self.args.width,
            quality: self.args.quality,
            max_secs: self.args.max_secs,
            out: self.args.out.clone(),
        };
        let proxy = self.proxy.clone();
        let s = stop.clone();
        thread::spawn(move || {
            let r = recorder::record(target, &opt, s);
            let _ = proxy.send_event(Ev::Done(r));
        });
        let how = match &self.hotkey_label {
            Some(k) => format!("按 {k} 或 Enter 停止"),
            None => "按 Enter 或 Ctrl+C 停止".into(),
        };
        println!("\r● 開始錄影（{how}）          ");
        self.phase = Phase::Recording(stop);
    }
}

impl ApplicationHandler<Ev> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if !matches!(self.phase, Phase::Init) {
            return;
        }
        let r: Result<()> = (|| {
            if self.args.select {
                let mon = pick_monitor(self.args.monitor)?;
                println!("拖曳滑鼠框選區域（單擊 = 整個螢幕，Esc 取消）");
                self.phase = Phase::Selecting(overlay::Overlay::open(el, &mon)?);
            } else if let Some((x, y, w, h)) = self.args.region {
                self.arm(region_target(x, y, w, h)?, Duration::ZERO);
            } else {
                let m = pick_monitor(self.args.monitor)?;
                self.arm(Target::Full { mx: m.x()?, my: m.y()? }, Duration::ZERO);
            }
            Ok(())
        })();
        if let Err(e) = r {
            self.fail(el, e);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        let Phase::Selecting(ov) = &mut self.phase else { return };
        if ov.id() != id {
            return;
        }
        match ov.handle(ev) {
            overlay::Outcome::Pending => {}
            overlay::Outcome::Cancelled => self.cancel(el),
            // 至少等 0.3 秒，確保覆蓋視窗已從畫面消失才開始截圖
            overlay::Outcome::Selected(t) => self.arm(t, Duration::from_millis(300)),
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: Ev) {
        match ev {
            Ev::Toggle => match std::mem::replace(&mut self.phase, Phase::Init) {
                Phase::Waiting(t) | Phase::Countdown { target: t, .. } => self.start_recording(t),
                Phase::Recording(stop) => {
                    stop.store(true, Ordering::SeqCst);
                    self.phase = Phase::Recording(stop);
                }
                other => self.phase = other,
            },
            Ev::Interrupt => match &self.phase {
                Phase::Recording(stop) => stop.store(true, Ordering::SeqCst),
                Phase::Finished => {}
                _ => self.cancel(el),
            },
            Ev::Done(r) => {
                match r {
                    Ok(s) => println!(
                        "完成：{}（{:.1} 秒，{} 張，{:.2} MB）",
                        self.args.out,
                        s.secs,
                        s.frames,
                        s.bytes as f64 / 1_048_576.0
                    ),
                    Err(e) => {
                        eprintln!("\n錯誤：{e:#}");
                        self.exit_code = 1;
                    }
                }
                self.phase = Phase::Finished;
                el.exit();
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if let Phase::Countdown { target, until } = self.phase {
            let now = Instant::now();
            if now >= until {
                self.start_recording(target);
                el.set_control_flow(ControlFlow::Wait);
            } else {
                let left = until - now;
                print!("\r{} 秒後開始…   ", left.as_secs_f64().ceil() as u64);
                let _ = io::stdout().flush();
                // 每 0.25 秒醒來一次更新倒數
                el.set_control_flow(ControlFlow::WaitUntil(now + left.min(Duration::from_millis(250))));
            }
        } else {
            el.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn main() {
    let code = match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("錯誤：{e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let args = Args::parse();
    if args.list {
        list_monitors()?;
        return Ok(0);
    }
    if args.fps == 0 || args.fps > 50 {
        bail!("fps 請設在 1–50 之間");
    }

    let event_loop = EventLoop::<Ev>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    // 全域熱鍵：必須在跑事件迴圈的主執行緒上註冊（Windows 要求）
    let mut _hk_manager = None;
    let mut hotkey_label = None;
    if !args.no_hotkey {
        let hk: HotKey = args
            .hotkey
            .parse()
            .map_err(|e| anyhow!("熱鍵格式錯誤「{}」：{e}", args.hotkey))?;
        match GlobalHotKeyManager::new().and_then(|m| m.register(hk).map(|_| m)) {
            Ok(m) => {
                let p = proxy.clone();
                let id = hk.id();
                GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                    if e.id == id && e.state == HotKeyState::Pressed {
                        let _ = p.send_event(Ev::Toggle);
                    }
                }));
                _hk_manager = Some(m);
                hotkey_label = Some(args.hotkey.clone());
            }
            Err(e) => eprintln!("（熱鍵 {} 註冊失敗：{e}，改用 Enter / Ctrl+C）", args.hotkey),
        }
    }
    if args.wait && hotkey_label.is_none() && args.select {
        // 沒熱鍵又要待命：只剩主控台 Enter 可用，提醒一下
        eprintln!("（提示：沒有熱鍵時，框選後請切回這個視窗按 Enter 開始）");
    }

    {
        let p = proxy.clone();
        ctrlc::set_handler(move || {
            let _ = p.send_event(Ev::Interrupt);
        })?;
        let p = proxy.clone();
        thread::spawn(move || {
            // 每讀到一行 = Toggle；沒有主控台（EOF）就結束這個執行緒
            for line in io::stdin().lock().lines() {
                if line.is_err() || p.send_event(Ev::Toggle).is_err() {
                    break;
                }
            }
        });
    }

    let mut app = App { args, hotkey_label, proxy, phase: Phase::Init, exit_code: 0 };
    event_loop.run_app(&mut app)?;
    Ok(app.exit_code)
}
