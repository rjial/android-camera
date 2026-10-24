//! Manual verification harness for Milestone 1 (see the plan): registers no COM state itself —
//! run `scripts/windows-vcam-register.ps1 -Register` as Administrator first — then creates the
//! "ACAM Test Camera" virtual camera and keeps it running until you press Enter, so you can check
//! for it in the Windows Camera app, Device Manager, or OBS.

use anyhow::{Context, Result};
use vcam_source::clsid::CLSID_STR;
use windows::Win32::Media::MediaFoundation::{
    MFCreateVirtualCamera, MFShutdown, MFStartup, MFSTARTUP_FULL, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType_SoftwareCameraSource, MF_VERSION,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::HSTRING;

fn main() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;

        let camera = MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            &HSTRING::from("ACAM Test Camera"),
            &HSTRING::from(CLSID_STR),
            None,
        )
        .context("MFCreateVirtualCamera (is the DLL registered? run scripts/windows-vcam-register.ps1 -Register as Administrator)")?;

        camera.Start(None).context("IMFVirtualCamera::Start")?;
        println!("ACAM Test Camera is live. Check the Windows Camera app, Device Manager > Cameras, or OBS.");
        println!("Press Enter to stop and remove it.");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();

        camera.Stop().context("IMFVirtualCamera::Stop")?;
        camera.Remove().context("IMFVirtualCamera::Remove")?;
        MFShutdown().context("MFShutdown")?;
        CoUninitialize();
    }
    Ok(())
}
