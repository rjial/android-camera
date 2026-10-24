//! The COM object actually registered under our CLSID. `IMFVirtualCamera::Start` locates it via
//! `IClassFactory::CreateInstance` asking for `IMFActivate` (confirmed by logging the requested
//! IID during Milestone 1 bring-up and matching it against `mfobjects.h`), not `IMFMediaSource`
//! directly — `ActivateObject` is where the real `MediaSource` gets built, lazily, on demand.
//!
//! `IMFActivate` extends `IMFAttributes`, so this also carries a generic attribute store (created
//! via `MFCreateAttributes`) and forwards every `IMFAttributes` method to it — Milestone 1 doesn't
//! need custom attributes, just a working store so callers that probe/set attributes don't fail.

use crate::media_source::MediaSource;
use core::ffi::c_void;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFAttributes_Impl, IMFActivate, IMFActivate_Impl, IMFMediaSourceEx, MF_ATTRIBUTE_TYPE, MF_ATTRIBUTES_MATCH_TYPE,
    MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, MFCreateAttributes,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, GUID, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result, implement};

#[implement(IMFActivate)]
pub struct Activate {
    inner: IMFAttributes,
    /// Built once here (not lazily per `ActivateObject` call) and handed out every time —
    /// mirrors the confirmed-working `smourier/VCamSample` reference, where `Activator::Initialize`
    /// builds its `MediaSource` once up front and `ActivateObject` just re-QIs the same instance.
    source: IMFMediaSourceEx,
}

impl Activate {
    pub fn create() -> Result<IMFActivate> {
        let mut inner = None;
        unsafe { MFCreateAttributes(&mut inner, 0)? };
        let inner = inner.expect("MFCreateAttributes sets Some on success");
        // Per the reference implementation; our own logs show the caller merely tolerating this
        // key's absence, but setting it matches the confirmed-working pattern.
        unsafe { inner.SetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1)? };
        let source = MediaSource::create(&inner)?;
        Ok(Self { inner, source }.into())
    }
}

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        unsafe { self.source.query(riid, ppv).ok() }
    }

    // Milestone 1's only client is the short-lived `create_camera` test binary, which calls
    // `IMFMediaSource::Shutdown` itself before dropping the camera — nothing left here to release.
    fn ShutdownObject(&self) -> Result<()> {
        Ok(())
    }

    fn DetachObject(&self) -> Result<()> {
        Ok(())
    }
}

impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self, guidkey: *const GUID, pvalue: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.inner.GetItem(guidkey, Some(pvalue)) }
    }

    fn GetItemType(&self, guidkey: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.inner.GetItemType(guidkey) }
    }

    fn CompareItem(&self, guidkey: *const GUID, value: *const PROPVARIANT) -> Result<BOOL> {
        unsafe { self.inner.CompareItem(guidkey, value) }
    }

    fn Compare(&self, ptheirs: Ref<'_, IMFAttributes>, matchtype: MF_ATTRIBUTES_MATCH_TYPE) -> Result<BOOL> {
        unsafe { self.inner.Compare(ptheirs.as_ref(), matchtype) }
    }

    fn GetUINT32(&self, guidkey: *const GUID) -> Result<u32> {
        unsafe { self.inner.GetUINT32(guidkey) }
    }

    fn GetUINT64(&self, guidkey: *const GUID) -> Result<u64> {
        unsafe { self.inner.GetUINT64(guidkey) }
    }

    fn GetDouble(&self, guidkey: *const GUID) -> Result<f64> {
        unsafe { self.inner.GetDouble(guidkey) }
    }

    fn GetGUID(&self, guidkey: *const GUID) -> Result<GUID> {
        unsafe { self.inner.GetGUID(guidkey) }
    }

    fn GetStringLength(&self, guidkey: *const GUID) -> Result<u32> {
        unsafe { self.inner.GetStringLength(guidkey) }
    }

    fn GetString(&self, guidkey: *const GUID, pwszvalue: PWSTR, cchbufsize: u32, pcchlength: *mut u32) -> Result<()> {
        let slice: &mut [u16] =
            if cchbufsize == 0 { &mut [] } else { unsafe { core::slice::from_raw_parts_mut(pwszvalue.0, cchbufsize as usize) } };
        unsafe { self.inner.GetString(guidkey, slice, Some(pcchlength)) }
    }

    fn GetAllocatedString(&self, guidkey: *const GUID, ppwszvalue: *mut PWSTR, pcchlength: *mut u32) -> Result<()> {
        unsafe { self.inner.GetAllocatedString(guidkey, ppwszvalue, pcchlength) }
    }

    fn GetBlobSize(&self, guidkey: *const GUID) -> Result<u32> {
        unsafe { self.inner.GetBlobSize(guidkey) }
    }

    fn GetBlob(&self, guidkey: *const GUID, pbuf: *mut u8, cbbufsize: u32, pcbblobsize: *mut u32) -> Result<()> {
        let slice: &mut [u8] = if cbbufsize == 0 { &mut [] } else { unsafe { core::slice::from_raw_parts_mut(pbuf, cbbufsize as usize) } };
        unsafe { self.inner.GetBlob(guidkey, slice, Some(pcbblobsize)) }
    }

    fn GetAllocatedBlob(&self, guidkey: *const GUID, ppbuf: *mut *mut u8, pcbsize: *mut u32) -> Result<()> {
        unsafe { self.inner.GetAllocatedBlob(guidkey, ppbuf, pcbsize) }
    }

    fn GetUnknown(&self, guidkey: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        unsafe { (Interface::vtable(&self.inner).GetUnknown)(Interface::as_raw(&self.inner), guidkey, riid, ppv).ok() }
    }

    fn SetItem(&self, guidkey: *const GUID, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.inner.SetItem(guidkey, value) }
    }

    fn DeleteItem(&self, guidkey: *const GUID) -> Result<()> {
        unsafe { self.inner.DeleteItem(guidkey) }
    }

    fn DeleteAllItems(&self) -> Result<()> {
        unsafe { self.inner.DeleteAllItems() }
    }

    fn SetUINT32(&self, guidkey: *const GUID, unvalue: u32) -> Result<()> {
        unsafe { self.inner.SetUINT32(guidkey, unvalue) }
    }

    fn SetUINT64(&self, guidkey: *const GUID, unvalue: u64) -> Result<()> {
        unsafe { self.inner.SetUINT64(guidkey, unvalue) }
    }

    fn SetDouble(&self, guidkey: *const GUID, fvalue: f64) -> Result<()> {
        unsafe { self.inner.SetDouble(guidkey, fvalue) }
    }

    fn SetGUID(&self, guidkey: *const GUID, guidvalue: *const GUID) -> Result<()> {
        unsafe { self.inner.SetGUID(guidkey, guidvalue) }
    }

    fn SetString(&self, guidkey: *const GUID, wszvalue: &PCWSTR) -> Result<()> {
        unsafe { self.inner.SetString(guidkey, *wszvalue) }
    }

    fn SetBlob(&self, guidkey: *const GUID, pbuf: *const u8, cbbufsize: u32) -> Result<()> {
        let slice: &[u8] = if cbbufsize == 0 { &[] } else { unsafe { core::slice::from_raw_parts(pbuf, cbbufsize as usize) } };
        unsafe { self.inner.SetBlob(guidkey, slice) }
    }

    fn SetUnknown(&self, guidkey: *const GUID, punknown: Ref<'_, IUnknown>) -> Result<()> {
        unsafe { self.inner.SetUnknown(guidkey, punknown.as_ref()) }
    }

    fn LockStore(&self) -> Result<()> {
        unsafe { self.inner.LockStore() }
    }

    fn UnlockStore(&self) -> Result<()> {
        unsafe { self.inner.UnlockStore() }
    }

    fn GetCount(&self) -> Result<u32> {
        unsafe { self.inner.GetCount() }
    }

    fn GetItemByIndex(&self, unindex: u32, pguidkey: *mut GUID, pvalue: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.inner.GetItemByIndex(unindex, pguidkey, Some(pvalue)) }
    }

    fn CopyAllItems(&self, pdest: Ref<'_, IMFAttributes>) -> Result<()> {
        unsafe { self.inner.CopyAllItems(pdest.as_ref()) }
    }
}
