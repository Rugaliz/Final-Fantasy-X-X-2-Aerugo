//! Vulkan layer for Aerugo (via `ash`): device setup, swapchain and frame pacing, pipelines, egui drawing.
//!
//! Requires Vulkan 1.3 with dynamic rendering and synchronization2. All `unsafe` Vulkan usage in Aerugo
//! lives in this crate.

pub mod context;
pub mod offscreen;
pub mod quad;
pub mod renderer;
pub mod swapchain;

pub use context::{Context, ContextOptions};
pub use quad::{Quad, QuadPipeline};
pub use renderer::{EguiFrame, FrameInput, Renderer};
pub use swapchain::PresentMode;
