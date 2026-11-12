//! StepCast proof of concept: record the screen with Rust and write a small GIF.
//!
//!   stepcast-poc record   [--seconds 5] [--fps 12] [--width 960] [--out stepcast-test.gif] [--no-dither]
//!   stepcast-poc selftest [--out selftest.gif]

#[cfg(windows)]
use stepcast_core::capture_win;
use stepcast_core::selftest;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use stepcast_core::encode::{encode_gif, EncodeOptions, EncodeStats};

struct Args {
    command: String,
    seconds: u32,
    fps: u32,
    width: u32,
    out: Option<PathBuf>,
    dither: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().unwrap_or_else(|| "help".into());
    let mut a = Args { command, seconds: 5, fps: 12, width: 960, out: None, dither: true };
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} 後面需要一個值"));
        let num = |s: String, name: &str| s.parse::<u32>().map_err(|_| format!("{name} 必須是正整數"));
        match flag.as_str() {
            "--seconds" => a.seconds = num(value("--seconds")?, "--seconds")?,
            "--fps" => a.fps = num(value("--fps")?, "--fps")?,
            "--width" => a.width = num(value("--width")?, "--width")?,
            "--out" => a.out = Some(PathBuf::from(value("--out")?)),
            "--no-dither" => a.dither = false,
            other => return Err(format!("不認得的參數：{other}")),
        }
    }
    if !(1..=60).contains(&a.fps) {
        return Err("--fps 須介於 1 到 60".into());
    }
    if !(1..=600).contains(&a.seconds) {
        return Err("--seconds 須介於 1 到 600".into());
    }
    Ok(a)
}

fn report(stats: &EncodeStats, path: &PathBuf, capture_info: Option<String>) {
    println!("輸出檔案：{}", path.display());
    if let Some(info) = capture_info {
        println!("{info}");
    }
    println!("輸入影格：{}，實際寫入：{}（其餘因畫面沒變而合併）", stats.input_frames, stats.written_frames);
    println!("編碼時間：{:.2} 秒", stats.encode_secs);
    println!("檔案大小：{:.1} KB", stats.file_bytes as f64 / 1024.0);
}

fn run() -> Result<(), String> {
    let a = parse_args()?;
    match a.command.as_str() {
        "selftest" => {
            let path = a.out.clone().unwrap_or_else(|| "selftest.gif".into());
            let (fps, seconds) = (12, 4);
            let frames = selftest::synthetic_frames(640, 400, fps, seconds);
            let opts = EncodeOptions { dither: a.dither, end_ms: seconds as u64 * 1000 };
            let stats = encode_gif(&frames, &path, &opts)?;
            report(&stats, &path, None);

            // Read the file back to make sure it is a valid GIF.
            let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut opts = gif::DecodeOptions::new();
            opts.set_color_output(gif::ColorOutput::Indexed);
            let mut dec = opts.read_info(file).map_err(|e| format!("GIF 解碼失敗：{e}"))?;
            let (w, h) = (dec.width(), dec.height());
            let mut count = 0;
            while dec.read_next_frame().map_err(|e| format!("GIF 解碼失敗：{e}"))?.is_some() {
                count += 1;
            }
            if (w, h) != (640, 400) || count != stats.written_frames {
                return Err(format!("自我測試失敗：{w}x{h}，{count} 格"));
            }
            if stats.written_frames >= stats.input_frames {
                return Err("自我測試失敗：靜止畫面沒有被合併".into());
            }
            println!("自我測試通過：{w}x{h}，{count} 格");
            Ok(())
        }
        "record" => record(&a),
        _ => {
            println!("StepCast 驗證程式 {}", env!("CARGO_PKG_VERSION"));
            println!("  stepcast-poc record   [--seconds 5] [--fps 12] [--width 960] [--out 檔名.gif] [--no-dither]");
            println!("  stepcast-poc selftest [--out selftest.gif]");
            Ok(())
        }
    }
}

#[cfg(windows)]
fn record(a: &Args) -> Result<(), String> {
    let path = a.out.clone().unwrap_or_else(|| "stepcast-test.gif".into());
    println!("3 秒後開始錄製主螢幕 {} 秒…", a.seconds);
    std::thread::sleep(std::time::Duration::from_secs(3));
    println!("錄製中…");
    let t = Instant::now();
    let (frames, end_ms) = capture_win::record(a.seconds, a.fps, a.width)?;
    let rec_secs = t.elapsed().as_secs_f64();
    let first = frames.first().ok_or("沒有收到任何影格（畫面可能完全沒變，請在錄製時移動滑鼠）")?;
    let info = format!(
        "錄製：{:.1} 秒，{}x{}，收到 {} 格（約 {:.1} FPS）",
        rec_secs,
        first.width,
        first.height,
        frames.len(),
        frames.len() as f64 / rec_secs
    );
    println!("編碼中…");
    let stats = encode_gif(&frames, &path, &EncodeOptions { dither: a.dither, end_ms })?;
    report(&stats, &path, Some(info));
    Ok(())
}

#[cfg(not(windows))]
fn record(_a: &Args) -> Result<(), String> {
    let _ = Instant::now();
    Err("record 只支援 Windows；其他平台請用 selftest".into())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("錯誤：{e}");
            ExitCode::FAILURE
        }
    }
}
