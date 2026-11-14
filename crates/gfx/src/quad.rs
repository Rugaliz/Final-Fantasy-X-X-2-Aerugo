//! A minimal pipeline that draws colored, rotated rectangles. Used by the timing test scene and the
//! headless test; real scene rendering arrives with the viewer milestone.

use anyhow::Result;
use ash::vk;

static VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.vert.spv"));
static FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.frag.spv"));

/// A rectangle in the square virtual space `[-1, 1]²` (y up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub center: [f32; 2],
    pub half_size: [f32; 2],
    /// Rotation in radians, counter-clockwise.
    pub rotation: f32,
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Push {
    center: [f32; 2],
    half_size: [f32; 2],
    rotation: [f32; 2],
    scale: [f32; 2],
    color: [f32; 4],
}

#[derive(Debug)]
pub struct QuadPipeline {
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl QuadPipeline {
    pub fn new(device: &ash::Device, color_format: vk::Format) -> Result<Self> {
        let vert = shader_module(device, VERT_SPV)?;
        let frag = shader_module(device, FRAG_SPV)?;
        let push_range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(size_of::<Push>() as u32)];
        let layout_info = vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&push_range);
        // SAFETY: valid device and create info.
        let layout = unsafe { device.create_pipeline_layout(&layout_info, None) }?;

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
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let formats = [color_format];
        let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(layout)
            .push_next(&mut rendering);
        // SAFETY: all referenced state lives until the call returns.
        let result = unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) };
        // SAFETY: modules are no longer needed once the pipeline is created (or creation failed).
        unsafe {
            device.destroy_shader_module(vert, None);
            device.destroy_shader_module(frag, None);
        }
        let pipeline = result.map_err(|(_, e)| e)?[0];
        Ok(Self { layout, pipeline })
    }

    /// Record draws for `quads` into a command buffer inside a dynamic rendering pass covering `extent`.
    /// The virtual square is fitted into the viewport (letterboxed on the longer axis).
    pub fn draw(&self, device: &ash::Device, cmd: vk::CommandBuffer, extent: vk::Extent2D, quads: &[Quad]) {
        if quads.is_empty() || extent.width == 0 || extent.height == 0 {
            return;
        }
        let (w, h) = (extent.width as f32, extent.height as f32);
        let side = w.min(h);
        let scale = [side / w, side / h];
        let viewport = [vk::Viewport { x: 0.0, y: 0.0, width: w, height: h, min_depth: 0.0, max_depth: 1.0 }];
        let scissor = [vk::Rect2D { offset: vk::Offset2D::default(), extent }];
        // SAFETY: the command buffer is recording inside a rendering pass with a matching color format.
        unsafe {
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            device.cmd_set_viewport(cmd, 0, &viewport);
            device.cmd_set_scissor(cmd, 0, &scissor);
            for q in quads {
                let (s, c) = q.rotation.sin_cos();
                let push = Push {
                    center: q.center,
                    half_size: q.half_size,
                    rotation: [c, s],
                    scale,
                    color: q.color,
                };
                // SAFETY: Push is repr(C) plain data; reading it as bytes is sound.
                let bytes =
                    std::slice::from_raw_parts((&push as *const Push).cast::<u8>(), size_of::<Push>());
                device.cmd_push_constants(
                    cmd,
                    self.layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytes,
                );
                device.cmd_draw(cmd, 6, 1, 0, 0);
            }
        }
    }

    /// # Safety
    /// The pipeline must not be in use by the GPU.
    pub unsafe fn destroy(&self, device: &ash::Device) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
        }
    }
}

pub(crate) fn shader_module(device: &ash::Device, spv: &[u8]) -> Result<vk::ShaderModule> {
    let words = ash::util::read_spv(&mut std::io::Cursor::new(spv))?;
    let info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: valid device; the code was produced and validated by the shader compiler at build time.
    Ok(unsafe { device.create_shader_module(&info, None) }?)
}
