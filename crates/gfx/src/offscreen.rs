//! Offscreen rendering with CPU readback, for tests and (later) screenshots.

use anyhow::{Context as _, Result};
use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};

use crate::context::Context;
use crate::quad::{Quad, QuadPipeline};
use crate::renderer::image_barrier;
use crate::swapchain::color_range;

pub const OFFSCREEN_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// Render `quads` over `clear_color` into a `width`×`height` RGBA8 image and return its pixels (row-major,
/// tightly packed).
pub fn render_quads(
    ctx: &Context,
    width: u32,
    height: u32,
    clear_color: [f32; 4],
    quads: &[Quad],
) -> Result<Vec<u8>> {
    let device = &ctx.device;
    let extent = vk::Extent2D { width, height };
    let byte_len = u64::from(width) * u64::from(height) * 4;
    let mut cleanup = Cleanup { ctx, image: None, view: None, buffer: None, pool: None, fence: None };

    // Color target.
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(OFFSCREEN_FORMAT)
        .extent(vk::Extent3D { width, height, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    // SAFETY: valid device and create info.
    let image = unsafe { device.create_image(&image_info, None) }?;
    // SAFETY: image was just created.
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let alloc = ctx.allocator().lock().expect("allocator lock").allocate(&AllocationCreateDesc {
        name: "offscreen color",
        requirements,
        location: MemoryLocation::GpuOnly,
        linear: false,
        allocation_scheme: AllocationScheme::GpuAllocatorManaged,
    })?;
    // SAFETY: memory satisfies the image's requirements and is bound once.
    unsafe { device.bind_image_memory(image, alloc.memory(), alloc.offset()) }?;
    cleanup.image = Some((image, alloc));

    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(OFFSCREEN_FORMAT)
        .subresource_range(color_range());
    // SAFETY: image is bound to memory.
    let view = unsafe { device.create_image_view(&view_info, None) }?;
    cleanup.view = Some(view);

    // Readback buffer.
    let buffer_info = vk::BufferCreateInfo::default()
        .size(byte_len)
        .usage(vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    // SAFETY: valid device and create info.
    let buffer = unsafe { device.create_buffer(&buffer_info, None) }?;
    // SAFETY: buffer was just created.
    let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let alloc = ctx.allocator().lock().expect("allocator lock").allocate(&AllocationCreateDesc {
        name: "offscreen readback",
        requirements,
        location: MemoryLocation::GpuToCpu,
        linear: true,
        allocation_scheme: AllocationScheme::GpuAllocatorManaged,
    })?;
    // SAFETY: memory satisfies the buffer's requirements and is bound once.
    unsafe { device.bind_buffer_memory(buffer, alloc.memory(), alloc.offset()) }?;
    cleanup.buffer = Some((buffer, alloc));

    let pipeline = QuadPipeline::new(device, OFFSCREEN_FORMAT)?;
    let result = (|| -> Result<()> {
        let pool_info = vk::CommandPoolCreateInfo::default().queue_family_index(ctx.queue_family);
        // SAFETY: valid device.
        let pool = unsafe { device.create_command_pool(&pool_info, None) }?;
        cleanup.pool = Some(pool);
        // SAFETY: valid device.
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }?;
        cleanup.fence = Some(fence);
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: valid pool.
        let cmd = unsafe { device.allocate_command_buffers(&alloc_info) }?[0];

        // SAFETY: freshly allocated command buffer; every handle recorded belongs to this device.
        unsafe {
            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            image_barrier(
                device,
                cmd,
                image,
                (vk::ImageLayout::UNDEFINED, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE),
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
                .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: clear_color } })];
            let rendering = vk::RenderingInfo::default()
                .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent })
                .layer_count(1)
                .color_attachments(&color);
            device.cmd_begin_rendering(cmd, &rendering);
            pipeline.draw(device, cmd, extent, quads);
            device.cmd_end_rendering(cmd);
            image_barrier(
                device,
                cmd,
                image,
                (
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                ),
                (
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                ),
            );
            let region = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D { width, height, depth: 1 })];
            device.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &region,
            );
            let to_host = [vk::BufferMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)
                .buffer(buffer)
                .size(vk::WHOLE_SIZE)];
            device
                .cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().buffer_memory_barriers(&to_host));
            device.end_command_buffer(cmd)?;
            let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = vk::SubmitInfo2::default().command_buffer_infos(&cmds);
            device.queue_submit2(ctx.queue, &[submit], fence)?;
            device.wait_for_fences(&[fence], true, u64::MAX)?;
        }
        Ok(())
    })();
    // SAFETY: the submission finished (or never happened), so the pipeline is unused.
    unsafe { pipeline.destroy(device) };
    result?;

    let (_, alloc) = cleanup.buffer.as_ref().expect("buffer allocated");
    let mapped = alloc.mapped_slice().context("readback buffer is not host-mapped")?;
    Ok(mapped[..byte_len as usize].to_vec())
}

/// Destroys whatever was created, in reverse order, on every exit path.
struct Cleanup<'a> {
    ctx: &'a Context,
    image: Option<(vk::Image, Allocation)>,
    view: Option<vk::ImageView>,
    buffer: Option<(vk::Buffer, Allocation)>,
    pool: Option<vk::CommandPool>,
    fence: Option<vk::Fence>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let device = &self.ctx.device;
        self.ctx.wait_idle();
        let mut allocator = self.ctx.allocator().lock().expect("allocator lock");
        // SAFETY: the device is idle; each object was created from it and is destroyed once.
        unsafe {
            if let Some(f) = self.fence.take() {
                device.destroy_fence(f, None);
            }
            if let Some(p) = self.pool.take() {
                device.destroy_command_pool(p, None);
            }
            if let Some((b, a)) = self.buffer.take() {
                device.destroy_buffer(b, None);
                allocator.free(a).ok();
            }
            if let Some(v) = self.view.take() {
                device.destroy_image_view(v, None);
            }
            if let Some((i, a)) = self.image.take() {
                device.destroy_image(i, None);
                allocator.free(a).ok();
            }
        }
    }
}
