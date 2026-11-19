//! Sampled textures: creation from CPU data (all levels and layers), views and destruction.

use anyhow::{Context as _, Result, bail};
use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};

use crate::context::Context;

/// Texel formats the games use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TexFormat {
    Bc1,
    Bc2,
    Bc3,
    Rgba8,
    Bgra8,
    /// Single channel, shown as grey.
    R8,
}

impl TexFormat {
    pub fn to_vk(self) -> vk::Format {
        match self {
            TexFormat::Bc1 => vk::Format::BC1_RGBA_UNORM_BLOCK,
            TexFormat::Bc2 => vk::Format::BC2_UNORM_BLOCK,
            TexFormat::Bc3 => vk::Format::BC3_UNORM_BLOCK,
            TexFormat::Rgba8 => vk::Format::R8G8B8A8_UNORM,
            TexFormat::Bgra8 => vk::Format::B8G8R8A8_UNORM,
            TexFormat::R8 => vk::Format::R8_UNORM,
        }
    }

    fn block(self) -> (u32, usize) {
        match self {
            TexFormat::Bc1 => (4, 8),
            TexFormat::Bc2 | TexFormat::Bc3 => (4, 16),
            TexFormat::Rgba8 | TexFormat::Bgra8 => (1, 4),
            TexFormat::R8 => (1, 1),
        }
    }

    /// Bytes of one level.
    pub fn level_size(self, width: u32, height: u32) -> usize {
        let (b, bytes) = self.block();
        (width.max(1).div_ceil(b) as usize) * (height.max(1).div_ceil(b) as usize) * bytes
    }

    fn components(self) -> vk::ComponentMapping {
        match self {
            TexFormat::R8 => vk::ComponentMapping {
                r: vk::ComponentSwizzle::R,
                g: vk::ComponentSwizzle::R,
                b: vk::ComponentSwizzle::R,
                a: vk::ComponentSwizzle::ONE,
            },
            _ => vk::ComponentMapping::default(),
        }
    }
}

/// CPU data for a texture: `data[layer * levels + level]`.
#[derive(Debug)]
pub struct TextureDesc<'a> {
    pub format: TexFormat,
    pub width: u32,
    pub height: u32,
    pub levels: u32,
    /// 1, or 6 for a cube map.
    pub layers: u32,
    pub cube: bool,
    pub data: Vec<&'a [u8]>,
    pub name: &'a str,
}

/// A device-local sampled image with a default view covering everything.
#[derive(Debug)]
pub struct Texture {
    pub image: vk::Image,
    pub view: vk::ImageView,
    /// The whole texture as Direct3D samples it (no channel swizzle; the same view unless `view` swizzles).
    pub raw_view: vk::ImageView,
    pub format: TexFormat,
    pub width: u32,
    pub height: u32,
    pub levels: u32,
    pub layers: u32,
    pub cube: bool,
    allocation: Option<Allocation>,
}

fn full_range(levels: u32, layers: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: levels,
        base_array_layer: 0,
        layer_count: layers,
    }
}

impl Texture {
    /// Create the image and upload all levels, waiting for the copy to finish.
    pub fn create(ctx: &Context, desc: &TextureDesc<'_>) -> Result<Texture> {
        let (w, h, levels, layers) =
            (desc.width.max(1), desc.height.max(1), desc.levels.max(1), desc.layers.max(1));
        if desc.data.len() != (levels * layers) as usize {
            bail!("{}: {} data slices for {levels} levels × {layers} layers", desc.name, desc.data.len());
        }
        let mut regions = Vec::with_capacity(desc.data.len());
        let mut total = 0usize;
        for layer in 0..layers {
            for level in 0..levels {
                let (lw, lh) = ((w >> level).max(1), (h >> level).max(1));
                let size = desc.format.level_size(lw, lh);
                let slice = desc.data[(layer * levels + level) as usize];
                if slice.len() < size {
                    bail!("{}: level {level} has {} bytes, needs {size}", desc.name, slice.len());
                }
                total = total.next_multiple_of(16);
                regions.push((total, layer, level, lw, lh, size));
                total += size;
            }
        }

        let device = &ctx.device;
        let mut flags = vk::ImageCreateFlags::empty();
        if desc.cube {
            flags |= vk::ImageCreateFlags::CUBE_COMPATIBLE;
        }
        let info = vk::ImageCreateInfo::default()
            .flags(flags)
            .image_type(vk::ImageType::TYPE_2D)
            .format(desc.format.to_vk())
            .extent(vk::Extent3D { width: w, height: h, depth: 1 })
            .mip_levels(levels)
            .array_layers(layers)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: valid device and create info (format support is a device requirement, see Context).
        let image = unsafe { device.create_image(&info, None) }.with_context(|| desc.name.to_owned())?;
        let mut texture = Texture {
            image,
            view: vk::ImageView::null(),
            raw_view: vk::ImageView::null(),
            format: desc.format,
            width: w,
            height: h,
            levels,
            layers,
            cube: desc.cube,
            allocation: None,
        };
        let result = (|| -> Result<()> {
            // SAFETY: image was just created.
            let requirements = unsafe { device.get_image_memory_requirements(image) };
            let alloc = ctx.allocator().lock().expect("allocator lock").allocate(&AllocationCreateDesc {
                name: "texture",
                requirements,
                location: MemoryLocation::GpuOnly,
                linear: false,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            })?;
            // SAFETY: memory satisfies the image's requirements and is bound once.
            unsafe { device.bind_image_memory(image, alloc.memory(), alloc.offset()) }?;
            texture.allocation = Some(alloc);
            texture.view = texture.create_view(ctx, None, None)?;
            texture.raw_view = if desc.format != TexFormat::R8 {
                texture.view
            } else {
                let info = vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(if texture.cube {
                        vk::ImageViewType::CUBE
                    } else {
                        vk::ImageViewType::TYPE_2D
                    })
                    .format(desc.format.to_vk())
                    .subresource_range(full_range(levels, layers));
                // SAFETY: the image is bound to memory.
                unsafe { device.create_image_view(&info, None) }?
            };
            upload(ctx, image, levels, layers, total, &regions, &desc.data)
        })();
        if let Err(e) = result {
            // SAFETY: nothing has been submitted that still uses the image (upload waits for completion).
            unsafe { texture.destroy(ctx) };
            return Err(e);
        }
        Ok(texture)
    }

    /// A view of the whole texture (`None`) or of one layer and level as a 2D image.
    pub fn create_view(
        &self,
        ctx: &Context,
        layer: Option<u32>,
        level: Option<u32>,
    ) -> Result<vk::ImageView> {
        let (view_type, range) = match (layer, level) {
            (None, None) => (
                if self.cube { vk::ImageViewType::CUBE } else { vk::ImageViewType::TYPE_2D },
                full_range(self.levels, self.layers),
            ),
            (layer, level) => (
                vk::ImageViewType::TYPE_2D,
                vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: level.unwrap_or(0).min(self.levels - 1),
                    level_count: if level.is_some() { 1 } else { self.levels },
                    base_array_layer: layer.unwrap_or(0).min(self.layers - 1),
                    layer_count: 1,
                },
            ),
        };
        let info = vk::ImageViewCreateInfo::default()
            .image(self.image)
            .view_type(view_type)
            .format(self.format.to_vk())
            .components(self.format.components())
            .subresource_range(range);
        // SAFETY: the image is bound to memory and the range lies inside it.
        Ok(unsafe { ctx.device.create_image_view(&info, None) }?)
    }

    /// Replace level 0 of layer 0 (e.g. a movie frame), waiting for the copy. Earlier GPU reads of the texture
    /// on the same queue finish first.
    pub fn update(&mut self, ctx: &Context, data: &[u8]) -> Result<()> {
        let size = self.format.level_size(self.width, self.height);
        if data.len() < size {
            bail!("texture update: {} bytes, needs {size}", data.len());
        }
        let device = &ctx.device;
        let mut staging = crate::scene::Buffer::new(
            ctx,
            size as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
        )?;
        let result = (|| -> Result<()> {
            staging.mapped().context("staging buffer is not host-visible")?[..size]
                .copy_from_slice(&data[..size]);
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let image = self.image;
            let copy = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 })];
            let buffer = staging.buffer;
            crate::scene::one_time_submit(ctx, |cmd| {
                // SAFETY: recording; the image and buffer belong to this device.
                unsafe {
                    let to_dst = [vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                        .src_access_mask(vk::AccessFlags2::NONE)
                        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                        .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .image(image)
                        .subresource_range(range)];
                    device.cmd_pipeline_barrier2(
                        cmd,
                        &vk::DependencyInfo::default().image_memory_barriers(&to_dst),
                    );
                    device.cmd_copy_buffer_to_image(
                        cmd,
                        buffer,
                        image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &copy,
                    );
                    let to_read = [vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::COPY)
                        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .image(image)
                        .subresource_range(range)];
                    device.cmd_pipeline_barrier2(
                        cmd,
                        &vk::DependencyInfo::default().image_memory_barriers(&to_read),
                    );
                }
            })
        })();
        // SAFETY: the copy completed (or never ran).
        unsafe { staging.destroy(ctx) };
        result
    }

    /// # Safety
    /// The GPU must no longer use the texture or views created from it.
    pub unsafe fn destroy(&mut self, ctx: &Context) {
        // SAFETY: guaranteed by the caller; null handles are ignored by Vulkan.
        unsafe {
            if self.raw_view != self.view {
                ctx.device.destroy_image_view(self.raw_view, None);
            }
            ctx.device.destroy_image_view(self.view, None);
            ctx.device.destroy_image(self.image, None);
        }
        self.raw_view = vk::ImageView::null();
        if let Some(a) = self.allocation.take() {
            ctx.allocator().lock().expect("allocator lock").free(a).ok();
        }
        self.view = vk::ImageView::null();
        self.image = vk::Image::null();
    }
}

type Region = (usize, u32, u32, u32, u32, usize);

/// Copy every region through a staging buffer and leave the image in SHADER_READ_ONLY_OPTIMAL.
fn upload(
    ctx: &Context,
    image: vk::Image,
    levels: u32,
    layers: u32,
    total: usize,
    regions: &[Region],
    data: &[&[u8]],
) -> Result<()> {
    let device = &ctx.device;
    let info = vk::BufferCreateInfo::default()
        .size(total.max(16) as u64)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    // SAFETY: valid device and create info.
    let buffer = unsafe { device.create_buffer(&info, None) }?;
    let mut staging: Option<Allocation> = None;
    let mut pool = vk::CommandPool::null();
    let mut fence = vk::Fence::null();
    let result = (|| -> Result<()> {
        // SAFETY: buffer was just created.
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let mut alloc = ctx.allocator().lock().expect("allocator lock").allocate(&AllocationCreateDesc {
            name: "texture staging",
            requirements,
            location: MemoryLocation::CpuToGpu,
            linear: true,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        })?;
        // SAFETY: memory satisfies the buffer's requirements and is bound once.
        unsafe { device.bind_buffer_memory(buffer, alloc.memory(), alloc.offset()) }?;
        let mapped = alloc.mapped_slice_mut().context("staging buffer is not host-visible")?;
        for (i, &(offset, _, _, _, _, size)) in regions.iter().enumerate() {
            mapped[offset..offset + size].copy_from_slice(&data[i][..size]);
        }
        staging = Some(alloc);

        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(ctx.queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: valid device.
        pool = unsafe { device.create_command_pool(&pool_info, None) }?;
        // SAFETY: valid device.
        fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }?;
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: valid pool.
        let cmd = unsafe { device.allocate_command_buffers(&alloc_info) }?[0];
        let copies: Vec<vk::BufferImageCopy> = regions
            .iter()
            .map(|&(offset, layer, level, w, h, _)| {
                vk::BufferImageCopy::default()
                    .buffer_offset(offset as u64)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: level,
                        base_array_layer: layer,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D { width: w, height: h, depth: 1 })
            })
            .collect();
        let range = full_range(levels, layers);
        // SAFETY: freshly allocated command buffer; all handles belong to this device.
        unsafe {
            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let to_dst = [vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .image(image)
                .subresource_range(range)];
            device.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&to_dst));
            device.cmd_copy_buffer_to_image(
                cmd,
                buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &copies,
            );
            let to_read = [vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image(image)
                .subresource_range(range)];
            device.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&to_read));
            device.end_command_buffer(cmd)?;
            let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = vk::SubmitInfo2::default().command_buffer_infos(&cmds);
            device.queue_submit2(ctx.queue, &[submit], fence)?;
            device.wait_for_fences(&[fence], true, u64::MAX)?;
        }
        Ok(())
    })();
    // SAFETY: the submission (if any) has completed; each object is destroyed once.
    unsafe {
        device.destroy_fence(fence, None);
        device.destroy_command_pool(pool, None);
        device.destroy_buffer(buffer, None);
    }
    if let Some(a) = staging {
        ctx.allocator().lock().expect("allocator lock").free(a).ok();
    }
    result
}
