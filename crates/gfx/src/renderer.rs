//! Windowed renderer: swapchain, frames in flight, the quad pipeline and egui drawing.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use crate::context::{Context, ContextOptions};
use crate::quad::{Quad, QuadPipeline};
use crate::scene::{
    DEPTH_FORMAT, MeshDesc, MeshHandle, SceneState, SceneView, Target, VIEWPORT_FORMAT, Viewport,
};
use crate::swapchain::{PresentMode, Swapchain, color_range};
use crate::texture::{TexFormat, Texture, TextureDesc};

const FRAMES_IN_FLIGHT: usize = 2;
/// Descriptor sets available for textures shown in the UI.
const UI_TEXTURE_SETS: u32 = 4096;

/// A texture owned by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureHandle(u64);

/// One UI view of a texture: layer, level, nearest filtering.
type ViewKey = (u32, u32, bool);

#[derive(Debug)]
struct TextureEntry {
    texture: Texture,
    views: HashMap<ViewKey, (vk::ImageView, vk::DescriptorSet, egui::TextureId)>,
}

/// Descriptor objects for showing our textures through egui (its layout: one combined image sampler).
#[derive(Debug)]
struct UiTextures {
    layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    linear: vk::Sampler,
    nearest: vk::Sampler,
}

impl UiTextures {
    fn new(device: &ash::Device) -> Result<UiTextures> {
        let bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: UI_TEXTURE_SETS,
        }];
        let sampler = |filter: vk::Filter| {
            vk::SamplerCreateInfo::default()
                .mag_filter(filter)
                .min_filter(filter)
                .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .max_lod(vk::LOD_CLAMP_NONE)
        };
        // SAFETY: valid device and create infos.
        unsafe {
            Ok(UiTextures {
                layout: device.create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )?,
                pool: device.create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                        .max_sets(UI_TEXTURE_SETS)
                        .pool_sizes(&sizes),
                    None,
                )?,
                linear: device.create_sampler(&sampler(vk::Filter::LINEAR), None)?,
                nearest: device.create_sampler(&sampler(vk::Filter::NEAREST), None)?,
            })
        }
    }

    /// # Safety
    /// No set from the pool may still be in use.
    unsafe fn destroy(&self, device: &ash::Device) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            device.destroy_sampler(self.linear, None);
            device.destroy_sampler(self.nearest, None);
            device.destroy_descriptor_pool(self.pool, None);
            device.destroy_descriptor_set_layout(self.layout, None);
        }
    }
}

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
    ui_textures: UiTextures,
    scene: SceneState,
    white: Option<TextureHandle>,
    /// What water parts sample where a texture is missing (black, flat normal, flat normal, white cube); parts
    /// without water bind these too.
    water_defaults: Option<[TextureHandle; crate::scene::WATER_TEXTURES]>,
    textures: HashMap<u64, TextureEntry>,
    next_texture: u64,
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
        Self::with_context(ctx, Some(swapchain), size, present_mode)
    }

    /// A renderer without a window, for offscreen rendering ([`Renderer::render_offscreen`]) in tests and tools.
    pub fn new_headless(options: &ContextOptions) -> Result<Self> {
        let ctx = Context::new_headless(options)?;
        Self::with_context(ctx, None, (1, 1), PresentMode::Fifo)
    }

    fn with_context(
        ctx: Context,
        swapchain: Option<Swapchain>,
        size: (u32, u32),
        present_mode: PresentMode,
    ) -> Result<Self> {
        let device = &ctx.device;
        let color_format = swapchain.as_ref().map_or(crate::offscreen::OFFSCREEN_FORMAT, |s| s.format);

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

        let quad = QuadPipeline::new(device, color_format)?;
        let egui = egui_ash_renderer::Renderer::with_gpu_allocator(
            ctx.allocator().clone(),
            device.clone(),
            egui_ash_renderer::RenderMode::DynamicRendering(egui_ash_renderer::DynamicRendering {
                color_attachment_format: color_format,
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

        let ui_textures = UiTextures::new(device)?;
        let scene = SceneState::new(&ctx, FRAMES_IN_FLIGHT)?;

        let mut renderer = Self {
            swapchain,
            frames,
            frame_index: 0,
            upload_pool,
            quad,
            egui: Some(egui),
            ui_textures,
            scene,
            white: None,
            water_defaults: None,
            textures: HashMap::new(),
            next_texture: 1,
            wanted_size: size,
            wanted_mode: present_mode,
            needs_recreate: false,
            ctx,
        };
        let white = [255u8; 4];
        renderer.white = Some(renderer.create_texture(&TextureDesc {
            format: TexFormat::Rgba8,
            width: 1,
            height: 1,
            levels: 1,
            layers: 1,
            cube: false,
            data: vec![&white],
            name: "white",
        })?);
        let pixel = |r: &mut Self, rgba: &[u8; 4], cube: bool, name: &str| {
            let layers = if cube { 6 } else { 1 };
            r.create_texture(&TextureDesc {
                format: TexFormat::Rgba8,
                width: 1,
                height: 1,
                levels: 1,
                layers,
                cube,
                data: vec![rgba.as_slice(); layers as usize],
                name,
            })
        };
        let black = pixel(&mut renderer, &[0, 0, 0, 0], false, "black")?;
        let flat = pixel(&mut renderer, &[128, 128, 255, 255], false, "flat normal")?;
        let cube = pixel(&mut renderer, &[255; 4], true, "white cube")?;
        let defaults = [black, flat, flat, cube];
        let views = defaults.map(|h| renderer.textures[&h.0].texture.view);
        renderer.scene.default_extra = views;
        renderer.scene.extra_set(&renderer.ctx, views)?;
        renderer.water_defaults = Some(defaults);
        Ok(renderer)
    }

    /// Upload a texture (all levels and layers); waits for the copy.
    pub fn create_texture(&mut self, desc: &TextureDesc<'_>) -> Result<TextureHandle> {
        let texture = Texture::create(&self.ctx, desc)?;
        let id = self.next_texture;
        self.next_texture += 1;
        self.textures.insert(id, TextureEntry { texture, views: HashMap::new() });
        Ok(TextureHandle(id))
    }

    /// Replace a texture's first level (same format and size).
    pub fn update_texture(&mut self, handle: TextureHandle, data: &[u8]) -> Result<()> {
        let entry = self.textures.get_mut(&handle.0).context("unknown texture")?;
        entry.texture.update(&self.ctx, data)
    }

    pub fn texture(&self, handle: TextureHandle) -> Option<&Texture> {
        self.textures.get(&handle.0).map(|e| &e.texture)
    }

    /// An egui texture id showing one layer and level (created on first use).
    pub fn ui_texture(
        &mut self,
        handle: TextureHandle,
        layer: u32,
        level: u32,
        nearest: bool,
    ) -> Result<egui::TextureId> {
        let entry = self.textures.get_mut(&handle.0).context("unknown texture")?;
        let key = (layer.min(entry.texture.layers - 1), level.min(entry.texture.levels - 1), nearest);
        if let Some(&(_, _, id)) = entry.views.get(&key) {
            return Ok(id);
        }
        let device = &self.ctx.device;
        let view = entry.texture.create_view(&self.ctx, Some(key.0), Some(key.1))?;
        let layouts = [self.ui_textures.layout];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.ui_textures.pool)
            .set_layouts(&layouts);
        // SAFETY: valid pool and layout.
        let set = match unsafe { device.allocate_descriptor_sets(&alloc) } {
            Ok(s) => s[0],
            Err(e) => {
                // SAFETY: the view was never used.
                unsafe { device.destroy_image_view(view, None) };
                return Err(e).context("too many textures shown at once");
            }
        };
        let image_info = [vk::DescriptorImageInfo::default()
            .sampler(if nearest { self.ui_textures.nearest } else { self.ui_textures.linear })
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info)];
        // SAFETY: the set is new and not in use.
        unsafe { device.update_descriptor_sets(&write, &[]) };
        let id = self.egui.as_mut().expect("egui renderer lives until drop").add_user_texture(set);
        entry.views.insert(key, (view, set, id));
        Ok(id)
    }

    /// Destroy a texture and its UI views. Waits for the GPU, so call it rarely (e.g. when a viewer closes
    /// an asset).
    pub fn destroy_texture(&mut self, handle: TextureHandle) {
        let Some(mut entry) = self.textures.remove(&handle.0) else { return };
        self.ctx.wait_idle();
        let device = &self.ctx.device;
        for (_, (view, set, id)) in entry.views.drain() {
            if let Some(egui) = self.egui.as_mut() {
                egui.remove_user_texture(id);
            }
            // SAFETY: the device is idle; the set and view are no longer referenced.
            unsafe {
                device.free_descriptor_sets(self.ui_textures.pool, &[set]).ok();
                device.destroy_image_view(view, None);
            }
        }
        self.scene.forget_view(&self.ctx, entry.texture.view);
        // SAFETY: the device is idle.
        unsafe { entry.texture.destroy(&self.ctx) };
    }

    /// Upload a mesh. Parts without a texture sample white.
    pub fn create_mesh(&mut self, desc: &MeshDesc<'_>) -> Result<MeshHandle> {
        let mut sets = Vec::with_capacity(desc.parts.len());
        let defaults = self.water_defaults.context("no default textures")?;
        for part in &desc.parts {
            let view_of = |h: TextureHandle| -> Result<vk::ImageView> {
                Ok(self.textures.get(&h.0).context("unknown texture")?.texture.view)
            };
            let view = view_of(part.texture.or(self.white).context("no white texture")?)?;
            let set = self.scene.material_set(&self.ctx, view)?;
            let mut views = self.scene.default_extra;
            if let Some(w) = &part.water {
                for (i, t) in w.textures.iter().enumerate() {
                    views[i] = view_of(t.unwrap_or(defaults[i]))?;
                }
            }
            sets.push((set, self.scene.extra_set(&self.ctx, views)?));
        }
        self.scene.create_mesh(&self.ctx, desc, sets)
    }

    /// Destroy a mesh (waits for the GPU).
    pub fn destroy_mesh(&mut self, mesh: MeshHandle) {
        self.ctx.wait_idle();
        self.scene.destroy_mesh(&self.ctx, mesh);
    }

    /// The UI texture showing the 3D viewport, (re)created at `size` pixels.
    pub fn viewport_texture(&mut self, size: (u32, u32)) -> Result<egui::TextureId> {
        let extent = vk::Extent2D { width: size.0.clamp(1, 16384), height: size.1.clamp(1, 16384) };
        if let Some(vp) = &self.scene.viewport
            && vp.extent == extent
        {
            return Ok(vp.ui_id);
        }
        self.ctx.wait_idle();
        self.drop_viewport();
        let ctx = &self.ctx;
        // The scene writes display-ready values; the UI renderer takes texture samples as linear and encodes
        // its output for a non-sRGB swapchain, so it samples the viewport through an sRGB view (which decodes
        // them) to show the scene unchanged.
        let mut color = Target::new(
            ctx,
            VIEWPORT_FORMAT,
            extent,
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC,
            vk::ImageAspectFlags::COLOR,
            Some(vk::Format::R8G8B8A8_SRGB),
        )?;
        let depth = match Target::new(
            ctx,
            DEPTH_FORMAT,
            extent,
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            vk::ImageAspectFlags::DEPTH,
            None,
        ) {
            Ok(d) => d,
            Err(e) => {
                // SAFETY: never used.
                unsafe { color.destroy(ctx) };
                return Err(e);
            }
        };
        // Start in the layout the UI samples from.
        crate::scene::one_time_submit(ctx, |cmd| {
            // SAFETY: recording; the image is ours.
            unsafe {
                image_barrier(
                    &ctx.device,
                    cmd,
                    color.image,
                    (vk::ImageLayout::UNDEFINED, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE),
                    (
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::PipelineStageFlags2::FRAGMENT_SHADER,
                        vk::AccessFlags2::SHADER_SAMPLED_READ,
                    ),
                )
            };
        })?;
        let layouts = [self.ui_textures.layout];
        // SAFETY: valid pool and layout.
        let set = unsafe {
            ctx.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.ui_textures.pool)
                    .set_layouts(&layouts),
            )
        }?[0];
        let info = [vk::DescriptorImageInfo::default()
            .sampler(self.ui_textures.linear)
            .image_view(color.alt_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info)];
        // SAFETY: the set is new.
        unsafe { ctx.device.update_descriptor_sets(&write, &[]) };
        let ui_id = self.egui.as_mut().expect("egui renderer lives until drop").add_user_texture(set);
        self.scene.viewport = Some(Viewport {
            color,
            depth,
            extent,
            ui_set: set,
            ui_id,
            color_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        });
        Ok(ui_id)
    }

    /// Release the viewport (waits for the GPU).
    pub fn release_viewport(&mut self) {
        if self.scene.viewport.is_some() {
            self.ctx.wait_idle();
            self.drop_viewport();
        }
    }

    fn drop_viewport(&mut self) {
        if let Some(mut vp) = self.scene.viewport.take() {
            if let Some(egui) = self.egui.as_mut() {
                egui.remove_user_texture(vp.ui_id);
            }
            // SAFETY: callers waited for the device.
            unsafe {
                self.ctx.device.free_descriptor_sets(self.ui_textures.pool, &[vp.ui_set]).ok();
                vp.color.destroy(&self.ctx);
                vp.depth.destroy(&self.ctx);
            }
        }
    }

    /// Draw `view` at `size` pixels right away (no window needed) and return the RGBA8 pixels, rows top to
    /// bottom. Uses (and resizes) the viewport target.
    pub fn render_offscreen(&mut self, view: SceneView, size: (u32, u32)) -> Result<Vec<u8>> {
        self.viewport_texture(size)?;
        self.ctx.wait_idle();
        let extent = self.scene.viewport.as_ref().expect("viewport was just created").extent;
        let bytes = u64::from(extent.width) * u64::from(extent.height) * 4;
        let mut buffer = crate::scene::Buffer::new(
            &self.ctx,
            bytes,
            vk::BufferUsageFlags::TRANSFER_DST,
            gpu_allocator::MemoryLocation::GpuToCpu,
        )?;
        self.scene.pending = Some(view);
        let (ctx, scene) = (&self.ctx, &mut self.scene);
        let mut recorded = Ok(());
        let submitted = crate::scene::one_time_submit(ctx, |cmd| {
            recorded = scene.record(ctx, cmd, 0);
            let image = scene.viewport.as_ref().expect("viewport exists").color.image;
            let device = &ctx.device;
            // SAFETY: recording outside a render pass; the image and buffer belong to this device and the buffer
            // holds the whole image.
            unsafe {
                image_barrier(
                    device,
                    cmd,
                    image,
                    (
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::PipelineStageFlags2::ALL_COMMANDS,
                        vk::AccessFlags2::SHADER_SAMPLED_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    ),
                    (
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::PipelineStageFlags2::COPY,
                        vk::AccessFlags2::TRANSFER_READ,
                    ),
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 });
                device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.buffer,
                    &[region],
                );
                image_barrier(
                    device,
                    cmd,
                    image,
                    (
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::PipelineStageFlags2::COPY,
                        vk::AccessFlags2::TRANSFER_READ,
                    ),
                    (
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::PipelineStageFlags2::FRAGMENT_SHADER,
                        vk::AccessFlags2::SHADER_SAMPLED_READ,
                    ),
                );
            }
        });
        let pixels = buffer.mapped().map(|m| m[..bytes as usize].to_vec());
        // SAFETY: the submission has completed (one_time_submit waits).
        unsafe { buffer.destroy(&self.ctx) };
        submitted?;
        recorded?;
        pixels.context("readback buffer is not host visible")
    }

    /// Draw `view` into the viewport this frame.
    pub fn submit_scene(&mut self, view: SceneView) {
        self.scene.pending = Some(view);
    }

    pub fn context(&self) -> &Context {
        &self.ctx
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
        if self.swapchain.is_none() {
            anyhow::bail!("a headless renderer has no window to draw to");
        }
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
            self.scene.record(&self.ctx, cmd, self.frame_index)?;
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
        self.drop_viewport();
        // Textures free their scene descriptor sets, so they go before the scene's pool.
        for handle in self.textures.keys().copied().collect::<Vec<_>>() {
            self.destroy_texture(TextureHandle(handle));
        }
        // SAFETY: the device is idle.
        unsafe { self.scene.destroy(&self.ctx) };
        let device = &self.ctx.device;
        // SAFETY: the device is idle; every object below was created from it and is destroyed once.
        unsafe {
            self.ui_textures.destroy(device);
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
