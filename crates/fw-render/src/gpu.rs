//! The engine's GPU device. The engine creates and owns it; a UI is a guest.

use std::ffi::CStr;
use std::sync::{Arc, Mutex, MutexGuard};

use ash::vk;
use fw_compile::VulkanVideoDevice;
use wgpu::hal::api::Vulkan;

use crate::RenderError;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    /// Held from staging semaphore waits and signals through the submit
    /// they belong to: wgpu attaches them to whichever submit comes next.
    /// Every submit to `queue` takes it, the UI's included.
    submit: Mutex<()>,
    video: Option<Video>,
}

/// What the device was given for hardware decoding.
struct Video {
    compute_family: u32,
    decode_family: u32,
    extensions: Vec<&'static CStr>,
}

/// Device extensions for decoding H.264, HEVC and AV1. Synchronization2 is
/// core in Vulkan 1.3, but FFmpeg records with it, so it's asked for too.
const VIDEO_EXTENSIONS: [&CStr; 6] = [
    c"VK_KHR_video_queue",
    c"VK_KHR_video_decode_queue",
    c"VK_KHR_video_decode_h264",
    c"VK_KHR_video_decode_h265",
    c"VK_KHR_video_decode_av1",
    c"VK_KHR_synchronization2",
];

impl Gpu {
    /// Creates a headless device. `FW_ADAPTER=<part of the name>` picks an
    /// adapter (e.g. `nvidia`, `llvmpipe`); otherwise the high-performance one.
    /// `WGPU_BACKEND` restricts the backends tried.
    ///
    /// On Vulkan with a video decode queue, the device is also set up for
    /// hardware decoding into it (design §12), unless `FW_HARDWARE_DECODE=0`.
    pub fn new() -> Result<Gpu, RenderError> {
        let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = match std::env::var("FW_ADAPTER") {
            Ok(want) => {
                let want = want.to_lowercase();
                pollster::block_on(instance.enumerate_adapters(backends))
                    .into_iter()
                    .find(|a| a.get_info().name.to_lowercase().contains(&want))
                    .ok_or_else(|| {
                        RenderError::NoAdapter(format!("FW_ADAPTER={want} matched nothing"))
                    })?
            }
            Err(_) => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            }))
            .map_err(|e| RenderError::NoAdapter(e.to_string()))?,
        };
        let info = adapter.get_info();
        let limits = wgpu::Limits {
            max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d.min(16384),
            ..wgpu::Limits::default()
        };
        let wanted = std::env::var("FW_HARDWARE_DECODE").map_or(true, |v| v != "0");
        if wanted && let Some(gpu) = Gpu::with_video(&adapter, &limits)? {
            return Ok(gpu);
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("fw-engine"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .map_err(|e| RenderError::Gpu(e.to_string()))?;
        Ok(Gpu {
            device,
            queue,
            info,
            submit: Mutex::new(()),
            video: None,
        })
    }

    /// A device with what FFmpeg's Vulkan decoder needs added to what wgpu
    /// asks for: the video extensions, a decode queue and a compute queue
    /// for the decoder alone, and the synchronization2 and YCbCr features.
    /// `None` when the adapter can't decode video.
    fn with_video(
        adapter: &wgpu::Adapter,
        limits: &wgpu::Limits,
    ) -> Result<Option<Gpu>, RenderError> {
        // P010's planes are viewed as 16-bit normalised textures.
        let features = adapter.features()
            & (wgpu::Features::TEXTURE_FORMAT_NV12
                | wgpu::Features::TEXTURE_FORMAT_P010
                | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        if !features.contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
            return Ok(None);
        }
        // SAFETY: the hal adapter is only used to open the device below.
        let Some(hal) = (unsafe { adapter.as_hal::<Vulkan>() }) else {
            return Ok(None);
        };
        let instance = hal.shared_instance().raw_instance();
        let physical = hal.raw_physical_device();
        let families = unsafe {
            let n = instance.get_physical_device_queue_family_properties2_len(physical);
            let mut video = vec![vk::QueueFamilyVideoPropertiesKHR::default(); n];
            let mut props: Vec<_> = video
                .iter_mut()
                .map(|v| vk::QueueFamilyProperties2::default().push_next(v))
                .collect();
            instance.get_physical_device_queue_family_properties2(physical, &mut props);
            let flags: Vec<_> = props
                .iter()
                .map(|p| p.queue_family_properties.queue_flags)
                .collect();
            flags.into_iter().zip(video).collect::<Vec<_>>()
        };
        // wgpu's own queue is in family 0.
        let compute = families.iter().enumerate().skip(1).position(|(_, (f, _))| {
            f.contains(vk::QueueFlags::COMPUTE) && !f.contains(vk::QueueFlags::GRAPHICS)
        });
        let decode = families.iter().position(|(f, v)| {
            f.contains(vk::QueueFlags::VIDEO_DECODE_KHR)
                && v.video_codec_operations
                    .contains(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
        });
        let (Some(compute), Some(decode)) =
            (compute.map(|i| i as u32 + 1), decode.map(|i| i as u32))
        else {
            return Ok(None);
        };
        let available: Vec<_> = unsafe { instance.enumerate_device_extension_properties(physical) }
            .map_err(|e| RenderError::Gpu(e.to_string()))?
            .iter()
            .filter_map(|e| e.extension_name_as_c_str().ok().map(CStr::to_owned))
            .collect();
        let extensions: Vec<&'static CStr> = VIDEO_EXTENSIONS
            .into_iter()
            .filter(|e| available.iter().any(|a| a.as_c_str() == *e))
            .collect();
        if !extensions.contains(&c"VK_KHR_video_decode_queue") {
            return Ok(None);
        }
        let mut sync2 =
            vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true);
        let mut ycbcr = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default()
            .sampler_ycbcr_conversion(true);
        let added = extensions.clone();
        let (sync2, ycbcr) = (&mut sync2, &mut ycbcr);
        let callback: Box<wgpu::hal::vulkan::CreateDeviceCallback<'_>> = Box::new(move |args| {
            for e in added {
                if !args.extensions.contains(&e) {
                    args.extensions.push(e);
                }
            }
            for family in [compute, decode] {
                args.queue_create_infos.push(
                    vk::DeviceQueueCreateInfo::default()
                        .queue_family_index(family)
                        .queue_priorities(&[1.0]),
                );
            }
            let info = std::mem::take(args.create_info);
            *args.create_info = info.push_next(sync2).push_next(ycbcr);
        });
        // SAFETY: the callback only adds to what wgpu asks for.
        let open = unsafe {
            hal.open_with_callback(
                features,
                limits,
                &wgpu::MemoryHints::Performance,
                Some(callback),
            )
        }
        .map_err(|e| RenderError::Gpu(e.to_string()))?;
        drop(hal);
        // SAFETY: `open` came from this adapter.
        let (device, queue) = unsafe {
            adapter.create_device_from_hal(
                open,
                &wgpu::DeviceDescriptor {
                    label: Some("fw-engine"),
                    required_features: features,
                    required_limits: limits.clone(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                },
            )
        }
        .map_err(|e| RenderError::Gpu(e.to_string()))?;
        Ok(Some(Gpu {
            device,
            queue,
            info: adapter.get_info(),
            submit: Mutex::new(()),
            video: Some(Video {
                compute_family: compute,
                decode_family: decode,
                extensions,
            }),
        }))
    }

    /// The device as a hardware decoder shares it, if it was set up for one.
    pub fn video_device(self: &Arc<Self>) -> Option<VulkanVideoDevice> {
        let video = self.video.as_ref()?;
        // SAFETY: only raw handles are read; the device outlives them through
        // `keep_alive`.
        let hal = unsafe { self.device.as_hal::<Vulkan>() }?;
        let shared = hal.shared_instance();
        let get_instance_proc_addr = shared.entry().static_fn().get_instance_proc_addr;
        Some(VulkanVideoDevice {
            get_instance_proc_addr: get_instance_proc_addr as usize,
            instance: vk::Handle::as_raw(shared.raw_instance().handle()),
            physical_device: vk::Handle::as_raw(hal.raw_physical_device()),
            device: vk::Handle::as_raw(hal.raw_device().handle()),
            render_family: hal.queue_family_index(),
            compute_family: video.compute_family,
            decode_family: video.decode_family,
            extensions: video.extensions.clone(),
            keep_alive: self.clone(),
        })
    }

    /// Takes the submit lock (see [`Gpu`]).
    pub fn lock_submits(&self) -> MutexGuard<'_, ()> {
        self.submit.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// e.g. `"NVIDIA GeForce RTX 3070 (Vulkan, driver 580.95)"`.
    pub fn describe(&self) -> String {
        format!(
            "{} ({:?}, driver {} {}{})",
            self.info.name,
            self.info.backend,
            self.info.driver,
            self.info.driver_info,
            if self.video.is_some() {
                ", hardware decoding"
            } else {
                ""
            }
        )
    }

    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}
