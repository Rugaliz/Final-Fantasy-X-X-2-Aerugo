//! Drawing with the games' own shaders: programs translated from their Direct3D 11 bytecode (see
//! `aerugo-dxbc`) and drawn as Direct3D 11 would draw them. A frame is a list of operations in the order the
//! engine issues them (set render targets, clear, draw, resolve, copy), with Direct3D 11's state descriptions,
//! formats (DXGI numbers) and sampler descriptions, so a frame can be compared draw for draw with the
//! original's.
//!
//! Conventions as DXVK's: one descriptor set per stage (vertex 0, pixel 1) with constant buffer `b<n>` at
//! binding n, sampler `s<n>` at 16 + n and shader resource `t<n>` at 32 + n; vertex inputs at their register's
//! location; Direct3D's clip space through a viewport of negative height (which keeps Direct3D's winding); the
//! pixel shader's sample count as specialization constant 0.

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow};
use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};

use crate::context::Context;
use crate::scene::Buffer;

/// First sampler binding in a stage's set.
pub const BINDING_SAMPLER: u32 = 16;
/// First shader-resource binding in a stage's set.
pub const BINDING_SRV: u32 = 32;

/// A shader stage of a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Pixel,
}

impl Stage {
    fn set(self) -> usize {
        match self {
            Stage::Vertex => 0,
            Stage::Pixel => 1,
        }
    }

    fn vk(self) -> vk::ShaderStageFlags {
        match self {
            Stage::Vertex => vk::ShaderStageFlags::VERTEX,
            Stage::Pixel => vk::ShaderStageFlags::FRAGMENT,
        }
    }
}

/// What a descriptor binding of a program holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingKind {
    ConstantBuffer,
    Sampler,
    Texture,
}

/// One vertex input: the register (location), the input slot it reads, its DXGI format and byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexAttribute {
    pub location: u32,
    pub slot: u32,
    pub dxgi_format: u32,
    pub offset: u32,
}

/// A program: translated vertex and pixel shaders, their bindings and the vertex layout.
#[derive(Debug, Clone)]
pub struct ProgramDesc {
    pub vertex: Vec<u32>,
    pub pixel: Vec<u32>,
    pub bindings: Vec<(Stage, u32, BindingKind)>,
    pub attributes: Vec<VertexAttribute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramHandle(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferHandle(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetHandle(u32);

/// `D3D11_RASTERIZER_DESC` (floats as bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RasterDesc {
    /// 2 wireframe, 3 solid.
    pub fill: u32,
    /// 1 none, 2 front, 3 back.
    pub cull: u32,
    pub front_ccw: bool,
    pub depth_bias: i32,
    pub depth_bias_clamp: u32,
    pub slope_scaled_depth_bias: u32,
    pub depth_clip: bool,
    pub scissor: bool,
    pub multisample: bool,
}

impl Default for RasterDesc {
    fn default() -> RasterDesc {
        RasterDesc {
            fill: 3,
            cull: 3,
            front_ccw: false,
            depth_bias: 0,
            depth_bias_clamp: 0,
            slope_scaled_depth_bias: 0,
            depth_clip: true,
            scissor: false,
            multisample: false,
        }
    }
}

/// `D3D11_DEPTH_STENCILOP_DESC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StencilOps {
    pub fail: u32,
    pub depth_fail: u32,
    pub pass: u32,
    pub func: u32,
}

impl Default for StencilOps {
    fn default() -> StencilOps {
        StencilOps { fail: 1, depth_fail: 1, pass: 1, func: 8 }
    }
}

/// `D3D11_DEPTH_STENCIL_DESC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepthDesc {
    pub enable: bool,
    pub write: bool,
    /// `D3D11_COMPARISON_FUNC` (1 never … 8 always).
    pub func: u32,
    pub stencil: bool,
    pub read_mask: u8,
    pub write_mask: u8,
    pub front: StencilOps,
    pub back: StencilOps,
}

impl Default for DepthDesc {
    fn default() -> DepthDesc {
        DepthDesc {
            enable: true,
            write: true,
            func: 2,
            stencil: false,
            read_mask: 0xFF,
            write_mask: 0xFF,
            front: StencilOps::default(),
            back: StencilOps::default(),
        }
    }
}

/// `D3D11_RENDER_TARGET_BLEND_DESC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetBlend {
    pub enable: bool,
    pub src: u32,
    pub dst: u32,
    pub op: u32,
    pub src_alpha: u32,
    pub dst_alpha: u32,
    pub op_alpha: u32,
    pub write_mask: u8,
}

impl Default for TargetBlend {
    fn default() -> TargetBlend {
        TargetBlend {
            enable: false,
            src: 2,
            dst: 1,
            op: 1,
            src_alpha: 2,
            dst_alpha: 1,
            op_alpha: 1,
            write_mask: 0xF,
        }
    }
}

/// `D3D11_BLEND_DESC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlendDesc {
    pub alpha_to_coverage: bool,
    pub independent: bool,
    pub targets: [TargetBlend; 8],
}

/// Fixed-function state of a draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RenderState {
    pub raster: RasterDesc,
    pub depth: DepthDesc,
    pub blend: BlendDesc,
}

/// `D3D11_SAMPLER_DESC` (floats as bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerDesc {
    /// `D3D11_FILTER`.
    pub filter: u32,
    /// `D3D11_TEXTURE_ADDRESS_MODE` (1 wrap, 2 mirror, 3 clamp, 4 border, 5 mirror once).
    pub address: [u32; 3],
    pub mip_lod_bias: u32,
    pub max_anisotropy: u32,
    pub comparison: u32,
    pub border: [u32; 4],
    pub min_lod: u32,
    pub max_lod: u32,
}

impl Default for SamplerDesc {
    /// Direct3D 11's default sampler state.
    fn default() -> SamplerDesc {
        SamplerDesc {
            filter: 0x15,
            address: [3; 3],
            mip_lod_bias: 0,
            max_anisotropy: 1,
            comparison: 8,
            border: [1f32.to_bits(); 4],
            min_lod: (-f32::MAX).to_bits(),
            max_lod: f32::MAX.to_bits(),
        }
    }
}

/// An image a draw samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageRef {
    Texture(crate::renderer::TextureHandle),
    Target(TargetHandle),
}

/// A render target: one of ours, or the frame's output (the viewport image).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetRef {
    Target(TargetHandle),
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexBinding {
    pub slot: u32,
    pub buffer: BufferHandle,
    pub offset: u64,
    pub stride: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndexBinding {
    pub buffer: BufferHandle,
    pub offset: u64,
    /// 32-bit indices (else 16-bit).
    pub wide: bool,
}

/// One draw call.
#[derive(Debug, Clone)]
pub struct GameDraw {
    pub program: ProgramHandle,
    pub state: RenderState,
    /// `D3D11_PRIMITIVE_TOPOLOGY` (4 triangle list, 5 triangle strip, …).
    pub topology: u32,
    pub vertex_buffers: Vec<VertexBinding>,
    pub index: Option<IndexBinding>,
    pub count: u32,
    pub first: u32,
    pub base_vertex: i32,
    pub instances: u32,
    /// Constant buffers: stage, register, contents.
    pub constants: Vec<(Stage, u32, Vec<u8>)>,
    pub images: Vec<(Stage, u32, ImageRef)>,
    pub samplers: Vec<(Stage, u32, SamplerDesc)>,
    /// `D3D11_VIEWPORT`: x, y, width, height, min depth, max depth.
    pub viewport: [f32; 6],
    pub blend_factor: [f32; 4],
    pub sample_mask: u32,
    pub stencil_ref: u32,
}

/// One operation of a frame, in the order the engine issues it.
#[derive(Debug, Clone)]
pub enum GameOp {
    SetTargets {
        colors: Vec<TargetRef>,
        depth: Option<TargetHandle>,
    },
    ClearColor(TargetRef, [f32; 4]),
    /// Depth, stencil, flags (1 depth, 2 stencil).
    ClearDepth(TargetHandle, f32, u8, u32),
    Draw(Box<GameDraw>),
    /// A multisampled target resolved into a single-sampled one.
    Resolve {
        src: TargetHandle,
        dst: TargetRef,
    },
    Copy {
        src: TargetHandle,
        dst: TargetRef,
    },
}

/// A frame: the operations and the size of the output.
#[derive(Debug, Clone, Default)]
pub struct GameFrame {
    pub ops: Vec<GameOp>,
}

/// A render target's description (DXGI format, size, samples).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetDesc {
    pub width: u32,
    pub height: u32,
    pub dxgi_format: u32,
    pub samples: u32,
}

/// Vulkan format of a DXGI format (render targets, vertex inputs and views).
pub fn vk_format(dxgi: u32, depth_fallback: bool) -> Option<vk::Format> {
    use vk::Format as F;
    Some(match dxgi {
        2 => F::R32G32B32A32_SFLOAT,
        3 => F::R32G32B32A32_UINT,
        4 => F::R32G32B32A32_SINT,
        6 => F::R32G32B32_SFLOAT,
        7 => F::R32G32B32_UINT,
        10 => F::R16G16B16A16_SFLOAT,
        11 => F::R16G16B16A16_UNORM,
        12 => F::R16G16B16A16_UINT,
        13 => F::R16G16B16A16_SNORM,
        14 => F::R16G16B16A16_SINT,
        16 => F::R32G32_SFLOAT,
        17 => F::R32G32_UINT,
        24 => F::A2B10G10R10_UNORM_PACK32,
        26 => F::B10G11R11_UFLOAT_PACK32,
        28 => F::R8G8B8A8_UNORM,
        29 => F::R8G8B8A8_SRGB,
        30 => F::R8G8B8A8_UINT,
        31 => F::R8G8B8A8_SNORM,
        32 => F::R8G8B8A8_SINT,
        34 => F::R16G16_SFLOAT,
        35 => F::R16G16_UNORM,
        36 => F::R16G16_UINT,
        37 => F::R16G16_SNORM,
        38 => F::R16G16_SINT,
        40 => F::D32_SFLOAT,
        41 => F::R32_SFLOAT,
        42 => F::R32_UINT,
        43 => F::R32_SINT,
        44..=47 => {
            if depth_fallback {
                F::D32_SFLOAT_S8_UINT
            } else {
                F::D24_UNORM_S8_UINT
            }
        }
        19..=22 => F::D32_SFLOAT_S8_UINT,
        49 => F::R8G8_UNORM,
        50 => F::R8G8_UINT,
        51 => F::R8G8_SNORM,
        54 => F::R16_SFLOAT,
        55 => F::D16_UNORM,
        56 => F::R16_UNORM,
        57 => F::R16_UINT,
        61 => F::R8_UNORM,
        62 => F::R8_UINT,
        65 => F::R8_UNORM,
        87 => F::B8G8R8A8_UNORM,
        88 => F::B8G8R8A8_UNORM,
        91 => F::B8G8R8A8_SRGB,
        _ => return None,
    })
}

fn is_depth(f: vk::Format) -> bool {
    matches!(
        f,
        vk::Format::D32_SFLOAT
            | vk::Format::D24_UNORM_S8_UINT
            | vk::Format::D32_SFLOAT_S8_UINT
            | vk::Format::D16_UNORM
    )
}

fn has_stencil(f: vk::Format) -> bool {
    matches!(f, vk::Format::D24_UNORM_S8_UINT | vk::Format::D32_SFLOAT_S8_UINT)
}

fn compare(func: u32) -> vk::CompareOp {
    match func {
        1 => vk::CompareOp::NEVER,
        2 => vk::CompareOp::LESS,
        3 => vk::CompareOp::EQUAL,
        4 => vk::CompareOp::LESS_OR_EQUAL,
        5 => vk::CompareOp::GREATER,
        6 => vk::CompareOp::NOT_EQUAL,
        7 => vk::CompareOp::GREATER_OR_EQUAL,
        _ => vk::CompareOp::ALWAYS,
    }
}

fn stencil_op(op: u32) -> vk::StencilOp {
    match op {
        2 => vk::StencilOp::ZERO,
        3 => vk::StencilOp::REPLACE,
        4 => vk::StencilOp::INCREMENT_AND_CLAMP,
        5 => vk::StencilOp::DECREMENT_AND_CLAMP,
        6 => vk::StencilOp::INVERT,
        7 => vk::StencilOp::INCREMENT_AND_WRAP,
        8 => vk::StencilOp::DECREMENT_AND_WRAP,
        _ => vk::StencilOp::KEEP,
    }
}

fn blend_factor(b: u32) -> vk::BlendFactor {
    use vk::BlendFactor as B;
    match b {
        1 => B::ZERO,
        3 => B::SRC_COLOR,
        4 => B::ONE_MINUS_SRC_COLOR,
        5 => B::SRC_ALPHA,
        6 => B::ONE_MINUS_SRC_ALPHA,
        7 => B::DST_ALPHA,
        8 => B::ONE_MINUS_DST_ALPHA,
        9 => B::DST_COLOR,
        10 => B::ONE_MINUS_DST_COLOR,
        11 => B::SRC_ALPHA_SATURATE,
        14 => B::CONSTANT_COLOR,
        15 => B::ONE_MINUS_CONSTANT_COLOR,
        16 => B::SRC1_COLOR,
        17 => B::ONE_MINUS_SRC1_COLOR,
        18 => B::SRC1_ALPHA,
        19 => B::ONE_MINUS_SRC1_ALPHA,
        _ => B::ONE,
    }
}

/// Alpha factors: Direct3D's colour factors read the alpha channel there.
fn blend_factor_alpha(b: u32) -> vk::BlendFactor {
    use vk::BlendFactor as B;
    match blend_factor(b) {
        B::SRC_COLOR => B::SRC_ALPHA,
        B::ONE_MINUS_SRC_COLOR => B::ONE_MINUS_SRC_ALPHA,
        B::DST_COLOR => B::DST_ALPHA,
        B::ONE_MINUS_DST_COLOR => B::ONE_MINUS_DST_ALPHA,
        B::CONSTANT_COLOR => B::CONSTANT_ALPHA,
        B::ONE_MINUS_CONSTANT_COLOR => B::ONE_MINUS_CONSTANT_ALPHA,
        B::SRC1_COLOR => B::SRC1_ALPHA,
        B::ONE_MINUS_SRC1_COLOR => B::ONE_MINUS_SRC1_ALPHA,
        f => f,
    }
}

fn blend_op(op: u32) -> vk::BlendOp {
    match op {
        2 => vk::BlendOp::SUBTRACT,
        3 => vk::BlendOp::REVERSE_SUBTRACT,
        4 => vk::BlendOp::MIN,
        5 => vk::BlendOp::MAX,
        _ => vk::BlendOp::ADD,
    }
}

fn address(a: u32) -> vk::SamplerAddressMode {
    match a {
        1 => vk::SamplerAddressMode::REPEAT,
        2 => vk::SamplerAddressMode::MIRRORED_REPEAT,
        4 => vk::SamplerAddressMode::CLAMP_TO_BORDER,
        5 => vk::SamplerAddressMode::MIRROR_CLAMP_TO_EDGE,
        _ => vk::SamplerAddressMode::CLAMP_TO_EDGE,
    }
}

/// Topology class for the pipeline and the exact topology (set dynamically).
fn topology(t: u32) -> (vk::PrimitiveTopology, vk::PrimitiveTopology) {
    use vk::PrimitiveTopology as T;
    let exact = match t {
        1 => T::POINT_LIST,
        2 => T::LINE_LIST,
        3 => T::LINE_STRIP,
        5 => T::TRIANGLE_STRIP,
        _ => T::TRIANGLE_LIST,
    };
    let class = match exact {
        T::POINT_LIST => T::POINT_LIST,
        T::LINE_LIST | T::LINE_STRIP => T::LINE_LIST,
        _ => T::TRIANGLE_LIST,
    };
    (class, exact)
}

#[derive(Debug)]
struct Program {
    vertex: vk::ShaderModule,
    pixel: vk::ShaderModule,
    set_layouts: [vk::DescriptorSetLayout; 2],
    layout: vk::PipelineLayout,
    bindings: Vec<(Stage, u32, BindingKind)>,
    attributes: Vec<VertexAttribute>,
    /// Input slots the vertex shader reads.
    slots: Vec<u32>,
}

#[derive(Debug)]
struct Image {
    image: vk::Image,
    /// View for attachments (all aspects).
    view: vk::ImageView,
    /// View for sampling (depth aspect only for depth formats).
    sample_view: vk::ImageView,
    format: vk::Format,
    extent: vk::Extent2D,
    samples: vk::SampleCountFlags,
    allocation: Option<Allocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PipelineKey {
    program: ProgramHandle,
    fill: u32,
    depth_bias: (i32, u32, u32),
    depth_clip: bool,
    depth: (bool, u32, u8, u8, StencilOps, StencilOps),
    blend: BlendDesc,
    colors: Vec<vk::Format>,
    depth_format: vk::Format,
    samples: vk::SampleCountFlags,
    class: vk::PrimitiveTopology,
    sample_mask: u32,
}

#[derive(Debug, Default)]
struct FrameRes {
    pool: vk::DescriptorPool,
    uniforms: Option<Buffer>,
}

const FRAME_SETS: u32 = 4096;

/// Everything kept for drawing with the games' shaders.
#[derive(Debug, Default)]
pub(crate) struct GameState {
    programs: HashMap<u32, Program>,
    buffers: HashMap<u32, Buffer>,
    targets: HashMap<u32, Image>,
    next: u32,
    pipelines: HashMap<PipelineKey, vk::Pipeline>,
    samplers: HashMap<SamplerDesc, vk::Sampler>,
    layouts: HashMap<vk::Image, vk::ImageLayout>,
    frames: Vec<FrameRes>,
    zero: Option<Buffer>,
    uniform_align: u64,
    depth_fallback: bool,
    pub pending: Option<GameFrame>,
}

/// What the renderer gives a frame to record into: the output image and the textures.
pub(crate) struct Output<'a> {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    /// The output's layout before the frame (it ends in shader-read layout).
    pub layout: vk::ImageLayout,
    pub textures: &'a dyn Fn(crate::renderer::TextureHandle) -> Option<(vk::Image, vk::ImageView)>,
}

impl GameState {
    fn init(&mut self, ctx: &Context) -> Result<()> {
        if self.uniform_align != 0 {
            return Ok(());
        }
        // SAFETY: valid physical device.
        let props = unsafe { ctx.instance.get_physical_device_properties(ctx.physical_device) };
        self.uniform_align = props.limits.min_uniform_buffer_offset_alignment.max(16);
        // SAFETY: as above.
        let d24 = unsafe {
            ctx.instance
                .get_physical_device_format_properties(ctx.physical_device, vk::Format::D24_UNORM_S8_UINT)
        };
        self.depth_fallback =
            !d24.optimal_tiling_features.contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT);
        let mut zero = Buffer::new(ctx, 256, vk::BufferUsageFlags::VERTEX_BUFFER, MemoryLocation::CpuToGpu)?;
        if let Some(m) = zero.mapped() {
            m.fill(0);
        }
        self.zero = Some(zero);
        self.next = 1;
        Ok(())
    }

    pub fn create_program(&mut self, ctx: &Context, desc: &ProgramDesc) -> Result<ProgramHandle> {
        self.init(ctx)?;
        let device = &ctx.device;
        let module = |code: &[u32]| {
            // SAFETY: valid device; the code is SPIR-V from the translator.
            unsafe { device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(code), None) }
        };
        let vertex = module(&desc.vertex)?;
        let pixel = module(&desc.pixel)?;
        let mut set_layouts = [vk::DescriptorSetLayout::null(); 2];
        for stage in [Stage::Vertex, Stage::Pixel] {
            let bindings: Vec<_> = desc
                .bindings
                .iter()
                .filter(|b| b.0 == stage)
                .map(|&(_, binding, kind)| {
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(binding)
                        .descriptor_type(match kind {
                            BindingKind::ConstantBuffer => vk::DescriptorType::UNIFORM_BUFFER,
                            BindingKind::Sampler => vk::DescriptorType::SAMPLER,
                            BindingKind::Texture => vk::DescriptorType::SAMPLED_IMAGE,
                        })
                        .descriptor_count(1)
                        .stage_flags(stage.vk())
                })
                .collect();
            // SAFETY: valid device and bindings.
            set_layouts[stage.set()] = unsafe {
                device.create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
            }?;
        }
        // SAFETY: valid set layouts.
        let layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts),
                None,
            )
        }?;
        let mut slots: Vec<u32> = desc.attributes.iter().map(|a| a.slot).collect();
        slots.sort_unstable();
        slots.dedup();
        let id = self.next;
        self.next += 1;
        self.programs.insert(
            id,
            Program {
                vertex,
                pixel,
                set_layouts,
                layout,
                bindings: desc.bindings.clone(),
                attributes: desc.attributes.clone(),
                slots,
            },
        );
        Ok(ProgramHandle(id))
    }

    pub fn create_buffer(&mut self, ctx: &Context, data: &[u8], index: bool) -> Result<BufferHandle> {
        self.init(ctx)?;
        let usage =
            if index { vk::BufferUsageFlags::INDEX_BUFFER } else { vk::BufferUsageFlags::VERTEX_BUFFER };
        let b = Buffer::with_data(ctx, usage, data)?;
        let id = self.next;
        self.next += 1;
        self.buffers.insert(id, b);
        Ok(BufferHandle(id))
    }

    /// The device must be idle.
    pub fn destroy_buffer(&mut self, ctx: &Context, h: BufferHandle) {
        if let Some(mut b) = self.buffers.remove(&h.0) {
            // SAFETY: the caller waited for the device.
            unsafe { b.destroy(ctx) };
        }
    }

    pub fn create_target(&mut self, ctx: &Context, desc: &TargetDesc) -> Result<TargetHandle> {
        self.init(ctx)?;
        let format = vk_format(desc.dxgi_format, self.depth_fallback)
            .ok_or_else(|| anyhow!("render target format {} not supported", desc.dxgi_format))?;
        let depth = is_depth(format);
        let samples = vk::SampleCountFlags::from_raw(desc.samples.max(1));
        let usage = vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST
            | if depth {
                vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
            } else {
                vk::ImageUsageFlags::COLOR_ATTACHMENT
            };
        let extent = vk::Extent2D { width: desc.width.max(1), height: desc.height.max(1) };
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let device = &ctx.device;
        // SAFETY: valid device and create info.
        let image = unsafe { device.create_image(&info, None) }?;
        // SAFETY: the image was just created.
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let alloc = ctx.allocator().lock().expect("allocator lock").allocate(&AllocationCreateDesc {
            name: "game target",
            requirements,
            location: MemoryLocation::GpuOnly,
            linear: false,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        })?;
        // SAFETY: memory satisfies the requirements and is bound once.
        unsafe { device.bind_image_memory(image, alloc.memory(), alloc.offset()) }?;
        let all = if depth {
            vk::ImageAspectFlags::DEPTH
                | if has_stencil(format) {
                    vk::ImageAspectFlags::STENCIL
                } else {
                    vk::ImageAspectFlags::empty()
                }
        } else {
            vk::ImageAspectFlags::COLOR
        };
        let view_info = |aspect| {
            vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
        };
        // SAFETY: the image is bound.
        let view = unsafe { device.create_image_view(&view_info(all), None) }?;
        let sample_aspect = if depth { vk::ImageAspectFlags::DEPTH } else { vk::ImageAspectFlags::COLOR };
        // SAFETY: as above.
        let sample_view = unsafe { device.create_image_view(&view_info(sample_aspect), None) }?;
        let id = self.next;
        self.next += 1;
        self.targets
            .insert(id, Image { image, view, sample_view, format, extent, samples, allocation: Some(alloc) });
        Ok(TargetHandle(id))
    }

    /// The device must be idle.
    pub fn destroy_target(&mut self, ctx: &Context, h: TargetHandle) {
        if let Some(mut t) = self.targets.remove(&h.0) {
            self.layouts.remove(&t.image);
            // SAFETY: the caller waited for the device.
            unsafe {
                ctx.device.destroy_image_view(t.sample_view, None);
                ctx.device.destroy_image_view(t.view, None);
                ctx.device.destroy_image(t.image, None);
            }
            if let Some(a) = t.allocation.take() {
                ctx.allocator().lock().expect("allocator lock").free(a).ok();
            }
        }
    }

    fn sampler(&mut self, ctx: &Context, d: &SamplerDesc) -> Result<vk::Sampler> {
        if let Some(&s) = self.samplers.get(d) {
            return Ok(s);
        }
        let f = d.filter;
        let anisotropic = f & 0x40 != 0;
        let linear =
            |bit: u32| if f & bit != 0 || anisotropic { vk::Filter::LINEAR } else { vk::Filter::NEAREST };
        let compare_on = f & 0x80 != 0;
        let border = d.border.map(f32::from_bits);
        let border_color = if border[3] == 0.0 {
            vk::BorderColor::FLOAT_TRANSPARENT_BLACK
        } else if border[0] == 0.0 {
            vk::BorderColor::FLOAT_OPAQUE_BLACK
        } else {
            vk::BorderColor::FLOAT_OPAQUE_WHITE
        };
        let max_aniso = ctx.max_anisotropy;
        let info = vk::SamplerCreateInfo::default()
            .mag_filter(linear(0x4))
            .min_filter(linear(0x10))
            .mipmap_mode(if f & 0x1 != 0 || anisotropic {
                vk::SamplerMipmapMode::LINEAR
            } else {
                vk::SamplerMipmapMode::NEAREST
            })
            .address_mode_u(address(d.address[0]))
            .address_mode_v(address(d.address[1]))
            .address_mode_w(address(d.address[2]))
            .mip_lod_bias(f32::from_bits(d.mip_lod_bias))
            .anisotropy_enable(anisotropic && max_aniso > 1.0)
            .max_anisotropy((d.max_anisotropy as f32).clamp(1.0, max_aniso.max(1.0)))
            .compare_enable(compare_on)
            .compare_op(compare(d.comparison))
            .min_lod(f32::from_bits(d.min_lod).max(0.0))
            .max_lod(f32::from_bits(d.max_lod).min(vk::LOD_CLAMP_NONE))
            .border_color(border_color);
        // SAFETY: valid device and create info.
        let s = unsafe { ctx.device.create_sampler(&info, None) }?;
        self.samplers.insert(*d, s);
        Ok(s)
    }

    fn pipeline(&mut self, ctx: &Context, key: &PipelineKey) -> Result<vk::Pipeline> {
        if let Some(&p) = self.pipelines.get(key) {
            return Ok(p);
        }
        let prog = self.programs.get(&key.program.0).context("unknown program")?;
        let entry = c"main";
        let samples_spec = [key.samples.as_raw()];
        let spec_entries = [vk::SpecializationMapEntry { constant_id: 0, offset: 0, size: 4 }];
        let spec =
            vk::SpecializationInfo::default().map_entries(&spec_entries).data(bytemuck_u32(&samples_spec));
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(prog.vertex)
                .name(entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(prog.pixel)
                .name(entry)
                .specialization_info(&spec),
        ];
        let bindings: Vec<_> = prog
            .slots
            .iter()
            .map(|&s| {
                vk::VertexInputBindingDescription::default()
                    .binding(s)
                    .stride(0)
                    .input_rate(vk::VertexInputRate::VERTEX)
            })
            .collect();
        let attributes: Vec<_> = prog
            .attributes
            .iter()
            .filter_map(|a| {
                Some(
                    vk::VertexInputAttributeDescription::default()
                        .location(a.location)
                        .binding(a.slot)
                        .format(vk_format(a.dxgi_format, false)?)
                        .offset(a.offset),
                )
            })
            .collect();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(key.class);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(if key.fill == 2 { vk::PolygonMode::LINE } else { vk::PolygonMode::FILL })
            .depth_clamp_enable(!key.depth_clip)
            .depth_bias_enable(key.depth_bias.0 != 0 || key.depth_bias.2 != 0)
            .depth_bias_constant_factor(key.depth_bias.0 as f32)
            .depth_bias_clamp(f32::from_bits(key.depth_bias.1))
            .depth_bias_slope_factor(f32::from_bits(key.depth_bias.2))
            .line_width(1.0);
        let mask = [key.sample_mask];
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(key.samples)
            .sample_mask(&mask)
            .alpha_to_coverage_enable(key.blend.alpha_to_coverage);
        let (stencil, read, write, front, back) =
            (key.depth.0, key.depth.2, key.depth.3, key.depth.4, key.depth.5);
        let stencil_state = |o: StencilOps| {
            vk::StencilOpState::default()
                .fail_op(stencil_op(o.fail))
                .depth_fail_op(stencil_op(o.depth_fail))
                .pass_op(stencil_op(o.pass))
                .compare_op(compare(o.func))
                .compare_mask(u32::from(read))
                .write_mask(u32::from(write))
        };
        let depth = vk::PipelineDepthStencilStateCreateInfo::default()
            .stencil_test_enable(stencil)
            .front(stencil_state(front))
            .back(stencil_state(back));
        let blends: Vec<_> = (0..key.colors.len())
            .map(|i| {
                let t = if key.blend.independent { key.blend.targets[i] } else { key.blend.targets[0] };
                vk::PipelineColorBlendAttachmentState::default()
                    .blend_enable(t.enable)
                    .src_color_blend_factor(blend_factor(t.src))
                    .dst_color_blend_factor(blend_factor(t.dst))
                    .color_blend_op(blend_op(t.op))
                    .src_alpha_blend_factor(blend_factor_alpha(t.src_alpha))
                    .dst_alpha_blend_factor(blend_factor_alpha(t.dst_alpha))
                    .alpha_blend_op(blend_op(t.op_alpha))
                    .color_write_mask(vk::ColorComponentFlags::from_raw(u32::from(t.write_mask & 0xF)))
            })
            .collect();
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blends);
        let dynamic = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::CULL_MODE,
            vk::DynamicState::FRONT_FACE,
            vk::DynamicState::DEPTH_TEST_ENABLE,
            vk::DynamicState::DEPTH_WRITE_ENABLE,
            vk::DynamicState::DEPTH_COMPARE_OP,
            vk::DynamicState::STENCIL_REFERENCE,
            vk::DynamicState::BLEND_CONSTANTS,
            vk::DynamicState::VERTEX_INPUT_BINDING_STRIDE,
            vk::DynamicState::PRIMITIVE_TOPOLOGY,
            vk::DynamicState::PRIMITIVE_RESTART_ENABLE,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&key.colors)
            .depth_attachment_format(key.depth_format)
            .stencil_attachment_format(if has_stencil(key.depth_format) {
                key.depth_format
            } else {
                vk::Format::UNDEFINED
            });
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(prog.layout)
            .push_next(&mut rendering);
        // SAFETY: all referenced objects are valid for the call.
        let p = unsafe { ctx.device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }
            .map_err(|(_, e)| e)?[0];
        self.pipelines.insert(key.clone(), p);
        Ok(p)
    }

    fn image_of(
        &self,
        r: ImageRef,
        out: &Output<'_>,
    ) -> Option<(vk::Image, vk::ImageView, vk::ImageAspectFlags)> {
        match r {
            ImageRef::Texture(t) => (out.textures)(t).map(|(i, v)| (i, v, vk::ImageAspectFlags::COLOR)),
            ImageRef::Target(t) => self.targets.get(&t.0).map(|i| {
                let aspect = if is_depth(i.format) {
                    vk::ImageAspectFlags::DEPTH
                } else {
                    vk::ImageAspectFlags::COLOR
                };
                (i.image, i.sample_view, aspect)
            }),
        }
    }

    /// (image, attachment view, format, extent, samples) of a target.
    fn target_of(
        &self,
        r: TargetRef,
        out: &Output<'_>,
    ) -> Option<(vk::Image, vk::ImageView, vk::Format, vk::Extent2D, vk::SampleCountFlags)> {
        match r {
            TargetRef::Output => {
                Some((out.image, out.view, out.format, out.extent, vk::SampleCountFlags::TYPE_1))
            }
            TargetRef::Target(t) => {
                self.targets.get(&t.0).map(|i| (i.image, i.view, i.format, i.extent, i.samples))
            }
        }
    }

    /// Move `image` to `layout` (whole image), with a full barrier.
    fn transition(
        &mut self,
        ctx: &Context,
        cmd: vk::CommandBuffer,
        image: vk::Image,
        format: vk::Format,
        layout: vk::ImageLayout,
    ) {
        let old = self.layouts.get(&image).copied().unwrap_or(vk::ImageLayout::UNDEFINED);
        if old == layout
            && layout != vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
            && layout != vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
        {
            return;
        }
        let aspect = if is_depth(format) {
            vk::ImageAspectFlags::DEPTH
                | if has_stencil(format) {
                    vk::ImageAspectFlags::STENCIL
                } else {
                    vk::ImageAspectFlags::empty()
                }
        } else {
            vk::ImageAspectFlags::COLOR
        };
        let barrier = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(old)
            .new_layout(layout)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: aspect,
                base_mip_level: 0,
                level_count: vk::REMAINING_MIP_LEVELS,
                base_array_layer: 0,
                layer_count: vk::REMAINING_ARRAY_LAYERS,
            })];
        // SAFETY: recording outside rendering; the image belongs to this device.
        unsafe {
            ctx.device
                .cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&barrier))
        };
        self.layouts.insert(image, layout);
    }

    fn frame_res(&mut self, ctx: &Context, frame: usize, uniform_bytes: u64) -> Result<()> {
        while self.frames.len() <= frame {
            self.frames.push(FrameRes::default());
        }
        let device = &ctx.device;
        let res = &mut self.frames[frame];
        if res.pool == vk::DescriptorPool::null() {
            let sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: FRAME_SETS * 2,
                },
                vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: FRAME_SETS * 4 },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLED_IMAGE,
                    descriptor_count: FRAME_SETS * 4,
                },
            ];
            // SAFETY: valid device and create info.
            res.pool = unsafe {
                device.create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default().max_sets(FRAME_SETS).pool_sizes(&sizes),
                    None,
                )
            }?;
        } else {
            // SAFETY: this frame slot's previous submission has completed (the caller waited for its fence).
            unsafe { device.reset_descriptor_pool(res.pool, vk::DescriptorPoolResetFlags::empty()) }?;
        }
        if res.uniforms.as_ref().is_none_or(|b| b.size < uniform_bytes) {
            if let Some(mut old) = res.uniforms.take() {
                // SAFETY: as above, the slot's last use has completed.
                unsafe { old.destroy(ctx) };
            }
            res.uniforms = Some(Buffer::new(
                ctx,
                uniform_bytes.next_power_of_two().max(1 << 20),
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                MemoryLocation::CpuToGpu,
            )?);
        }
        Ok(())
    }

    /// Record the pending frame (outside any rendering). `frame` is the frame slot whose fence was waited.
    pub fn record(
        &mut self,
        ctx: &Context,
        cmd: vk::CommandBuffer,
        frame: usize,
        out: &Output<'_>,
    ) -> Result<bool> {
        let Some(game) = self.pending.take() else { return Ok(false) };
        self.init(ctx)?;
        let align = self.uniform_align;
        let uniform_bytes: u64 = game
            .ops
            .iter()
            .filter_map(|o| if let GameOp::Draw(d) = o { Some(d) } else { None })
            .flat_map(|d| d.constants.iter().map(|c| (c.2.len() as u64).div_ceil(align) * align))
            .sum();
        self.frame_res(ctx, frame, uniform_bytes.max(256))?;
        self.layouts.insert(out.image, out.layout);
        let device = ctx.device.clone();
        let mut uniform_at = 0u64;
        let mut colors: Vec<TargetRef> = Vec::new();
        let mut depth_target: Option<TargetHandle> = None;
        let mut rendering = false;
        let end = |rendering: &mut bool| {
            if *rendering {
                // SAFETY: a rendering instance begun below is active.
                unsafe { device.cmd_end_rendering(cmd) };
                *rendering = false;
            }
        };
        for op in &game.ops {
            match op {
                GameOp::SetTargets { colors: c, depth: d } => {
                    end(&mut rendering);
                    colors = c.clone();
                    depth_target = *d;
                }
                GameOp::ClearColor(t, c) => {
                    end(&mut rendering);
                    let Some((image, _, format, _, _)) = self.target_of(*t, out) else { continue };
                    self.transition(ctx, cmd, image, format, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
                    let range = vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    // SAFETY: the image is in transfer-destination layout.
                    unsafe {
                        device.cmd_clear_color_image(
                            cmd,
                            image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &vk::ClearColorValue { float32: *c },
                            &[range],
                        )
                    };
                }
                GameOp::ClearDepth(t, d, s, flags) => {
                    end(&mut rendering);
                    let Some((image, _, format, _, _)) = self.target_of(TargetRef::Target(*t), out) else {
                        continue;
                    };
                    self.transition(ctx, cmd, image, format, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
                    let mut aspect = vk::ImageAspectFlags::empty();
                    if flags & 1 != 0 {
                        aspect |= vk::ImageAspectFlags::DEPTH;
                    }
                    if flags & 2 != 0 && has_stencil(format) {
                        aspect |= vk::ImageAspectFlags::STENCIL;
                    }
                    if aspect.is_empty() {
                        continue;
                    }
                    let range = vk::ImageSubresourceRange {
                        aspect_mask: aspect,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    // SAFETY: the image is in transfer-destination layout.
                    unsafe {
                        device.cmd_clear_depth_stencil_image(
                            cmd,
                            image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &vk::ClearDepthStencilValue { depth: *d, stencil: u32::from(*s) },
                            &[range],
                        )
                    };
                }
                GameOp::Resolve { src, dst } | GameOp::Copy { src, dst } => {
                    end(&mut rendering);
                    let resolve = matches!(op, GameOp::Resolve { .. });
                    let (Some(s), Some(d)) =
                        (self.target_of(TargetRef::Target(*src), out), self.target_of(*dst, out))
                    else {
                        continue;
                    };
                    self.transition(ctx, cmd, s.0, s.2, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
                    self.transition(ctx, cmd, d.0, d.2, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
                    let layers = vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    let extent = vk::Extent3D {
                        width: s.3.width.min(d.3.width),
                        height: s.3.height.min(d.3.height),
                        depth: 1,
                    };
                    // SAFETY: both images are in the matching transfer layouts.
                    unsafe {
                        if resolve {
                            device.cmd_resolve_image(
                                cmd,
                                s.0,
                                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                                d.0,
                                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                                &[vk::ImageResolve::default()
                                    .src_subresource(layers)
                                    .dst_subresource(layers)
                                    .extent(extent)],
                            );
                        } else {
                            device.cmd_copy_image(
                                cmd,
                                s.0,
                                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                                d.0,
                                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                                &[vk::ImageCopy::default()
                                    .src_subresource(layers)
                                    .dst_subresource(layers)
                                    .extent(extent)],
                            );
                        }
                    }
                }
                GameOp::Draw(draw) => {
                    // Images the draw samples must be readable before rendering starts.
                    let mut reads = Vec::new();
                    for &(_, _, r) in &draw.images {
                        if let Some((image, _, _)) = self.image_of(r, out) {
                            let format = match r {
                                ImageRef::Target(t) => self.targets.get(&t.0).map(|i| i.format),
                                ImageRef::Texture(_) => None,
                            };
                            if let Some(format) = format
                                && self.layouts.get(&image).copied()
                                    != Some(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            {
                                reads.push((image, format));
                            }
                        }
                    }
                    if !reads.is_empty() {
                        end(&mut rendering);
                        for (image, format) in reads {
                            self.transition(
                                ctx,
                                cmd,
                                image,
                                format,
                                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                            );
                        }
                    }
                    let mut color_info = Vec::new();
                    for &c in &colors {
                        if let Some(t) = self.target_of(c, out) {
                            color_info.push(t);
                        }
                    }
                    let depth_info = depth_target.and_then(|d| self.target_of(TargetRef::Target(d), out));
                    let extent =
                        color_info.first().or(depth_info.as_ref()).map(|t| t.3).unwrap_or(out.extent);
                    let samples = color_info
                        .first()
                        .or(depth_info.as_ref())
                        .map(|t| t.4)
                        .unwrap_or(vk::SampleCountFlags::TYPE_1);
                    if !rendering {
                        for t in &color_info {
                            if self.layouts.get(&t.0).copied()
                                != Some(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                            {
                                self.transition(
                                    ctx,
                                    cmd,
                                    t.0,
                                    t.2,
                                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                                );
                            }
                        }
                        if let Some(t) = &depth_info
                            && self.layouts.get(&t.0).copied()
                                != Some(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                        {
                            self.transition(
                                ctx,
                                cmd,
                                t.0,
                                t.2,
                                vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                            );
                        }
                        let attachments: Vec<_> = color_info
                            .iter()
                            .map(|t| {
                                vk::RenderingAttachmentInfo::default()
                                    .image_view(t.1)
                                    .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                                    .load_op(vk::AttachmentLoadOp::LOAD)
                                    .store_op(vk::AttachmentStoreOp::STORE)
                            })
                            .collect();
                        let depth_att = depth_info.as_ref().map(|t| {
                            vk::RenderingAttachmentInfo::default()
                                .image_view(t.1)
                                .image_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                                .load_op(vk::AttachmentLoadOp::LOAD)
                                .store_op(vk::AttachmentStoreOp::STORE)
                        });
                        let mut info = vk::RenderingInfo::default()
                            .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent })
                            .layer_count(1)
                            .color_attachments(&attachments);
                        if let Some(d) = depth_att.as_ref() {
                            info = info.depth_attachment(d);
                            if depth_info.as_ref().is_some_and(|t| has_stencil(t.2)) {
                                info = info.stencil_attachment(d);
                            }
                        }
                        // SAFETY: the attachments are in attachment layouts.
                        unsafe { device.cmd_begin_rendering(cmd, &info) };
                        rendering = true;
                    }
                    let Some(prog) = self.programs.get(&draw.program.0) else { continue };
                    let (layout, set_layouts, bindings, slots) =
                        (prog.layout, prog.set_layouts, prog.bindings.clone(), prog.slots.clone());
                    let (class, exact) = topology(draw.topology);
                    let st = &draw.state;
                    let key = PipelineKey {
                        program: draw.program,
                        fill: st.raster.fill,
                        depth_bias: (
                            st.raster.depth_bias,
                            st.raster.depth_bias_clamp,
                            st.raster.slope_scaled_depth_bias,
                        ),
                        depth_clip: st.raster.depth_clip,
                        depth: (
                            st.depth.stencil && depth_info.is_some(),
                            0,
                            st.depth.read_mask,
                            st.depth.write_mask,
                            st.depth.front,
                            st.depth.back,
                        ),
                        blend: st.blend,
                        colors: color_info.iter().map(|t| t.2).collect(),
                        depth_format: depth_info.as_ref().map_or(vk::Format::UNDEFINED, |t| t.2),
                        samples,
                        class,
                        sample_mask: draw.sample_mask,
                    };
                    let pipeline = self.pipeline(ctx, &key)?;
                    // Descriptor sets: constant buffers from the frame's uniform buffer, samplers, images.
                    let pool = self.frames[frame].pool;
                    // SAFETY: valid pool and layouts.
                    let sets = unsafe {
                        device.allocate_descriptor_sets(
                            &vk::DescriptorSetAllocateInfo::default()
                                .descriptor_pool(pool)
                                .set_layouts(&set_layouts),
                        )
                    }
                    .context("too many game draws in a frame")?;
                    let uniform_buffer =
                        self.frames[frame].uniforms.as_ref().map(|b| b.buffer).unwrap_or_default();
                    let mut buffer_infos = Vec::new();
                    let mut image_infos = Vec::new();
                    let mut writes_meta = Vec::new();
                    for &(stage, binding, kind) in &bindings {
                        let set = sets[stage.set()];
                        match kind {
                            BindingKind::ConstantBuffer => {
                                let data = draw
                                    .constants
                                    .iter()
                                    .find(|c| c.0 == stage && c.1 == binding)
                                    .map(|c| c.2.as_slice())
                                    .unwrap_or(&[]);
                                let len = (data.len() as u64).max(16);
                                if let Some(m) = self.frames[frame].uniforms.as_mut().and_then(|b| b.mapped())
                                {
                                    let at = uniform_at as usize;
                                    if at + data.len() <= m.len() {
                                        m[at..at + data.len()].copy_from_slice(data);
                                    }
                                }
                                buffer_infos.push(
                                    vk::DescriptorBufferInfo::default()
                                        .buffer(uniform_buffer)
                                        .offset(uniform_at)
                                        .range(len),
                                );
                                writes_meta.push((
                                    set,
                                    binding,
                                    vk::DescriptorType::UNIFORM_BUFFER,
                                    buffer_infos.len() - 1,
                                ));
                                uniform_at += len.div_ceil(align) * align;
                            }
                            BindingKind::Sampler => {
                                let reg = binding - BINDING_SAMPLER;
                                let desc = draw
                                    .samplers
                                    .iter()
                                    .find(|s| s.0 == stage && s.1 == reg)
                                    .map(|s| s.2)
                                    .unwrap_or_default();
                                let sampler = self.sampler(ctx, &desc)?;
                                image_infos.push(vk::DescriptorImageInfo::default().sampler(sampler));
                                writes_meta.push((
                                    set,
                                    binding,
                                    vk::DescriptorType::SAMPLER,
                                    image_infos.len() - 1,
                                ));
                            }
                            BindingKind::Texture => {
                                let reg = binding - BINDING_SRV;
                                let r = draw.images.iter().find(|i| i.0 == stage && i.1 == reg).map(|i| i.2);
                                let Some((_, view, _)) = r.and_then(|r| self.image_of(r, out)) else {
                                    return Err(anyhow!("draw samples t{reg} but no image is bound"));
                                };
                                image_infos.push(
                                    vk::DescriptorImageInfo::default()
                                        .image_view(view)
                                        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
                                );
                                writes_meta.push((
                                    set,
                                    binding,
                                    vk::DescriptorType::SAMPLED_IMAGE,
                                    image_infos.len() - 1,
                                ));
                            }
                        }
                    }
                    let writes: Vec<_> = writes_meta
                        .iter()
                        .map(|&(set, binding, ty, i)| {
                            let w = vk::WriteDescriptorSet::default()
                                .dst_set(set)
                                .dst_binding(binding)
                                .descriptor_type(ty);
                            if ty == vk::DescriptorType::UNIFORM_BUFFER {
                                w.buffer_info(std::slice::from_ref(&buffer_infos[i]))
                            } else {
                                w.image_info(std::slice::from_ref(&image_infos[i]))
                            }
                        })
                        .collect();
                    let zero = self.zero.as_ref().map(|b| b.buffer).unwrap_or_default();
                    let v = draw.viewport;
                    // SAFETY: a rendering instance is active; every handle belongs to this device.
                    unsafe {
                        device.update_descriptor_sets(&writes, &[]);
                        device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
                        device.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            layout,
                            0,
                            &sets,
                            &[],
                        );
                        // Direct3D's viewport, flipped (negative height) so its clip space and winding hold.
                        device.cmd_set_viewport(
                            cmd,
                            0,
                            &[vk::Viewport {
                                x: v[0],
                                y: v[1] + v[3],
                                width: v[2],
                                height: -v[3],
                                min_depth: v[4],
                                max_depth: v[5],
                            }],
                        );
                        device.cmd_set_scissor(
                            cmd,
                            0,
                            &[vk::Rect2D { offset: vk::Offset2D::default(), extent }],
                        );
                        device.cmd_set_cull_mode(
                            cmd,
                            match st.raster.cull {
                                2 => vk::CullModeFlags::FRONT,
                                3 => vk::CullModeFlags::BACK,
                                _ => vk::CullModeFlags::NONE,
                            },
                        );
                        device.cmd_set_front_face(
                            cmd,
                            if st.raster.front_ccw {
                                vk::FrontFace::COUNTER_CLOCKWISE
                            } else {
                                vk::FrontFace::CLOCKWISE
                            },
                        );
                        let depth_on = st.depth.enable && depth_info.is_some();
                        device.cmd_set_depth_test_enable(cmd, depth_on);
                        device.cmd_set_depth_write_enable(cmd, depth_on && st.depth.write);
                        device.cmd_set_depth_compare_op(cmd, compare(st.depth.func));
                        device.cmd_set_stencil_reference(
                            cmd,
                            vk::StencilFaceFlags::FRONT_AND_BACK,
                            draw.stencil_ref,
                        );
                        device.cmd_set_blend_constants(cmd, &draw.blend_factor);
                        device.cmd_set_primitive_topology(cmd, exact);
                        device.cmd_set_primitive_restart_enable(
                            cmd,
                            matches!(
                                exact,
                                vk::PrimitiveTopology::TRIANGLE_STRIP | vk::PrimitiveTopology::LINE_STRIP
                            ),
                        );
                        for &slot in &slots {
                            let vb = draw.vertex_buffers.iter().find(|b| b.slot == slot);
                            let (buffer, offset, stride) = match vb.and_then(|b| {
                                self.buffers.get(&b.buffer.0).map(|x| (x.buffer, b.offset, b.stride))
                            }) {
                                Some(x) => x,
                                // An unbound slot reads zeros, as in Direct3D 11.
                                None => (zero, 0, 0),
                            };
                            device.cmd_bind_vertex_buffers2(
                                cmd,
                                slot,
                                &[buffer],
                                &[offset],
                                None,
                                Some(&[u64::from(stride)]),
                            );
                        }
                        match draw.index.and_then(|i| self.buffers.get(&i.buffer.0).map(|b| (b.buffer, i))) {
                            Some((buffer, i)) => {
                                device.cmd_bind_index_buffer(
                                    cmd,
                                    buffer,
                                    i.offset,
                                    if i.wide { vk::IndexType::UINT32 } else { vk::IndexType::UINT16 },
                                );
                                device.cmd_draw_indexed(
                                    cmd,
                                    draw.count,
                                    draw.instances.max(1),
                                    draw.first,
                                    draw.base_vertex,
                                    0,
                                );
                            }
                            None => device.cmd_draw(cmd, draw.count, draw.instances.max(1), draw.first, 0),
                        }
                    }
                }
            }
        }
        end(&mut rendering);
        let out_format = out.format;
        self.transition(ctx, cmd, out.image, out_format, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        self.layouts.remove(&out.image);
        Ok(true)
    }

    /// # Safety
    /// The device must be idle.
    pub unsafe fn destroy(&mut self, ctx: &Context) {
        let device = &ctx.device;
        let targets: Vec<u32> = self.targets.keys().copied().collect();
        for t in targets {
            self.destroy_target(ctx, TargetHandle(t));
        }
        for (_, mut b) in self.buffers.drain() {
            // SAFETY: the device is idle.
            unsafe { b.destroy(ctx) };
        }
        if let Some(mut z) = self.zero.take() {
            // SAFETY: as above.
            unsafe { z.destroy(ctx) };
        }
        // SAFETY: the device is idle; nothing below is in use.
        unsafe {
            for (_, p) in self.pipelines.drain() {
                device.destroy_pipeline(p, None);
            }
            for (_, s) in self.samplers.drain() {
                device.destroy_sampler(s, None);
            }
            for (_, p) in self.programs.drain() {
                device.destroy_shader_module(p.vertex, None);
                device.destroy_shader_module(p.pixel, None);
                device.destroy_pipeline_layout(p.layout, None);
                for l in p.set_layouts {
                    device.destroy_descriptor_set_layout(l, None);
                }
            }
            for mut f in self.frames.drain(..) {
                if f.pool != vk::DescriptorPool::null() {
                    device.destroy_descriptor_pool(f.pool, None);
                }
                if let Some(mut u) = f.uniforms.take() {
                    u.destroy(ctx);
                }
            }
        }
    }
}

fn bytemuck_u32(v: &[u32]) -> &[u8] {
    // SAFETY: u32 has no padding; the slice is reinterpreted as its bytes for the specialization data.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}
