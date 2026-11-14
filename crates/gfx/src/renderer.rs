//! Windowed renderer: swapchain, frames in flight, the quad pipeline and egui drawing.

use anyhow::{Context as _, Result};
use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use crate::context::{Context, ContextOptions};
use crate::quad::{Quad, QuadPipeline};
use crate::swapchain::{PresentMode, Swapchain, color_range};

const FRAMES_IN_FLIGHT: usize = 2;

/// egui output for one frame.
#[derive(Debug)]
pub struct EguiFrame<'a> {
    pub textures: &'a egui::TexturesDelta,
    pub primitives: &'a [egui::ClippedPrimitive],
    pub pixels_per_point: f32,
}

/// What to draw in one frame.
#[derive(Debug)]
pub struct FrameInput<'a> {
    pub clear_color: [f32; 4],
    pub quads: &'a [Quad],
    pub egui: Option<EguiFrame<'a>>,
}

struct FrameSlot {
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    image_available: vk::Semaphore,
    in_flight: vk::Fence,
    /// egui textures to free once this slot's previous submission has finished.
    pending_frees: Vec<egui::TextureId>,
}

/// Owns everything needed to draw to the window. Drop order matters: the egui renderer and the pipelines
/// are destroyed before the context (see `Drop`).
pub struct Renderer {
    swapchain: Option<Swapchain>,
    frames: Vec<FrameSlot>,
    frame_index: usize,
    upload_pool: vk::CommandPool,
    quad: QuadPipeline,
    egui: Option<egui_ash_renderer::Renderer<egui_ash_renderer::allocator::GpuAllocator>>,
    wanted_size: (u32, u32),
    wanted_mode: PresentMode,
    needs_recreate: bool,
    ctx: Context,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("ctx", &self.ctx)
            .field("swapchain", &self.swapchain)
            .finish_non_exhaustive()
    }
}

impl Renderer {
    pub fn new(
        window: &(impl HasWindowHandle + HasDisplayHandle),
        instance_extensions: &[String],
        size: (u32, u32),
        present_mode: PresentMode,
        options: &ContextOptions,
    ) -> Result<Self> {
        let ctx = Context::new_windowed(window, instance_extensions, options)?;
        let swapchain = Swapchain::new(&ctx, size, present_mode, None)?;
        let device = &ctx.device;

        let mut frames = Vec::with_capacity(FRAMES_IN_FLIGHT);
        for _ in 0..FRAMES_IN_FLIGHT {
            let pool_info = vk::CommandPoolCreateInfo::default()
                .queue_family_index(ctx.queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
            // SAFETY: valid device and create infos for all objects below.
            let (pool, cmd, image_available, in_flight) = unsafe {
                let pool = device.create_command_pool(&pool_info, None)?;
                let alloc = vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1);
                let cmd = device.allocate_command_buffers(&alloc)?[0];
                let sem = device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
                let fence = device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )?;
                (pool, cmd, sem, fence)
            };
            frames.push(FrameSlot { pool, cmd, image_available, in_flight, pending_frees: Vec::new() });
        }
        let upload_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(ctx.queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT | vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: valid device.
        let upload_pool = unsafe { device.create_command_pool(&upload_info, None) }?;

        let quad = QuadPipeline::new(device, swapchain.format)?;
        let egui = egui_ash_renderer::Renderer::with_gpu_allocator(
            ctx.allocator().clone(),
            device.clone(),
            egui_ash_renderer::RenderMode::DynamicRendering(egui_ash_renderer::DynamicRendering {
                color_attachment_format: swapchain.format,
                depth_attachment_format: None,
                stencil_attachment_format: None,
            }),
            egui_ash_renderer::Options {
                in_flight_frames: FRAMES_IN_FLIGHT,
                enable_depth_test: false,
                enable_depth_write: false,
                srgb_framebuffer: false,
            },
        )
        .context("creating the egui renderer")?;

        Ok(Self {
            swapchain: Some(swapchain),
            frames,
            frame_index: 0,
            upload_pool,
            quad,
            egui: Some(egui),
            wanted_size: size,
            wanted_mode: present_mode,
            needs_recreate: false,
            ctx,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.ctx.device_name
    }

    pub fn swapchain(&self) -> &Swapchain {
        self.swapchain.as_ref().expect("swapchain exists between frames")
    }

    pub fn present_mode(&self) -> PresentMode {
        self.swapchain().present_mode
    }

    pub fn available_present_modes(&self) -> &[PresentMode] {
        &self.swapchain().available_modes
    }

    /// Window drawable size changed.
    pub fn resize(&mut self, size: (u32, u32)) {
        if size != self.wanted_size {
            self.wanted_size = size;
            self.needs_recreate = true;
        }
    }

    pub fn set_present_mode(&mut self, mode: PresentMode) {
        if mode != self.wanted_mode {
            self.wanted_mode = mode;
            self.needs_recreate = true;
        }
    }

    fn recreate_swapchain(&mut self) -> Result<()> {
        self.ctx.wait_idle();
        let old = self.swapchain.take();
        let new = Swapchain::new(&self.ctx, self.wanted_size, self.wanted_mode, old.as_ref());
        if let Some(old) = old {
            // SAFETY: the device is idle.
            unsafe { old.destroy(&self.ctx.device) };
        }
        self.swapchain = Some(new?);
        self.needs_recreate = false;
        Ok(())
    }

    /// Draw one frame. Returns `false` when nothing was presented (minimized window, swapchain rebuilt).
    /// egui texture changes are always applied, even when the frame is skipped.
    pub fn render(&mut self, input: &FrameInput<'_>) -> Result<bool> {
        let minimized = self.wanted_size.0 == 0 || self.wanted_size.1 == 0;
        if self.needs_recreate && !minimized {
            self.recreate_swapchain()?;
        }
        let device = self.ctx.device.clone();
        let slot = &mut self.frames[self.frame_index];
        // SAFETY: the fence belongs to this device; waiting has no other preconditions.
        unsafe { device.wait_for_fences(&[slot.in_flight], true, u64::MAX) }?;

        // The previous submission of this slot is finished, so textures it used can be freed now.
        let egui = self.egui.as_mut().expect("egui renderer lives until drop");
        for id in slot.pending_frees.drain(..) {
            egui.free_texture(id)?;
        }
        if let Some(frame) = &input.egui {
            for (id, deltas) in &frame.textures.set {
                for delta in deltas {
                    egui.set_texture(self.ctx.queue, self.upload_pool, *id, delta)?;
                }
            }
            slot.pending_frees.extend(frame.textures.free.iter().copied());
        }
        if minimized {
            return Ok(false);
        }

        let swapchain = self.swapchain.as_ref().expect("swapchain exists");
        // SAFETY: valid swapchain and an unsignalled semaphore owned by this slot.
        let acquired = unsafe {
            swapchain.loader.acquire_next_image(
                swapchain.handle,
                u64::MAX,
                slot.image_available,
                vk::Fence::null(),
            )
        };
        let image_index = match acquired {
            Ok((index, suboptimal)) => {
                if suboptimal {
                    self.needs_recreate = true;
                }
                index
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.needs_recreate = true;
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        };

        let image = swapchain.images[image_index as usize];
        let view = swapchain.views[image_index as usize];
        let extent = swapchain.extent;
        let cmd = slot.cmd;
        // SAFETY: the slot's previous work is complete (fence waited), so its command buffer can be reset and
        // recorded; all handles belong to this device.
        unsafe {
            device.reset_fences(&[slot.in_flight])?;
            device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            image_barrier(
                &device,
                cmd,
                image,
                (
                    vk::ImageLayout::UNDEFINED,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::NONE,
                ),
                (
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                ),
            );
            let color = [vk::RenderingAttachmentInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: input.clear_color } })];
            let rendering = vk::RenderingInfo::default()
                .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent })
                .layer_count(1)
                .color_attachments(&color);
            device.cmd_begin_rendering(cmd, &rendering);
            self.quad.draw(&device, cmd, extent, input.quads);
            if let Some(frame) = &input.egui {
                egui.cmd_draw(cmd, extent, frame.pixels_per_point, frame.primitives)?;
            }
            device.cmd_end_rendering(cmd);
            image_barrier(
                &device,
                cmd,
                image,
                (
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                ),
                (vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE),
            );
            device.end_command_buffer(cmd)?;

            let wait = [vk::SemaphoreSubmitInfo::default()
                .semaphore(slot.image_available)
                .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
            let render_finished = swapchain.render_finished[image_index as usize];
            let signal = [vk::SemaphoreSubmitInfo::default()
                .semaphore(render_finished)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = vk::SubmitInfo2::default()
                .wait_semaphore_infos(&wait)
                .command_buffer_infos(&cmds)
                .signal_semaphore_infos(&signal);
            device.queue_submit2(self.ctx.queue, &[submit], slot.in_flight)?;

            let wait_present = [render_finished];
            let swapchains = [swapchain.handle];
            let indices = [image_index];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&wait_present)
                .swapchains(&swapchains)
                .image_indices(&indices);
            match swapchain.loader.queue_present(self.ctx.queue, &present) {
                Ok(false) => {}
                Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.needs_recreate = true,
                Err(e) => return Err(e.into()),
            }
        }
        self.frame_index = (self.frame_index + 1) % FRAMES_IN_FLIGHT;
        Ok(true)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.ctx.wait_idle();
        // egui's renderer frees its GPU memory through the shared allocator; drop it before the context.
        self.egui = None;
        let device = &self.ctx.device;
        // SAFETY: the device is idle; every object below was created from it and is destroyed once.
        unsafe {
            self.quad.destroy(device);
            for f in &self.frames {
                device.destroy_fence(f.in_flight, None);
                device.destroy_semaphore(f.image_available, None);
                device.destroy_command_pool(f.pool, None);
            }
            device.destroy_command_pool(self.upload_pool, None);
            if let Some(s) = self.swapchain.take() {
                s.destroy(device);
            }
        }
        // `ctx` drops last (field order), destroying the device, surface and instance.
    }
}

/// Record a single-image layout transition (synchronization2).
///
/// # Safety
/// `cmd` must be recording and `image` a color image owned by the device.
pub(crate) unsafe fn image_barrier(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    from: (vk::ImageLayout, vk::PipelineStageFlags2, vk::AccessFlags2),
    to: (vk::ImageLayout, vk::PipelineStageFlags2, vk::AccessFlags2),
) {
    let barrier = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(from.1)
        .src_access_mask(from.2)
        .old_layout(from.0)
        .dst_stage_mask(to.1)
        .dst_access_mask(to.2)
        .new_layout(to.0)
        .image(image)
        .subresource_range(color_range())];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barrier);
    // SAFETY: guaranteed by the caller.
    unsafe { device.cmd_pipeline_barrier2(cmd, &dep) };
}
