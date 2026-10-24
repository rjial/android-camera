//! The custom `IMFMediaSource` that Frame Server loads to back the virtual camera. Milestone 1:
//! one hardcoded video stream serving the test pattern from `testpattern.rs` — no real phone
//! video, no IPC to `acam.exe` yet (see the Windows plan).

use crate::media_stream::{Inner, MediaStream};
use std::sync::Arc;
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::{GUID, Interface, IUnknown, PCWSTR, Ref, Result, implement, w};

const GUID_NULL: GUID = GUID::zeroed();

/// `KSCAMERAPROFILE_Legacy` (`ksmedia.h`) — not exposed by the `windows` crate. Declares "this
/// camera only has the one, default mode," which is all Milestone 1's single hardcoded stream is.
const KSCAMERAPROFILE_LEGACY: GUID = GUID::from_values(0xB4894D81, 0x62B7, 0x4EEC, [0x87, 0x40, 0x80, 0x65, 0x8C, 0x4A, 0x9D, 0x3E]);

// Frame Server requires `IMFMediaSourceEx` specifically (confirmed by logging the IID
// `IMFVirtualCamera::Start` -> `IMFActivate::ActivateObject` queries for and matching it against
// `mfidl.h`) — plain `IMFMediaSource` is not enough, even though that's the interface this type
// actually cares about implementing.
#[implement(IMFMediaSourceEx)]
pub struct MediaSource {
    events: IMFMediaEventQueue,
    inner: Arc<Inner>,
    /// Handed out via the `MENewStream` event; Frame Server calls back into it directly from then on.
    stream: IMFMediaStream,
    started: std::sync::atomic::AtomicBool,
    /// Built once in `create()`, not freshly per `GetSourceAttributes` call — a fresh store every
    /// call (the original approach) left the sensor-profile-collection setup redone every time and,
    /// more importantly, handed the caller a different COM object identity on each call.
    attrs: IMFAttributes,
}

impl MediaSource {
    /// Builds the source and its one stream, then links the stream's back-reference to the
    /// source. Must run before the `IMFMediaSource` COM wrapper is handed to anything external —
    /// see `activate.rs`, which does exactly that and nothing else in between.
    ///
    /// `copy_from` is the owning `Activate`'s own attribute store; per the confirmed-working
    /// `smourier/VCamSample` reference, the media source's attributes start as a copy of whatever
    /// the activator already carries (e.g. `MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES`).
    pub fn create(copy_from: &IMFAttributes) -> Result<IMFMediaSourceEx> {
        let inner = Arc::new(Inner::new()?);
        let stream: IMFMediaStream = MediaStream::new(inner.clone()).into();

        let attrs: IMFAttributes = {
            let mut a = None;
            unsafe { MFCreateAttributes(&mut a, 0)? };
            a.expect("MFCreateAttributes sets Some on success")
        };
        unsafe { copy_from.CopyAllItems(&attrs)? };
        // Frame Server requires `GetSourceAttributes` to succeed and to carry a real
        // `MF_DEVICEMFT_SENSORPROFILE_COLLECTION` (confirmed via Microsoft's "Camera Profile V2
        // Device MFT Support" docs and the VCamSample reference, after local SDK headers alone
        // weren't enough to explain the `MF_E_ATTRIBUTENOTFOUND` this caused when absent).
        let profiles = unsafe { MFCreateSensorProfileCollection()? };
        let profile = unsafe { MFCreateSensorProfile(&KSCAMERAPROFILE_LEGACY, 0, PCWSTR::null())? };
        // Filter-set mini-language per Microsoft's documented Device MFT sample ("Camera Profile V2
        // Device MFT Support"): RES/FRT/SUT with nothing after `==` means "no constraint." Matches
        // our single stream 0 at up to 30 fps, any resolution/subtype.
        unsafe { profile.AddProfileFilter(0, w!("((RES==;FRT<=30,1;SUT==))"))? };
        unsafe { profiles.AddProfile(&profile)? };
        unsafe { attrs.SetUnknown(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION, &profiles)? };
        // Hint the capture pipeline to minimise buffering latency for this live source.
        unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, 1)? };

        let source: IMFMediaSourceEx = Self {
            events: unsafe { MFCreateEventQueue()? },
            inner: inner.clone(),
            stream: stream.clone(),
            started: std::sync::atomic::AtomicBool::new(false),
            attrs,
        }
        .into();
        inner.source.set(source.cast()?).map_err(|_| ()).expect("source set exactly once");
        Ok(source)
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.events.GetEvent(flags.0) }
    }

    fn BeginGetEvent(&self, callback: Ref<'_, IMFAsyncCallback>, state: Ref<'_, IUnknown>) -> Result<()> {
        unsafe { self.events.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<'_, IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.events.EndGetEvent(result.ok()?) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        extended_type: *const GUID,
        status: windows::core::HRESULT,
        value: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> Result<()> {
        unsafe { self.events.QueueEventParamVar(met, extended_type, status, value) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        let pd = unsafe { MFCreatePresentationDescriptor(Some(&[Some(self.inner.stream_descriptor.clone())]))? };
        unsafe { pd.SelectStream(0)? };
        Ok(pd)
    }

    fn Start(
        &self,
        _presentation_descriptor: Ref<'_, IMFPresentationDescriptor>,
        _time_format: *const GUID,
        _start_position: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> Result<()> {
        let first_start = !self.started.swap(true, std::sync::atomic::Ordering::SeqCst);
        unsafe {
            if first_start {
                self.events.QueueEventParamUnk(MENewStream.0 as u32, &GUID_NULL, windows::core::HRESULT(0), &self.stream)?;
            }
            self.events.QueueEventParamVar(MESourceStarted.0 as u32, &GUID_NULL, windows::core::HRESULT(0), std::ptr::null())?;
            self.inner.events.QueueEventParamVar(MEStreamStarted.0 as u32, &GUID_NULL, windows::core::HRESULT(0), std::ptr::null())?;
        }
        Ok(())
    }

    fn Stop(&self) -> Result<()> {
        self.started.store(false, std::sync::atomic::Ordering::SeqCst);
        unsafe {
            self.inner.events.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID_NULL, windows::core::HRESULT(0), std::ptr::null())?;
            self.events.QueueEventParamVar(MESourceStopped.0 as u32, &GUID_NULL, windows::core::HRESULT(0), std::ptr::null())?;
        }
        Ok(())
    }

    fn Pause(&self) -> Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        unsafe {
            self.inner.events.Shutdown()?;
            self.events.Shutdown()
        }
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    /// Returns the same persistent attribute store every call (built once in `create()`) rather
    /// than a fresh one each time — see the field doc on `MediaSource::attrs`.
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        Ok(self.attrs.clone())
    }

    fn GetStreamAttributes(&self, _stream_identifier: u32) -> Result<IMFAttributes> {
        Ok(self.inner.stream_attrs.clone())
    }

    /// No Direct3D surfaces produced by this source (plain system-memory NV12 buffers).
    fn SetD3DManager(&self, _manager: Ref<'_, IUnknown>) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}
