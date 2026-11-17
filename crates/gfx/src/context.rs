//! Vulkan instance, device, queue and memory allocator.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use ash::vk;
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Options for creating a [`Context`].
#[derive(Debug, Clone, Default)]
pub struct ContextOptions {
    /// Enable the Khronos validation layer if it is installed.
    pub validation: bool,
    /// Prefer the first GPU whose name contains this text (case-insensitive).
    pub preferred_gpu: Option<String>,
}

/// Vulkan objects that live for the whole run.
pub struct Context {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    pub surface_fn: Option<ash::khr::surface::Instance>,
    pub surface: vk::SurfaceKHR,
    pub physical_device: vk::PhysicalDevice,
    pub device_name: String,
    pub device: ash::Device,
    pub queue_family: u32,
    pub queue: vk::Queue,
    pub allocator: Option<Arc<Mutex<Allocator>>>,
    /// Largest sampler anisotropy the device allows (1 without anisotropic filtering).
    pub max_anisotropy: f32,
    /// RGBA8 images can be written from compute shaders (post-processing in place).
    pub storage_rgba8: bool,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context").field("device_name", &self.device_name).finish_non_exhaustive()
    }
}

impl Context {
    /// Context that presents to `window`. `instance_extensions` are the ones the windowing library asks for.
    pub fn new_windowed(
        window: &(impl HasWindowHandle + HasDisplayHandle),
        instance_extensions: &[String],
        options: &ContextOptions,
    ) -> Result<Self> {
        Self::create(Some((window, instance_extensions)), options)
    }

    /// Context without a surface, for offscreen rendering and tests.
    pub fn new_headless(options: &ContextOptions) -> Result<Self> {
        Self::create::<NoWindow>(None, options)
    }

    fn create<W: HasWindowHandle + HasDisplayHandle>(
        window: Option<(&W, &[String])>,
        options: &ContextOptions,
    ) -> Result<Self> {
        // SAFETY: loads the system Vulkan loader; no other Vulkan calls are in flight.
        let entry = unsafe { ash::Entry::load() }.context("loading the Vulkan library")?;

        let mut extensions: Vec<CString> = Vec::new();
        if let Some((_, exts)) = window {
            for e in exts {
                extensions.push(CString::new(e.as_str())?);
            }
        }
        // SAFETY: plain enumeration on a valid entry.
        let layer_props = unsafe { entry.enumerate_instance_layer_properties() }.unwrap_or_default();
        let validation = options.validation
            && layer_props.iter().any(|l| l.layer_name_as_c_str().is_ok_and(|n| n == VALIDATION_LAYER));
        if options.validation && !validation {
            log::warn!("Vulkan validation requested but {VALIDATION_LAYER:?} is not installed");
        }
        if validation {
            extensions.push(ash::ext::debug_utils::NAME.to_owned());
        }
        let ext_ptrs: Vec<*const c_char> = extensions.iter().map(|e| e.as_ptr()).collect();
        let layers: Vec<*const c_char> = if validation { vec![VALIDATION_LAYER.as_ptr()] } else { vec![] };

        let app_info = vk::ApplicationInfo::default()
            .application_name(c"Aerugo")
            .engine_name(c"Aerugo")
            .api_version(vk::API_VERSION_1_3);
        let create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&ext_ptrs)
            .enabled_layer_names(&layers);
        // SAFETY: create info and the strings it points to outlive the call.
        let instance =
            unsafe { entry.create_instance(&create_info, None) }.context("creating the Vulkan instance")?;

        let debug = if validation {
            let loader = ash::ext::debug_utils::Instance::new(&entry, &instance);
            let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                        | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(debug_callback));
            // SAFETY: valid instance with the debug-utils extension enabled.
            let messenger = unsafe { loader.create_debug_utils_messenger(&info, None) }?;
            Some((loader, messenger))
        } else {
            None
        };

        let (surface_fn, surface) = match window {
            Some((w, _)) => {
                let surface_fn = ash::khr::surface::Instance::new(&entry, &instance);
                let display = w.display_handle().map_err(|e| anyhow!("display handle: {e}"))?.as_raw();
                let handle = w.window_handle().map_err(|e| anyhow!("window handle: {e}"))?.as_raw();
                // SAFETY: the handles come from a live window that outlives the surface (owned by the caller).
                let surface = unsafe { ash_window::create_surface(&entry, &instance, display, handle, None) }
                    .context("creating the window surface")?;
                (Some(surface_fn), surface)
            }
            None => (None, vk::SurfaceKHR::null()),
        };

        let (physical_device, queue_family, device_name) =
            pick_device(&instance, surface_fn.as_ref(), surface, options.preferred_gpu.as_deref())?;
        log::info!("GPU: {device_name}");

        let priorities = [1.0];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let device_exts: Vec<*const c_char> =
            if surface_fn.is_some() { vec![ash::khr::swapchain::NAME.as_ptr()] } else { vec![] };
        let mut features13 =
            vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true).synchronization2(true);
        // The games' textures are BC1–BC3; every desktop GPU supports them. Anisotropic filtering where available
        // (the remaster's high setting is 16×).
        // SAFETY: valid physical device.
        let supported = unsafe { instance.get_physical_device_features(physical_device) };
        // SAFETY: as above.
        let limits = unsafe { instance.get_physical_device_properties(physical_device) }.limits;
        let anisotropy = supported.sampler_anisotropy == vk::TRUE;
        let max_anisotropy = if anisotropy { limits.max_sampler_anisotropy.max(1.0) } else { 1.0 };
        // SAFETY: as above.
        let rgba8 = unsafe {
            instance.get_physical_device_format_properties(physical_device, vk::Format::R8G8B8A8_UNORM)
        };
        let storage_rgba8 = rgba8.optimal_tiling_features.contains(vk::FormatFeatureFlags::STORAGE_IMAGE);
        let core =
            vk::PhysicalDeviceFeatures::default().texture_compression_bc(true).sampler_anisotropy(anisotropy);
        let mut features2 = vk::PhysicalDeviceFeatures2::default().features(core);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_exts)
            .push_next(&mut features2)
            .push_next(&mut features13);
        // SAFETY: physical device and queue family were validated by pick_device.
        let device = unsafe { instance.create_device(physical_device, &device_info, None) }
            .context("creating the Vulkan device")?;
        // SAFETY: queue 0 of a family we requested.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };

        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })
        .context("creating the GPU memory allocator")?;

        Ok(Self {
            entry,
            instance,
            debug,
            surface_fn,
            surface,
            physical_device,
            device_name,
            device,
            queue_family,
            queue,
            allocator: Some(Arc::new(Mutex::new(allocator))),
            max_anisotropy,
            storage_rgba8,
        })
    }

    pub fn allocator(&self) -> &Arc<Mutex<Allocator>> {
        self.allocator.as_ref().expect("allocator lives until drop")
    }

    pub fn wait_idle(&self) {
        // SAFETY: valid device; waiting has no other preconditions.
        unsafe { self.device.device_wait_idle() }.ok();
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.wait_idle();
        // The allocator must release its memory before the device goes away. Other holders of the
        // allocator (renderers) must have been dropped first.
        if let Some(a) = self.allocator.take()
            && Arc::strong_count(&a) > 1
        {
            log::error!("GPU allocator still shared at shutdown; leaking it");
            std::mem::forget(a);
        }
        // SAFETY: all child objects are destroyed; destruction order is device, surface, messenger, instance.
        unsafe {
            self.device.destroy_device(None);
            if let Some(s) = &self.surface_fn {
                s.destroy_surface(self.surface, None);
            }
            if let Some((loader, messenger)) = &self.debug {
                loader.destroy_debug_utils_messenger(*messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

fn pick_device(
    instance: &ash::Instance,
    surface_fn: Option<&ash::khr::surface::Instance>,
    surface: vk::SurfaceKHR,
    preferred: Option<&str>,
) -> Result<(vk::PhysicalDevice, u32, String)> {
    // SAFETY: plain queries on a valid instance.
    let devices = unsafe { instance.enumerate_physical_devices() }?;
    let mut candidates = Vec::new();
    for pd in devices {
        // SAFETY: `pd` comes from the enumeration above.
        let props = unsafe { instance.get_physical_device_properties(pd) };
        let name = props.device_name_as_c_str().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if props.api_version < vk::API_VERSION_1_3 {
            log::info!("skipping {name}: Vulkan 1.3 required");
            continue;
        }
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut f13);
        // SAFETY: valid physical device; the chain lives on the stack for the call.
        unsafe { instance.get_physical_device_features2(pd, &mut f2) };
        let bc = f2.features.texture_compression_bc;
        if f13.dynamic_rendering == vk::FALSE || f13.synchronization2 == vk::FALSE {
            continue;
        }
        if bc == vk::FALSE {
            log::info!("skipping {name}: BC texture compression required");
            continue;
        }
        if surface_fn.is_some() {
            // SAFETY: valid physical device.
            let exts = unsafe { instance.enumerate_device_extension_properties(pd) }.unwrap_or_default();
            if !exts.iter().any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == ash::khr::swapchain::NAME))
            {
                continue;
            }
        }
        // SAFETY: valid physical device.
        let families = unsafe { instance.get_physical_device_queue_family_properties(pd) };
        let family = families.iter().enumerate().find_map(|(i, f)| {
            let graphics = f.queue_flags.contains(vk::QueueFlags::GRAPHICS);
            let present = surface_fn.is_none_or(|s| {
                // SAFETY: valid physical device, family index in range, valid surface.
                unsafe { s.get_physical_device_surface_support(pd, i as u32, surface) }.unwrap_or(false)
            });
            (graphics && present).then_some(i as u32)
        });
        let Some(family) = family else { continue };
        let rank = match props.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 0,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
            vk::PhysicalDeviceType::CPU => 4,
            _ => 3,
        };
        let preferred_match =
            preferred.is_some_and(|p| name.to_ascii_lowercase().contains(&p.to_ascii_lowercase()));
        candidates.push(((!preferred_match, rank), pd, family, name));
    }
    candidates.sort_by_key(|c| c.0);
    match candidates.into_iter().next() {
        Some((_, pd, family, name)) => Ok((pd, family, name)),
        None => bail!("no Vulkan 1.3 device with dynamic rendering and synchronization2 found"),
    }
}

static VALIDATION_ERRORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Validation-layer errors reported so far in this process (for tests).
pub fn validation_error_count() -> u64 {
    VALIDATION_ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut c_void,
) -> vk::Bool32 {
    // SAFETY: the loader passes a valid callback data pointer whose message is a NUL-terminated string.
    let message = unsafe { data.as_ref().and_then(|d| d.message_as_c_str()) }
        .map(|m| m.to_string_lossy().into_owned())
        .unwrap_or_default();
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        VALIDATION_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        eprintln!("Vulkan validation: {message}");
        log::error!("Vulkan: {message}");
    } else {
        log::warn!("Vulkan: {message}");
    }
    vk::FALSE
}

/// Placeholder window type for headless contexts.
struct NoWindow;

impl HasWindowHandle for NoWindow {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::NotSupported)
    }
}

impl HasDisplayHandle for NoWindow {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::NotSupported)
    }
}
