//! Standard COM class factory: `DllGetClassObject` (see `lib.rs`) hands one of these out, and
//! `IMFVirtualCamera::Start` calls `CreateInstance` on it asking for `IMFActivate` — not
//! `IMFMediaSource` directly, see `activate.rs` for why.

use crate::activate::Activate;
use windows::Win32::Foundation::CLASS_E_NOAGGREGATION;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::{BOOL, GUID, Interface, IUnknown, Ref, Result, implement};

#[implement(IClassFactory)]
pub struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        if outer.as_ref().is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activate = Activate::create()?;
        unsafe { activate.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}
