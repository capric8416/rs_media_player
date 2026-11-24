//! D3D12 video-decoder and heap creation plus checked `DecodeFrame` argument assembly.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE, D3D12_RESOURCE_DIMENSION_BUFFER,
    D3D12_RESOURCE_STATE_VIDEO_DECODE_READ, D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE,
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12Device, ID3D12PipelineState, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_RATIONAL;
use windows::Win32::Media::MediaFoundation::{
    D3D12_VIDEO_DECODE_ARGUMENT_TYPE, D3D12_VIDEO_DECODE_ARGUMENT_TYPE_HUFFMAN_TABLE,
    D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
    D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
    D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL, D3D12_VIDEO_DECODE_COMPRESSED_BITSTREAM,
    D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_HEIGHT_ALIGNMENT_MULTIPLE_32_REQUIRED,
    D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_REFERENCE_ONLY_ALLOCATIONS_REQUIRED,
    D3D12_VIDEO_DECODE_FRAME_ARGUMENT, D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS,
    D3D12_VIDEO_DECODE_OUTPUT_STREAM_ARGUMENTS, D3D12_VIDEO_DECODE_REFERENCE_FRAMES,
    D3D12_VIDEO_DECODE_TIER, D3D12_VIDEO_DECODE_TIER_1, D3D12_VIDEO_DECODER_DESC,
    D3D12_VIDEO_DECODER_HEAP_DESC, ID3D12VideoDecodeCommandList, ID3D12VideoDecoder,
    ID3D12VideoDecoderHeap,
};
use windows::core::Interface;

use crate::avc::{AvcDecodeState, PreparedAvcPicture};
use crate::d3d12_device::{
    D3d12CompletionToken, D3d12DecodeCapability, D3d12Device, D3d12Queue, D3d12SubmissionError,
    D3d12Texture, D3d12UploadBuffer, decode_configuration, dxgi_format,
};
use crate::hevc::{HevcDecodeState, PreparedHevcPicture};
use crate::{
    BitstreamFormat, Codec, CodecDecodeState, ColorInfo, D3dDecode, DecodeCapability,
    DecoderConfig, EncodedSample, FrameInfo, FrameSize, VideoError, VideoErrorKind, VideoResult,
    WaitStatus,
};

const MAX_D3D12_FRAME_ARGUMENTS: usize = 10;
const MAX_D3D12_REFERENCE_FRAMES: usize = 16;

/// One inline codec argument copied into D3D12's transient frame-argument array.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct D3d12DecodeArgument {
    pub kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE,
    pub data: Vec<u8>,
}

/// GPU-resident compressed bitstream range consumed by `DecodeFrame`.
#[derive(Clone, Debug)]
pub struct D3d12InputBitstream {
    pub resource: ID3D12Resource,
    pub offset: u64,
    pub size: u64,
}

impl D3d12InputBitstream {
    /// Uses the complete byte range of an upload buffer as the compressed bitstream.
    #[must_use]
    pub fn from_upload_buffer(buffer: &D3d12UploadBuffer) -> Self {
        Self {
            resource: buffer.resource().clone(),
            offset: 0,
            size: buffer.size(),
        }
    }
}

/// D3D12 decoder and heap for one codec profile and coded size.
pub struct D3d12Decoder {
    device: D3d12Device,
    decoder: ID3D12VideoDecoder,
    heap: ID3D12VideoDecoderHeap,
    size: FrameSize,
    surface_size: FrameSize,
    output_format: crate::PixelFormat,
    capability: D3d12DecodeCapability,
    max_reference_frames: u32,
    tier: D3D12_VIDEO_DECODE_TIER,
}

/// Prepared AVC buffers and deferred DPB commit for one D3D12 picture.
pub struct D3d12PreparedAvcPicture<'decoder, 'state> {
    decoder: &'decoder D3d12Decoder,
    state: &'state mut AvcDecodeState,
    prepared: PreparedAvcPicture,
    target_surface: u8,
    bitstream: D3d12UploadBuffer,
    arguments: Vec<D3d12DecodeArgument>,
    recorded: bool,
    recorded_list: Option<ID3D12VideoDecodeCommandList>,
    recorded_resources: Vec<ID3D12Resource>,
}

/// Prepared HEVC buffers and deferred DPB commit for one D3D12 picture.
pub struct D3d12PreparedHevcPicture<'decoder, 'state> {
    decoder: &'decoder D3d12Decoder,
    state: &'state mut HevcDecodeState,
    prepared: PreparedHevcPicture,
    target_surface: u8,
    bitstream: D3d12UploadBuffer,
    arguments: Vec<D3d12DecodeArgument>,
    recorded: bool,
    recorded_list: Option<ID3D12VideoDecodeCommandList>,
    recorded_resources: Vec<ID3D12Resource>,
}

/// Prepared AV1 key/intra picture for later D3D12 recording.
pub struct D3d12PreparedAv1Picture<'decoder> {
    decoder: &'decoder D3d12Decoder,
    bitstream: D3d12UploadBuffer,
    arguments: Vec<D3d12DecodeArgument>,
    recorded_list: Option<ID3D12VideoDecodeCommandList>,
    recorded_resources: Vec<ID3D12Resource>,
    render_size: FrameSize,
    target_surface: u8,
    reference_bindings: Vec<(u64, u8)>,
}

/// AV1 picture prepared with reference-slot state committed on queue submission.
pub struct D3d12PreparedAv1StatefulPicture<'decoder, 'state> {
    picture: D3d12PreparedAv1Picture<'decoder>,
    state: &'state mut crate::Av1DecodeState,
    input: crate::av1::Av1FrameDxvaInput,
    surface_index: u8,
}

impl D3d12PreparedAv1StatefulPicture<'_, '_> {
    /// Render dimensions signaled by the AV1 frame header.
    #[must_use]
    pub const fn render_size(&self) -> FrameSize {
        self.picture.render_size()
    }

    /// Pixel aspect ratio derived from coded and render dimensions.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the derived ratio cannot be represented.
    pub fn sample_aspect_ratio(&self) -> VideoResult<crate::SampleAspectRatio> {
        self.picture.sample_aspect_ratio()
    }

    /// Records an AV1 key/intra picture that has no active reference textures.
    /// Inter frames must use `record_with_references`.
    ///
    /// # Errors
    ///
    /// Returns invalid-state or invalid-input errors for incompatible output resources.
    pub fn record(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        output: &D3d12Texture,
        output_subresource: u32,
    ) -> VideoResult<()> {
        self.picture
            .record_with_references(command_list, output, output_subresource, &[])
    }

    /// Records decode with a table indexed by the AV1 surface indices in picture parameters.
    ///
    /// Supply every active reference surface in `references`; entries must be indexed by surface
    /// index, not AV1 slot number. The table is validated before command recording.
    ///
    /// # Errors
    ///
    /// Returns invalid-state or invalid-input when the reference table or output resources do not
    /// match this picture.
    pub fn record_with_references(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        output: &D3d12Texture,
        output_subresource: u32,
        references: &[Option<(&D3d12Texture, u32)>],
    ) -> VideoResult<()> {
        self.picture
            .record_with_references(command_list, output, output_subresource, references)
    }

    /// Submits the recorded picture and commits reference slots once execution is submitted.
    ///
    /// # Errors
    ///
    /// Returns the queue error and whether execution was submitted.
    pub fn submit(
        self,
        queue: &mut D3d12Queue,
        command_list: &ID3D12VideoDecodeCommandList,
        allocator: &ID3D12CommandAllocator,
        textures: &[&D3d12Texture],
    ) -> Result<D3d12PendingDecode, D3d12SubmissionError> {
        let result = self
            .picture
            .submit(queue, command_list, allocator, textures);
        if result.as_ref().is_ok() || result.as_ref().is_err_and(|error| error.submitted) {
            self.state.commit_frame(&self.input, self.surface_index);
        }
        result
    }
}

/// Owns decode-side resources until a queue fence signals completion.
///
/// The token must be signaled on the queue after the decode command list was submitted; matching
/// the device alone cannot prove that it covers the submission.
/// The caller must separately retain output/reference textures and command allocator until the
/// same fence completes. Dropping this guard waits; if the fence fails, resources are retained
/// for the process lifetime.
pub struct D3d12PendingDecode {
    bitstream: D3d12UploadBuffer,
    decoder: ID3D12VideoDecoder,
    heap: ID3D12VideoDecoderHeap,
    completion: D3d12CompletionToken,
    resources: Vec<ID3D12Resource>,
    command_list: Option<ID3D12VideoDecodeCommandList>,
    allocator: Option<ID3D12CommandAllocator>,
}

impl D3d12PendingDecode {
    /// Clones the token for the same submission, for example to wrap its output as a scale frame.
    /// Keep this guard alive until completion to retain the decoder's other GPU resources.
    #[must_use]
    pub fn completion(&self) -> D3d12CompletionToken {
        self.completion.clone()
    }

    /// Returns whether the associated fence has completed.
    ///
    /// # Errors
    ///
    /// Returns a device-lost error when the fence indicates device removal.
    pub fn is_ready(&self) -> VideoResult<bool> {
        self.completion.is_ready()
    }

    /// Waits up to `timeout` for the associated fence.
    ///
    /// # Errors
    ///
    /// Returns an error when the fence cannot be queried or waited on.
    pub fn wait(&self, timeout: Duration) -> VideoResult<WaitStatus> {
        self.completion.wait(timeout)
    }

    /// Retains the submitted command list, its allocator, and all output/reference textures.
    ///
    /// Call this while the caller still owns these objects and keep the guard until completion.
    /// The allocator must not be reset until the fence completes. The caller must provide every
    /// resource referenced by the command list; this method cannot inspect recorded commands.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for objects from another device, or a backend/allocation error.
    pub fn retain_submission_resources(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        allocator: &ID3D12CommandAllocator,
        textures: &[&D3d12Texture],
    ) -> VideoResult<()> {
        if self.command_list.is_some() {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PendingDecode::retain_submission_resources",
                "submission resources have already been registered",
            ));
        }
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: The COM objects are live and the output slot owns the returned device interface.
        unsafe { command_list.GetDevice(&raw mut device) }.map_err(|error| {
            VideoError::new(
                VideoErrorKind::Backend,
                "decode command-list GetDevice",
                "GetDevice failed",
            )
            .with_backend_code(error.code().0)
        })?;
        check_pending_device(device.as_ref(), self.completion.device())?;
        device = None;
        // SAFETY: The allocator is live and the output slot is valid.
        unsafe { allocator.GetDevice(&raw mut device) }.map_err(|error| {
            VideoError::new(
                VideoErrorKind::Backend,
                "decode allocator GetDevice",
                "GetDevice failed",
            )
            .with_backend_code(error.code().0)
        })?;
        check_pending_device(device.as_ref(), self.completion.device())?;
        for texture in textures {
            device = None;
            // SAFETY: Each texture is live and the output slot is valid.
            unsafe { texture.resource().GetDevice(&raw mut device) }.map_err(|error| {
                VideoError::new(
                    VideoErrorKind::Backend,
                    "decode texture GetDevice",
                    "GetDevice failed",
                )
                .with_backend_code(error.code().0)
            })?;
            check_pending_device(device.as_ref(), self.completion.device())?;
        }
        self.resources
            .try_reserve(textures.len())
            .map_err(|_| allocation_error("D3d12PendingDecode::retain_submission_resources"))?;
        self.resources
            .extend(textures.iter().map(|texture| texture.resource().clone()));
        self.command_list = Some(command_list.clone());
        self.allocator = Some(allocator.clone());
        Ok(())
    }
}

fn check_pending_device(actual: Option<&ID3D12Device>, expected: &ID3D12Device) -> VideoResult<()> {
    if actual != Some(expected) {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3d12PendingDecode::retain_submission_resources",
            "resource and completion fence belong to different devices",
        ));
    }
    Ok(())
}

impl Drop for D3d12PendingDecode {
    fn drop(&mut self) {
        match self.completion.wait(Duration::from_secs(30)) {
            Ok(WaitStatus::Complete) => {}
            Ok(WaitStatus::TimedOut) | Err(_) => {
                std::mem::forget(self.bitstream.resource().clone());
                std::mem::forget(self.decoder.clone());
                std::mem::forget(self.heap.clone());
                for resource in &self.resources {
                    std::mem::forget(resource.clone());
                }
                if let Some(command_list) = &self.command_list {
                    std::mem::forget(command_list.clone());
                }
                if let Some(allocator) = &self.allocator {
                    std::mem::forget(allocator.clone());
                }
            }
        }
    }
}

fn pending_decode(
    decoder: &D3d12Decoder,
    bitstream: D3d12UploadBuffer,
    completion: D3d12CompletionToken,
) -> VideoResult<D3d12PendingDecode> {
    if completion.device() != decoder.device.device() {
        // The token cannot establish completion for this decoder's device.
        std::mem::forget(bitstream.resource().clone());
        std::mem::forget(decoder.decoder.clone());
        std::mem::forget(decoder.heap.clone());
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3d12PendingDecode",
            "fence and decoder belong to different devices",
        ));
    }
    Ok(D3d12PendingDecode {
        bitstream,
        decoder: decoder.decoder.clone(),
        heap: decoder.heap.clone(),
        completion,
        resources: Vec::new(),
        command_list: None,
        allocator: None,
    })
}

fn collect_recorded_resources(
    output: &D3d12Texture,
    references: &[Option<(&D3d12Texture, u32)>],
) -> VideoResult<Vec<ID3D12Resource>> {
    let mut resources = Vec::new();
    resources
        .try_reserve_exact(references.len().saturating_add(1))
        .map_err(|_| allocation_error("decode recorded resource list"))?;
    resources.push(output.resource().clone());
    resources.extend(
        references
            .iter()
            .flatten()
            .map(|(texture, _)| texture.resource().clone()),
    );
    Ok(resources)
}

fn validate_reference_slots<T>(
    bindings: &[(u64, u8)],
    references: &[Option<T>],
) -> VideoResult<()> {
    if bindings.iter().any(|(_, index)| {
        references
            .get(usize::from(*index))
            .is_none_or(Option::is_none)
    }) {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3D12 reference table",
            "reference table lacks a texture for an active surface index",
        ));
    }
    Ok(())
}

fn validate_target_surface(
    target: usize,
    capacity: u32,
    operation: &'static str,
) -> VideoResult<u8> {
    if u64::try_from(target).map_or(true, |index| index >= u64::from(capacity)) {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            operation,
            "target surface index exceeds decoder heap capacity",
        ));
    }
    u8::try_from(target).map_err(|_| {
        decode_error(
            VideoErrorKind::InvalidInput,
            operation,
            "target surface index exceeds u8",
        )
    })
}

fn validate_decode_target_surface(
    expected: u8,
    actual: u32,
    operation: &'static str,
) -> VideoResult<()> {
    if actual != u32::from(expected) {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            operation,
            "output subresource does not match the surface index in picture parameters",
        ));
    }
    Ok(())
}

fn validate_recorded_submission(
    recorded_list: Option<&ID3D12VideoDecodeCommandList>,
    recorded_resources: &[ID3D12Resource],
    command_list: &ID3D12VideoDecodeCommandList,
    textures: &[&D3d12Texture],
) -> Result<(), D3d12SubmissionError> {
    if recorded_list != Some(command_list)
        || recorded_resources.iter().any(|resource| {
            !textures
                .iter()
                .any(|texture| texture.resource() == resource)
        })
    {
        return Err(D3d12SubmissionError {
            error: decode_error(
                VideoErrorKind::InvalidInput,
                "prepared decode submission",
                "command list or recorded output/reference textures do not match",
            ),
            submitted: false,
        });
    }
    Ok(())
}

/// Submits a recorded decode command list and retains the supplied resources until completion.
///
/// The list must already be closed and contain the decode commands associated with `bitstream`.
/// Supply every texture referenced by the list; do not reset the allocator until the returned
/// guard completes. The caller remains responsible for committing codec state after submission.
/// If submission succeeds but signaling fails, the error reports `submitted = true` and the GPU
/// resource references are retained for process lifetime because completion is unknown.
///
/// # Errors
///
/// Reports validation, allocation, device or queue failures and whether execution was submitted.
#[expect(
    clippy::too_many_lines,
    reason = "submission must validate and retain all GPU resources before execution"
)]
pub fn submit_recorded_decode(
    decoder: &D3d12Decoder,
    queue: &mut D3d12Queue,
    command_list: &ID3D12VideoDecodeCommandList,
    allocator: &ID3D12CommandAllocator,
    bitstream: D3d12UploadBuffer,
    textures: &[&D3d12Texture],
) -> Result<D3d12PendingDecode, D3d12SubmissionError> {
    let before = |error| D3d12SubmissionError {
        error,
        submitted: false,
    };
    if queue.queue_type() != D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE {
        return Err(before(decode_error(
            VideoErrorKind::InvalidInput,
            "submit_recorded_decode",
            "queue must be a video decode queue",
        )));
    }
    let expected = decoder.device.device();
    let mut actual: Option<ID3D12Device> = None;
    // SAFETY: The allocator and output slot are live for this synchronous query.
    unsafe { allocator.GetDevice(&raw mut actual) }.map_err(|error| {
        before(
            VideoError::new(
                VideoErrorKind::Backend,
                "decode allocator GetDevice",
                "GetDevice failed",
            )
            .with_backend_code(error.code().0),
        )
    })?;
    check_decode_device(actual.as_ref(), expected).map_err(before)?;
    let mut resources = Vec::new();
    resources
        .try_reserve_exact(textures.len())
        .map_err(|_| before(allocation_error("submit_recorded_decode")))?;
    for texture in textures {
        actual = None;
        // SAFETY: The texture and output slot are live for this synchronous query.
        unsafe { texture.resource().GetDevice(&raw mut actual) }.map_err(|error| {
            before(
                VideoError::new(
                    VideoErrorKind::Backend,
                    "decode texture GetDevice",
                    "GetDevice failed",
                )
                .with_backend_code(error.code().0),
            )
        })?;
        check_decode_device(actual.as_ref(), expected).map_err(before)?;
        resources.push(texture.resource().clone());
    }
    actual = None;
    // SAFETY: The upload buffer and output slot are live for this synchronous query.
    unsafe { bitstream.resource().GetDevice(&raw mut actual) }.map_err(|error| {
        before(
            VideoError::new(
                VideoErrorKind::Backend,
                "decode bitstream GetDevice",
                "GetDevice failed",
            )
            .with_backend_code(error.code().0),
        )
    })?;
    check_decode_device(actual.as_ref(), expected).map_err(before)?;
    let generic: ID3D12CommandList = command_list.cast().map_err(|error| {
        before(
            VideoError::new(
                VideoErrorKind::Backend,
                "decode command-list cast",
                "cast failed",
            )
            .with_backend_code(error.code().0),
        )
    })?;
    actual = None;
    // SAFETY: The command list and output slot are live for this synchronous query.
    unsafe { command_list.GetDevice(&raw mut actual) }.map_err(|error| {
        before(
            VideoError::new(
                VideoErrorKind::Backend,
                "decode command-list GetDevice",
                "GetDevice failed",
            )
            .with_backend_code(error.code().0),
        )
    })?;
    check_decode_device(actual.as_ref(), expected).map_err(before)?;
    let completion = match queue.execute_with_completion(&[generic]) {
        Ok(token) => token,
        Err(error) => {
            if error.submitted {
                std::mem::forget(bitstream.resource().clone());
                std::mem::forget(decoder.decoder.clone());
                std::mem::forget(decoder.heap.clone());
                std::mem::forget(command_list.clone());
                std::mem::forget(allocator.clone());
                for resource in resources {
                    std::mem::forget(resource);
                }
            }
            return Err(error);
        }
    };
    Ok(D3d12PendingDecode {
        bitstream,
        decoder: decoder.decoder.clone(),
        heap: decoder.heap.clone(),
        completion,
        resources,
        command_list: Some(command_list.clone()),
        allocator: Some(allocator.clone()),
    })
}

impl D3d12PreparedAvcPicture<'_, '_> {
    /// Clones the upload resource for submission while keeping this picture available for commit.
    #[must_use]
    pub fn upload_buffer(&self) -> D3d12UploadBuffer {
        self.bitstream.clone()
    }

    /// Submits the closed recorded list and commits DPB state if execution was submitted.
    ///
    /// `textures` must include every output and reference texture recorded for this picture, and
    /// any other textures used by the list. The allocator must remain unreset until completion.
    ///
    /// # Errors
    ///
    /// Returns the queue error and whether execution was submitted. After a post-submission signal
    /// failure, resources are retained and the caller must recover the decoder before continuing.
    pub fn submit(
        self,
        queue: &mut D3d12Queue,
        command_list: &ID3D12VideoDecodeCommandList,
        allocator: &ID3D12CommandAllocator,
        textures: &[&D3d12Texture],
    ) -> Result<D3d12PendingDecode, D3d12SubmissionError> {
        validate_recorded_submission(
            self.recorded_list.as_ref(),
            &self.recorded_resources,
            command_list,
            textures,
        )?;
        let result = submit_recorded_decode(
            self.decoder,
            queue,
            command_list,
            allocator,
            self.bitstream,
            textures,
        );
        if !result.as_ref().is_err_and(|error| !error.submitted) {
            self.state.commit(self.prepared);
        }
        result
    }

    /// Records the prepared picture to its selected decoder surface. Commit state after the caller
    /// closes and successfully submits this command list to a video-decode queue.
    ///
    /// # Errors
    ///
    /// Returns invalid-state or invalid-input errors for incompatible resources/arguments.
    pub fn record(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        output: &D3d12Texture,
        output_subresource: u32,
        references: &[Option<(&D3d12Texture, u32)>],
    ) -> VideoResult<()> {
        if self.recorded {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PreparedAvcPicture::record",
                "picture was already recorded",
            ));
        }
        validate_decode_target_surface(
            self.target_surface,
            output_subresource,
            "D3d12PreparedAvcPicture::record",
        )?;
        validate_reference_slots(self.state.reference_surfaces(), references)?;
        let recorded_resources = collect_recorded_resources(output, references)?;
        self.decoder.decode_frame(
            command_list,
            output,
            output_subresource,
            &D3d12InputBitstream::from_upload_buffer(&self.bitstream),
            references,
            &self.arguments,
        )?;
        self.recorded = true;
        self.recorded_list = Some(command_list.clone());
        self.recorded_resources = recorded_resources;
        Ok(())
    }

    /// Commits AVC DPB state after successful command-list queue submission and returns the
    /// bitstream upload buffer. Retain the returned buffer until the submission fence completes.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error when no decode command was recorded.
    pub fn commit(self) -> VideoResult<D3d12UploadBuffer> {
        if !self.recorded {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PreparedAvcPicture::commit",
                "picture must be recorded before its DPB state can be committed",
            ));
        }
        self.state.commit(self.prepared);
        Ok(self.bitstream)
    }

    /// Commits state and retains decode resources until the supplied submission fence completes.
    ///
    /// # Errors
    ///
    /// Returns invalid-state if unrecorded, or invalid-input if the fence uses another device.
    pub fn commit_with_completion(
        self,
        completion: D3d12CompletionToken,
    ) -> VideoResult<D3d12PendingDecode> {
        if !self.recorded {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PreparedAvcPicture::commit_with_completion",
                "picture must be recorded before commit",
            ));
        }
        let pending = pending_decode(self.decoder, self.bitstream, completion)?;
        self.state.commit(self.prepared);
        Ok(pending)
    }
}

impl D3d12PreparedHevcPicture<'_, '_> {
    /// Clones the upload resource for submission while keeping this picture available for commit.
    #[must_use]
    pub fn upload_buffer(&self) -> D3d12UploadBuffer {
        self.bitstream.clone()
    }

    /// Submits the closed recorded list and commits DPB state if execution was submitted.
    ///
    /// `textures` must include every output/reference texture for this picture and any other
    /// textures used by the list. The allocator must remain unreset until completion.
    ///
    /// # Errors
    ///
    /// Returns the queue error and whether execution was submitted. Recover the decoder after a
    /// post-submission signal failure; the retained resources have unknown completion.
    pub fn submit(
        self,
        queue: &mut D3d12Queue,
        command_list: &ID3D12VideoDecodeCommandList,
        allocator: &ID3D12CommandAllocator,
        textures: &[&D3d12Texture],
    ) -> Result<D3d12PendingDecode, D3d12SubmissionError> {
        validate_recorded_submission(
            self.recorded_list.as_ref(),
            &self.recorded_resources,
            command_list,
            textures,
        )?;
        let result = submit_recorded_decode(
            self.decoder,
            queue,
            command_list,
            allocator,
            self.bitstream,
            textures,
        );
        if !result.as_ref().is_err_and(|error| !error.submitted) {
            self.state.commit(self.prepared);
        }
        result
    }

    /// Records the prepared picture to its selected decoder surface. Commit state after the caller
    /// closes and successfully submits this command list to a video-decode queue.
    ///
    /// # Errors
    ///
    /// Returns invalid-state or invalid-input errors for incompatible resources/arguments.
    pub fn record(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        output: &D3d12Texture,
        output_subresource: u32,
        references: &[Option<(&D3d12Texture, u32)>],
    ) -> VideoResult<()> {
        if self.recorded {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PreparedHevcPicture::record",
                "picture was already recorded",
            ));
        }
        validate_decode_target_surface(
            self.target_surface,
            output_subresource,
            "D3d12PreparedHevcPicture::record",
        )?;
        validate_reference_slots(self.state.reference_surfaces(), references)?;
        let recorded_resources = collect_recorded_resources(output, references)?;
        self.decoder.decode_frame(
            command_list,
            output,
            output_subresource,
            &D3d12InputBitstream::from_upload_buffer(&self.bitstream),
            references,
            &self.arguments,
        )?;
        self.recorded = true;
        self.recorded_list = Some(command_list.clone());
        self.recorded_resources = recorded_resources;
        Ok(())
    }

    /// Commits HEVC DPB state after successful command-list queue submission and returns the
    /// bitstream upload buffer. Retain the returned buffer until the submission fence completes.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error when no decode command was recorded.
    pub fn commit(self) -> VideoResult<D3d12UploadBuffer> {
        if !self.recorded {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PreparedHevcPicture::commit",
                "picture must be recorded before its DPB state can be committed",
            ));
        }
        self.state.commit(self.prepared);
        Ok(self.bitstream)
    }

    /// Commits state and retains decode resources until the supplied submission fence completes.
    ///
    /// # Errors
    ///
    /// Returns invalid-state if unrecorded, or invalid-input if the fence uses another device.
    pub fn commit_with_completion(
        self,
        completion: D3d12CompletionToken,
    ) -> VideoResult<D3d12PendingDecode> {
        if !self.recorded {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PreparedHevcPicture::commit_with_completion",
                "picture must be recorded before commit",
            ));
        }
        let pending = pending_decode(self.decoder, self.bitstream, completion)?;
        self.state.commit(self.prepared);
        Ok(pending)
    }
}

impl D3d12PreparedAv1Picture<'_> {
    /// Returns the AV1 render dimensions. These describe display aspect and may differ from the
    /// decoded texture dimensions; they are not a crop rectangle.
    #[must_use]
    pub const fn render_size(&self) -> FrameSize {
        self.render_size
    }

    /// Derives pixel aspect from the decoded dimensions and AV1 render dimensions.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the derived ratio cannot be represented.
    pub fn sample_aspect_ratio(&self) -> VideoResult<crate::SampleAspectRatio> {
        crate::SampleAspectRatio::from_render_size(self.decoder.coded_size(), self.render_size)
    }

    /// Clones the upload resource for submission while retaining this prepared picture.
    #[must_use]
    pub fn upload_buffer(&self) -> D3d12UploadBuffer {
        self.bitstream.clone()
    }

    /// Submits the closed recorded list and retains decode resources until completion.
    ///
    /// Supply the recorded output texture and any other textures referenced by the command list.
    /// Do not reset its allocator until the returned guard reports completion.
    ///
    /// # Errors
    ///
    /// Reports validation or queue failure and whether the command list was submitted. A signal
    /// failure after submission requires decoder recovery because completion is unknown.
    pub fn submit(
        self,
        queue: &mut D3d12Queue,
        command_list: &ID3D12VideoDecodeCommandList,
        allocator: &ID3D12CommandAllocator,
        textures: &[&D3d12Texture],
    ) -> Result<D3d12PendingDecode, D3d12SubmissionError> {
        validate_recorded_submission(
            self.recorded_list.as_ref(),
            &self.recorded_resources,
            command_list,
            textures,
        )?;
        submit_recorded_decode(
            self.decoder,
            queue,
            command_list,
            allocator,
            self.bitstream,
            textures,
        )
    }

    /// Retains AV1 decode resources until the supplied submission fence completes.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the fence uses a different device.
    pub fn retain_until_completion(
        self,
        completion: D3d12CompletionToken,
    ) -> VideoResult<D3d12PendingDecode> {
        pending_decode(self.decoder, self.bitstream, completion)
    }

    /// Records the AV1 picture with no reference frames (key/intra-only path).
    /// Inter frames must use `record_with_references`.
    /// Keep this prepared picture alive until the submitted GPU work completes, because it owns
    /// the upload buffer read by the decoder.
    ///
    /// # Errors
    ///
    /// Returns invalid-state or invalid-input errors for incompatible output resources.
    pub fn record(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        output: &D3d12Texture,
        output_subresource: u32,
    ) -> VideoResult<()> {
        self.record_with_references(command_list, output, output_subresource, &[])
    }

    /// Records the AV1 picture to its selected decoder surface with caller-owned reference slices.
    ///
    /// # Errors
    ///
    /// Returns invalid-state or invalid-input errors for an incomplete reference table or
    /// incompatible output resources.
    pub fn record_with_references(
        &mut self,
        command_list: &ID3D12VideoDecodeCommandList,
        output: &D3d12Texture,
        output_subresource: u32,
        references: &[Option<(&D3d12Texture, u32)>],
    ) -> VideoResult<()> {
        if self.recorded_list.is_some() {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12PreparedAv1Picture::record",
                "picture was already recorded",
            ));
        }
        validate_decode_target_surface(
            self.target_surface,
            output_subresource,
            "D3d12PreparedAv1Picture::record",
        )?;
        validate_reference_slots(&self.reference_bindings, references)?;
        let resources = collect_recorded_resources(output, references)?;
        self.decoder.decode_frame(
            command_list,
            output,
            output_subresource,
            &D3d12InputBitstream::from_upload_buffer(&self.bitstream),
            references,
            &self.arguments,
        )?;
        self.recorded_list = Some(command_list.clone());
        self.recorded_resources = resources;
        Ok(())
    }
}

impl D3d12Decoder {
    /// Configures D3D12 decoding and creates the matching codec parameter state.
    ///
    /// The returned state parses and tracks codec headers; callers still submit the parsed
    /// picture parameters and reference resources through the D3D12 decode command API.
    ///
    /// # Errors
    ///
    /// Returns codec-private-data, unsupported-profile, decoder support, or heap creation errors.
    pub fn configure_with_state(
        device: &D3d12Device,
        config: &crate::DecoderConfig,
        max_reference_frames: u32,
    ) -> VideoResult<(Self, crate::CodecDecodeState)> {
        let state = config.create_codec_state()?;
        let decoder = Self::configure(device, config, max_reference_frames)?;
        Ok((decoder, state))
    }

    /// Selects the device capability matching a shared decoder configuration.
    ///
    /// # Errors
    ///
    /// Returns validation, unsupported-profile, decoder support, or heap creation errors.
    pub fn configure(
        device: &D3d12Device,
        config: &crate::DecoderConfig,
        max_reference_frames: u32,
    ) -> VideoResult<Self> {
        config.validate()?;
        let capability = device
            .capabilities()
            .iter()
            .find(|capability| {
                capability.profile == config.profile && capability.bit_depth == config.bit_depth
            })
            .cloned()
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::Unsupported,
                    "D3d12Decoder::configure",
                    "requested codec profile and bit depth are not supported",
                )
            })?;
        Self::create(device, &capability, config.coded_size, max_reference_frames)
    }

    /// Creates a video decoder and decoder heap after querying profile support at `size`.
    ///
    /// The output texture and command list remain caller-owned. The caller transitions output
    /// resources to `VIDEO_DECODE_WRITE`, reference resources to `VIDEO_DECODE_READ`, records
    /// `decode_frame`, closes and executes the command list on a video-decode queue, then signals
    /// a queue fence.
    ///
    /// # Errors
    ///
    /// Returns unsupported, invalid-input, or backend errors when capability or heap creation
    /// fails.
    pub fn create(
        device: &D3d12Device,
        capability: &D3d12DecodeCapability,
        size: FrameSize,
        max_reference_frames: u32,
    ) -> VideoResult<Self> {
        if max_reference_frames == 0
            || usize::try_from(max_reference_frames)
                .map_or(true, |count| count > MAX_D3D12_REFERENCE_FRAMES)
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::create",
                "reference frame capacity must be in 1..=16",
            ));
        }
        if !device.capabilities().contains(capability) {
            return Err(decode_error(
                VideoErrorKind::Unsupported,
                "D3d12Decoder::create",
                "decode capability was not advertised by this device",
            ));
        }
        let support = device.check_decode_support(capability, size)?;
        if !support.supported {
            return Err(decode_error(
                VideoErrorKind::Unsupported,
                "D3d12Decoder::create",
                "driver rejected the requested profile, format, or coded size",
            ));
        }
        let surface_size = decode_surface_size(size, support.configuration_flags)?;
        let configuration = decode_configuration(capability.decoder_profile);
        let decoder_desc = D3D12_VIDEO_DECODER_DESC {
            NodeMask: 0,
            Configuration: configuration,
        };
        // SAFETY: Decoder description is initialized and remains live during creation; requested
        // profile support was queried successfully for this device and size.
        let decoder = unsafe {
            device
                .video_device()
                .CreateVideoDecoder::<ID3D12VideoDecoder>(&raw const decoder_desc)
        }
        .map_err(|error| backend_error("CreateVideoDecoder", error.code().0))?;
        let format = dxgi_format(capability.output_format).ok_or_else(|| {
            decode_error(
                VideoErrorKind::Unsupported,
                "D3d12Decoder::create",
                "requested output format is not a D3D12 video decode format",
            )
        })?;
        let heap_desc = D3D12_VIDEO_DECODER_HEAP_DESC {
            NodeMask: 0,
            Configuration: configuration,
            DecodeWidth: size.width(),
            DecodeHeight: size.height(),
            Format: format,
            FrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            BitRate: 0,
            MaxDecodePictureBufferCount: max_reference_frames,
        };
        // SAFETY: Heap descriptor is initialized and matches the checked profile, output format,
        // and coded dimensions; the retained video device owns creation of the heap.
        let heap = unsafe {
            device
                .video_device()
                .CreateVideoDecoderHeap::<ID3D12VideoDecoderHeap>(&raw const heap_desc)
        }
        .map_err(|error| backend_error("CreateVideoDecoderHeap", error.code().0))?;
        Ok(Self {
            device: device.clone(),
            decoder,
            heap,
            size,
            surface_size,
            output_format: capability.output_format,
            capability: capability.clone(),
            max_reference_frames,
            tier: support.tier,
        })
    }

    /// Prepares an AVC sample with the shared parser/DPB translator for later D3D12 recording.
    ///
    /// The returned picture holds an exclusive borrow of `state`; call `record`, queue the closed
    /// command list successfully, then call `commit`. Dropping it leaves reference state intact.
    ///
    /// # Errors
    ///
    /// Returns parser, state, unsupported-syntax, invalid-input, allocation, or backend errors.
    pub fn prepare_avc_sample<'decoder, 'state>(
        &'decoder self,
        state: &'state mut AvcDecodeState,
        target_surface: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
    ) -> VideoResult<D3d12PreparedAvcPicture<'decoder, 'state>> {
        if self.capability.profile.codec() != Codec::Avc {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::prepare_avc_sample",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = validate_target_surface(
            target_surface,
            self.max_reference_frames,
            "D3d12Decoder::prepare_avc_sample",
        )?;
        if state.dimensions_in_mbs()
            != (
                self.size.width().div_ceil(16),
                self.size.height().div_ceil(16),
            )
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::prepare_avc_sample",
                "AVC SPS dimensions do not match the configured decoder size",
            ));
        }
        let mut slices = Vec::new();
        for nal in
            crate::d3d9_decode::codec_nals(sample.data, format, "D3d12Decoder::prepare_avc_sample")?
        {
            state.validate_parameter_set(nal)?;
            if nal
                .first()
                .is_some_and(|header| matches!(header & 0x1f, 1 | 5))
            {
                slices
                    .try_reserve(1)
                    .map_err(|_| allocation_error("D3d12Decoder::prepare_avc_sample"))?;
                slices.push(nal);
            }
        }
        let prepared = state.prepare(&slices, surface_index)?;
        let quantization = state.quant_matrix()?;
        let [bitstream, slice_control] =
            crate::d3d9_decode::build_avc_slice_buffers(sample, format, self.size)?;
        let upload = self.device.create_upload_buffer(&bitstream.data)?;
        let arguments = make_picture_arguments(
            prepared.picture_parameters.clone(),
            quantization,
            slice_control.data,
            "D3d12Decoder::prepare_avc_sample",
        )?;
        Ok(D3d12PreparedAvcPicture {
            decoder: self,
            state,
            prepared,
            target_surface: surface_index,
            bitstream: upload,
            arguments,
            recorded: false,
            recorded_list: None,
            recorded_resources: Vec::new(),
        })
    }

    /// Prepares an HEVC sample with the shared parser/DPB translator for later D3D12 recording.
    ///
    /// Ordered independent and dependent slice segments are supported within one picture.
    /// Dependent segments inherit syntax state from the preceding independent segment.
    /// Dropping the returned picture does not advance the HEVC DPB.
    ///
    /// # Errors
    ///
    /// Returns parser, missing-reference, unsupported-syntax, invalid-input, allocation, or
    /// backend errors.
    pub fn prepare_hevc_sample<'decoder, 'state>(
        &'decoder self,
        state: &'state mut HevcDecodeState,
        target_surface: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
    ) -> VideoResult<D3d12PreparedHevcPicture<'decoder, 'state>> {
        if self.capability.profile.codec() != Codec::Hevc {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::prepare_hevc_sample",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = validate_target_surface(
            target_surface,
            self.max_reference_frames,
            "D3d12Decoder::prepare_hevc_sample",
        )?;
        if state.dimensions() != (self.size.width(), self.size.height()) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::prepare_hevc_sample",
                "HEVC SPS dimensions do not match the configured decoder size",
            ));
        }
        let mut slices = Vec::new();
        for nal in crate::d3d9_decode::codec_nals(
            sample.data,
            format,
            "D3d12Decoder::prepare_hevc_sample",
        )? {
            state.validate_parameter_set(nal)?;
            if nal.len() >= 2 && (nal[0] >> 1) & 0x3f <= 31 {
                slices
                    .try_reserve(1)
                    .map_err(|_| allocation_error("D3d12Decoder::prepare_hevc_sample"))?;
                slices.push(nal);
            }
        }
        let prepared = state.prepare(&slices, surface_index)?;
        let quantization = state.quant_matrix()?;
        let [bitstream, slice_control] =
            crate::d3d9_decode::build_hevc_slice_buffers(sample, format)?;
        let upload = self.device.create_upload_buffer(&bitstream.data)?;
        let arguments = make_picture_arguments(
            prepared.picture_parameters.clone(),
            quantization,
            slice_control.data,
            "D3d12Decoder::prepare_hevc_sample",
        )?;
        Ok(D3d12PreparedHevcPicture {
            decoder: self,
            state,
            prepared,
            target_surface: surface_index,
            bitstream: upload,
            arguments,
            recorded: false,
            recorded_list: None,
            recorded_resources: Vec::new(),
        })
    }

    /// Resolves an AV1 show-existing-frame header to its previously decoded surface index.
    ///
    /// No command list is needed because a show-existing frame does not decode pixels. The caller
    /// must keep the selected texture slice and its producer completion token alive for display.
    ///
    /// # Errors
    ///
    /// Returns invalid-state for a non-AV1 decoder and parser errors for unavailable slots or
    /// mismatched display frame IDs.
    pub fn av1_show_existing_surface(
        &self,
        state: &crate::Av1DecodeState,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
    ) -> VideoResult<Option<u8>> {
        if self.capability.profile.codec() != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::av1_show_existing_surface",
                "decoder was configured for a different codec",
            ));
        }
        let surface = state.show_existing_surface(sequence_header_obu, frame_header_obu)?;
        if surface.is_some_and(|index| u32::from(index) >= self.max_reference_frames) {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::av1_show_existing_surface",
                "AV1 reference slot points outside this decoder's surface pool",
            ));
        }
        Ok(surface)
    }

    /// Prepares an AV1 key/intra-only picture with complete, ordered tile coverage. A combined
    /// `OBU_FRAME` is accepted with an empty `tile_group_obus` slice.
    ///
    /// This stateless entry point handles key/intra-only frames. Stateful inter decoding is
    /// provided by `prepare_av1_intra_obus_with_state` and `record_with_references`.
    /// The AV1 D3D12 argument mapping mirrors `FFmpeg`: picture parameters are a picture argument,
    /// tile descriptors are slice-control, and tile bytes are the compressed bitstream.
    ///
    /// # Errors
    ///
    /// Returns parser, unsupported-syntax, invalid-input, allocation, or backend errors.
    pub fn prepare_av1_intra_obus<'decoder>(
        &'decoder self,
        target_surface: usize,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        tile_group_obus: &[&[u8]],
    ) -> VideoResult<D3d12PreparedAv1Picture<'decoder>> {
        if self.capability.profile.codec() != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::prepare_av1_intra_obus",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = validate_target_surface(
            target_surface,
            self.max_reference_frames,
            "D3d12Decoder::prepare_av1_intra_obus",
        )?;
        let built = crate::av1::build_intra_dxva_input(
            sequence_header_obu,
            frame_header_obu,
            tile_group_obus,
            surface_index,
        )?;
        self.prepare_av1_picture(surface_index, &built)
    }

    fn prepare_av1_picture<'decoder>(
        &'decoder self,
        target_surface: u8,
        built: &crate::av1::Av1FrameDxvaInput,
    ) -> VideoResult<D3d12PreparedAv1Picture<'decoder>> {
        if self.capability.profile.codec() != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::prepare_av1_picture",
                "decoder was configured for a different codec",
            ));
        }
        if u32::from(built.width) != self.size.width()
            || u32::from(built.height) != self.size.height()
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::prepare_av1_picture",
                "AV1 frame dimensions do not match the configured decoder size",
            ));
        }
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(built.tiles.len())
            .map_err(|_| allocation_error("D3d12Decoder::prepare_av1_picture"))?;
        tiles.extend(
            built
                .tiles
                .iter()
                .map(|tile| crate::d3d9_decode::Dxva2Av1Tile {
                    data_offset: tile.data_offset,
                    data_size: tile.data_size,
                    row: tile.row,
                    column: tile.column,
                }),
        );
        let [_, tile_control] =
            crate::d3d9_decode::build_av1_tile_buffers(&built.tile_payload, &tiles)?;
        let bitstream = self.device.create_upload_buffer(&built.tile_payload)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(2)
            .map_err(|_| allocation_error("D3d12Decoder::prepare_av1_picture"))?;
        arguments.push(D3d12DecodeArgument {
            kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
            data: built.picture_parameters.clone(),
        });
        arguments.push(D3d12DecodeArgument {
            kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
            data: tile_control.data,
        });
        validate_argument_list(&arguments)?;
        Ok(D3d12PreparedAv1Picture {
            decoder: self,
            bitstream,
            arguments,
            recorded_list: None,
            recorded_resources: Vec::new(),
            render_size: FrameSize::new(built.render_width, built.render_height)?,
            target_surface,
            reference_bindings: Vec::new(),
        })
    }

    /// Prepares an AV1 key/intra picture or supported error-resilient inter frame while retaining
    /// codec state through submission.
    ///
    /// Reference slots are committed only if queue execution was submitted. Use
    /// `Av1DecodeState::show_existing_surface` for display-only frame headers.
    ///
    /// # Errors
    ///
    /// Returns parser, missing-reference, unsupported-syntax, invalid-input, allocation, or
    /// backend errors.
    pub fn prepare_av1_intra_obus_with_state<'decoder, 'state>(
        &'decoder self,
        state: &'state mut crate::Av1DecodeState,
        target_surface: usize,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        tile_group_obus: &[&[u8]],
    ) -> VideoResult<D3d12PreparedAv1StatefulPicture<'decoder, 'state>> {
        let surface_index = validate_target_surface(
            target_surface,
            self.max_reference_frames,
            "D3d12Decoder::prepare_av1_intra_obus_with_state",
        )?;
        let input = crate::av1::build_intra_dxva_input_with_slots(
            sequence_header_obu,
            frame_header_obu,
            tile_group_obus,
            surface_index,
            state.slots(),
        )?;
        if !state.can_overwrite_surface(surface_index, input.refresh_frame_flags) {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::prepare_av1_intra_obus_with_state",
                "target surface still backs an AV1 slot not refreshed by this frame",
            ));
        }
        let mut picture = self.prepare_av1_picture(surface_index, &input)?;
        picture.reference_bindings = if input.frame_type == 1 || input.frame_type == 3 {
            input.reference_surface_bindings()?
        } else {
            Vec::new()
        };
        Ok(D3d12PreparedAv1StatefulPicture {
            picture,
            state,
            input,
            surface_index,
        })
    }

    #[must_use]
    pub const fn coded_size(&self) -> FrameSize {
        self.size
    }

    /// Dimensions required for decode output and reference texture allocation.
    #[must_use]
    pub const fn surface_size(&self) -> FrameSize {
        self.surface_size
    }

    /// Allocates a decode texture array with the adapter-required surface dimensions.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input, unsupported or backend error when allocation fails.
    pub fn create_output_texture(&self, array_size: u16) -> VideoResult<D3d12Texture> {
        self.device
            .create_texture(self.surface_size, self.output_format, array_size)
    }

    #[must_use]
    pub const fn output_format(&self) -> crate::PixelFormat {
        self.output_format
    }

    #[must_use]
    pub const fn capability(&self) -> &D3d12DecodeCapability {
        &self.capability
    }

    /// Records one codec-prepared picture into an open D3D12 video-decode command list.
    ///
    /// Reference pairs are indexed by the surface indices encoded in the picture parameters,
    /// not by the order of the codec's live DPB entries. The caller must supply the correct texture
    /// and subresource for each index; use `None` for unused indices. The generic recorder cannot
    /// validate that the supplied texture contains the picture described by its index. The output
    /// may share a texture array with references when they use different slices and each slice has
    /// the required tracked state.
    /// `arguments` commonly
    /// contains picture parameters, inverse-quantization data, slice/tile control, and AV1 Huffman
    /// data. All CPU argument bytes remain alive through the immediate `DecodeFrame` call.
    ///
    /// # Errors
    ///
    /// Returns invalid-input or device-lost errors for malformed resource ranges or frame args.
    #[expect(
        clippy::too_many_lines,
        reason = "the ABI argument arrays and their referenced COM resources must stay live through DecodeFrame"
    )]
    pub fn decode_frame(
        &self,
        command_list: &ID3D12VideoDecodeCommandList,
        output: &D3d12Texture,
        output_subresource: u32,
        bitstream: &D3d12InputBitstream,
        references: &[Option<(&D3d12Texture, u32)>],
        arguments: &[D3d12DecodeArgument],
    ) -> VideoResult<()> {
        validate_frame_arguments(bitstream, references, arguments)?;
        if references.len() > self.max_reference_frames as usize {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::decode_frame",
                "reference table exceeds decoder heap capacity",
            ));
        }
        if output_subresource >= u32::from(output.array_size())
            || references
                .iter()
                .flatten()
                .any(|(texture, index)| *index >= u32::from(texture.array_size()))
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::decode_frame",
                "output or reference texture array slice is out of range",
            ));
        }
        if output.size() != self.surface_size
            || output.format() != self.output_format
            || output.slice_state(output_subresource)? != D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE
        {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12Decoder::decode_frame",
                "output texture dimensions, format, or resource state do not match this decoder",
            ));
        }
        if references.iter().flatten().any(|(texture, slice)| {
            texture.resource() == output.resource() && *slice == output_subresource
        }) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::decode_frame",
                "output and reference frames cannot use the same texture array slice",
            ));
        }
        for (texture, slice) in references.iter().flatten() {
            if texture.size() != self.surface_size
                || texture.format() != self.output_format
                || texture.slice_state(*slice)? != D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
            {
                return Err(decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d12Decoder::decode_frame",
                    "reference texture dimensions, format, or resource state are invalid",
                ));
            }
        }
        if self.tier == D3D12_VIDEO_DECODE_TIER_1 {
            let mut resources = references
                .iter()
                .flatten()
                .map(|(texture, _)| texture.resource());
            if let Some(first) = resources.next()
                && resources.any(|resource| resource != first)
            {
                return Err(decode_error(
                    VideoErrorKind::Unsupported,
                    "D3d12Decoder::decode_frame",
                    "decode tier 1 requires all reference frames in one texture array",
                ));
            }
        }
        validate_bitstream_resource(bitstream)?;
        validate_decode_devices(
            self.device.device(),
            command_list,
            output,
            bitstream,
            references,
        )?;
        let mut frame_arguments = [D3D12_VIDEO_DECODE_FRAME_ARGUMENT::default(); 10];
        for (slot, argument) in frame_arguments.iter_mut().zip(arguments) {
            slot.Type = argument.kind;
            slot.Size = u32::try_from(argument.data.len()).map_err(|_| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d12Decoder::decode_frame",
                    "frame argument exceeds the D3D12 inline argument size limit",
                )
            })?;
            slot.pData = argument.data.as_ptr().cast_mut().cast();
        }
        let mut reference_resources = Vec::new();
        let mut reference_subresources = Vec::new();
        reference_resources
            .try_reserve_exact(references.len())
            .map_err(|_| allocation_error("D3d12Decoder::decode_frame"))?;
        reference_subresources
            .try_reserve_exact(references.len())
            .map_err(|_| allocation_error("D3d12Decoder::decode_frame"))?;
        reference_resources.extend(
            references
                .iter()
                .map(|entry| entry.map(|(texture, _)| texture.resource().clone())),
        );
        reference_subresources.extend(
            references
                .iter()
                .map(|entry| entry.map_or(0, |(_, subresource)| subresource)),
        );
        let input = D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS {
            NumFrameArguments: u32::try_from(arguments.len()).map_err(|_| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d12Decoder::decode_frame",
                    "too many D3D12 frame arguments",
                )
            })?,
            FrameArguments: frame_arguments,
            ReferenceFrames: D3D12_VIDEO_DECODE_REFERENCE_FRAMES {
                NumTexture2Ds: u32::try_from(references.len()).map_err(|_| {
                    decode_error(
                        VideoErrorKind::InvalidInput,
                        "D3d12Decoder::decode_frame",
                        "too many D3D12 reference frames",
                    )
                })?,
                ppTexture2Ds: reference_resources.as_mut_ptr(),
                pSubresources: reference_subresources.as_mut_ptr(),
                // All references use the current decoder heap; resolution changes require a new
                // decoder. FFmpeg's D3D12 path likewise leaves this optional array null.
                ppHeaps: ptr::null_mut(),
            },
            CompressedBitstream: D3D12_VIDEO_DECODE_COMPRESSED_BITSTREAM {
                pBuffer: ManuallyDrop::new(Some(bitstream.resource.clone())),
                Offset: bitstream.offset,
                Size: bitstream.size,
            },
            pHeap: ManuallyDrop::new(Some(self.heap.clone())),
        };
        let output_arguments = D3D12_VIDEO_DECODE_OUTPUT_STREAM_ARGUMENTS {
            pOutputTexture2D: ManuallyDrop::new(Some(output.resource().clone())),
            OutputSubresource: output_subresource,
            ..D3D12_VIDEO_DECODE_OUTPUT_STREAM_ARGUMENTS::default()
        };
        // SAFETY: All resources, reference arrays, heap handles, argument arrays, and pointed-to
        // argument bytes remain alive for the duration of the synchronous command recording call.
        unsafe {
            command_list.DecodeFrame(&self.decoder, &raw const output_arguments, &raw const input);
        };
        // SAFETY: Release only the ManuallyDrop COM clones installed into the ABI structures; the
        // command list has copied their interface pointers during DecodeFrame.
        unsafe {
            drop(ManuallyDrop::into_inner(ptr::read(ptr::addr_of!(
                input.CompressedBitstream.pBuffer
            ))));
            drop(ManuallyDrop::into_inner(ptr::read(ptr::addr_of!(
                input.pHeap
            ))));
            drop(ManuallyDrop::into_inner(ptr::read(ptr::addr_of!(
                output_arguments.pOutputTexture2D
            ))));
        }
        Ok(())
    }
}

fn validate_decode_devices(
    expected: &ID3D12Device,
    command_list: &ID3D12VideoDecodeCommandList,
    output: &D3d12Texture,
    bitstream: &D3d12InputBitstream,
    references: &[Option<(&D3d12Texture, u32)>],
) -> VideoResult<()> {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: The command list is live and the output slot receives an owned COM reference.
    unsafe { command_list.GetDevice(&raw mut device) }.map_err(|error| {
        VideoError::new(
            VideoErrorKind::Backend,
            "D3D12 decode command-list GetDevice",
            "GetDevice failed",
        )
        .with_backend_code(error.code().0)
    })?;
    check_decode_device(device.as_ref(), expected)?;
    for resource in std::iter::once(output.resource())
        .chain(std::iter::once(&bitstream.resource))
        .chain(
            references
                .iter()
                .flatten()
                .map(|(texture, _)| texture.resource()),
        )
    {
        device = None;
        // SAFETY: Each resource is live and the output slot receives an owned COM reference.
        unsafe { resource.GetDevice(&raw mut device) }.map_err(|error| {
            VideoError::new(
                VideoErrorKind::Backend,
                "D3D12 decode resource GetDevice",
                "GetDevice failed",
            )
            .with_backend_code(error.code().0)
        })?;
        check_decode_device(device.as_ref(), expected)?;
    }
    Ok(())
}

fn check_decode_device(actual: Option<&ID3D12Device>, expected: &ID3D12Device) -> VideoResult<()> {
    if actual != Some(expected) {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3d12Decoder::decode_frame",
            "command list, bitstream and textures must belong to the decoder device",
        ));
    }
    Ok(())
}

fn validate_frame_arguments(
    bitstream: &D3d12InputBitstream,
    references: &[Option<(&D3d12Texture, u32)>],
    arguments: &[D3d12DecodeArgument],
) -> VideoResult<()> {
    if bitstream.size == 0
        || !bitstream.offset.is_multiple_of(128)
        || bitstream.offset.checked_add(bitstream.size).is_none()
        || references.len() > MAX_D3D12_REFERENCE_FRAMES
        || arguments.is_empty()
        || arguments.len() > MAX_D3D12_FRAME_ARGUMENTS
    {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3d12Decoder::decode_frame",
            "bitstream range or 128-byte alignment, reference count, or frame argument count is invalid",
        ));
    }
    validate_argument_list(arguments)
}

fn decode_surface_size(
    size: FrameSize,
    flags: windows::Win32::Media::MediaFoundation::D3D12_VIDEO_DECODE_CONFIGURATION_FLAGS,
) -> VideoResult<FrameSize> {
    if flags.contains(D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_REFERENCE_ONLY_ALLOCATIONS_REQUIRED) {
        return Err(decode_error(
            VideoErrorKind::Unsupported,
            "D3d12Decoder::create",
            "adapter requires separate reference-only decode surfaces, which are not implemented",
        ));
    }
    let height = if flags
        .contains(D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_HEIGHT_ALIGNMENT_MULTIPLE_32_REQUIRED)
    {
        size.height().checked_add(31).ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::create",
                "aligned surface height exceeds u32",
            )
        })? & !31
    } else {
        size.height()
    };
    FrameSize::new(size.width(), height)
}

fn make_picture_arguments(
    picture_parameters: Vec<u8>,
    quantization: Vec<u8>,
    slice_control: Vec<u8>,
    operation: &'static str,
) -> VideoResult<Vec<D3d12DecodeArgument>> {
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(3)
        .map_err(|_| allocation_error(operation))?;
    arguments.push(D3d12DecodeArgument {
        kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
        data: picture_parameters,
    });
    arguments.push(D3d12DecodeArgument {
        kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
        data: quantization,
    });
    arguments.push(D3d12DecodeArgument {
        kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
        data: slice_control,
    });
    validate_argument_list(&arguments)?;
    Ok(arguments)
}

fn validate_bitstream_resource(bitstream: &D3d12InputBitstream) -> VideoResult<()> {
    // SAFETY: GetDesc is a read-only query on the retained live resource.
    let description = unsafe { bitstream.resource.GetDesc() };
    let end = bitstream.offset.checked_add(bitstream.size);
    if description.Dimension != D3D12_RESOURCE_DIMENSION_BUFFER
        || end.is_none_or(|end| end > description.Width)
    {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3d12Decoder::decode_frame",
            "compressed bitstream range exceeds its D3D12 buffer resource",
        ));
    }
    Ok(())
}

fn validate_argument_list(arguments: &[D3d12DecodeArgument]) -> VideoResult<()> {
    if arguments.is_empty() || arguments.len() > MAX_D3D12_FRAME_ARGUMENTS {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3d12Decoder::decode_frame",
            "frame argument count must be in 1..=10",
        ));
    }
    for (index, argument) in arguments.iter().enumerate() {
        if argument.data.is_empty()
            || u32::try_from(argument.data.len()).is_err()
            || !matches!(
                argument.kind,
                D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS
                    | D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX
                    | D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL
                    | D3D12_VIDEO_DECODE_ARGUMENT_TYPE_HUFFMAN_TABLE
            )
            || arguments[..index]
                .iter()
                .any(|previous| previous.kind == argument.kind)
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12Decoder::decode_frame",
                "frame arguments must be nonempty, supported, and unique by type",
            ));
        }
    }
    Ok(())
}

fn decode_error(kind: VideoErrorKind, operation: &'static str, detail: &'static str) -> VideoError {
    VideoError::new(kind, operation, detail)
}

fn backend_error(operation: &'static str, code: i32) -> VideoError {
    let kind = crate::error::backend_kind_from_hresult(code);
    VideoError::new(kind, operation, "D3D12 video decode operation failed").with_backend_code(code)
}

fn allocation_error(operation: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::Backend,
        operation,
        "unable to allocate D3D12 decode arguments",
    )
}

/// D3D12 send/receive decoder owning codec state, a texture-array DPB and completion-tracked output.
pub struct D3d12DecodeSession {
    device: D3d12Device,
    surface_count: u32,
    capabilities: Vec<DecodeCapability>,
    decoder: Option<D3d12Decoder>,
    output: Option<Arc<Mutex<D3d12Texture>>>,
    queue: Option<D3d12Queue>,
    state: Option<CodecDecodeState>,
    config: Option<DecoderConfig>,
    av1_sequence_header: Option<Vec<u8>>,
    pending: VecDeque<crate::D3d12ScaleFrame>,
    leases: Vec<Arc<AtomicUsize>>,
    completions: Vec<Option<D3d12CompletionToken>>,
    draining: bool,
    failed: bool,
}

impl D3d12DecodeSession {
    /// Creates a decoder session; the adapter and profile are selected by `configure`.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for an invalid surface count and backend errors while querying
    /// capabilities.
    pub fn new(device: D3d12Device, surface_count: u32) -> VideoResult<Self> {
        if !(1..=16).contains(&surface_count) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d12DecodeSession::new",
                "surface count must be in 1..=16",
            ));
        }
        let min_size = FrameSize::new(1, 1)?;
        let max_size = FrameSize::new(16_384, 16_384)?;
        let capabilities = device
            .capabilities()
            .iter()
            .map(|c| DecodeCapability {
                profile: c.profile,
                bit_depth: c.bit_depth,
                chroma_sampling: c.chroma_sampling,
                output_formats: vec![c.output_format],
                min_size,
                max_size,
            })
            .collect();
        let len = usize::try_from(surface_count)
            .map_err(|_| allocation_error("D3d12DecodeSession::new"))?;
        Ok(Self {
            device,
            surface_count,
            capabilities,
            decoder: None,
            output: None,
            queue: None,
            state: None,
            config: None,
            av1_sequence_header: None,
            pending: VecDeque::new(),
            leases: (0..len).map(|_| Arc::new(AtomicUsize::new(0))).collect(),
            completions: vec![None; len],
            draining: false,
            failed: false,
        })
    }

    fn target(&self) -> VideoResult<usize> {
        let state = self.state.as_ref().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d12DecodeSession::send_sample",
                "decoder is not configured",
            )
        })?;
        self.leases
            .iter()
            .enumerate()
            .find_map(|(index, lease)| {
                if lease.load(Ordering::Acquire) != 0 {
                    return None;
                }
                let id = u8::try_from(index).ok()?;
                let free = match state {
                    CodecDecodeState::Avc(s) => s
                        .reference_surfaces()
                        .iter()
                        .all(|(_, surface)| *surface != id),
                    CodecDecodeState::Hevc(s) => s
                        .reference_surfaces()
                        .iter()
                        .all(|(_, surface)| *surface != id),
                    CodecDecodeState::Av1(s) => s.can_overwrite_surface(id, 0),
                };
                free.then_some(index)
            })
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d12DecodeSession::send_sample",
                    "all output surfaces are leased or referenced",
                )
            })
    }

    fn command_resources(
        &self,
    ) -> VideoResult<(ID3D12CommandAllocator, ID3D12VideoDecodeCommandList)> {
        // SAFETY: The allocator and list use the same decode-list type as the session queue.
        let allocator = unsafe {
            self.device
                .device()
                .CreateCommandAllocator::<ID3D12CommandAllocator>(
                    D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE,
                )
        }
        .map_err(|e| backend_error("Create video-decode allocator", e.code().0))?;
        // SAFETY: The newly created allocator is live and no graphics PSO is needed.
        let list = unsafe {
            self.device
                .device()
                .CreateCommandList::<_, _, ID3D12VideoDecodeCommandList>(
                    0,
                    D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE,
                    &allocator,
                    None::<&ID3D12PipelineState>,
                )
        }
        .map_err(|e| backend_error("Create video-decode command list", e.code().0))?;
        Ok((allocator, list))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one sample submission must keep barriers, codec state commit and frame lease atomic"
    )]
    fn decode_one(
        &mut self,
        sample: EncodedSample<'_>,
        target: usize,
        codec: Codec,
        format: BitstreamFormat,
    ) -> VideoResult<()> {
        self.pending
            .try_reserve(1)
            .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?;
        let (allocator, list) = self.command_resources()?;
        let output_arc = self
            .output
            .as_ref()
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d12DecodeSession::send_sample",
                    "output texture is not configured",
                )
            })?
            .clone();
        let mut texture = output_arc.lock().map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d12DecodeSession::send_sample",
                "output texture lock is poisoned",
            )
        })?;
        let mut previous = Vec::new();
        previous
            .try_reserve_exact(self.surface_count as usize)
            .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?;
        for slice in 0..self.surface_count {
            let state = texture.slice_state(slice)?;
            previous.push(state);
            let wanted = if slice as usize == target {
                D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE
            } else {
                D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
            };
            if state != wanted
                && let Err(error) = texture.transition_decode_slice(&list, slice, wanted)
            {
                for (i, old) in previous.iter().copied().enumerate() {
                    if let Ok(index) = u32::try_from(i) {
                        texture.restore_unsubmitted_slice_state(index, old);
                    }
                }
                return Err(error);
            }
        }
        let prepared_avc: VideoResult<Result<_, D3d12SubmissionError>> = (|| {
            let submission = if codec == Codec::Avc {
                let decoder = self.decoder.as_ref().ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "decoder is not configured",
                    )
                })?;
                let Some(CodecDecodeState::Avc(state)) = self.state.as_mut() else {
                    return Err(decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "AVC state mismatch",
                    ));
                };
                let mut prepared = decoder.prepare_avc_sample(state, target, sample, format)?;
                {
                    let references = decode_reference_table(&texture, self.surface_count, target)?;
                    prepared.record(
                        &list,
                        &texture,
                        u32::try_from(target)
                            .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?,
                        &references,
                    )?;
                }
                restore_decode_texture_states(&mut texture, &list, &previous)?;
                // SAFETY: The prepared picture has recorded all commands and this list has not executed.
                unsafe { list.Close() }
                    .map_err(|e| backend_error("Close video-decode command list", e.code().0))?;
                let queue = self.queue.as_mut().ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "decode queue is not configured",
                    )
                })?;
                let result = prepared.submit(queue, &list, &allocator, &[&texture]);
                result.map(|pending| (pending, None))
            } else if codec == Codec::Hevc {
                let decoder = self.decoder.as_ref().ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "decoder is not configured",
                    )
                })?;
                let Some(CodecDecodeState::Hevc(state)) = self.state.as_mut() else {
                    return Err(decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "HEVC state mismatch",
                    ));
                };
                let mut prepared = decoder.prepare_hevc_sample(state, target, sample, format)?;
                {
                    let references = decode_reference_table(&texture, self.surface_count, target)?;
                    prepared.record(
                        &list,
                        &texture,
                        u32::try_from(target)
                            .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?,
                        &references,
                    )?;
                }
                restore_decode_texture_states(&mut texture, &list, &previous)?;
                // SAFETY: The prepared picture has recorded all commands and this list has not executed.
                unsafe { list.Close() }
                    .map_err(|e| backend_error("Close video-decode command list", e.code().0))?;
                let queue = self.queue.as_mut().ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "decode queue is not configured",
                    )
                })?;
                let result = prepared.submit(queue, &list, &allocator, &[&texture]);
                result.map(|pending| (pending, None))
            } else {
                let decoder = self.decoder.as_ref().ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "decoder is not configured",
                    )
                })?;
                let sequence = self.av1_sequence_header.as_deref().ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "AV1 sequence header is missing",
                    )
                })?;
                let (header, tiles) = av1_sample_parts(sample.data)?;
                let Some(CodecDecodeState::Av1(state)) = self.state.as_mut() else {
                    return Err(decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "AV1 state mismatch",
                    ));
                };
                let mut prepared = decoder
                    .prepare_av1_intra_obus_with_state(state, target, sequence, header, &tiles)?;
                let aspect = prepared.sample_aspect_ratio()?;
                {
                    let references = decode_reference_table(&texture, self.surface_count, target)?;
                    prepared.record_with_references(
                        &list,
                        &texture,
                        u32::try_from(target)
                            .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?,
                        &references,
                    )?;
                }
                restore_decode_texture_states(&mut texture, &list, &previous)?;
                // SAFETY: All picture commands are recorded and this list has not executed.
                unsafe { list.Close() }
                    .map_err(|e| backend_error("Close video-decode command list", e.code().0))?;
                let queue = self.queue.as_mut().ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "decode queue is not configured",
                    )
                })?;
                let result = prepared.submit(queue, &list, &allocator, &[&texture]);
                result.map(|pending| (pending, Some(aspect)))
            };
            Ok(submission)
        })();
        let prepared_avc = match prepared_avc {
            Ok(submission) => submission,
            Err(error) => {
                for (i, old) in previous.iter().copied().enumerate() {
                    if let Ok(index) = u32::try_from(i) {
                        texture.restore_unsubmitted_slice_state(index, old);
                    }
                }
                return Err(error);
            }
        };
        let (pending, av1_metadata) = match prepared_avc {
            Ok(pending) => pending,
            Err(error) => {
                if error.submitted {
                    self.failed = true;
                } else {
                    for (i, old) in previous.iter().copied().enumerate() {
                        if let Ok(index) = u32::try_from(i) {
                            texture.restore_unsubmitted_slice_state(index, old);
                        }
                    }
                }
                return Err(error.error);
            }
        };
        drop(texture);
        let completion = pending.completion();
        self.completions[target] = Some(completion);
        let decoder = self.decoder.as_ref().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d12DecodeSession::send_sample",
                "decoder is not configured",
            )
        })?;
        let info = FrameInfo {
            size: decoder.coded_size(),
            format: decoder.output_format(),
            color: ColorInfo::default(),
            pts: sample.timing.pts,
            duration: sample.timing.duration,
            time_base: sample.timing.time_base,
        };
        let mut frame = crate::D3d12ScaleFrame::from_decoder_output(
            output_arc,
            u32::try_from(target)
                .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?,
            info,
            pending,
            self.leases[target].clone(),
        )?;
        if let Some(aspect) = av1_metadata {
            frame.set_sample_aspect_ratio(aspect);
        }
        self.pending.push_back(frame);
        Ok(())
    }
}

fn restore_decode_texture_states(
    texture: &mut D3d12Texture,
    list: &ID3D12VideoDecodeCommandList,
    states: &[windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES],
) -> VideoResult<()> {
    for (index, state) in states.iter().copied().enumerate() {
        let slice = u32::try_from(index)
            .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?;
        if texture.slice_state(slice)? != state {
            texture.transition_decode_slice(list, slice, state)?;
        }
    }
    Ok(())
}

fn decode_reference_table(
    texture: &D3d12Texture,
    surface_count: u32,
    target: usize,
) -> VideoResult<Vec<Option<(&D3d12Texture, u32)>>> {
    let count = usize::try_from(surface_count)
        .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?;
    let mut references = Vec::new();
    references
        .try_reserve_exact(count)
        .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?;
    for index in 0..count {
        references.push(if index == target {
            None
        } else {
            Some((
                texture,
                u32::try_from(index)
                    .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?,
            ))
        });
    }
    Ok(references)
}

impl D3dDecode for D3d12DecodeSession {
    type Frame = crate::D3d12ScaleFrame;
    fn capabilities(&self) -> &[DecodeCapability] {
        &self.capabilities
    }
    fn configure(&mut self, config: &DecoderConfig) -> VideoResult<()> {
        let state = config.create_codec_state()?;
        let private = config.parse_codec_private()?;
        let sequence = match private {
            crate::CodecPrivateData::Av1 {
                sequence_header_obu,
            } => Some(sequence_header_obu),
            _ => None,
        };
        let decoder = D3d12Decoder::configure(&self.device, config, self.surface_count)?;
        let texture = decoder.create_output_texture(
            u16::try_from(self.surface_count)
                .map_err(|_| allocation_error("D3d12DecodeSession::configure"))?,
        )?;
        let queue = self
            .device
            .create_queue(D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE)?;
        self.pending.clear();
        self.output = Some(Arc::new(Mutex::new(texture)));
        self.queue = Some(queue);
        self.decoder = Some(decoder);
        self.state = Some(state);
        self.config = Some(config.clone());
        self.av1_sequence_header = sequence;
        self.completions.fill(None);
        self.draining = false;
        self.failed = false;
        Ok(())
    }
    fn send_sample(&mut self, sample: EncodedSample<'_>) -> VideoResult<()> {
        if self.draining || self.failed {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12DecodeSession::send_sample",
                "decoder is draining or must be reconfigured after submission failure",
            ));
        }
        sample.validate()?;
        let (codec, format) = self
            .config
            .as_ref()
            .map(|c| (c.profile.codec(), c.bitstream_format))
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d12DecodeSession::send_sample",
                    "decoder is not configured",
                )
            })?;
        if codec == Codec::Av1 {
            let (header, _) = av1_sample_parts(sample.data)?;
            let decoder = self.decoder.as_ref().ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d12DecodeSession::send_sample",
                    "decoder is not configured",
                )
            })?;
            let sequence = self.av1_sequence_header.as_deref().ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d12DecodeSession::send_sample",
                    "AV1 sequence header is missing",
                )
            })?;
            let existing = match self.state.as_ref() {
                Some(CodecDecodeState::Av1(state)) => {
                    decoder.av1_show_existing_surface(state, sequence, header)?
                }
                _ => {
                    return Err(decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d12DecodeSession::send_sample",
                        "AV1 state mismatch",
                    ));
                }
            };
            if let Some(surface) = existing {
                self.pending
                    .try_reserve(1)
                    .map_err(|_| allocation_error("D3d12DecodeSession::send_sample"))?;
                let render_size = match self.state.as_ref() {
                    Some(CodecDecodeState::Av1(state)) => state.surface_render_size(surface)?,
                    _ => decoder.coded_size(),
                };
                let aspect =
                    crate::SampleAspectRatio::from_render_size(decoder.coded_size(), render_size)?;
                let info = FrameInfo {
                    size: decoder.coded_size(),
                    format: decoder.output_format(),
                    color: ColorInfo::default(),
                    pts: sample.timing.pts,
                    duration: sample.timing.duration,
                    time_base: sample.timing.time_base,
                };
                let output = self
                    .output
                    .as_ref()
                    .ok_or_else(|| {
                        decode_error(
                            VideoErrorKind::InvalidState,
                            "D3d12DecodeSession::send_sample",
                            "output texture is not configured",
                        )
                    })?
                    .clone();
                let index = usize::from(surface);
                let mut frame = crate::D3d12ScaleFrame::from_existing_decoder_output(
                    output,
                    u32::from(surface),
                    info,
                    self.completions[index].clone(),
                    self.leases[index].clone(),
                )?;
                frame.set_sample_aspect_ratio(aspect);
                self.pending.push_back(frame);
                return Ok(());
            }
        }
        let target = self.target()?;
        self.decode_one(sample, target, codec, format)
    }
    fn receive_frame(&mut self) -> VideoResult<Option<Self::Frame>> {
        Ok(self.pending.pop_front())
    }
    fn drain(&mut self) -> VideoResult<()> {
        if self.decoder.is_none() {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12DecodeSession::drain",
                "decoder is not configured",
            ));
        }
        self.draining = true;
        Ok(())
    }
    fn flush(&mut self) -> VideoResult<()> {
        if self.failed {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d12DecodeSession::flush",
                "a post-submit failure left GPU completion unknown; configure a new decoder before resuming",
            ));
        }
        self.pending.clear();
        if let Some(state) = &mut self.state {
            state.reset();
        }
        self.completions.fill(None);
        self.draining = false;
        Ok(())
    }
}

fn av1_sample_parts(data: &[u8]) -> VideoResult<(&[u8], Vec<&[u8]>)> {
    let mut header = None;
    let mut tiles = Vec::new();
    for obu in crate::obu::ObuIter::new(data) {
        let obu = obu?;
        match obu.kind {
            crate::obu::ObuType::FRAME_HEADER
            | crate::obu::ObuType::REDUNDANT_FRAME_HEADER
            | crate::obu::ObuType::FRAME => {
                if header.replace(obu.raw).is_some() {
                    return Err(decode_error(
                        VideoErrorKind::InvalidInput,
                        "D3d12DecodeSession::send_sample",
                        "sample contains multiple AV1 frame headers",
                    ));
                }
            }
            crate::obu::ObuType::TILE_GROUP => tiles.push(obu.raw),
            _ => {}
        }
    }
    header.map(|value| (value, tiles)).ok_or_else(|| {
        decode_error(
            VideoErrorKind::InvalidInput,
            "D3d12DecodeSession::send_sample",
            "sample has no AV1 frame header",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{D3d12DecodeArgument, make_picture_arguments, validate_argument_list};
    use crate::{VideoError, VideoErrorKind, VideoResult};
    use windows::Win32::Media::MediaFoundation::{
        D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
        D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
        D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
    };

    #[test]
    fn validates_unique_nonempty_d3d12_frame_arguments() -> VideoResult<()> {
        let valid = [
            D3d12DecodeArgument {
                kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
                data: vec![1, 2],
            },
            D3d12DecodeArgument {
                kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
                data: vec![3, 4],
            },
        ];
        validate_argument_list(&valid)?;

        let duplicate = [valid[0].clone(), valid[0].clone()];
        let error = validate_argument_list(&duplicate).err().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "test",
                "duplicate frame argument unexpectedly passed",
            )
        })?;
        assert_eq!(error.kind(), VideoErrorKind::InvalidInput);

        let empty = [D3d12DecodeArgument {
            kind: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
            data: Vec::new(),
        }];
        let error = validate_argument_list(&empty).err().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "test",
                "empty frame argument unexpectedly passed",
            )
        })?;
        assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
        Ok(())
    }

    #[test]
    fn assembles_codec_picture_quantization_and_slice_arguments() -> VideoResult<()> {
        let arguments = make_picture_arguments(vec![1], vec![2], vec![3], "test")?;
        assert_eq!(arguments.len(), 3);
        assert_eq!(
            arguments
                .iter()
                .map(|argument| argument.kind)
                .collect::<Vec<_>>(),
            [
                D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
                D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
                D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
            ]
        );
        Ok(())
    }

    #[test]
    fn reference_table_covers_sparse_surface_indices() {
        let sparse = [None, None, Some(2), None, None, None, None, Some(7)];
        assert!(super::validate_reference_slots(&[(1, 2), (2, 7)], &sparse).is_ok());
        assert!(super::validate_reference_slots(&[(1, 2), (2, 7)], &sparse[..2]).is_err());
        assert!(super::validate_reference_slots(&[(1, 3)], &sparse).is_err());
        assert!(super::validate_reference_slots::<u8>(&[], &[]).is_ok());
    }

    #[test]
    fn target_surface_must_fit_decoder_heap() {
        assert_eq!(super::validate_target_surface(0, 4, "test").ok(), Some(0));
        assert_eq!(super::validate_target_surface(3, 4, "test").ok(), Some(3));
        assert!(super::validate_target_surface(4, 4, "test").is_err());
        assert!(super::validate_target_surface(usize::MAX, 16, "test").is_err());
    }

    #[test]
    fn rejects_unimplemented_decode_configuration_requirements() -> VideoResult<()> {
        use windows::Win32::Media::MediaFoundation::{
            D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_HEIGHT_ALIGNMENT_MULTIPLE_32_REQUIRED,
            D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_NONE,
            D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_REFERENCE_ONLY_ALLOCATIONS_REQUIRED,
        };
        let aligned = crate::FrameSize::new(1920, 1088)?;
        let unaligned = crate::FrameSize::new(1920, 1080)?;
        assert!(
            super::decode_surface_size(unaligned, D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_NONE)
                .is_ok()
        );
        assert!(
            super::decode_surface_size(
                aligned,
                D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_HEIGHT_ALIGNMENT_MULTIPLE_32_REQUIRED
            )
            .is_ok()
        );
        assert_eq!(
            super::decode_surface_size(
                unaligned,
                D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_HEIGHT_ALIGNMENT_MULTIPLE_32_REQUIRED
            )?,
            aligned
        );
        assert!(
            super::decode_surface_size(
                crate::FrameSize::new(1, u32::MAX)?,
                D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_HEIGHT_ALIGNMENT_MULTIPLE_32_REQUIRED
            )
            .is_err()
        );
        assert!(
            super::decode_surface_size(
                aligned,
                D3D12_VIDEO_DECODE_CONFIGURATION_FLAG_REFERENCE_ONLY_ALLOCATIONS_REQUIRED
            )
            .is_err()
        );
        Ok(())
    }
}
