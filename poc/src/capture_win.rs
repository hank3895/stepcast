//! Screen capture of the primary monitor with Windows Graphics Capture
//! (windows-capture crate, MIT).

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::encode::RgbFrame;
use crate::scale;

#[derive(Clone)]
pub struct Shared {
    pub frames: Arc<Mutex<Vec<RgbFrame>>>,
    pub start: Instant,
    pub interval: Duration,
    pub max_width: u32,
}

struct Recorder {
    shared: Shared,
    last_kept: Option<Instant>,
    scratch: Vec<u8>,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl GraphicsCaptureApiHandler for Recorder {
    type Flags = Shared;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { shared: ctx.flags, last_kept: None, scratch: Vec::new() })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let now = Instant::now();
        if let Some(last) = self.last_kept {
            // Windows only sends a frame when the screen changes; drop extra ones above the target FPS.
            if now.duration_since(last) + Duration::from_millis(2) < self.shared.interval {
                return Ok(());
            }
        }
        self.last_kept = Some(now);

        let mut buffer = frame.buffer()?;
        let (sw, sh) = (buffer.width(), buffer.height());
        let rgba = buffer.as_nopadding_buffer(&mut self.scratch);
        let (dw, dh) = scale::fit(sw, sh, self.shared.max_width);
        let rgb = scale::rgba_to_rgb_scaled(rgba, sw, sh, dw, dh);
        let t_ms = now.duration_since(self.shared.start).as_millis() as u64;

        self.shared
            .frames
            .lock()
            .map_err(|_| "frame list lock poisoned")?
            .push(RgbFrame { width: dw, height: dh, rgb, t_ms });
        Ok(())
    }
}

/// Records the primary monitor for `seconds`. Returns the frames and the recording length in ms.
pub fn record(seconds: u32, fps: u32, max_width: u32) -> Result<(Vec<RgbFrame>, u64), String> {
    let monitor = Monitor::primary().map_err(|e| format!("找不到主螢幕：{e}"))?;
    let shared = Shared {
        frames: Arc::new(Mutex::new(Vec::new())),
        start: Instant::now(),
        interval: Duration::from_millis(1000 / fps.max(1) as u64),
        max_width,
    };

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        shared.clone(),
    );

    let control = Recorder::start_free_threaded(settings).map_err(|e| format!("無法開始錄製：{e}"))?;
    thread::sleep(Duration::from_secs(seconds as u64));
    let end_ms = shared.start.elapsed().as_millis() as u64;
    control.stop().map_err(|e| format!("停止錄製失敗：{e}"))?;

    let frames = std::mem::take(&mut *shared.frames.lock().map_err(|_| "frame list lock poisoned")?);
    Ok((frames, end_ms))
}
