//! 3D scene drawing for the viewers: meshes with a base texture, optional GPU skinning, rendered into an
//! offscreen target that the UI shows as an image.

use anyhow::{Context as _, Result};
use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};

use crate::context::Context;

static VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh.vert.spv"));
static FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh.frag.spv"));

pub const VIEWPORT_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
pub const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

/// One vertex as the mesh pipeline reads it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SceneVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    pub weights: [f32; 4],
    pub joints: [u8; 4],
    /// Second texture coordinate set (water: second base texture and second normal map).
    pub uv2: [f32; 2],
    /// Tangent of the first texture coordinate set (water normal mapping).
    pub tangent: [f32; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScenePush {
    pub view_proj: [f32; 16],
    /// Model matrix rows 0–2 (affine).
    pub model: [[f32; 4]; 3],
    /// x: first joint (water: first constant slot), y: skinned, z: alpha cutoff, w: shading mode.
    pub params: [f32; 4],
}

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
    ) -> Result<Target> {
        let flags = if alt_format.is_some() {
            vk::ImageCreateFlags::MUTABLE_FORMAT
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
            .samples(vk::SampleCountFlags::TYPE_1)
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
                // SAFETY: the image is bound and was created mutable-format.
                t.alt_view = unsafe { ctx.device.create_image_view(&view_info.format(alt), None) }?;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    DstColor,
    OneMinusDstColor,
    SrcAlphaSaturate,
    Constant,
    OneMinusConstant,
}

impl BlendFactor {
    fn vk(self) -> vk::BlendFactor {
        match self {
            BlendFactor::Zero => vk::BlendFactor::ZERO,
            BlendFactor::One => vk::BlendFactor::ONE,
            BlendFactor::SrcColor => vk::BlendFactor::SRC_COLOR,
            BlendFactor::OneMinusSrcColor => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
            BlendFactor::SrcAlpha => vk::BlendFactor::SRC_ALPHA,
            BlendFactor::OneMinusSrcAlpha => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
            BlendFactor::DstAlpha => vk::BlendFactor::DST_ALPHA,
            BlendFactor::OneMinusDstAlpha => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
            BlendFactor::DstColor => vk::BlendFactor::DST_COLOR,
            BlendFactor::OneMinusDstColor => vk::BlendFactor::ONE_MINUS_DST_COLOR,
            BlendFactor::SrcAlphaSaturate => vk::BlendFactor::SRC_ALPHA_SATURATE,
            BlendFactor::Constant => vk::BlendFactor::CONSTANT_COLOR,
            BlendFactor::OneMinusConstant => vk::BlendFactor::ONE_MINUS_CONSTANT_COLOR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

impl BlendOp {
    fn vk(self) -> vk::BlendOp {
        match self {
            BlendOp::Add => vk::BlendOp::ADD,
            BlendOp::Subtract => vk::BlendOp::SUBTRACT,
            BlendOp::ReverseSubtract => vk::BlendOp::REVERSE_SUBTRACT,
            BlendOp::Min => vk::BlendOp::MIN,
            BlendOp::Max => vk::BlendOp::MAX,
        }
    }
}

/// Colour and alpha blend equations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlendState {
    pub src: BlendFactor,
    pub dst: BlendFactor,
    pub op: BlendOp,
    pub src_alpha: BlendFactor,
    pub dst_alpha: BlendFactor,
    pub op_alpha: BlendOp,
}

impl BlendState {
    pub const ALPHA: BlendState = BlendState {
        src: BlendFactor::SrcAlpha,
        dst: BlendFactor::OneMinusSrcAlpha,
        op: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::OneMinusSrcAlpha,
        op_alpha: BlendOp::Add,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CullMode {
    None,
    Front,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareOp {
    Never,
    Less,
    Equal,
    LessOrEqual,
    Greater,
    NotEqual,
    GreaterOrEqual,
    Always,
}

impl CompareOp {
    fn vk(self) -> vk::CompareOp {
        match self {
            CompareOp::Never => vk::CompareOp::NEVER,
            CompareOp::Less => vk::CompareOp::LESS,
            CompareOp::Equal => vk::CompareOp::EQUAL,
            CompareOp::LessOrEqual => vk::CompareOp::LESS_OR_EQUAL,
            CompareOp::Greater => vk::CompareOp::GREATER,
            CompareOp::NotEqual => vk::CompareOp::NOT_EQUAL,
            CompareOp::GreaterOrEqual => vk::CompareOp::GREATER_OR_EQUAL,
            CompareOp::Always => vk::CompareOp::ALWAYS,
        }
    }
}

/// How a mesh part is drawn: the fixed-function state its material asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaterialState {
    /// `None` draws opaque (in the first pass, before every blended part).
    pub blend: Option<BlendState>,
    /// RGBA write mask (bit 0 = red).
    pub write_mask: u8,
    pub cull: CullMode,
    /// Counter-clockwise triangles (on screen) are front faces.
    pub front_ccw: bool,
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_compare: CompareOp,
}

impl Default for MaterialState {
    /// What the viewers used before materials: alpha blending, no culling, depth test and write.
    fn default() -> MaterialState {
        MaterialState {
            blend: Some(BlendState::ALPHA),
            write_mask: 0xF,
            cull: CullMode::None,
            front_ccw: false,
            depth_test: true,
            depth_write: true,
            depth_compare: CompareOp::LessOrEqual,
        }
    }
}

/// What a pipeline is built for; everything else in [`MaterialState`] is dynamic state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    blend: Option<BlendState>,
    write_mask: u8,
}

impl From<&MaterialState> for PipelineKey {
    fn from(m: &MaterialState) -> PipelineKey {
        PipelineKey { blend: m.blend, write_mask: m.write_mask }
    }
}

/// The mesh pipelines (one per blend state) and their descriptor layouts.
#[derive(Debug)]
pub(crate) struct ScenePipeline {
    pub material_layout: vk::DescriptorSetLayout,
    pub joints_layout: vk::DescriptorSetLayout,
    /// Set 2: the water shader's second texture, two normal maps and cube map.
    pub extra_layout: vk::DescriptorSetLayout,
    pub layout: vk::PipelineLayout,
    vert: vk::ShaderModule,
    frag: vk::ShaderModule,
    pipelines: std::collections::HashMap<PipelineKey, vk::Pipeline>,
}

impl ScenePipeline {
    pub fn new(device: &ash::Device) -> Result<ScenePipeline> {
        let material_bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        ];
        let joint_bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)];
        // SAFETY: valid device and create infos.
        let material_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&material_bindings),
                None,
            )
        }?;
        // SAFETY: as above.
        let joints_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&joint_bindings),
                None,
            )
        }?;
        let extra_bindings: Vec<_> = (0..WATER_TEXTURES as u32)
            .map(|b| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(b)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            })
            .collect();
        // SAFETY: as above.
        let extra_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&extra_bindings),
                None,
            )
        }?;
        let set_layouts = [material_layout, joints_layout, extra_layout];
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(size_of::<ScenePush>() as u32)];
        // SAFETY: valid device and layouts.
        let layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&set_layouts)
                    .push_constant_ranges(&push),
                None,
            )
        }?;
        let vert = crate::quad::shader_module(device, VERT_SPV)?;
        let frag = crate::quad::shader_module(device, FRAG_SPV)?;
        let mut p = ScenePipeline {
            material_layout,
            joints_layout,
            extra_layout,
            layout,
            vert,
            frag,
            pipelines: Default::default(),
        };
        p.get(device, &MaterialState::default())?;
        Ok(p)
    }

    /// The pipeline for a material's blend state (built on first use).
    pub fn get(&mut self, device: &ash::Device, m: &MaterialState) -> Result<vk::Pipeline> {
        let key = PipelineKey::from(m);
        if let Some(&p) = self.pipelines.get(&key) {
            return Ok(p);
        }
        let (vert, frag, layout) = (self.vert, self.frag, self.layout);
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag)
                .name(c"main"),
        ];
        let binding = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<SceneVertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attr = |location: u32, format: vk::Format, offset: usize| {
            vk::VertexInputAttributeDescription::default()
                .location(location)
                .binding(0)
                .format(format)
                .offset(offset as u32)
        };
        let attributes = [
            attr(0, vk::Format::R32G32B32_SFLOAT, std::mem::offset_of!(SceneVertex, position)),
            attr(1, vk::Format::R32G32B32_SFLOAT, std::mem::offset_of!(SceneVertex, normal)),
            attr(2, vk::Format::R32G32_SFLOAT, std::mem::offset_of!(SceneVertex, uv)),
            attr(3, vk::Format::R32G32B32A32_SFLOAT, std::mem::offset_of!(SceneVertex, color)),
            attr(4, vk::Format::R32G32B32A32_SFLOAT, std::mem::offset_of!(SceneVertex, weights)),
            attr(5, vk::Format::R8G8B8A8_UINT, std::mem::offset_of!(SceneVertex, joints)),
            attr(6, vk::Format::R32G32_SFLOAT, std::mem::offset_of!(SceneVertex, uv2)),
            attr(7, vk::Format::R32G32B32_SFLOAT, std::mem::offset_of!(SceneVertex, tangent)),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&binding)
            .vertex_attribute_descriptions(&attributes);
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let b = key.blend.unwrap_or(BlendState::ALPHA);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(key.blend.is_some())
            .src_color_blend_factor(b.src.vk())
            .dst_color_blend_factor(b.dst.vk())
            .color_blend_op(b.op.vk())
            .src_alpha_blend_factor(b.src_alpha.vk())
            .dst_alpha_blend_factor(b.dst_alpha.vk())
            .alpha_blend_op(b.op_alpha.vk())
            .color_write_mask(vk::ColorComponentFlags::from_raw(u32::from(key.write_mask & 0xF)))];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        // Culling, winding and depth state vary per part without new pipelines (core in Vulkan 1.3).
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::CULL_MODE,
            vk::DynamicState::FRONT_FACE,
            vk::DynamicState::DEPTH_TEST_ENABLE,
            vk::DynamicState::DEPTH_WRITE_ENABLE,
            vk::DynamicState::DEPTH_COMPARE_OP,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let formats = [VIEWPORT_FORMAT];
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&formats)
            .depth_attachment_format(DEPTH_FORMAT);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(layout)
            .push_next(&mut rendering);
        // SAFETY: all referenced objects are valid for the call.
        let pipeline = unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }
            .map_err(|(_, e)| e)?[0];
        self.pipelines.insert(key, pipeline);
        Ok(pipeline)
    }

    /// # Safety
    /// The pipeline must not be in use.
    pub unsafe fn destroy(&self, device: &ash::Device) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            for &p in self.pipelines.values() {
                device.destroy_pipeline(p, None);
            }
            device.destroy_shader_module(self.vert, None);
            device.destroy_shader_module(self.frag, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.material_layout, None);
            device.destroy_descriptor_set_layout(self.joints_layout, None);
            device.destroy_descriptor_set_layout(self.extra_layout, None);
        }
    }
}

/// A mesh the renderer owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeshHandle(pub(crate) u64);

/// A range of a mesh's indices drawn with one texture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshPart {
    pub first_index: u32,
    pub index_count: u32,
    /// Base texture; `None` draws white (vertex colour only).
    pub texture: Option<crate::renderer::TextureHandle>,
    /// Fragments with lower alpha are discarded.
    pub alpha_cutoff: f32,
    pub material: MaterialState,
    /// The remaster's colour curve after texture × vertex colour (its field shaders brighten the PS2 colours).
    pub curve: ToneCurve,
    /// Texture coordinates move by this much per second of [`SceneView::time`] (UV-animated field parts).
    pub uv_scroll: [f32; 2],
    /// Drawn with the remaster's water shader.
    pub water: Option<Water>,
}

/// Textures a water part samples besides its base texture.
pub const WATER_TEXTURES: usize = 4;

/// What the remaster's water shader adds to a part: c = (base + (0,0,0,1) + second texture) × colour × cube(n) +
/// cube(reflect(view, n)) × specular, with n from two scrolling normal maps; then × vertex colour, fog, colour
/// scale and the field curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Water {
    /// Second base texture, first and second normal map, cube map (`None` uses neutral defaults).
    pub textures: [Option<crate::renderer::TextureHandle>; WATER_TEXTURES],
    /// Texture scroll per second: base, second base, normal map 1, normal map 2 (u, v each).
    pub scroll: [[f32; 2]; 4],
    /// Whether each normal map reads the second coordinate set ([`SceneVertex::uv2`]) rather than the first.
    pub normal_uv2: [bool; 2],
    /// Specular colour and shininess.
    pub specular: [f32; 4],
    /// MaterialColour × MultiTexBlendFactor.
    pub color: [f32; 4],
    /// clrScaleFactor.
    pub scale: [f32; 4],
    /// fgEff (per-channel share of the fog).
    pub fog: [f32; 3],
}

impl Water {
    /// The two constant slots the shaders read (see `mesh.frag`).
    fn constants(&self) -> [[f32; 16]; 2] {
        let s = &self.scroll;
        let mut a = [0.0; 16];
        a[..8].copy_from_slice(&[s[0][0], s[0][1], s[1][0], s[1][1], s[2][0], s[2][1], s[3][0], s[3][1]]);
        a[8..12].copy_from_slice(&self.specular);
        a[12..].copy_from_slice(&self.color);
        let mut b = [0.0; 16];
        b[..4].copy_from_slice(&self.scale);
        b[4..7].copy_from_slice(&self.fog);
        b[8] = f32::from(u8::from(self.normal_uv2[0]));
        b[9] = f32::from(u8::from(self.normal_uv2[1]));
        [a, b]
    }
}

/// The colour curves of the remaster's field shaders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToneCurve {
    /// Colours as they are.
    #[default]
    None,
    /// Lit map and water shaders: per channel 2.1·min(c, 0.4) + 1.08·clamp(c, 0.4, 0.58) − 0.47 (about twice
    /// as bright up to 0.4, then a shoulder to 1).
    Field,
    /// The UV-animated sky: c' = min(0.3·c + c^(1/1.7), 1), then 0.5 − 0.5·cos(π·c').
    Sky,
}

impl ToneCurve {
    fn code(self) -> f32 {
        match self {
            ToneCurve::None => 0.0,
            ToneCurve::Field => 2.0,
            ToneCurve::Sky => 3.0,
        }
    }
}

#[derive(Debug)]
pub struct MeshDesc<'a> {
    pub vertices: &'a [SceneVertex],
    pub indices: &'a [u32],
    pub parts: Vec<MeshPart>,
}

/// One mesh drawn with a transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Draw {
    pub mesh: MeshHandle,
    /// Column-major model matrix.
    pub model: [f32; 16],
    /// First joint matrix in [`SceneView::joints`] when the mesh is skinned.
    pub joint_base: Option<u32>,
    pub lit: bool,
    /// Draw only this part (all when `None`).
    pub part: Option<usize>,
}

/// A frame of the 3D viewport.
#[derive(Debug, Clone, Default)]
pub struct SceneView {
    /// Column-major view-projection matrix (Vulkan clip space: y down, depth 0–1).
    pub view_proj: [f32; 16],
    pub clear: [f32; 4],
    pub draws: Vec<Draw>,
    /// Column-major joint matrices for skinned draws.
    pub joints: Vec<[f32; 16]>,
    /// Apply the materials' face culling (off draws both sides of everything).
    pub culling: bool,
    /// Reverse the triangle winding the view matrix implies (front faces follow the handedness of
    /// `view_proj` on their own; this is for tests and special views).
    pub mirrored: bool,
    /// Distance fog for parts drawn with a field curve (the remaster's map shaders).
    pub fog: Option<Fog>,
    /// Scene time in seconds (moves UV-animated parts).
    pub time: f32,
}

/// The remaster's field fog: the share of fog colour at view depth `d` is
/// `min(saturate((d − near) / (far − near + 0.01))^curve, limit)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fog {
    pub color: [f32; 3],
    /// View depths (the units of `view_proj`'s space).
    pub near: f32,
    pub far: f32,
    pub curve: f32,
    pub limit: f32,
}

impl Fog {
    /// The scene constants the shaders read from the first slot of the joint buffer: fog, then the eye
    /// position and scene time.
    fn constants(fog: Option<Fog>, eye: [f32; 3], time: f32) -> [f32; 16] {
        let mut c = [0.0; 16];
        c[8..11].copy_from_slice(&eye);
        c[11] = time;
        if let Some(f) = fog {
            c[..3].copy_from_slice(&f.color);
            c[3] = 1.0;
            c[4] = f.near;
            c[5] = f.far;
            c[6] = f.curve;
            c[7] = f.limit;
        }
        c
    }
}

#[derive(Debug)]
pub(crate) struct GpuMesh {
    pub vertices: Buffer,
    pub indices: Buffer,
    /// Each part with its base texture set (set 0) and extra texture set (set 2).
    pub parts: Vec<(MeshPart, vk::DescriptorSet, vk::DescriptorSet)>,
}

#[derive(Debug)]
pub(crate) struct JointSlot {
    pub buffer: Buffer,
    pub set: vk::DescriptorSet,
}

#[derive(Debug)]
pub(crate) struct Viewport {
    pub color: Target,
    pub depth: Target,
    pub extent: vk::Extent2D,
    pub ui_set: vk::DescriptorSet,
    pub ui_id: egui::TextureId,
    pub color_layout: vk::ImageLayout,
}

/// Everything the renderer keeps for 3D viewports.
#[derive(Debug)]
pub(crate) struct SceneState {
    pub pipeline: ScenePipeline,
    pub pool: vk::DescriptorPool,
    pub sampler: vk::Sampler,
    pub material_sets: std::collections::HashMap<vk::ImageView, vk::DescriptorSet>,
    /// Water texture sets by their views (set 2).
    pub extra_sets: std::collections::HashMap<[vk::ImageView; WATER_TEXTURES], vk::DescriptorSet>,
    /// Views bound in set 2 for parts without water (neutral textures; set by the renderer at start-up).
    pub default_extra: [vk::ImageView; WATER_TEXTURES],
    pub meshes: std::collections::HashMap<u64, GpuMesh>,
    pub next_mesh: u64,
    pub joints: Vec<JointSlot>,
    pub viewport: Option<Viewport>,
    pub pending: Option<SceneView>,
}

const SCENE_SETS: u32 = 8192;
/// Water texture sets (one per distinct combination of textures).
const EXTRA_SETS: u32 = 256;

impl SceneState {
    pub fn new(ctx: &Context, frames: usize) -> Result<SceneState> {
        let device = &ctx.device;
        let pipeline = ScenePipeline::new(device)?;
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: SCENE_SETS + EXTRA_SETS * WATER_TEXTURES as u32,
            },
            vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: SCENE_SETS },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: frames as u32,
            },
        ];
        // SAFETY: valid device and create info.
        let pool = unsafe {
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                    .max_sets(SCENE_SETS + EXTRA_SETS + frames as u32)
                    .pool_sizes(&sizes),
                None,
            )
        }?;
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::REPEAT)
            .address_mode_v(vk::SamplerAddressMode::REPEAT)
            .address_mode_w(vk::SamplerAddressMode::REPEAT)
            .max_lod(vk::LOD_CLAMP_NONE);
        // SAFETY: valid device.
        let sampler = unsafe { device.create_sampler(&sampler_info, None) }?;
        let mut state = SceneState {
            pipeline,
            pool,
            sampler,
            material_sets: Default::default(),
            extra_sets: Default::default(),
            default_extra: [vk::ImageView::null(); WATER_TEXTURES],
            meshes: Default::default(),
            next_mesh: 1,
            joints: Vec::new(),
            viewport: None,
            pending: None,
        };
        for _ in 0..frames {
            let slot = state.joint_slot(ctx, 256)?;
            state.joints.push(slot);
        }
        Ok(state)
    }

    fn joint_slot(&self, ctx: &Context, matrices: usize) -> Result<JointSlot> {
        SceneStateRef { pipeline: &self.pipeline, pool: self.pool }.joint_slot(ctx, matrices)
    }

    /// Descriptor set sampling `view` (created once per view).
    pub fn material_set(&mut self, ctx: &Context, view: vk::ImageView) -> Result<vk::DescriptorSet> {
        if let Some(&s) = self.material_sets.get(&view) {
            return Ok(s);
        }
        let layouts = [self.pipeline.material_layout];
        // SAFETY: valid pool and layout.
        let set = unsafe {
            ctx.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.pool).set_layouts(&layouts),
            )
        }
        .context("too many scene textures")?[0];
        let image = [vk::DescriptorImageInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let sampler = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&image),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&sampler),
        ];
        // SAFETY: the set is new.
        unsafe { ctx.device.update_descriptor_sets(&writes, &[]) };
        self.material_sets.insert(view, set);
        Ok(set)
    }

    /// Descriptor set with the water textures `views` (created once per combination).
    pub fn extra_set(
        &mut self,
        ctx: &Context,
        views: [vk::ImageView; WATER_TEXTURES],
    ) -> Result<vk::DescriptorSet> {
        if let Some(&s) = self.extra_sets.get(&views) {
            return Ok(s);
        }
        let layouts = [self.pipeline.extra_layout];
        // SAFETY: valid pool and layout.
        let set = unsafe {
            ctx.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.pool).set_layouts(&layouts),
            )
        }
        .context("too many water materials")?[0];
        let images: Vec<[vk::DescriptorImageInfo; 1]> = views
            .iter()
            .map(|&v| {
                [vk::DescriptorImageInfo::default()
                    .image_view(v)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
            })
            .collect();
        let writes: Vec<_> = images
            .iter()
            .enumerate()
            .map(|(b, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(b as u32)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info)
            })
            .collect();
        // SAFETY: the set is new.
        unsafe { ctx.device.update_descriptor_sets(&writes, &[]) };
        self.extra_sets.insert(views, set);
        Ok(set)
    }

    /// Forget the sets of a view that is being destroyed (the device must be idle).
    pub fn forget_view(&mut self, ctx: &Context, view: vk::ImageView) {
        let mut sets: Vec<vk::DescriptorSet> = self.material_sets.remove(&view).into_iter().collect();
        self.extra_sets.retain(|k, s| {
            let keep = !k.contains(&view);
            if !keep {
                sets.push(*s);
            }
            keep
        });
        if !sets.is_empty() {
            // SAFETY: the device is idle (caller), so the sets are unused.
            unsafe { ctx.device.free_descriptor_sets(self.pool, &sets).ok() };
        }
    }

    pub fn create_mesh(
        &mut self,
        ctx: &Context,
        desc: &MeshDesc<'_>,
        sets: Vec<(vk::DescriptorSet, vk::DescriptorSet)>,
    ) -> Result<MeshHandle> {
        // SAFETY: SceneVertex is plain old data (repr(C), no padding-sensitive invariants).
        let vbytes = unsafe {
            std::slice::from_raw_parts(
                desc.vertices.as_ptr().cast::<u8>(),
                std::mem::size_of_val(desc.vertices),
            )
        };
        let ibytes: Vec<u8> = desc.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let vertices = Buffer::with_data(ctx, vk::BufferUsageFlags::VERTEX_BUFFER, vbytes)?;
        let indices = match Buffer::with_data(ctx, vk::BufferUsageFlags::INDEX_BUFFER, &ibytes) {
            Ok(b) => b,
            Err(e) => {
                let mut v = vertices;
                // SAFETY: never used by the GPU after the completed upload.
                unsafe { v.destroy(ctx) };
                return Err(e);
            }
        };
        for part in &desc.parts {
            self.pipeline.get(&ctx.device, &part.material)?;
        }
        let id = self.next_mesh;
        self.next_mesh += 1;
        let parts = desc.parts.iter().copied().zip(sets).map(|(p, (s, e))| (p, s, e)).collect();
        self.meshes.insert(id, GpuMesh { vertices, indices, parts });
        Ok(MeshHandle(id))
    }

    /// The device must be idle.
    pub fn destroy_mesh(&mut self, ctx: &Context, h: MeshHandle) {
        if let Some(mut m) = self.meshes.remove(&h.0) {
            // SAFETY: the caller waited for the device.
            unsafe {
                m.vertices.destroy(ctx);
                m.indices.destroy(ctx);
            }
        }
    }

    /// Record the pending scene into the viewport (outside any render pass).
    pub fn record(&mut self, ctx: &Context, cmd: vk::CommandBuffer, frame: usize) -> Result<()> {
        let Some(view) = self.pending.take() else { return Ok(()) };
        if self.viewport.is_none() {
            return Ok(());
        }
        let device = &ctx.device;
        // Joint matrices for this frame slot (its previous submission has finished).
        // The joint buffer starts with the scene constants (fog, eye, time); joint matrices follow, then two
        // constant slots per drawn water part.
        let mut water: Vec<[[f32; 16]; 2]> = Vec::new();
        let mut water_slot = std::collections::HashMap::new();
        for (di, d) in view.draws.iter().enumerate() {
            let Some(mesh) = self.meshes.get(&d.mesh.0) else { continue };
            for (pi, (part, _, _)) in mesh.parts.iter().enumerate() {
                if let Some(w) = &part.water
                    && d.part.is_none_or(|p| p == pi)
                {
                    water_slot.insert((di, pi), (1 + view.joints.len() + 2 * water.len()) as f32);
                    water.push(w.constants());
                }
            }
        }
        let slots = view.joints.len() + 1 + 2 * water.len();
        if slots * 64 > self.joints[frame].buffer.size as usize {
            let fresh = SceneStateRef { pipeline: &self.pipeline, pool: self.pool }
                .joint_slot(ctx, slots.next_power_of_two())?;
            let mut old = std::mem::replace(&mut self.joints[frame], fresh);
            // SAFETY: this slot's last use has completed (its fence was waited before recording).
            unsafe {
                device.free_descriptor_sets(self.pool, &[old.set]).ok();
                old.buffer.destroy(ctx);
            }
        }
        if let Some(m) = self.joints[frame].buffer.mapped() {
            let scene = Fog::constants(view.fog, eye_of(&view.view_proj), view.time);
            for (i, j) in
                std::iter::once(&scene).chain(view.joints.iter()).chain(water.iter().flatten()).enumerate()
            {
                for (k, f) in j.iter().enumerate() {
                    m[i * 64 + k * 4..i * 64 + k * 4 + 4].copy_from_slice(&f.to_le_bytes());
                }
            }
        }
        let vp = self.viewport.as_mut().expect("checked above");
        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let depth_range =
            vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::DEPTH, ..color_range };
        let barriers = [
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                .src_access_mask(vk::AccessFlags2::NONE)
                .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .image(vp.color.image)
                .subresource_range(color_range),
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS)
                .src_access_mask(vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS)
                .dst_access_mask(
                    vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE
                        | vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_READ,
                )
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .image(vp.depth.image)
                .subresource_range(depth_range),
        ];
        let extent = vp.extent;
        // SAFETY: `cmd` is recording outside a render pass; all handles belong to this device.
        unsafe {
            device
                .cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&barriers));
            let color = [vk::RenderingAttachmentInfo::default()
                .image_view(vp.color.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: view.clear } })];
            let depth = vk::RenderingAttachmentInfo::default()
                .image_view(vp.depth.view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 },
                });
            let rendering = vk::RenderingInfo::default()
                .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent })
                .layer_count(1)
                .color_attachments(&color)
                .depth_attachment(&depth);
            device.cmd_begin_rendering(cmd, &rendering);
            device.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_set_scissor(cmd, 0, &[vk::Rect2D { offset: vk::Offset2D::default(), extent }]);
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline.layout,
                1,
                &[self.joints[frame].set],
                &[],
            );
            // Parts without water bind the neutral extra set once.
            let default_extra = self.extra_sets.get(&self.default_extra).copied();
            let mut bound_extra = vk::DescriptorSet::null();
            // Opaque parts first, then blended parts in submission order.
            let mut bound = vk::Pipeline::null();
            // Materials state winding as the original renderer sees it; a view whose linear part has a positive
            // determinant (in Vulkan clip space) shows it the other way round.
            let flip = (det3(&view.view_proj) > 0.0) != view.mirrored;
            for blended in [false, true] {
                for (di, d) in view.draws.iter().enumerate() {
                    let Some(mesh) = self.meshes.get(&d.mesh.0) else { continue };
                    device.cmd_bind_vertex_buffers(cmd, 0, &[mesh.vertices.buffer], &[0]);
                    device.cmd_bind_index_buffer(cmd, mesh.indices.buffer, 0, vk::IndexType::UINT32);
                    let m = d.model;
                    // A mirrored model matrix (some maps are exported that way) turns its triangles round too.
                    let flip = flip != (det3(&m) < 0.0);
                    for (pi, (part, set, extra)) in mesh.parts.iter().enumerate() {
                        if d.part.is_some_and(|p| p != pi)
                            || part.index_count == 0
                            || part.material.blend.is_some() != blended
                        {
                            continue;
                        }
                        let mat = &part.material;
                        let Some(&pipeline) = self.pipeline.pipelines.get(&PipelineKey::from(mat)) else {
                            continue;
                        };
                        if pipeline != bound {
                            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
                            bound = pipeline;
                        }
                        let cull = match (view.culling, mat.cull) {
                            (false, _) | (_, CullMode::None) => vk::CullModeFlags::NONE,
                            (true, CullMode::Front) => vk::CullModeFlags::FRONT,
                            (true, CullMode::Back) => vk::CullModeFlags::BACK,
                        };
                        device.cmd_set_cull_mode(cmd, cull);
                        device.cmd_set_front_face(
                            cmd,
                            if mat.front_ccw != flip {
                                vk::FrontFace::COUNTER_CLOCKWISE
                            } else {
                                vk::FrontFace::CLOCKWISE
                            },
                        );
                        device.cmd_set_depth_test_enable(cmd, mat.depth_test);
                        device.cmd_set_depth_write_enable(cmd, mat.depth_write);
                        device.cmd_set_depth_compare_op(cmd, mat.depth_compare.vk());
                        let push = ScenePush {
                            view_proj: view.view_proj,
                            model: [
                                [m[0], m[4], m[8], m[12]],
                                [m[1], m[5], m[9], m[13]],
                                [m[2], m[6], m[10], m[14]],
                            ],
                            // Field parts (never skinned) take their texture scroll in the joint slots; water
                            // parts their constant slot.
                            params: if let Some(&slot) = water_slot.get(&(di, pi)) {
                                [slot, 0.0, part.alpha_cutoff, 4.0]
                            } else if part.curve != ToneCurve::None && !d.lit {
                                [
                                    (part.uv_scroll[0] * view.time).fract(),
                                    (part.uv_scroll[1] * view.time).fract(),
                                    part.alpha_cutoff,
                                    part.curve.code(),
                                ]
                            } else {
                                [
                                    d.joint_base.unwrap_or(0) as f32,
                                    if d.joint_base.is_some() { 1.0 } else { 0.0 },
                                    part.alpha_cutoff,
                                    if d.lit { 1.0 } else { part.curve.code() },
                                ]
                            },
                        };
                        let bytes = std::slice::from_raw_parts(
                            (&push as *const ScenePush).cast::<u8>(),
                            size_of::<ScenePush>(),
                        );
                        device.cmd_push_constants(
                            cmd,
                            self.pipeline.layout,
                            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                            0,
                            bytes,
                        );
                        device.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.pipeline.layout,
                            0,
                            &[*set],
                            &[],
                        );
                        let extra = if part.water.is_some() { Some(*extra) } else { default_extra };
                        if let Some(e) = extra
                            && e != bound_extra
                        {
                            device.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::GRAPHICS,
                                self.pipeline.layout,
                                2,
                                &[e],
                                &[],
                            );
                            bound_extra = e;
                        }
                        device.cmd_draw_indexed(cmd, part.index_count, 1, part.first_index, 0, 0);
                    }
                }
            }
            device.cmd_end_rendering(cmd);
            let to_read = [vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image(vp.color.image)
                .subresource_range(color_range)];
            device.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&to_read));
        }
        vp.color_layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        Ok(())
    }

    /// # Safety
    /// The device must be idle.
    pub unsafe fn destroy(&mut self, ctx: &Context) {
        let ids: Vec<u64> = self.meshes.keys().copied().collect();
        for id in ids {
            self.destroy_mesh(ctx, MeshHandle(id));
        }
        for mut j in self.joints.drain(..) {
            // SAFETY: the device is idle.
            unsafe { j.buffer.destroy(ctx) };
        }
        if let Some(mut vp) = self.viewport.take() {
            // SAFETY: the device is idle.
            unsafe {
                vp.color.destroy(ctx);
                vp.depth.destroy(ctx);
            }
        }
        // SAFETY: the device is idle; sets are freed with the pool.
        unsafe {
            ctx.device.destroy_sampler(self.sampler, None);
            ctx.device.destroy_descriptor_pool(self.pool, None);
            self.pipeline.destroy(&ctx.device);
        }
    }
}

/// The parts of [`SceneState`] needed to allocate joint slots while other fields are borrowed.
struct SceneStateRef<'a> {
    pipeline: &'a ScenePipeline,
    pool: vk::DescriptorPool,
}

impl SceneStateRef<'_> {
    fn joint_slot(&self, ctx: &Context, matrices: usize) -> Result<JointSlot> {
        let mut buffer = Buffer::new(
            ctx,
            (matrices * 64) as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            MemoryLocation::CpuToGpu,
        )?;
        if let Some(m) = buffer.mapped() {
            for chunk in m.chunks_exact_mut(64) {
                for (k, f) in IDENTITY.iter().enumerate() {
                    chunk[k * 4..k * 4 + 4].copy_from_slice(&f.to_le_bytes());
                }
            }
        }
        let layouts = [self.pipeline.joints_layout];
        // SAFETY: valid pool and layout.
        let set = unsafe {
            ctx.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.pool).set_layouts(&layouts),
            )
        }?[0];
        let info =
            [vk::DescriptorBufferInfo::default().buffer(buffer.buffer).offset(0).range(vk::WHOLE_SIZE)];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&info)];
        // SAFETY: the set is new.
        unsafe { ctx.device.update_descriptor_sets(&write, &[]) };
        Ok(JointSlot { buffer, set })
    }
}

/// The eye of a perspective view-projection (column-major): the point where clip x, y and w are all zero.
/// Zero when the matrix has none (an orthographic projection).
fn eye_of(m: &[f32; 16]) -> [f32; 3] {
    let row = |r: usize| [m[r], m[4 + r], m[8 + r], m[12 + r]];
    let (a, b, c) = (row(0), row(1), row(3));
    // Solve [a; b; c]·e = −[a.w; b.w; c.w] by Cramer's rule.
    let det = |x: [f32; 3], y: [f32; 3], z: [f32; 3]| {
        x[0] * (y[1] * z[2] - y[2] * z[1]) - x[1] * (y[0] * z[2] - y[2] * z[0])
            + x[2] * (y[0] * z[1] - y[1] * z[0])
    };
    let (r0, r1, r2) = ([a[0], a[1], a[2]], [b[0], b[1], b[2]], [c[0], c[1], c[2]]);
    let d = det(r0, r1, r2);
    if d.abs() < 1e-12 {
        return [0.0; 3];
    }
    let rhs = [-a[3], -b[3], -c[3]];
    let col = |i: usize| {
        let mut m = [r0, r1, r2];
        for (k, row) in m.iter_mut().enumerate() {
            row[i] = rhs[k];
        }
        det(m[0], m[1], m[2]) / d
    };
    [col(0), col(1), col(2)]
}

/// Determinant of the upper-left 3×3 of a column-major 4×4 matrix.
fn det3(m: &[f32; 16]) -> f32 {
    let (a, b, c) = ([m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]);
    a[0] * (b[1] * c[2] - b[2] * c[1]) - b[0] * (a[1] * c[2] - a[2] * c[1])
        + c[0] * (a[1] * b[2] - a[2] * b[1])
}

pub const IDENTITY: [f32; 16] =
    [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
