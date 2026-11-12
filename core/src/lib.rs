//! StepCast core: screen capture (Windows) and GIF encoding, shared by the CLI and the app.

pub mod encode;
pub mod scale;
pub mod selftest;
#[cfg(windows)]
pub mod capture_win;
