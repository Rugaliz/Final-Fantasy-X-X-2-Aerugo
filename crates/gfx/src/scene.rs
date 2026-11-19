//! The 3D viewport's image and the helpers the game renderer builds on: buffers, images, one-time submits.

use anyhow::{Context as _, Result};
use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};

use crate::context::Context;

/// Format of the viewport image (the frame the game renderer copies its picture into).
pub const VIEWPORT_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// A buffer with its memory.
#[derive(Debug)]
pub(crate) struct Buffer {
    pub buffer: vk::Buffer,
    pub size: u64,
    allocation: Option<Allocation>,
}

impl Buffer {
    pub fn new(
        ctx: &Context,
        size: u64,
        usage: vk::BufferUsageFlags,
        location: MemoryLocation,
    ) -> Result<Buffer> {
        let info = vk::BufferCreateInfo::default()
            .size(size.max(16))
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: valid device and create info.
        let buffer = unsafe { ctx.device.create_buffer(&info, None) }?;
        // SAFETY: buffer was just created.
        let requirements = unsafe { ctx.device.get_buffer_memory_requirements(buffer) };
        let alloc = ctx.allocator().lock().expect("allocator lock").allocate(&AllocationCreateDesc {
            name: "scene buffer",
            requirements,
            location,
            linear: true,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        });
        let alloc = match alloc {
            Ok(a) => a,
            Err(e) => {
                // SAFETY: never bound or used.
                unsafe { ctx.device.destroy_buffer(buffer, None) };
                return Err(e.into());
            }
        };
        // SAFETY: memory satisfies the requirements and is bound once.
        unsafe { ctx.device.bind_buffer_memory(buffer, alloc.memory(), alloc.offset()) }?;
        Ok(Buffer { buffer, size: size.max(16), allocation: Some(alloc) })
    }

    pub fn mapped(&mut self) -> Option<&mut [u8]> {
        self.allocation.as_mut()?.mapped_slice_mut()
    }

    /// A device-local buffer filled through a staging copy (waits for completion).
    pub fn with_data(ctx: &Context, usage: vk::BufferUsageFlags, data: &[u8]) -> Result<Buffer> {
        let mut staging = Buffer::new(
            ctx,
            data.len() as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
        )?;
        let result = (|| -> Result<Buffer> {
            staging.mapped().context("staging buffer is not host-visible")?[..data.len()]
                .copy_from_slice(data);
            let dst = Buffer::new(
                ctx,
                data.len() as u64,
                usage | vk::BufferUsageFlags::TRANSFER_DST,
                MemoryLocation::GpuOnly,
            )?;
            let region = [vk::BufferCopy::default().size(data.len() as u64)];
            // Empty data still gets a (minimum-size) buffer, but nothing to copy.
            let copy = if data.is_empty() {
                Ok(())
            } else {
                one_time_submit(ctx, |cmd| {
                    // SAFETY: both buffers are valid and large enough.
                    unsafe { ctx.device.cmd_copy_buffer(cmd, staging.buffer, dst.buffer, &region) };
                })
            };
            if let Err(e) = copy {
                let mut dst = dst;
                // SAFETY: the copy failed to submit or completed.
                unsafe { dst.destroy(ctx) };
                return Err(e);
            }
            Ok(dst)
        })();
        // SAFETY: the copy has completed (or never ran).
        unsafe { staging.destroy(ctx) };
        result
    }

    /// # Safety
    /// The GPU must no longer use the buffer.
    pub unsafe fn destroy(&mut self, ctx: &Context) {
        // SAFETY: guaranteed by the caller.
        unsafe { ctx.device.destroy_buffer(self.buffer, None) };
        if let Some(a) = self.allocation.take() {
            ctx.allocator().lock().expect("allocator lock").free(a).ok();
        }
        self.buffer = vk::Buffer::null();
    }
}

/// Record commands with `f`, submit them and wait.
pub(crate) fn one_time_submit(ctx: &Context, f: impl FnOnce(vk::CommandBuffer)) -> Result<()> {
    let device = &ctx.device;
    let pool_info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(ctx.queue_family)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT);
    // SAFETY: valid device.
    let pool = unsafe { device.create_command_pool(&pool_info, None) }?;
    let result = (|| -> Result<()> {
        // SAFETY: valid device.
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }?;
        let r = (|| -> Result<()> {
            let alloc = vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            // SAFETY: valid pool.
            let cmd = unsafe { device.allocate_command_buffers(&alloc) }?[0];
            // SAFETY: fresh command buffer.
            unsafe {
                device.begin_command_buffer(
                    cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )?
            };
            f(cmd);
            // SAFETY: recording; the queue is used from this thread only.
            unsafe {
                device.end_command_buffer(cmd)?;
                let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
                device.queue_submit2(
                    ctx.queue,
                    &[vk::SubmitInfo2::default().command_buffer_infos(&cmds)],
                    fence,
                )?;
                device.wait_for_fences(&[fence], true, u64::MAX)?;
            }
            Ok(())
        })();
        // SAFETY: the submission completed or never happened.
        unsafe { device.destroy_fence(fence, None) };
        r
    })();
    // SAFETY: no command buffer from the pool is pending.
    unsafe { device.destroy_command_pool(pool, None) };
    result
}

/// An image with memory and a view.
#[derive(Debug)]
pub(crate) struct Target {
    pub image: vk::Image,
    pub view: vk::ImageView,
    /// A second view of the image in another format (null when not asked for).
    pub alt_view: vk::ImageView,
    allocation: Option<Allocation>,
}

impl Target {
    pub fn new(
        ctx: &Context,
        format: vk::Format,
        extent: vk::Extent2D,
        usage: vk::ImageUsageFlags,
        aspect: vk::ImageAspectFlags,
        alt_format: Option<vk::Format>,
        samples: vk::SampleCountFlags,
    ) -> Result<Target> {
        // The alternative view may not support every usage of the image (an sRGB view cannot be a storage
        // image), so the image allows views with fewer usages.
        let flags = if alt_format.is_some() {
            vk::ImageCreateFlags::MUTABLE_FORMAT | vk::ImageCreateFlags::EXTENDED_USAGE
        } else {
            vk::ImageCreateFlags::empty()
        };
        let info = vk::ImageCreateInfo::default()
            .flags(flags)
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: valid device and create info.
        let image = unsafe { ctx.device.create_image(&info, None) }?;
        let mut t =
            Target { image, view: vk::ImageView::null(), alt_view: vk::ImageView::null(), allocation: None };
        let r = (|| -> Result<()> {
            // SAFETY: image was just created.
            let requirements = unsafe { ctx.device.get_image_memory_requirements(image) };
            let alloc = ctx.allocator().lock().expect("allocator lock").allocate(&AllocationCreateDesc {
                name: "scene target",
                requirements,
                location: MemoryLocation::GpuOnly,
                linear: false,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            })?;
            // SAFETY: memory satisfies the requirements and is bound once.
            unsafe { ctx.device.bind_image_memory(image, alloc.memory(), alloc.offset()) }?;
            t.allocation = Some(alloc);
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            // SAFETY: the image is bound.
            t.view = unsafe { ctx.device.create_image_view(&view_info, None) }?;
            if let Some(alt) = alt_format {
                let mut alt_usage =
                    vk::ImageViewUsageCreateInfo::default().usage(usage & !vk::ImageUsageFlags::STORAGE);
                // SAFETY: the image is bound and was created mutable-format with extended usage.
                t.alt_view = unsafe {
                    ctx.device.create_image_view(&view_info.format(alt).push_next(&mut alt_usage), None)
                }?;
            }
            Ok(())
        })();
        if let Err(e) = r {
            // SAFETY: never used.
            unsafe { t.destroy(ctx) };
            return Err(e);
        }
        Ok(t)
    }

    /// # Safety
    /// The GPU must no longer use the image.
    pub unsafe fn destroy(&mut self, ctx: &Context) {
        // SAFETY: guaranteed by the caller; null handles are ignored.
        unsafe {
            ctx.device.destroy_image_view(self.alt_view, None);
            ctx.device.destroy_image_view(self.view, None);
            ctx.device.destroy_image(self.image, None);
        }
        if let Some(a) = self.allocation.take() {
            ctx.allocator().lock().expect("allocator lock").free(a).ok();
        }
    }
}

/// Blend factors of a material.
/// The 3D viewport: its image and the UI's handle to it.
#[derive(Debug)]
pub(crate) struct Viewport {
    pub color: Target,
    pub extent: vk::Extent2D,
    pub ui_set: vk::DescriptorSet,
    pub ui_id: egui::TextureId,
    pub color_layout: vk::ImageLayout,
}

/// The viewport and the post-processing settings the game renderer reads.
#[derive(Debug, Default)]
pub(crate) struct SceneState {
    pub viewport: Option<Viewport>,
    /// HDAO level (0 off, 1–3).
    pub hdao_level: u32,
    /// (Brightness setting − 50) / 100.
    pub brightness: f32,
}
