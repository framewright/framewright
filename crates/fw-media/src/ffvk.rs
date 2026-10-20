//! Hand-written mirrors of FFmpeg 8's `libavutil/hwcontext_vulkan.h`
//! (libavutil 60, where `FF_API_VULKAN_FIXED_QUEUES` is still on), which
//! rsmpeg doesn't bind. The offsets are asserted against values printed by a
//! C program compiled with the real header.

use std::ffi::{c_char, c_int, c_void};
use std::mem::{offset_of, size_of};

use ash::vk;
use rsmpeg::ffi::{AVHWDeviceContext, AVHWFramesContext};

pub const NUM_DATA_POINTERS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct QueueFamily {
    pub idx: c_int,
    pub num: c_int,
    pub flags: vk::QueueFlags,
    pub video_caps: vk::VideoCodecOperationFlagsKHR,
}

pub type LockQueue = unsafe extern "C" fn(*mut AVHWDeviceContext, u32, u32);

#[repr(C)]
pub struct DeviceContext {
    pub alloc: *const vk::AllocationCallbacks<'static>,
    pub get_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pub inst: vk::Instance,
    pub phys_dev: vk::PhysicalDevice,
    pub act_dev: vk::Device,
    pub device_features: vk::PhysicalDeviceFeatures2<'static>,
    pub enabled_inst_extensions: *const *const c_char,
    pub nb_enabled_inst_extensions: c_int,
    pub enabled_dev_extensions: *const *const c_char,
    pub nb_enabled_dev_extensions: c_int,
    /// The deprecated fixed queue fields (five index/count pairs).
    pub fixed_queues: [c_int; 10],
    pub lock_queue: Option<LockQueue>,
    pub unlock_queue: Option<LockQueue>,
    pub qf: [QueueFamily; 64],
    pub nb_qf: c_int,
}

pub type LockFrame = unsafe extern "C" fn(*mut AVHWFramesContext, *mut VkFrame);

#[repr(C)]
pub struct FramesContext {
    pub tiling: vk::ImageTiling,
    pub usage: vk::ImageUsageFlags,
    pub create_pnext: *mut c_void,
    pub alloc_pnext: [*mut c_void; NUM_DATA_POINTERS],
    pub flags: c_int,
    pub img_flags: vk::ImageCreateFlags,
    pub format: [vk::Format; NUM_DATA_POINTERS],
    pub nb_layers: c_int,
    pub lock_frame: Option<LockFrame>,
    pub unlock_frame: Option<LockFrame>,
}

#[repr(C)]
pub struct VkFrame {
    pub img: [vk::Image; NUM_DATA_POINTERS],
    pub tiling: vk::ImageTiling,
    pub mem: [vk::DeviceMemory; NUM_DATA_POINTERS],
    pub size: [usize; NUM_DATA_POINTERS],
    pub flags: vk::MemoryPropertyFlags,
    pub access: [vk::AccessFlags; NUM_DATA_POINTERS],
    pub layout: [vk::ImageLayout; NUM_DATA_POINTERS],
    pub sem: [vk::Semaphore; NUM_DATA_POINTERS],
    pub sem_value: [u64; NUM_DATA_POINTERS],
    pub internal: *mut c_void,
    pub offset: [isize; NUM_DATA_POINTERS],
    pub queue_family: [u32; NUM_DATA_POINTERS],
}

// From gcc against FFmpeg 8.0.1's header on x86_64.
const _: () = {
    assert!(size_of::<QueueFamily>() == 16);
    assert!(size_of::<DeviceContext>() == 1400);
    assert!(offset_of!(DeviceContext, device_features) == 40);
    assert!(offset_of!(DeviceContext, enabled_dev_extensions) == 296);
    assert!(offset_of!(DeviceContext, lock_queue) == 352);
    assert!(offset_of!(DeviceContext, qf) == 368);
    assert!(offset_of!(DeviceContext, nb_qf) == 1392);
    assert!(size_of::<FramesContext>() == 144);
    assert!(offset_of!(FramesContext, flags) == 80);
    assert!(offset_of!(FramesContext, format) == 88);
    assert!(offset_of!(FramesContext, lock_frame) == 128);
    assert!(offset_of!(FramesContext, unlock_frame) == 136);
    assert!(size_of::<VkFrame>() == 504);
    assert!(offset_of!(VkFrame, layout) == 236);
    assert!(offset_of!(VkFrame, sem) == 272);
    assert!(offset_of!(VkFrame, sem_value) == 336);
    assert!(offset_of!(VkFrame, internal) == 400);
    assert!(offset_of!(VkFrame, queue_family) == 472);
};
