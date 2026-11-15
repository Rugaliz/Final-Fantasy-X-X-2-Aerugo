//! Audio for Aerugo: a float mixer with windowed-sinc resampling and a master limiter, fed to an SDL3
//! playback stream. The mixer is plain Rust and runs on SDL's audio thread; the game talks to it through
//! commands.

pub mod device;
pub mod mixer;
pub mod resample;

pub use device::AudioOutput;
pub use mixer::{Mixer, SharedStream, Sound, Stream, VoiceId, VoiceParams};
