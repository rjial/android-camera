//! Milestone 1 of the Windows virtual-webcam plan: a Media Foundation `IMFMediaSource` that
//! serves a hardcoded NV12 test pattern, loaded by the Windows Frame Server service and exposed
//! system-wide as a camera via `MFCreateVirtualCamera` (see `src/bin/create_camera.rs`).
//!
//! This DLL is a standard in-process COM server: regsvr32 loads it and calls
//! `DllRegisterServer`/`DllUnregisterServer` below, which write/remove this crate's fixed CLSID
//! (see `clsid.rs`) under `HKEY_CLASSES_ROOT\CLSID`. No real phone video and no IPC to `acam.exe`
//! yet — that is Milestone 2.

mod activate;
mod class_factory;
pub mod clsid;
mod media_source;
mod media_stream;
mod registry;
pub mod testpattern;

use class_factory::ClassFactory;
use windows::Win32::Foundation::{E_NOINTERFACE, S_FALSE, S_OK};
use windows::Win32::System::Ole::SELFREG_E_CLASS;
use windows::core::{GUID, HRESULT, Interface};

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> HRESULT {
    unsafe {
        if *rclsid != clsid::CLSID {
            return E_NOINTERFACE;
        }
        let factory: windows::Win32::System::Com::IClassFactory = ClassFactory.into();
        factory.query(riid, ppv)
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    // Milestone 1 does no reference-count tracking of outstanding objects; always report "busy"
    // so Frame Server never unloads the DLL out from under a live camera. Revisit if real unload
    // support turns out to matter.
    S_FALSE
}

#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match registry::register() {
        Ok(()) => S_OK,
        Err(_) => SELFREG_E_CLASS,
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    match registry::unregister() {
        Ok(()) => S_OK,
        Err(_) => SELFREG_E_CLASS,
    }
}
