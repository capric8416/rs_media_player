//! D3D12 device, decode capability, texture-state and fence primitives.

use std::mem::ManuallyDrop;
use std::ptr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE, D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAGS,
    D3D12_COMMAND_QUEUE_PRIORITY_NORMAL, D3D12_FENCE_FLAG_NONE, D3D12_HEAP_FLAG_NONE,
    D3D12_HEAP_FLAGS, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_UPLOAD,
    D3D12_RANGE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
    D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_FLAGS,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_GENERIC_READ, D3D12_RESOURCE_STATES,
    D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
    D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12CreateDevice, ID3D12CommandList, ID3D12CommandQueue,
    ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_RATIONAL,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIFactory4,
};
use windows::Win32::Media::MediaFoundation::{
    D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE, D3D12_FEATURE_DATA_VIDEO_DECODE_FORMAT_COUNT,
    D3D12_FEATURE_DATA_VIDEO_DECODE_FORMATS, D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT,
    D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES, D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT,
    D3D12_FEATURE_VIDEO, D3D12_FEATURE_VIDEO_DECODE_FORMAT_COUNT,
    D3D12_FEATURE_VIDEO_DECODE_FORMATS, D3D12_FEATURE_VIDEO_DECODE_PROFILE_COUNT,
    D3D12_FEATURE_VIDEO_DECODE_PROFILES, D3D12_FEATURE_VIDEO_DECODE_SUPPORT,
    D3D12_VIDEO_DECODE_CONFIGURATION, D3D12_VIDEO_DECODE_CONFIGURATION_FLAGS,
    D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE0, D3D12_VIDEO_DECODE_PROFILE_H264,
    D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN, D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN10,
    D3D12_VIDEO_DECODE_SUPPORT_FLAG_SUPPORTED, D3D12_VIDEO_DECODE_SUPPORT_FLAGS,
    D3D12_VIDEO_DECODE_TIER, D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
    ID3D12VideoDecodeCommandList, ID3D12VideoDevice,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObjectEx};
use windows::core::{GUID, Interface};

use crate::{
    BitDepth, ChromaSampling, CodecProfile, FrameSize, PixelFormat, VideoError, VideoErrorKind,
    VideoResult, WaitStatus,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d12DeviceOptions {
    pub adapter_ordinal: u32,
    pub resource_flags: D3D12_RESOURCE_FLAGS,
    pub heap_flags: D3D12_HEAP_FLAGS,
}

impl Default for D3d12DeviceOptions {
    fn default() -> Self {
        Self {
            adapter_ordinal: 0,
            resource_flags: D3D12_RESOURCE_FLAG_NONE,
            heap_flags: D3D12_HEAP_FLAG_NONE,
        }
    }
}

/// Shared D3D12 device, video interface, resource options and advertised decode formats.
#[derive(Clone)]
pub struct D3d12Device {
    inner: Arc<D3d12DeviceInner>,
}

struct D3d12DeviceInner {
    device: ID3D12Device,
    video_device: ID3D12VideoDevice,
    capabilities: Vec<D3d12DecodeCapability>,
    resource_flags: D3D12_RESOURCE_FLAGS,
    heap_flags: D3D12_HEAP_FLAGS,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct D3d12DecodeCapability {
    pub profile: CodecProfile,
    pub bit_depth: BitDepth,
    pub chroma_sampling: ChromaSampling,
    pub output_format: PixelFormat,
    pub decoder_profile: GUID,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d12DecodeSupport {
    pub supported: bool,
    pub tier: D3D12_VIDEO_DECODE_TIER,
    pub configuration_flags: D3D12_VIDEO_DECODE_CONFIGURATION_FLAGS,
}

impl D3d12Device {
    /// Creates a D3D12 device from the selected DXGI adapter at feature level 12.0.
    ///
    /// # Errors
    ///
    /// Returns a backend error if adapter, D3D12 or video-device initialization fails.
    pub fn create(options: D3d12DeviceOptions) -> VideoResult<Self> {
        // SAFETY: The factory interface is requested as a typed COM result.
        let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(|error| backend_error("CreateDXGIFactory2", error.code().0))?;
        // SAFETY: Adapter index is selected by the caller and EnumAdapters1 returns an owned COM
        // reference. Invalid indexes are reported as a typed backend error.
        let adapter = unsafe { factory.EnumAdapters1(options.adapter_ordinal) }
            .map_err(|error| backend_error("IDXGIFactory4::EnumAdapters1", error.code().0))?;
        let mut device = None;
        // SAFETY: Adapter is a live DXGI adapter, feature level is a supported D3D12 minimum,
        // and the output pointer remains valid for the call.
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &raw mut device) }
            .map_err(|error| backend_error("D3D12CreateDevice", error.code().0))?;
        let device = device.ok_or_else(|| null_interface("D3D12CreateDevice"))?;
        Self::from_device_with_options(device, options.resource_flags, options.heap_flags)
    }

    /// Wraps a caller-owned device, querying its video interface and decode formats.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the device does not expose video decode or feature queries fail.
    pub fn from_device(device: ID3D12Device) -> VideoResult<Self> {
        Self::from_device_with_options(device, D3D12_RESOURCE_FLAG_NONE, D3D12_HEAP_FLAG_NONE)
    }

    /// Wraps a caller-owned device with resource flags applied to allocated textures.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the video interface or capability queries fail.
    pub fn from_device_with_options(
        device: ID3D12Device,
        resource_flags: D3D12_RESOURCE_FLAGS,
        heap_flags: D3D12_HEAP_FLAGS,
    ) -> VideoResult<Self> {
        let video_device: ID3D12VideoDevice = device
            .cast()
            .map_err(|error| backend_error("ID3D12Device::cast video device", error.code().0))?;
        let capabilities = discover_capabilities(&video_device)?;
        Ok(Self {
            inner: Arc::new(D3d12DeviceInner {
                device,
                video_device,
                capabilities,
                resource_flags,
                heap_flags,
            }),
        })
    }

    #[must_use]
    pub fn device(&self) -> &ID3D12Device {
        &self.inner.device
    }

    #[must_use]
    pub fn video_device(&self) -> &ID3D12VideoDevice {
        &self.inner.video_device
    }

    #[must_use]
    pub fn capabilities(&self) -> &[D3d12DecodeCapability] {
        &self.inner.capabilities
    }

    /// Creates a queue and fence for the requested D3D12 command-list type.
    ///
    /// # Errors
    ///
    /// Returns a backend error if queue or fence creation fails.
    pub fn create_queue(&self, queue_type: D3D12_COMMAND_LIST_TYPE) -> VideoResult<D3d12Queue> {
        let description = D3D12_COMMAND_QUEUE_DESC {
            Type: queue_type,
            Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
            Flags: D3D12_COMMAND_QUEUE_FLAGS(0),
            NodeMask: 0,
        };
        // SAFETY: Queue description is initialized; typed result owns the returned interface.
        let queue = unsafe {
            self.inner
                .device
                .CreateCommandQueue::<ID3D12CommandQueue>(&raw const description)
        }
        .map_err(|error| backend_error("ID3D12Device::CreateCommandQueue", error.code().0))?;
        // SAFETY: Fence is created at zero with no special flags by the owning device.
        let fence = unsafe {
            self.inner
                .device
                .CreateFence::<ID3D12Fence>(0, D3D12_FENCE_FLAG_NONE)
        }
        .map_err(|error| backend_error("ID3D12Device::CreateFence", error.code().0))?;
        Ok(D3d12Queue {
            device: self.inner.device.clone(),
            queue,
            fence,
            queue_type,
            next_fence_value: 0,
        })
    }

    /// Allocates a committed NV12/P010 texture or texture array in COMMON state.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input, unsupported or backend error when allocation is rejected.
    pub fn create_texture(
        &self,
        size: FrameSize,
        format: PixelFormat,
        array_size: u16,
    ) -> VideoResult<D3d12Texture> {
        let dxgi_format = dxgi_format(format).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d12Device::create_texture",
                "D3D12 video surfaces support NV12 and P010 in this crate",
            )
        })?;
        if array_size == 0 {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Device::create_texture",
                "texture array size must be non-zero",
            ));
        }
        if !self
            .inner
            .capabilities
            .iter()
            .any(|capability| capability.output_format == format)
        {
            return Err(VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d12Device::create_texture",
                "adapter has not advertised the requested decode surface format",
            ));
        }
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..D3D12_HEAP_PROPERTIES::default()
        };
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: u64::from(size.width()),
            Height: size.height(),
            DepthOrArraySize: array_size,
            MipLevels: 1,
            Format: dxgi_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: self.inner.resource_flags,
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: Heap and resource descriptions are initialized, dimensions/array length are
        // nonzero and format is restricted to the DXVA2-compatible decode surfaces.
        unsafe {
            self.inner.device.CreateCommittedResource(
                &raw const heap,
                self.inner.heap_flags,
                &raw const description,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &raw mut resource,
            )
        }
        .map_err(|error| backend_error("ID3D12Device::CreateCommittedResource", error.code().0))?;
        let resource =
            resource.ok_or_else(|| null_interface("ID3D12Device::CreateCommittedResource"))?;
        Ok(D3d12Texture {
            resource,
            size,
            format,
            array_size,
            states: vec![D3D12_RESOURCE_STATE_COMMON; usize::from(array_size)],
        })
    }

    /// Creates an NV12/P010 or BGRA8 output texture with the render-target flag required by D3D12
    /// video processing. Exact format/size support is checked when creating the video processor.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, unsupported or backend errors when allocation fails.
    pub fn create_video_process_texture(
        &self,
        size: FrameSize,
        format: PixelFormat,
    ) -> VideoResult<D3d12Texture> {
        let format_dxgi = match format {
            PixelFormat::Nv12 => DXGI_FORMAT_NV12,
            PixelFormat::P010 => DXGI_FORMAT_P010,
            PixelFormat::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
            PixelFormat::Rgb10A2 => {
                return Err(VideoError::new(
                    VideoErrorKind::Unsupported,
                    "D3d12Device::create_video_process_texture",
                    "RGB10A2 video-process textures are not yet supported",
                ));
            }
        };
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..D3D12_HEAP_PROPERTIES::default()
        };
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: u64::from(size.width()),
            Height: size.height(),
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format_dxgi,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: The resource description is initialized for a non-empty supported output texture
        // and carries ALLOW_RENDER_TARGET as required for video-process output resources.
        unsafe {
            self.inner.device.CreateCommittedResource(
                &raw const heap,
                D3D12_HEAP_FLAG_NONE,
                &raw const description,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &raw mut resource,
            )
        }
        .map_err(|error| {
            backend_error(
                "D3D12 video-process output texture creation",
                error.code().0,
            )
        })?;
        let resource =
            resource.ok_or_else(|| null_interface("D3D12 video-process output texture"))?;
        Ok(D3d12Texture {
            resource,
            size,
            format,
            array_size: 1,
            states: vec![D3D12_RESOURCE_STATE_COMMON],
        })
    }

    /// Creates and fills a CPU-writable GPU buffer for D3D12 compressed bitstream input.
    ///
    /// The returned upload-heap resource remains valid as long as the wrapper is retained. Use it
    /// as `D3d12InputBitstream` with offset zero and the returned byte length.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, allocation, or backend errors if the data is empty or buffer
    /// creation/mapping fails.
    pub fn create_upload_buffer(&self, data: &[u8]) -> VideoResult<D3d12UploadBuffer> {
        let byte_count = u64::try_from(data.len()).map_err(|_| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Device::create_upload_buffer",
                "upload buffer length exceeds u64",
            )
        })?;
        if byte_count == 0 {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Device::create_upload_buffer",
                "upload buffer data must not be empty",
            ));
        }
        let written_range = D3D12_RANGE {
            Begin: 0,
            End: data.len(),
        };
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_UPLOAD,
            ..D3D12_HEAP_PROPERTIES::default()
        };
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: byte_count,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT(0),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: Heap/resource descriptors are initialized for an upload buffer; the initial
        // state is GENERIC_READ as required for upload heaps and the output pointer is valid.
        unsafe {
            self.inner.device.CreateCommittedResource(
                &raw const heap,
                D3D12_HEAP_FLAG_NONE,
                &raw const description,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &raw mut resource,
            )
        }
        .map_err(|error| backend_error("CreateCommittedResource upload", error.code().0))?;
        let resource = resource.ok_or_else(|| null_interface("CreateCommittedResource upload"))?;
        let read_range = D3D12_RANGE { Begin: 0, End: 0 };
        let mut mapped = ptr::null_mut();
        // SAFETY: The committed upload resource is live, subresource zero is valid, and the
        // zero-length read range states that the CPU will not read from this resource.
        unsafe { resource.Map(0, Some(&raw const read_range), Some(&raw mut mapped)) }
            .map_err(|error| backend_error("ID3D12Resource::Map upload", error.code().0))?;
        if mapped.is_null() {
            // SAFETY: Map succeeded for subresource zero, so it must be paired with Unmap.
            unsafe { resource.Unmap(0, None) };
            return Err(null_interface("ID3D12Resource::Map upload"));
        }
        // SAFETY: Map returned a writable range of `byte_count` bytes and the source slice has the
        // same length; source and resource memory do not overlap.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast(), data.len()) };
        // SAFETY: The mapped resource is live and the written range describes exactly the bytes
        // copied above.
        unsafe { resource.Unmap(0, Some(&raw const written_range)) };
        Ok(D3d12UploadBuffer {
            resource,
            size: byte_count,
        })
    }

    /// Queries support at the supplied coded dimensions, including the driver's decode tier.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the video driver rejects the feature query.
    pub fn check_decode_support(
        &self,
        capability: &D3d12DecodeCapability,
        size: FrameSize,
    ) -> VideoResult<D3d12DecodeSupport> {
        let configuration = decode_configuration(capability.decoder_profile);
        let mut support = D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT {
            NodeIndex: 0,
            Configuration: configuration,
            Width: size.width(),
            Height: size.height(),
            DecodeFormat: dxgi_format(capability.output_format)
                .ok_or_else(|| null_interface("D3D12 decode capability format"))?,
            FrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            BitRate: 0,
            SupportFlags: D3D12_VIDEO_DECODE_SUPPORT_FLAGS::default(),
            ConfigurationFlags: D3D12_VIDEO_DECODE_CONFIGURATION_FLAGS::default(),
            DecodeTier: D3D12_VIDEO_DECODE_TIER::default(),
        };
        check_video_feature(
            &self.inner.video_device,
            D3D12_FEATURE_VIDEO_DECODE_SUPPORT,
            &mut support,
        )
        .map_err(|error| {
            d3d12_error(
                &self.inner.device,
                "ID3D12VideoDevice::CheckFeatureSupport decode",
                error,
            )
        })?;
        Ok(D3d12DecodeSupport {
            supported: support
                .SupportFlags
                .contains(D3D12_VIDEO_DECODE_SUPPORT_FLAG_SUPPORTED),
            tier: support.DecodeTier,
            configuration_flags: support.ConfigurationFlags,
        })
    }
}

/// CPU-writable upload resource retained for a D3D12 GPU read.
#[derive(Clone)]
pub struct D3d12UploadBuffer {
    resource: ID3D12Resource,
    size: u64,
}

impl D3d12UploadBuffer {
    #[must_use]
    pub fn resource(&self) -> &ID3D12Resource {
        &self.resource
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

/// A texture resource with a tracked state and array metadata.
pub struct D3d12Texture {
    resource: ID3D12Resource,
    size: FrameSize,
    format: PixelFormat,
    array_size: u16,
    states: Vec<D3D12_RESOURCE_STATES>,
}

impl D3d12Texture {
    #[must_use]
    pub fn resource(&self) -> &ID3D12Resource {
        &self.resource
    }

    #[must_use]
    pub const fn size(&self) -> FrameSize {
        self.size
    }

    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    #[must_use]
    pub const fn array_size(&self) -> u16 {
        self.array_size
    }

    /// Returns luma and chroma plane subresource indices for an NV12/P010 array slice.
    ///
    /// Texture resources created by this device have one mip level. Video decode/process calls
    /// use the first index; per-plane resource barriers must cover both indices.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the array slice is out of bounds.
    pub fn plane_subresources(&self, array_slice: u32) -> VideoResult<[u32; 2]> {
        if !matches!(self.format, PixelFormat::Nv12 | PixelFormat::P010) {
            return Err(VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d12Texture::plane_subresources",
                "the texture format does not have two video planes",
            ));
        }
        video_plane_subresources(self.array_size, array_slice)
    }

    /// Returns the common state when every array slice has the same tracked state.
    #[must_use]
    pub fn state(&self) -> Option<D3D12_RESOURCE_STATES> {
        uniform_slice_state(&self.states)
    }

    /// Returns the tracked state for both planes of one texture-array slice.
    ///
    /// # Errors
    ///
    /// Returns invalid-input when the array slice is out of bounds.
    pub fn slice_state(&self, array_slice: u32) -> VideoResult<D3D12_RESOURCE_STATES> {
        let index = usize::try_from(array_slice).map_err(|_| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Texture::slice_state",
                "array slice exceeds addressable size",
            )
        })?;
        self.states.get(index).copied().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Texture::slice_state",
                "array slice is out of bounds",
            )
        })
    }

    // Used only when a command list containing recorded transitions is abandoned before submission.
    pub(crate) fn restore_unsubmitted_slice_state(
        &mut self,
        slice: u32,
        state: D3D12_RESOURCE_STATES,
    ) {
        if let Ok(index) = usize::try_from(slice)
            && let Some(entry) = self.states.get_mut(index)
        {
            *entry = state;
        }
    }

    /// Restores bookkeeping for a slice after discarding every unsubmitted transition affecting it.
    /// The state must be the slice's last actually submitted state, and no discarded list may later
    /// be submitted. The caller is responsible for synchronizing other queues and recordings.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the slice is out of bounds.
    pub fn discard_slice_transitions(
        &mut self,
        slice: u32,
        last_submitted_state: D3D12_RESOURCE_STATES,
    ) -> VideoResult<()> {
        self.slice_state(slice)?;
        self.restore_unsubmitted_slice_state(slice, last_submitted_state);
        Ok(())
    }

    /// Records a transition on a graphics command list and updates the tracked state.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error for a no-op state transition.
    pub fn transition_graphics(
        &mut self,
        command_list: &ID3D12GraphicsCommandList,
        after: D3D12_RESOURCE_STATES,
    ) -> VideoResult<()> {
        self.transition_with(after, |barrier| {
            // SAFETY: Barrier references this live resource and tracks its current state; the
            // barrier slice remains valid for the duration of this recording call.
            unsafe { command_list.ResourceBarrier(barrier) };
        })
    }

    /// Records a transition on a D3D12 video decode command list.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error for a no-op state transition.
    pub fn transition_decode(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        after: D3D12_RESOURCE_STATES,
    ) -> VideoResult<()> {
        self.transition_with(after, |barrier| {
            // SAFETY: Barrier references this live resource and tracks its current state; the
            // barrier slice remains valid for the duration of this recording call.
            unsafe { command_list.ResourceBarrier(barrier) };
        })
    }

    /// Records decode-list transitions for both planes of one array slice.
    ///
    /// The caller must execute transitions in recorded order and synchronize all queues using the
    /// slice. If the list is discarded, the texture's tracked state must be restored by the caller.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for an invalid slice or invalid-state for a no-op transition.
    pub fn transition_decode_slice(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        array_slice: u32,
        after: D3D12_RESOURCE_STATES,
    ) -> VideoResult<()> {
        self.transition_slice_with(array_slice, after, |barriers| {
            // SAFETY: The barriers reference both planes of this live resource and remain valid
            // through the synchronous command recording call.
            unsafe { command_list.ResourceBarrier(barriers) };
        })
    }

    /// Records video-process transitions for both planes of one array slice.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for an invalid slice or invalid-state for a no-op transition.
    pub fn transition_process_slice(
        &mut self,
        command_list: &windows::Win32::Media::MediaFoundation::ID3D12VideoProcessCommandList,
        array_slice: u32,
        after: D3D12_RESOURCE_STATES,
    ) -> VideoResult<()> {
        self.transition_slice_with(array_slice, after, |barriers| {
            // SAFETY: The barriers reference both planes of this live texture array slice.
            unsafe { command_list.ResourceBarrier(barriers) };
        })
    }

    /// Records a transition on a D3D12 video-process command list.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error for a no-op state transition.
    pub fn transition_process(
        &mut self,
        command_list: &windows::Win32::Media::MediaFoundation::ID3D12VideoProcessCommandList,
        after: D3D12_RESOURCE_STATES,
    ) -> VideoResult<()> {
        self.transition_with(after, |barrier| {
            // SAFETY: Barrier references this live resource and tracked state; the descriptor
            // slice stays valid for the synchronous video-process ResourceBarrier call.
            unsafe { command_list.ResourceBarrier(barrier) };
        })
    }

    fn transition_with(
        &mut self,
        after: D3D12_RESOURCE_STATES,
        mut record: impl FnMut(&[D3D12_RESOURCE_BARRIER]),
    ) -> VideoResult<()> {
        if self.states.iter().all(|state| *state == after) {
            return validate_state_transition(after, after);
        }
        for slice in 0..u32::from(self.array_size) {
            if self.slice_state(slice)? != after {
                self.transition_slice_with(slice, after, &mut record)?;
            }
        }
        Ok(())
    }

    fn transition_slice_with(
        &mut self,
        slice: u32,
        after: D3D12_RESOURCE_STATES,
        record: impl FnOnce(&[D3D12_RESOURCE_BARRIER]),
    ) -> VideoResult<()> {
        let before = self.slice_state(slice)?;
        validate_state_transition(before, after)?;
        let (subresources, subresource_count) = self.subresources_for_slice(slice)?;
        let mut barriers = Vec::new();
        barriers
            .try_reserve_exact(subresource_count)
            .map_err(|_| allocation_error("D3D12 resource transition barriers"))?;
        for plane in subresources.into_iter().take(subresource_count) {
            let transition = D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(self.resource.clone())),
                Subresource: plane,
                StateBefore: before,
                StateAfter: after,
            };
            barriers.push(D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: ManuallyDrop::new(transition),
                },
            });
        }
        record(&barriers);
        // SAFETY: The transition member is active in this union; move out and release the COM
        // clone placed in its ManuallyDrop option after ResourceBarrier copied the descriptor.
        for barrier in &barriers {
            // SAFETY: This barrier was constructed with Transition as its active union member.
            let transition = unsafe {
                ManuallyDrop::into_inner(ptr::read(ptr::addr_of!(barrier.Anonymous.Transition)))
            };
            drop(ManuallyDrop::into_inner(transition.pResource));
        }
        self.states[usize::try_from(slice).map_err(|_| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Texture::transition",
                "slice index overflow",
            )
        })?] = after;
        Ok(())
    }

    fn subresources_for_slice(&self, slice: u32) -> VideoResult<([u32; 2], usize)> {
        if matches!(self.format, PixelFormat::Nv12 | PixelFormat::P010) {
            return Ok((video_plane_subresources(self.array_size, slice)?, 2));
        }
        if slice >= u32::from(self.array_size) {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Texture::transition",
                "array slice is out of bounds",
            ));
        }
        Ok(([slice, 0], 1))
    }
}

fn video_plane_subresources(array_size: u16, array_slice: u32) -> VideoResult<[u32; 2]> {
    if array_slice >= u32::from(array_size) {
        return Err(VideoError::new(
            VideoErrorKind::InvalidInput,
            "D3d12Texture::plane_subresources",
            "array slice is out of bounds",
        ));
    }
    Ok([array_slice, array_slice + u32::from(array_size)])
}

fn uniform_slice_state(states: &[D3D12_RESOURCE_STATES]) -> Option<D3D12_RESOURCE_STATES> {
    let first = *states.first()?;
    states.iter().all(|state| *state == first).then_some(first)
}

/// A command queue with a monotonically increasing private fence timeline.
pub struct D3d12Queue {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    queue_type: D3D12_COMMAND_LIST_TYPE,
    next_fence_value: u64,
}

/// A queue submission failure. If `submitted` is true, the GPU may still be using the resources.
#[derive(Debug)]
pub struct D3d12SubmissionError {
    pub error: VideoError,
    pub submitted: bool,
}

impl D3d12Queue {
    #[must_use]
    pub fn queue(&self) -> &ID3D12CommandQueue {
        &self.queue
    }

    #[must_use]
    pub const fn queue_type(&self) -> D3D12_COMMAND_LIST_TYPE {
        self.queue_type
    }

    /// Executes command lists on this queue.
    pub fn execute(&self, command_lists: &[ID3D12CommandList]) {
        let command_lists: Vec<_> = command_lists.iter().cloned().map(Some).collect();
        // SAFETY: Each option contains a live command-list COM reference and the slice remains
        // valid until the queue has copied the list pointers.
        unsafe { self.queue.ExecuteCommandLists(&command_lists) };
    }

    /// Checks and submits closed command lists, then signals this queue's completion fence.
    ///
    /// The caller owns command lists and every GPU resource they reference through completion.
    /// A failure with `submitted == true` means the GPU may still be using those resources; keep
    /// them alive until completion can be established or the device is torn down. The API cannot
    /// verify that lists are closed, or that their allocators have not been reset prematurely.
    ///
    /// # Errors
    ///
    /// Reports whether commands were submitted along with any validation or signaling error.
    pub fn execute_with_completion(
        &mut self,
        command_lists: &[ID3D12CommandList],
    ) -> Result<D3d12CompletionToken, D3d12SubmissionError> {
        let before = |error| D3d12SubmissionError {
            error,
            submitted: false,
        };
        if command_lists.is_empty() {
            return Err(before(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Queue::execute_with_completion",
                "at least one command list is required",
            )));
        }
        let value = self.next_fence_value.checked_add(1).ok_or_else(|| {
            before(VideoError::new(
                VideoErrorKind::InvalidState,
                "D3d12Queue::execute_with_completion",
                "fence value exhausted",
            ))
        })?;
        let event = OwnedEvent::create(&self.fence, value).map_err(before)?;
        let mut submitted_lists = Vec::new();
        submitted_lists
            .try_reserve_exact(command_lists.len())
            .map_err(|_| before(allocation_error("D3D12 queue command lists")))?;
        for list in command_lists {
            let mut device: Option<ID3D12Device> = None;
            // SAFETY: The list and its device output pointer are live during the queries.
            unsafe { list.GetDevice(&raw mut device) }.map_err(|error| {
                before(d3d12_error(
                    &self.device,
                    "ID3D12CommandList::GetDevice",
                    error.code().0,
                ))
            })?;
            // SAFETY: GetType is a read-only query on a live command list.
            let list_type = unsafe { list.GetType() };
            if device.as_ref() != Some(&self.device) || list_type != self.queue_type {
                return Err(before(VideoError::new(
                    VideoErrorKind::InvalidInput,
                    "D3d12Queue::execute_with_completion",
                    "command list device or type does not match the queue",
                )));
            }
            submitted_lists.push(Some(list.clone()));
        }
        // SAFETY: Each list is a live, device- and type-matched COM interface; caller closes lists.
        unsafe { self.queue.ExecuteCommandLists(&submitted_lists) };
        // SAFETY: This queue and fence share a device and the fence value advances monotonically.
        unsafe { self.queue.Signal(&self.fence, value) }.map_err(|error| D3d12SubmissionError {
            error: d3d12_error(&self.device, "ID3D12CommandQueue::Signal", error.code().0),
            submitted: true,
        })?;
        self.next_fence_value = value;
        Ok(D3d12CompletionToken {
            device: self.device.clone(),
            fence: self.fence.clone(),
            event: Arc::new(event),
            value,
        })
    }

    /// Signals a new fence value after all previously submitted queue work.
    ///
    /// # Errors
    ///
    /// Returns a backend or device-lost error if fence signaling or event creation fails.
    pub fn signal_completion(&mut self) -> VideoResult<D3d12CompletionToken> {
        let value = self.next_fence_value.checked_add(1).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "D3d12Queue::signal_completion",
                "fence value exhausted",
            )
        })?;
        let event = OwnedEvent::create(&self.fence, value)?;
        // SAFETY: Queue and fence belong to the same D3D12 device and value strictly advances.
        unsafe { self.queue.Signal(&self.fence, value) }.map_err(|error| {
            d3d12_error(&self.device, "ID3D12CommandQueue::Signal", error.code().0)
        })?;
        self.next_fence_value = value;
        Ok(D3d12CompletionToken {
            device: self.device.clone(),
            fence: self.fence.clone(),
            event: Arc::new(event),
            value,
        })
    }

    /// Adds a GPU-side wait for another queue's completion token.
    ///
    /// Commands submitted after this call wait for the token's fence value without blocking the
    /// CPU. Retain the token and resources through their final GPU use and record the appropriate
    /// resource barriers on the consuming command list. The token must represent the actual work
    /// producing the resource; matching devices alone does not establish that relationship.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the token belongs to another device, or a backend/device-lost
    /// error if the queue wait cannot be scheduled.
    pub fn wait_for_completion(&self, token: &D3d12CompletionToken) -> VideoResult<()> {
        if token.device() != &self.device {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d12Queue::wait_for_completion",
                "queue and fence token belong to different devices",
            ));
        }
        // SAFETY: Queue and fence are live and belong to the same device; the caller retains the
        // token while the queue's pending wait may still refer to its fence.
        unsafe { self.queue.Wait(&token.fence, token.value) }
            .map_err(|error| d3d12_error(&self.device, "ID3D12CommandQueue::Wait", error.code().0))
    }

    /// Signals the queue and waits until all earlier work has completed.
    ///
    /// # Errors
    ///
    /// Returns a backend or device-lost error if signaling or waiting fails.
    pub fn wait_idle(&mut self, timeout: Duration) -> VideoResult<WaitStatus> {
        self.signal_completion()?.wait(timeout)
    }
}

/// D3D12 fence token with a shared manual-reset event for the same fence value.
#[derive(Clone)]
pub struct D3d12CompletionToken {
    device: ID3D12Device,
    fence: ID3D12Fence,
    event: Arc<OwnedEvent>,
    value: u64,
}

impl D3d12CompletionToken {
    #[must_use]
    pub(crate) fn device(&self) -> &ID3D12Device {
        &self.device
    }

    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Returns whether the fence has reached this token's value.
    ///
    /// # Errors
    ///
    /// Returns a typed device-lost error if the fence reports removal.
    pub fn is_ready(&self) -> VideoResult<bool> {
        // SAFETY: GetCompletedValue is a read-only query on the retained fence interface.
        let completed = unsafe { self.fence.GetCompletedValue() };
        if completed == u64::MAX {
            return Err(VideoError::new(
                VideoErrorKind::DeviceLost,
                "ID3D12Fence::GetCompletedValue",
                "device removal is indicated by the fence",
            ));
        }
        Ok(completed >= self.value)
    }

    /// Waits for the fence until `timeout` expires.
    ///
    /// # Errors
    ///
    /// Returns a backend or device-lost error if event registration or waiting fails.
    pub fn wait(&self, timeout: Duration) -> VideoResult<WaitStatus> {
        if self.is_ready()? {
            return Ok(WaitStatus::Complete);
        }
        // SAFETY: Event and fence are live; this token's manual-reset event stays signaled after
        // completion, so concurrent waiters cannot consume one another's signal.
        let registration = self.event.registration.get_or_init(|| {
            // SAFETY: The event and fence remain alive for registration, and OnceLock ensures
            // exactly one pending registration uses this shared event handle.
            unsafe {
                self.fence
                    .SetEventOnCompletion(self.value, self.event.handle)
            }
            .map_err(|error| error.code().0)
        });
        registration
            .map_err(|code| d3d12_error(&self.device, "ID3D12Fence::SetEventOnCompletion", code))?;
        let deadline = std::time::Instant::now().checked_add(timeout);
        loop {
            let remaining = deadline.map_or_else(
                || Duration::from_millis(u64::from(u32::MAX - 1)),
                |end| end.saturating_duration_since(std::time::Instant::now()),
            );
            let milliseconds = timeout_millis(remaining);
            // SAFETY: The token owns a live event handle and the finite timeout fits Win32's API.
            let wait_result =
                unsafe { WaitForSingleObjectEx(self.event.handle, milliseconds, false) };
            if wait_result == WAIT_OBJECT_0 {
                if self.is_ready()? {
                    return Ok(WaitStatus::Complete);
                }
                return Err(VideoError::new(
                    VideoErrorKind::Backend,
                    "D3d12CompletionToken::wait",
                    "fence event was signaled before completion",
                ));
            }
            if wait_result == WAIT_TIMEOUT {
                if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                    return Ok(WaitStatus::TimedOut);
                }
                continue;
            }
            return Err(VideoError::new(
                VideoErrorKind::Backend,
                "WaitForSingleObjectEx",
                "fence wait event failed",
            ));
        }
    }
}

struct OwnedEvent {
    handle: HANDLE,
    fence: ID3D12Fence,
    value: u64,
    registration: OnceLock<Result<(), i32>>,
}

// SAFETY: Win32 event handles can be waited on and signaled across threads. The handle is closed
// only when the final Arc-owned token releases it, after all borrowed waits have returned.
unsafe impl Send for OwnedEvent {}
// SAFETY: Concurrent waits use a manual-reset event and do not mutate the handle's ownership.
unsafe impl Sync for OwnedEvent {}

impl OwnedEvent {
    fn create(fence: &ID3D12Fence, value: u64) -> VideoResult<Self> {
        // SAFETY: Null security/name use process-local defaults. Manual reset lets every waiter
        // observe the same fence completion without consuming the signal.
        let event = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|error| backend_error("CreateEventW", error.code().0))?;
        Ok(Self {
            handle: event,
            fence: fence.clone(),
            value,
            registration: OnceLock::new(),
        })
    }
}

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        if self.registration.get().is_some_and(Result::is_ok) {
            // SAFETY: The retained fence is live for this read-only completion query.
            let completed = unsafe { self.fence.GetCompletedValue() };
            // SAFETY: The event handle is live; a zero timeout does not block in Drop.
            let signaled = unsafe { WaitForSingleObjectEx(self.handle, 0, false) } == WAIT_OBJECT_0;
            if completed == u64::MAX || completed < self.value || !signaled {
                // A registered fence may still reference this event after a timed-out wait.
                // Keep the handle open when completion cannot be established.
                return;
            }
        }
        // SAFETY: This wrapper owns the handle and no pending fence registration can use it.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

fn discover_capabilities(
    video_device: &ID3D12VideoDevice,
) -> VideoResult<Vec<D3d12DecodeCapability>> {
    let mut profile_count = D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT {
        NodeIndex: 0,
        ProfileCount: 0,
    };
    check_video_feature(
        video_device,
        D3D12_FEATURE_VIDEO_DECODE_PROFILE_COUNT,
        &mut profile_count,
    )
    .map_err(|error| backend_error("D3D12 decode profile count query", error))?;
    if profile_count.ProfileCount == 0 {
        return Ok(Vec::new());
    }
    let mut profiles = Vec::new();
    profiles
        .try_reserve_exact(profile_count.ProfileCount as usize)
        .map_err(|_| allocation_error("decode profile list"))?;
    profiles.resize(profile_count.ProfileCount as usize, GUID::zeroed());
    let mut profile_data = D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES {
        NodeIndex: 0,
        ProfileCount: profile_count.ProfileCount,
        pProfiles: profiles.as_mut_ptr(),
    };
    check_video_feature(
        video_device,
        D3D12_FEATURE_VIDEO_DECODE_PROFILES,
        &mut profile_data,
    )
    .map_err(|error| backend_error("D3D12 decode profiles query", error))?;

    let mut capabilities = Vec::new();
    for decoder_profile in profiles {
        for (profile, bit_depth, output_format) in codec_formats() {
            if !known_profile_guids(profile, bit_depth).contains(&decoder_profile) {
                continue;
            }
            let configuration = decode_configuration(decoder_profile);
            let mut count_data = D3D12_FEATURE_DATA_VIDEO_DECODE_FORMAT_COUNT {
                NodeIndex: 0,
                Configuration: configuration,
                FormatCount: 0,
            };
            if check_video_feature(
                video_device,
                D3D12_FEATURE_VIDEO_DECODE_FORMAT_COUNT,
                &mut count_data,
            )
            .is_err()
                || count_data.FormatCount == 0
            {
                continue;
            }
            let mut formats = Vec::new();
            formats
                .try_reserve_exact(count_data.FormatCount as usize)
                .map_err(|_| allocation_error("decode output format list"))?;
            formats.resize(count_data.FormatCount as usize, DXGI_FORMAT(0));
            let mut formats_data = D3D12_FEATURE_DATA_VIDEO_DECODE_FORMATS {
                NodeIndex: 0,
                Configuration: configuration,
                FormatCount: count_data.FormatCount,
                pOutputFormats: formats.as_mut_ptr(),
            };
            if check_video_feature(
                video_device,
                D3D12_FEATURE_VIDEO_DECODE_FORMATS,
                &mut formats_data,
            )
            .is_err()
            {
                continue;
            }
            if dxgi_format(output_format).is_some_and(|format| formats.contains(&format)) {
                capabilities.push(D3d12DecodeCapability {
                    profile,
                    bit_depth,
                    chroma_sampling: ChromaSampling::Yuv420,
                    output_format,
                    decoder_profile,
                });
            }
        }
    }
    Ok(capabilities)
}

fn check_video_feature<T>(
    video_device: &ID3D12VideoDevice,
    feature: D3D12_FEATURE_VIDEO,
    data: &mut T,
) -> Result<(), i32> {
    let size = u32::try_from(std::mem::size_of::<T>()).map_err(|_| -1)?;
    // SAFETY: `data` is a live, correctly typed SDK feature structure whose size matches `T`.
    unsafe { video_device.CheckFeatureSupport(feature, (&raw mut *data).cast(), size) }
        .map_err(|error| error.code().0)
}

pub(crate) fn decode_configuration(profile: GUID) -> D3D12_VIDEO_DECODE_CONFIGURATION {
    D3D12_VIDEO_DECODE_CONFIGURATION {
        DecodeProfile: profile,
        BitstreamEncryption: D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE,
        InterlaceType: D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
    }
}

fn validate_state_transition(
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> VideoResult<()> {
    if before == after {
        return Err(VideoError::new(
            VideoErrorKind::InvalidState,
            "D3d12Texture::transition",
            "resource already has the requested state",
        ));
    }
    Ok(())
}

fn timeout_millis(timeout: Duration) -> u32 {
    match u32::try_from(timeout.as_millis()) {
        Ok(milliseconds) => milliseconds.min(u32::MAX - 1),
        Err(_) => u32::MAX - 1,
    }
}

fn codec_formats() -> [(CodecProfile, BitDepth, PixelFormat); 7] {
    [
        (
            CodecProfile::AvcBaseline,
            BitDepth::Eight,
            PixelFormat::Nv12,
        ),
        (CodecProfile::AvcMain, BitDepth::Eight, PixelFormat::Nv12),
        (CodecProfile::AvcHigh, BitDepth::Eight, PixelFormat::Nv12),
        (CodecProfile::HevcMain, BitDepth::Eight, PixelFormat::Nv12),
        (CodecProfile::HevcMain10, BitDepth::Ten, PixelFormat::P010),
        (CodecProfile::Av1Main, BitDepth::Eight, PixelFormat::Nv12),
        (CodecProfile::Av1Main, BitDepth::Ten, PixelFormat::P010),
    ]
}

fn known_profile_guids(profile: CodecProfile, bit_depth: BitDepth) -> Vec<GUID> {
    match (profile, bit_depth) {
        (
            CodecProfile::AvcBaseline | CodecProfile::AvcMain | CodecProfile::AvcHigh,
            BitDepth::Eight,
        ) => vec![D3D12_VIDEO_DECODE_PROFILE_H264],
        (CodecProfile::HevcMain, BitDepth::Eight) => vec![D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN],
        (CodecProfile::HevcMain10, BitDepth::Ten) => {
            vec![D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN10]
        }
        (CodecProfile::Av1Main, BitDepth::Eight | BitDepth::Ten) => {
            vec![D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE0]
        }
        _ => Vec::new(),
    }
}

pub(crate) fn dxgi_format(format: PixelFormat) -> Option<DXGI_FORMAT> {
    match format {
        PixelFormat::Nv12 => Some(DXGI_FORMAT_NV12),
        PixelFormat::P010 => Some(DXGI_FORMAT_P010),
        PixelFormat::Bgra8 | PixelFormat::Rgb10A2 => None,
    }
}

fn allocation_error(what: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::Backend,
        "D3d12Device",
        format!("unable to allocate {what}"),
    )
}

fn null_interface(operation: &'static str) -> VideoError {
    backend_error(operation, 0x8000_4003_u32.cast_signed())
}

fn backend_error(operation: &'static str, code: i32) -> VideoError {
    let kind = crate::error::backend_kind_from_hresult(code);
    VideoError::new(kind, operation, "D3D12 operation failed").with_backend_code(code)
}

fn d3d12_error(device: &ID3D12Device, operation: &'static str, code: i32) -> VideoError {
    // SAFETY: This read-only device query is valid for the retained ID3D12Device.
    let removed = unsafe { device.GetDeviceRemovedReason() }.is_err();
    if removed || matches!(code.cast_unsigned(), 0x887a_0005..=0x887a_0007) {
        VideoError::new(
            VideoErrorKind::DeviceLost,
            operation,
            "D3D12 device was removed or reset",
        )
        .with_backend_code(code)
    } else {
        backend_error(operation, code)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        codec_formats, dxgi_format, known_profile_guids, timeout_millis, validate_state_transition,
    };
    use crate::{BitDepth, CodecProfile, PixelFormat};
    use std::time::Duration;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010};

    #[test]
    fn maps_supported_d3d12_profiles_and_bit_depths() {
        for (profile, depth, _) in codec_formats() {
            assert_ne!(known_profile_guids(profile, depth).len(), 0);
        }
        assert_eq!(
            known_profile_guids(CodecProfile::HevcMain10, BitDepth::Eight).len(),
            0
        );
        assert_eq!(
            known_profile_guids(CodecProfile::Av1Main, BitDepth::Eight),
            known_profile_guids(CodecProfile::Av1Main, BitDepth::Ten)
        );
    }

    #[test]
    fn maps_only_d3d12_decode_surface_formats() {
        assert_eq!(dxgi_format(PixelFormat::Nv12), Some(DXGI_FORMAT_NV12));
        assert_eq!(dxgi_format(PixelFormat::P010), Some(DXGI_FORMAT_P010));
        assert_eq!(dxgi_format(PixelFormat::Bgra8), None);
    }

    #[test]
    fn tracks_state_changes_and_rejects_noop_barriers() {
        use windows::Win32::Graphics::Direct3D12::{
            D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_DECODE_READ,
        };

        assert!(
            validate_state_transition(
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
            )
            .is_ok()
        );
        assert_eq!(
            validate_state_transition(D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COMMON)
                .err()
                .map(|error| error.kind()),
            Some(crate::VideoErrorKind::InvalidState)
        );
    }

    #[test]
    fn clamps_wait_slices_to_win32_timeout_limit() {
        assert_eq!(timeout_millis(Duration::ZERO), 0);
        assert_eq!(timeout_millis(Duration::from_millis(20)), 20);
        assert_eq!(timeout_millis(Duration::from_secs(u64::MAX)), u32::MAX - 1);
    }

    #[test]
    fn completion_token_can_be_shared_across_threads() {
        fn assert_traits<T: Clone + Send + Sync>() {}
        assert_traits::<super::D3d12CompletionToken>();
    }

    #[test]
    fn finds_both_planes_for_each_video_array_slice() {
        assert_eq!(super::video_plane_subresources(1, 0).ok(), Some([0, 1]));
        assert_eq!(super::video_plane_subresources(4, 2).ok(), Some([2, 6]));
        assert_eq!(super::video_plane_subresources(4, 3).ok(), Some([3, 7]));
        assert!(super::video_plane_subresources(4, 4).is_err());
        assert!(super::video_plane_subresources(0, 0).is_err());
    }

    #[test]
    fn common_state_requires_all_slices_to_match() {
        use windows::Win32::Graphics::Direct3D12::{
            D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_DECODE_READ,
        };
        assert_eq!(super::uniform_slice_state(&[]), None);
        assert_eq!(
            super::uniform_slice_state(&[D3D12_RESOURCE_STATE_COMMON; 2]),
            Some(D3D12_RESOURCE_STATE_COMMON)
        );
        assert_eq!(
            super::uniform_slice_state(&[
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
            ]),
            None
        );
    }
}
