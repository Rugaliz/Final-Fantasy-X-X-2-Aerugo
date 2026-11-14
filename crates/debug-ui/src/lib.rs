//! egui on top of SDL3: input translation, platform output (cursor, clipboard) and shared debug widgets.
//! Drawing goes through `aerugo-gfx`; this crate has no Vulkan code.

pub mod fonts;
mod input;
pub mod widgets;

pub use egui;
pub use input::{EguiSdl, PreparedFrame};
