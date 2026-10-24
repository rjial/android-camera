//! The single video stream exposed by `MediaSource`. Frame Server pulls samples from here on
//! demand (`RequestSample`); there is no internal timer — the consumer sets the pace.

use crate::testpattern;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::{GUID, IUnknown, Ref, Result, implement};

/// 30 fps in 100ns units (the unit Media Foundation timestamps use).
const FRAME_DURATION_HNS: i64 = 10_000_000 / testpattern::FPS as i64;
const GUID_NULL: GUID = GUID::zeroed();

/// `PINNAME_VIDEO_CAPTURE` (`ksmedia.h`) — not exposed by the `windows` crate. The standard pin
/// category for a video capture stream, expected in `MF_DEVICESTREAM_STREAM_CATEGORY` below.
const PINNAME_VIDEO_CAPTURE: GUID = GUID::from_values(0xFB6C4281, 0x0353, 0x11d1, [0x90, 0x5F, 0x00, 0x00, 0xC0, 0xCC, 0x16, 0xBA]);

/// Shared between `MediaSource` and `MediaStream` so the source can push lifecycle events into
/// the stream's own event queue without going through the `IMFMediaStream` COM interface.
pub struct Inner {
    pub stream_descriptor: IMFStreamDescriptor,
    /// Returned by `MediaSource::GetStreamAttributes` — built once here rather than fresh per call,
    /// same reasoning as `MediaSource::attrs`. Contents mirror the confirmed-working
    /// `smourier/VCamSample` reference's `MediaStream::Initialize`.
    pub stream_attrs: IMFAttributes,
    pub events: IMFMediaEventQueue,
    frame_index: AtomicU64,
    /// Shared memory `acam.exe` publishes phone frames into; opened lazily on the first sample
    /// request and reused. `None` until `acam.exe` has created it — until then `RequestSample` falls
    /// back to the test pattern, so the camera is alive even with no phone connected.
    reader: Mutex<Option<vcam_ipc::Reader>>,
    /// The last live frame actually served. The consumer pulls far faster than `acam.exe` publishes
    /// (seen at 300+ fps), so most reads either repeat the latest frame or occasionally catch the
    /// writer mid-update; on such a transient miss we re-serve this instead of flashing the test
    /// pattern. `has_live` guards it — before the first live frame it is not yet meaningful.
    last_good: Mutex<Vec<u8>>,
    has_live: AtomicBool,
    /// Set once, right after the owning `MediaSource` COM object is constructed (see
    /// `class_factory.rs`) — a plain back-reference, not a `Weak`, so this pair of objects leaks a
    /// reference cycle. Acceptable for Milestone 1's short-lived manual test process; revisit
    /// before any longer-running use.
    pub source: OnceLock<IMFMediaSource>,
}

impl Inner {
    pub fn new() -> Result<Self> {
        let media_type = unsafe { MFCreateMediaType()? };
        unsafe {
            media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(testpattern::WIDTH as u32, testpattern::HEIGHT as u32))?;
            media_type.SetUINT64(&MF_MT_FRAME_RATE, pack(testpattern::FPS, 1))?;
            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            media_type.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
            media_type.SetUINT32(&MF_MT_SAMPLE_SIZE, testpattern::FRAME_LEN as u32)?;
            media_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, testpattern::WIDTH as u32)?;
        }
        let stream_descriptor = unsafe { MFCreateStreamDescriptor(0, &[Some(media_type.clone())])? };
        let handler = unsafe { stream_descriptor.GetMediaTypeHandler()? };
        unsafe { handler.SetCurrentMediaType(&media_type)? };
        // Must match the stream index used in the sensor profile's `AddProfileFilter(0, ...)` call
        // (media_source.rs) — Microsoft's Camera Profile V2 docs: "the Pin ID ... must match the
        // MF_DEVICESTREAM_STREAM_ID attribute presented on the output pin's attribute store."
        unsafe { stream_descriptor.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)? };

        let stream_attrs: IMFAttributes = {
            let mut a = None;
            unsafe { MFCreateAttributes(&mut a, 0)? };
            a.expect("MFCreateAttributes sets Some on success")
        };
        unsafe {
            stream_attrs.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            stream_attrs.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
            stream_attrs.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            stream_attrs.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;
            // Hint the capture pipeline to minimise buffering latency for this live source.
            stream_attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
        }

        Ok(Self {
            stream_descriptor,
            stream_attrs,
            events: unsafe { MFCreateEventQueue()? },
            frame_index: AtomicU64::new(0),
            reader: Mutex::new(None),
            last_good: Mutex::new(vec![0u8; testpattern::FRAME_LEN]),
            has_live: AtomicBool::new(false),
            source: OnceLock::new(),
        })
    }
}

fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

#[implement(IMFMediaStream)]
pub struct MediaStream {
    inner: std::sync::Arc<Inner>,
}

impl MediaStream {
    pub fn new(inner: std::sync::Arc<Inner>) -> Self {
        Self { inner }
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.inner.events.GetEvent(flags.0) }
    }

    fn BeginGetEvent(&self, callback: Ref<'_, IMFAsyncCallback>, state: Ref<'_, IUnknown>) -> Result<()> {
        unsafe { self.inner.events.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<'_, IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.inner.events.EndGetEvent(result.ok()?) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        extended_type: *const GUID,
        status: windows::core::HRESULT,
        value: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> Result<()> {
        unsafe { self.inner.events.QueueEventParamVar(met, extended_type, status, value) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        Ok(self.inner.source.get().expect("source set right after construction, see class_factory.rs").clone())
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        Ok(self.inner.stream_descriptor.clone())
    }

    fn RequestSample(&self, token: Ref<'_, IUnknown>) -> Result<()> {
        let index = self.inner.frame_index.fetch_add(1, Ordering::Relaxed);
        let mut frame = vec![0u8; testpattern::FRAME_LEN];
        // Prefer a live frame from acam.exe; fall back to the test pattern when it isn't streaming.
        let published_ns = {
            let mut guard = self.inner.reader.lock().unwrap();
            if guard.is_none() {
                *guard = vcam_ipc::Reader::open();
            }
            guard.as_ref().and_then(|r| r.read(&mut frame))
        };
        match published_ns {
            Some(_) => {
                self.inner.last_good.lock().unwrap().copy_from_slice(&frame);
                self.inner.has_live.store(true, Ordering::Relaxed);
            }
            // A transient seqlock miss while streaming: re-serve the last live frame rather than
            // flashing the test pattern. Only show the test pattern before any live frame arrives.
            None if self.inner.has_live.load(Ordering::Relaxed) => {
                frame.copy_from_slice(&self.inner.last_good.lock().unwrap());
            }
            None => testpattern::fill_test_pattern(&mut frame, index),
        }

        unsafe {
            let buffer = MFCreateMemoryBuffer(frame.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(frame.as_ptr(), ptr, frame.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(frame.len() as u32)?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            // Live source: stamp with the real system clock, not a counter from 0. A renderer whose
            // presentation clock is already at wall-clock time treats zero-based timestamps as far in
            // the past and drops every frame — the stream runs but shows black.
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(FRAME_DURATION_HNS)?;
            if let Some(token) = token.as_ref() {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            self.inner.events.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID_NULL, windows::core::HRESULT(0), &sample)?;
        }
        Ok(())
    }
}
