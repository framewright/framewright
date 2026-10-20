//! Hardware decoding (design §12): FFmpeg's Vulkan decoder, on the engine's
//! own device, so decoded pictures never leave the GPU.
//!
//! The renderer creates the device with a decode queue and a compute queue
//! it never uses, and hands both to FFmpeg with the device's handles. The
//! renderer's own queue family is listed too, only so decoded images are
//! shared with it; nothing asks FFmpeg for graphics, so it never submits
//! there.

use std::sync::{Arc, Mutex};

use ash::vk;
use fw_compile::{Acquired, HardwareFrame, VulkanVideoDevice};
use rsmpeg::avutil::AVHWDeviceContext;
use rsmpeg::ffi;

use crate::ffvk;

/// FFmpeg's view of the engine's device, shared by every hardware reader.
pub struct HardwareDevice {
    context: AVHWDeviceContext,
    device: ash::Device,
    compute_family: u32,
    queue: vk::Queue,
    /// Command buffers for moving decoded images out of the decoder's
    /// layout, used in turn, each with a fence for its last use.
    ring: Mutex<Ring>,
    pool: vk::CommandPool,
    /// Which decoder is faster for each kind of video, once measured.
    choices: Mutex<Vec<DecoderChoice>>,
    /// Held while measuring, so one kind of video is measured once.
    measuring: Mutex<()>,
    /// Keeps the device alive as long as this does.
    _vulkan: VulkanVideoDevice,
}

/// Which decoder a kind of video uses, and how they compared.
#[derive(Debug, Clone, PartialEq)]
pub struct DecoderChoice {
    /// The codec, profile, pixel format and size.
    pub format: String,
    pub hardware: bool,
    /// Decoding the calibration frames, the best of two tries each.
    pub hardware_time: std::time::Duration,
    pub software_time: std::time::Duration,
}

/// Hardware decoding is chosen unless software is more than this much
/// faster: it leaves the CPU free for everything else.
pub const GPU_BIAS: f64 = 1.25;

/// Frames decoded from the start of a file to compare the decoders: a
/// keyframe and the frames after it, as a seek decodes.
const CALIBRATION_FRAMES: u64 = 24;

/// Whether hardware decoding taking `hardware` beats software taking
/// `software`, with the bias towards the GPU.
pub fn prefers_hardware(hardware: std::time::Duration, software: std::time::Duration) -> bool {
    hardware.as_secs_f64() <= software.as_secs_f64() * GPU_BIAS
}

struct Ring {
    slots: Vec<(vk::CommandBuffer, vk::Fence)>,
    next: usize,
}

const RING: usize = 4;

// SAFETY: the FFmpeg context is only used through FFmpeg, which locks its
// queues and frames; the Vulkan objects here are guarded by `ring`.
unsafe impl Send for HardwareDevice {}
unsafe impl Sync for HardwareDevice {}

/// The FFmpeg context of each device in use, by `VkDevice`. Queues need
/// their submits serialised, and FFmpeg's locks are per context, so every
/// decoder on a device must share one.
static DEVICES: Mutex<Vec<(u64, std::sync::Weak<HardwareDevice>)>> = Mutex::new(Vec::new());

impl HardwareDevice {
    /// FFmpeg's context for the device, created on first use and shared.
    pub fn new(vulkan: VulkanVideoDevice) -> Result<Arc<HardwareDevice>, String> {
        let mut devices = DEVICES.lock().unwrap_or_else(|e| e.into_inner());
        // First, so a dead device's handle, which a new device may reuse,
        // can't match.
        devices.retain(|(_, d)| d.strong_count() > 0);
        if let Some(device) = devices
            .iter()
            .find(|(handle, _)| *handle == vulkan.device)
            .and_then(|(_, d)| d.upgrade())
        {
            return Ok(device);
        }
        let device = HardwareDevice::create(vulkan)?;
        devices.push((device._vulkan.device, Arc::downgrade(&device)));
        Ok(device)
    }

    fn create(vulkan: VulkanVideoDevice) -> Result<Arc<HardwareDevice>, String> {
        // SAFETY: the handles are the renderer's live device, kept alive by
        // `vulkan.keep_alive`; `get_instance_proc_addr` is the loader's.
        unsafe {
            let static_fn = ash::StaticFn {
                get_instance_proc_addr: std::mem::transmute::<usize, vk::PFN_vkGetInstanceProcAddr>(
                    vulkan.get_instance_proc_addr,
                ),
            };
            let instance = ash::Instance::load(&static_fn, vk::Handle::from_raw(vulkan.instance));
            let device = ash::Device::load(instance.fp_v1_0(), vk::Handle::from_raw(vulkan.device));
            let mut context = AVHWDeviceContext::alloc(ffi::AV_HWDEVICE_TYPE_VULKAN);
            let ctx = (*context.as_ptr()).data as *mut ffi::AVHWDeviceContext;
            let vk_ctx = &mut *((*ctx).hwctx as *mut ffvk::DeviceContext);
            vk_ctx.get_proc_addr = Some(static_fn.get_instance_proc_addr);
            vk_ctx.inst = vk::Handle::from_raw(vulkan.instance);
            vk_ctx.phys_dev = vk::Handle::from_raw(vulkan.physical_device);
            vk_ctx.act_dev = vk::Handle::from_raw(vulkan.device);
            vk_ctx.device_features = vk::PhysicalDeviceFeatures2::default();
            let names: Vec<_> = vulkan.extensions.iter().map(|e| e.as_ptr()).collect();
            // FFmpeg keeps the pointer; the names are static and so is this list.
            let names: &'static [*const std::ffi::c_char] = Box::leak(names.into_boxed_slice());
            vk_ctx.enabled_dev_extensions = names.as_ptr();
            vk_ctx.nb_enabled_dev_extensions = names.len() as i32;
            // FFmpeg picks queues by flags, first match first.
            let family = |idx: u32, flags| ffvk::QueueFamily {
                idx: idx as i32,
                num: 1,
                flags,
                video_caps: vk::VideoCodecOperationFlagsKHR::empty(),
            };
            vk_ctx.qf[0] = family(
                vulkan.compute_family,
                vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER,
            );
            vk_ctx.qf[1] = family(vulkan.decode_family, vk::QueueFlags::VIDEO_DECODE_KHR);
            vk_ctx.qf[2] = family(vulkan.render_family, vk::QueueFlags::GRAPHICS);
            vk_ctx.nb_qf = 3;
            context
                .init()
                .map_err(|e| format!("FFmpeg won't use the device: {e}"))?;

            let pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(vulkan.compute_family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(|e| e.to_string())?;
            let buffers = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(RING as u32),
                )
                .map_err(|e| e.to_string())?;
            let slots = buffers
                .into_iter()
                .map(|b| {
                    let fence = device.create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )?;
                    Ok((b, fence))
                })
                .collect::<Result<Vec<_>, vk::Result>>()
                .map_err(|e| e.to_string())?;
            let queue = device.get_device_queue(vulkan.compute_family, 0);
            Ok(Arc::new(HardwareDevice {
                context,
                device,
                compute_family: vulkan.compute_family,
                queue,
                ring: Mutex::new(Ring { slots, next: 0 }),
                pool,
                choices: Mutex::new(Vec::new()),
                measuring: Mutex::new(()),
                _vulkan: vulkan,
            }))
        }
    }

    /// Whether to decode `path` (already probed as `info`) in hardware: the
    /// faster decoder for its kind of video, measured the first time.
    pub(crate) fn choose(
        self: &Arc<Self>,
        path: &std::path::Path,
        info: &crate::VideoInfo,
    ) -> bool {
        let format = format!("{} {}×{}", info.format, info.width, info.height);
        let known = |choices: &Vec<DecoderChoice>| {
            choices
                .iter()
                .find(|c| c.format == format)
                .map(|c| c.hardware)
        };
        if let Some(hardware) = known(&self.choices.lock().unwrap_or_else(|e| e.into_inner())) {
            return hardware;
        }
        let _measuring = self.measuring.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hardware) = known(&self.choices.lock().unwrap_or_else(|e| e.into_inner())) {
            return hardware;
        }
        let choice = self.measure(path, info, format);
        let hardware = choice.hardware;
        self.choices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(choice);
        hardware
    }

    /// Times both decoders on the start of the file, twice each, keeping
    /// the faster try. Hardware is timed until the GPU has finished.
    fn measure(
        self: &Arc<Self>,
        path: &std::path::Path,
        info: &crate::VideoInfo,
        format: String,
    ) -> DecoderChoice {
        use crate::VideoReader;
        use std::time::{Duration, Instant};
        let times: Vec<_> = (0..info.frames.count().min(CALIBRATION_FRAMES))
            .filter_map(|i| info.frames.time_of(i))
            .collect();
        let run = |reader: &mut VideoReader, hardware: bool| -> Option<Duration> {
            let started = Instant::now();
            for at in &times {
                let decoded = reader.frame_decoded(*at).ok()?;
                if hardware && !matches!(decoded, fw_compile::Decoded::Hardware(_)) {
                    return None;
                }
            }
            reader.wait_for_gpu().ok()?;
            Some(started.elapsed())
        };
        let best = |reader: Option<VideoReader>, hardware: bool| {
            let mut reader = reader?;
            let first = run(&mut reader, hardware)?;
            Some(first.min(run(&mut reader, hardware)?))
        };
        let software = best(VideoReader::open_probed(path, info).ok(), false);
        let hardware = best(
            VideoReader::open_hardware(path, info, self.clone()).ok(),
            true,
        );
        let (hardware_time, software_time) = (
            hardware.unwrap_or(Duration::MAX),
            software.unwrap_or(Duration::MAX),
        );
        DecoderChoice {
            format,
            hardware: hardware.is_some() && prefers_hardware(hardware_time, software_time),
            hardware_time,
            software_time,
        }
    }

    /// The choices made so far.
    pub fn choices(&self) -> Vec<DecoderChoice> {
        self.choices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Waits until `frame`, from a decoder on this device, is decoded.
    pub(crate) fn wait_decoded(&self, frame: &rsmpeg::avutil::AVFrame) -> Result<(), String> {
        // SAFETY: the frame's state is read under its lock.
        unsafe {
            let vkf = frame.data[0] as *mut ffvk::VkFrame;
            let frames = (*frame.hw_frames_ctx).data as *mut ffi::AVHWFramesContext;
            let fc = &*((*frames).hwctx as *const ffvk::FramesContext);
            (fc.lock_frame.expect("FFmpeg sets it"))(frames, vkf);
            let (semaphore, value) = ((*vkf).sem[0], (*vkf).sem_value[0]);
            (fc.unlock_frame.expect("FFmpeg sets it"))(frames, vkf);
            self.device
                .wait_semaphores(
                    &vk::SemaphoreWaitInfo::default()
                        .semaphores(&[semaphore])
                        .values(&[value]),
                    u64::MAX,
                )
                .map_err(|e| e.to_string())
        }
    }

    /// The context to give a decoder.
    pub(crate) fn context(&self) -> &AVHWDeviceContext {
        &self.context
    }

    /// Records and submits, on FFmpeg's compute queue, a barrier moving
    /// `image` from `layout` to one shaders read, once `semaphore` reaches
    /// `value`; it then signals `value + 1`.
    ///
    /// # Safety
    /// `image` and `semaphore` belong to a frame of this device, locked.
    unsafe fn move_to_shader_read(
        &self,
        image: vk::Image,
        layout: vk::ImageLayout,
        semaphore: vk::Semaphore,
        value: u64,
    ) -> Result<(), String> {
        let d = &self.device;
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let (cmd, fence) = ring.slots[ring.next];
        ring.next = (ring.next + 1) % RING;
        let err = |e: vk::Result| e.to_string();
        unsafe {
            d.wait_for_fences(&[fence], true, u64::MAX).map_err(err)?;
            d.reset_fences(&[fence]).map_err(err)?;
            d.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(err)?;
            let barrier = vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
                .old_layout(layout)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                // Images are shared between the queue families (concurrent).
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            d.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&[barrier]),
            );
            d.end_command_buffer(cmd).map_err(err)?;
            let wait = [vk::SemaphoreSubmitInfo::default()
                .semaphore(semaphore)
                .value(value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let signal = [vk::SemaphoreSubmitInfo::default()
                .semaphore(semaphore)
                .value(value + 1)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let ctx = (*self.context.as_ptr()).data as *mut ffi::AVHWDeviceContext;
            let vk_ctx = &*((*ctx).hwctx as *const ffvk::DeviceContext);
            (vk_ctx.lock_queue.expect("FFmpeg sets it"))(ctx, self.compute_family, 0);
            let submitted = d.queue_submit2(
                self.queue,
                &[vk::SubmitInfo2::default()
                    .wait_semaphore_infos(&wait)
                    .signal_semaphore_infos(&signal)
                    .command_buffer_infos(&cmds)],
                fence,
            );
            (vk_ctx.unlock_queue.expect("FFmpeg sets it"))(ctx, self.compute_family, 0);
            submitted.map_err(err)
        }
    }
}

impl Drop for HardwareDevice {
    fn drop(&mut self) {
        let ring = self.ring.get_mut().unwrap_or_else(|e| e.into_inner());
        // SAFETY: waiting for the last uses before destroying what they used.
        unsafe {
            let fences: Vec<_> = ring.slots.iter().map(|(_, f)| *f).collect();
            let _ = self.device.wait_for_fences(&fences, true, u64::MAX);
            for f in fences {
                self.device.destroy_fence(f, None);
            }
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}

/// A decoded image, held by a reference to its frame.
pub(crate) struct VulkanFrame {
    frame: *mut ffi::AVFrame,
    device: Arc<HardwareDevice>,
}

// SAFETY: the frame is only touched under FFmpeg's frame lock, and freeing a
// reference is thread-safe.
unsafe impl Send for VulkanFrame {}
unsafe impl Sync for VulkanFrame {}

impl VulkanFrame {
    /// A new reference to `frame`, a hardware frame from a decoder on `device`.
    pub(crate) fn new(frame: &rsmpeg::avutil::AVFrame, device: Arc<HardwareDevice>) -> VulkanFrame {
        VulkanFrame {
            frame: unsafe { ffi::av_frame_clone(&**frame) },
            device,
        }
    }

    /// The frame's Vulkan state and its frames context.
    ///
    /// # Safety
    /// Fields of the returned `VkFrame` may only be changed under its lock.
    unsafe fn parts(
        &self,
    ) -> (
        *mut ffvk::VkFrame,
        *mut ffi::AVHWFramesContext,
        &ffvk::FramesContext,
    ) {
        unsafe {
            let vkf = (*self.frame).data[0] as *mut ffvk::VkFrame;
            let frames = (*(*self.frame).hw_frames_ctx).data as *mut ffi::AVHWFramesContext;
            (
                vkf,
                frames,
                &*((*frames).hwctx as *const ffvk::FramesContext),
            )
        }
    }
}

impl HardwareFrame for VulkanFrame {
    /// Under the frame's lock: after decoding (semaphore value `v`), move
    /// the image to a shader-readable layout (signalling `v + 1`), and
    /// reserve `v + 2` for the reader. The lock isn't held after, so the
    /// decoder can use the frame as a reference meanwhile; its own work on
    /// the frame waits for `v + 2` on the GPU.
    fn acquire(&self) -> Result<Acquired, String> {
        // SAFETY: the frame is changed only between lock and unlock.
        unsafe {
            let (vkf, frames, fc) = self.parts();
            (fc.lock_frame.expect("FFmpeg sets it"))(frames, vkf);
            let vkf = &mut *vkf;
            let value = vkf.sem_value[0];
            let result =
                self.device
                    .move_to_shader_read(vkf.img[0], vkf.layout[0], vkf.sem[0], value);
            if result.is_ok() {
                vkf.layout[0] = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                vkf.access[0] = vk::AccessFlags::SHADER_READ;
                vkf.sem_value[0] = value + 2;
            }
            (fc.unlock_frame.expect("FFmpeg sets it"))(frames, vkf);
            result?;
            Ok(Acquired {
                image: vk::Handle::as_raw(vkf.img[0]),
                semaphore: vk::Handle::as_raw(vkf.sem[0]),
                wait: value + 1,
                signal: value + 2,
            })
        }
    }

    fn abandon(&self, acquired: &Acquired) {
        let semaphore = vk::Handle::from_raw(acquired.semaphore);
        let d = &self.device.device;
        // A host signal must come after the barrier's own signal.
        // SAFETY: the semaphore is the frame's, which this holds.
        unsafe {
            let _ = d.wait_semaphores(
                &vk::SemaphoreWaitInfo::default()
                    .semaphores(&[semaphore])
                    .values(&[acquired.wait]),
                u64::MAX,
            );
            let _ = d.signal_semaphore(
                &vk::SemaphoreSignalInfo::default()
                    .semaphore(semaphore)
                    .value(acquired.signal),
            );
        }
    }
}

impl Drop for VulkanFrame {
    fn drop(&mut self) {
        unsafe { ffi::av_frame_free(&mut self.frame) };
    }
}

/// For a decoder's `get_format`: Vulkan if FFmpeg offers it, otherwise the
/// first software format, so streams the hardware can't decode (or whose
/// hardware setup fails, when FFmpeg asks again) decode in software.
pub(crate) unsafe extern "C" fn pick_vulkan(
    _: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let mut software = ffi::AV_PIX_FMT_NONE;
    let mut f = formats;
    // SAFETY: FFmpeg passes a list ended by `AV_PIX_FMT_NONE`.
    unsafe {
        while *f != ffi::AV_PIX_FMT_NONE {
            if *f == ffi::AV_PIX_FMT_VULKAN {
                return *f;
            }
            let desc = ffi::av_pix_fmt_desc_get(*f);
            let hw = !desc.is_null() && (*desc).flags & ffi::AV_PIX_FMT_FLAG_HWACCEL as u64 != 0;
            if !hw && software == ffi::AV_PIX_FMT_NONE {
                software = *f;
            }
            f = f.add(1);
        }
    }
    software
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn hardware_wins_ties_and_near_ties() {
        let ms = Duration::from_millis;
        assert!(prefers_hardware(ms(50), ms(80)));
        assert!(prefers_hardware(ms(100), ms(100)));
        assert!(prefers_hardware(ms(120), ms(100)), "within the bias");
        assert!(!prefers_hardware(ms(130), ms(100)));
        assert!(
            !prefers_hardware(ms(292), ms(150)),
            "the Ryzen's integrated GPU"
        );
    }
}
