//! Swapchain creation and recreation.

use anyhow::{Result, anyhow};
use ash::vk;

use crate::context::Context;

/// Presentation modes offered to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresentMode {
    /// Vertical sync; never tears. Always available.
    Fifo,
    /// Vertical sync that tears when a frame is late.
    FifoRelaxed,
    /// No tearing, newest frame wins; lowest latency without tearing.
    Mailbox,
    /// No synchronization; may tear.
    Immediate,
}

impl PresentMode {
    pub const ALL: [PresentMode; 4] =
        [PresentMode::Fifo, PresentMode::FifoRelaxed, PresentMode::Mailbox, PresentMode::Immediate];

    pub fn to_vk(self) -> vk::PresentModeKHR {
        match self {
            PresentMode::Fifo => vk::PresentModeKHR::FIFO,
            PresentMode::FifoRelaxed => vk::PresentModeKHR::FIFO_RELAXED,
            PresentMode::Mailbox => vk::PresentModeKHR::MAILBOX,
            PresentMode::Immediate => vk::PresentModeKHR::IMMEDIATE,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PresentMode::Fifo => "V-sync",
            PresentMode::FifoRelaxed => "Adaptive v-sync",
            PresentMode::Mailbox => "Mailbox (no tearing, low latency)",
            PresentMode::Immediate => "Immediate (may tear)",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PresentMode::Fifo => "fifo",
            PresentMode::FifoRelaxed => "fifo-relaxed",
            PresentMode::Mailbox => "mailbox",
            PresentMode::Immediate => "immediate",
        }
    }

    pub fn from_name(name: &str) -> Option<PresentMode> {
        PresentMode::ALL.into_iter().find(|m| m.name() == name)
    }
}

pub struct Swapchain {
    pub loader: ash::khr::swapchain::Device,
    pub handle: vk::SwapchainKHR,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    /// Signalled when rendering to the image of the same index has finished (waited on by present).
    pub render_finished: Vec<vk::Semaphore>,
    pub present_mode: PresentMode,
    pub available_modes: Vec<PresentMode>,
}

impl std::fmt::Debug for Swapchain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Swapchain")
            .field("format", &self.format)
            .field("extent", &self.extent)
            .field("images", &self.images.len())
            .field("present_mode", &self.present_mode)
            .finish()
    }
}

impl Swapchain {
    pub fn new(ctx: &Context, size: (u32, u32), mode: PresentMode, old: Option<&Swapchain>) -> Result<Self> {
        let surface_fn = ctx.surface_fn.as_ref().ok_or_else(|| anyhow!("context has no surface"))?;
        let pd = ctx.physical_device;
        // SAFETY: valid physical device and surface owned by the context.
        let (caps, formats, modes) = unsafe {
            (
                surface_fn.get_physical_device_surface_capabilities(pd, ctx.surface)?,
                surface_fn.get_physical_device_surface_formats(pd, ctx.surface)?,
                surface_fn.get_physical_device_surface_present_modes(pd, ctx.surface)?,
            )
        };
        // 8-bit UNORM output: the games blend in display space, so the swapchain must not apply sRGB encoding.
        let format = formats
            .iter()
            .find(|f| {
                matches!(f.format, vk::Format::B8G8R8A8_UNORM | vk::Format::R8G8B8A8_UNORM)
                    && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .or(formats.first())
            .copied()
            .ok_or_else(|| anyhow!("surface reports no formats"))?;

        let available_modes: Vec<PresentMode> =
            PresentMode::ALL.into_iter().filter(|m| modes.contains(&m.to_vk())).collect();
        let present_mode = if available_modes.contains(&mode) { mode } else { PresentMode::Fifo };

        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: size.0.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: size.1.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            image_count = image_count.min(caps.max_image_count);
        }

        let loader = ash::khr::swapchain::Device::new(&ctx.instance, &ctx.device);
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(ctx.surface)
            .min_image_count(image_count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(present_mode.to_vk())
            .clipped(true)
            .old_swapchain(old.map_or(vk::SwapchainKHR::null(), |o| o.handle));
        // SAFETY: valid device and surface; the old swapchain (if any) is retired by this call.
        let handle = unsafe { loader.create_swapchain(&info, None) }?;
        // SAFETY: handle was just created.
        let images = unsafe { loader.get_swapchain_images(handle) }?;
        let mut views = Vec::with_capacity(images.len());
        let mut render_finished = Vec::with_capacity(images.len());
        for &image in &images {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format.format)
                .subresource_range(color_range());
            // SAFETY: image belongs to the swapchain created above.
            views.push(unsafe { ctx.device.create_image_view(&view_info, None) }?);
            // SAFETY: valid device.
            render_finished
                .push(unsafe { ctx.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }?);
        }
        log::info!(
            "swapchain {}x{} {:?}, {} images, {:?}",
            extent.width,
            extent.height,
            format.format,
            images.len(),
            present_mode
        );
        Ok(Self {
            loader,
            handle,
            format: format.format,
            extent,
            images,
            views,
            render_finished,
            present_mode,
            available_modes,
        })
    }

    /// # Safety
    /// The GPU must be idle with respect to this swapchain's images and semaphores.
    pub unsafe fn destroy(&self, device: &ash::Device) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            for &v in &self.views {
                device.destroy_image_view(v, None);
            }
            for &s in &self.render_finished {
                device.destroy_semaphore(s, None);
            }
            self.loader.destroy_swapchain(self.handle, None);
        }
    }
}

pub(crate) fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}
