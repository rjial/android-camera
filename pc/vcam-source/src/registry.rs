//! Minimal `regsvr32`-compatible COM registration: writes/removes
//! `HKEY_CLASSES_ROOT\CLSID\{...}\InprocServer32` pointing at this DLL. `regsvr32` itself just
//! loads the DLL and calls `DllRegisterServer`/`DllUnregisterServer` (see `lib.rs`) — it does not
//! touch the registry on its own.

use crate::clsid::CLSID_STR;
use anyhow::{Result, bail};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::core::{HSTRING, PCWSTR};

fn check(result: WIN32_ERROR, what: &str) -> Result<()> {
    if result.0 == 0 { Ok(()) } else { bail!("{what} failed: Windows error {}", result.0) }
}

fn inproc_server_subkey() -> String {
    format!("CLSID\\{CLSID_STR}\\InprocServer32")
}

fn clsid_subkey() -> String {
    format!("CLSID\\{CLSID_STR}")
}

/// Path to this DLL, found via an address inside it rather than relying on `DllMain` to stash
/// the module handle (there's no `DllMain` here — only the exports in `lib.rs`).
fn dll_path() -> Result<String> {
    let mut module = HMODULE::default();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            PCWSTR(dll_path as *const () as *const u16),
            &mut module,
        )?;
    }
    let mut buf = vec![0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) };
    if len == 0 || len as usize >= buf.len() {
        bail!("GetModuleFileNameW failed or path too long");
    }
    Ok(String::from_utf16_lossy(&buf[..len as usize]))
}

fn set_string_value(key: HKEY, name: PCWSTR, value: &str) -> Result<()> {
    let mut data: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
    data.extend_from_slice(&[0, 0]); // NUL terminator
    check(unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(&data)) }, "RegSetValueExW")
}

pub fn register() -> Result<()> {
    let path = dll_path()?;
    let mut key = HKEY::default();
    check(
        unsafe {
            RegCreateKeyExW(
                HKEY_CLASSES_ROOT,
                &HSTRING::from(inproc_server_subkey()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        },
        "RegCreateKeyExW",
    )?;
    set_string_value(key, PCWSTR::null(), &path)?;
    let threading_model = HSTRING::from("ThreadingModel");
    set_string_value(key, PCWSTR::from_raw(threading_model.as_ptr()), "Both")?;
    check(unsafe { RegCloseKey(key) }, "RegCloseKey")
}

pub fn unregister() -> Result<()> {
    check(unsafe { RegDeleteTreeW(HKEY_CLASSES_ROOT, &HSTRING::from(clsid_subkey())) }, "RegDeleteTreeW")
}
