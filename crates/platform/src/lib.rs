//! Platform layer: SDL3 window and events, user directories, frame pacing.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

pub use sdl3;

/// Window creation options.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
}

/// SDL3 context, the main window and its event pump.
pub struct Platform {
    pub sdl: sdl3::Sdl,
    pub video: sdl3::VideoSubsystem,
    pub window: sdl3::video::Window,
    pub events: sdl3::EventPump,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform").field("window_id", &self.window.id()).finish_non_exhaustive()
    }
}

impl Platform {
    pub fn new(config: &WindowConfig) -> Result<Self> {
        let sdl = sdl3::init().map_err(|e| anyhow!("SDL init failed: {e}"))?;
        let video = sdl.video().map_err(|e| anyhow!("SDL video init failed: {e}"))?;
        let mut builder = video.window(&config.title, config.width, config.height);
        builder.vulkan().resizable().high_pixel_density().position_centered();
        let mut window = builder.build().context("creating the window")?;
        if config.fullscreen {
            window.set_fullscreen(true).map_err(|e| anyhow!("fullscreen: {e}"))?;
        }
        let events = sdl.event_pump().map_err(|e| anyhow!("SDL event pump: {e}"))?;
        video.text_input().start(&window);
        Ok(Self { sdl, video, window, events })
    }

    /// Vulkan instance extensions SDL needs for presenting to this window.
    pub fn vulkan_instance_extensions(&self) -> Result<Vec<String>> {
        self.window.vulkan_instance_extensions().map_err(|e| anyhow!("SDL Vulkan extensions: {e}"))
    }

    /// Drawable size in physical pixels.
    pub fn drawable_size(&self) -> (u32, u32) {
        self.window.size_in_pixels()
    }

    /// Ratio of physical pixels to logical units (HiDPI).
    pub fn display_scale(&self) -> f32 {
        self.window.display_scale()
    }

    pub fn set_fullscreen(&mut self, on: bool) -> Result<()> {
        self.window.set_fullscreen(on).map_err(|e| anyhow!("fullscreen: {e}"))
    }
}

/// Folder for settings and saves. With `portable`, a folder next to the executable.
/// The user's documents folder (where Windows games keep their saves).
pub fn documents_dir() -> Option<PathBuf> {
    dirs::document_dir()
}

pub fn user_dir(portable: bool) -> PathBuf {
    if portable {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from));
        return exe_dir.unwrap_or_else(|| PathBuf::from(".")).join("aerugo-portable");
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("aerugo")
}

/// Caps the frame rate by sleeping until the next frame deadline. Sleeps coarsely, then spins the last
/// stretch for accuracy. A limit of `None` disables it.
#[derive(Debug, Clone)]
pub struct FrameLimiter {
    limit_hz: Option<u32>,
    next: Option<Instant>,
}

const SPIN_MARGIN: Duration = Duration::from_micros(1500);

impl FrameLimiter {
    pub fn new(limit_hz: Option<u32>) -> Self {
        Self { limit_hz: limit_hz.filter(|&hz| hz > 0), next: None }
    }

    pub fn set_limit(&mut self, limit_hz: Option<u32>) {
        let limit_hz = limit_hz.filter(|&hz| hz > 0);
        if limit_hz != self.limit_hz {
            self.limit_hz = limit_hz;
            self.next = None;
        }
    }

    pub fn limit(&self) -> Option<u32> {
        self.limit_hz
    }

    /// Call once per frame, before starting the frame's work.
    pub fn wait(&mut self) {
        let Some(hz) = self.limit_hz else { return };
        let period = Duration::from_nanos(1_000_000_000 / hz as u64);
        let now = Instant::now();
        let target = match self.next {
            // If we fell more than a frame behind, restart the schedule instead of bursting.
            Some(t) if t + period > now => t,
            _ => now,
        };
        if target > now + SPIN_MARGIN {
            std::thread::sleep(target - now - SPIN_MARGIN);
        }
        while Instant::now() < target {
            std::hint::spin_loop();
        }
        self.next = Some(target + period);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_holds_the_rate() {
        let mut l = FrameLimiter::new(Some(200));
        let start = Instant::now();
        for _ in 0..41 {
            l.wait();
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        assert!((195.0..260.0).contains(&ms), "40 frames at 200 Hz took {ms:.1} ms");
    }

    #[test]
    fn unlimited_does_not_wait() {
        let mut l = FrameLimiter::new(None);
        let start = Instant::now();
        for _ in 0..1000 {
            l.wait();
        }
        assert!(start.elapsed() < Duration::from_millis(50));
        assert_eq!(FrameLimiter::new(Some(0)).limit(), None);
    }
}
