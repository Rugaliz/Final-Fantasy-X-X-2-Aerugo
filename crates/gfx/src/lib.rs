//! Vulkan layer for Aerugo (via `ash`): device setup, swapchain and frame pacing, pipelines, egui drawing.
//!
//! Requires Vulkan 1.3 with dynamic rendering and synchronization2. All `unsafe` Vulkan usage in Aerugo
//! lives in this crate.

pub mod aspect;
pub mod context;
pub mod offscreen;
pub mod quad;
pub mod renderer;
pub mod scene;
pub mod swapchain;
pub mod texture;

pub use context::{Context, ContextOptions, validation_error_count};
pub use quad::{Quad, QuadPipeline};
pub use renderer::{EguiFrame, FrameInput, Renderer, TextureHandle};
pub use scene::{Draw, MeshDesc, MeshHandle, MeshPart, SceneVertex, SceneView};
pub use swapchain::PresentMode;
pub use texture::{TexFormat, Texture, TextureDesc};
