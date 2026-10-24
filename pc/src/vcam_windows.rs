//! Windows virtual webcam output. It publishes decoded frames into the shared memory the
//! `vcam-source` DLL reads (see `vcam-ipc`), and it creates the "ACAM Camera" device itself with
//! `MFCreateVirtualCamera`: the DLL, loaded by the Windows Frame Server, is the camera's media
//! source. The camera has session lifetime, so Windows removes it when acam exits, even on a crash.
//! (`vcam-source`'s `create_camera` binary does the same on its own, for testing the DLL alone.)
//!
//! The camera streams a fixed [`vcam_ipc::WIDTH`]x[`vcam_ipc::HEIGHT`] NV12 image (the media type the
//! DLL advertises); phone frames of any size are scaled/letterboxed into it, exactly like the Linux
//! v4l2loopback output.
//!
//! The actual shared-memory write runs on its own thread with a single-slot "latest frame" mailbox:
//! the engine's decode/network loop must never block on output, or the incoming stream backs up and
//! latency grows. [`write_i420`] does the (cheap, CPU-bound) scale + NV12 conversion inline, then
//! hands the finished frame to that thread and returns; if the writer can't keep up it simply drops
//! older frames.

use crate::webcam::VirtualCamera;
use crate::yuv;
use anyhow::{Context, Result, bail};
use rust_i18n::t;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use vcam_ipc::{CLSID_STR, FRAME_LEN, HEIGHT, WIDTH, Writer};
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MF_VERSION, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup, MFVirtualCameraAccess,
    MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime, MFVirtualCameraLifetime_Session, MFVirtualCameraType,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::{BOOL, GUID, HRESULT, HSTRING, Interface, PCWSTR, s, w};

/// What apps list the camera as.
const FRIENDLY_NAME: &str = "ACAM Camera";

/// The virtual camera device. It lives on its own thread, which owns the COM object for its whole
/// life and removes the camera when told to stop.
struct Host {
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Host {
    fn start() -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
            match create_camera() {
                Ok(camera) => {
                    let _ = ready_tx.send(Ok(()));
                    // Until stopped, or until the WindowsCamera is dropped (the sender goes away).
                    let _ = stop_rx.recv();
                    let _ = camera.Stop();
                    let _ = camera.Remove();
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
            let _ = MFShutdown();
        });
        ready_rx.recv().context("virtual camera thread exited")??;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type IsSupportedFn = unsafe extern "system" fn(MFVirtualCameraType, *mut BOOL) -> HRESULT;
type CreateFn = unsafe extern "system" fn(
    MFVirtualCameraType,
    MFVirtualCameraLifetime,
    MFVirtualCameraAccess,
    PCWSTR,
    PCWSTR,
    *const GUID,
    u32,
    *mut *mut core::ffi::c_void,
) -> HRESULT;

/// `MFIsVirtualCameraTypeSupported` and `MFCreateVirtualCamera`, looked up at run time: they only
/// exist from Windows 11, and importing them statically would stop acam from starting at all on
/// Windows 10 (where it still works, just without the virtual camera).
unsafe fn virtual_camera_api() -> Option<(IsSupportedFn, CreateFn)> {
    unsafe {
        let dll = LoadLibraryExW(w!("mfsensorgroup.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let supported = GetProcAddress(dll, s!("MFIsVirtualCameraTypeSupported"))?;
        let create = GetProcAddress(dll, s!("MFCreateVirtualCamera"))?;
        Some((std::mem::transmute::<_, IsSupportedFn>(supported), std::mem::transmute::<_, CreateFn>(create)))
    }
}

unsafe fn create_camera() -> Result<IMFVirtualCamera> {
    unsafe {
        let Some((is_supported, create)) = virtual_camera_api() else { bail!("{}", t!("err.vcam_unsupported")) };
        let mut supported = BOOL(0);
        if is_supported(MFVirtualCameraType_SoftwareCameraSource, &mut supported).is_err() || !supported.as_bool() {
            bail!("{}", t!("err.vcam_unsupported"));
        }
        let (name, clsid) = (HSTRING::from(FRIENDLY_NAME), HSTRING::from(CLSID_STR));
        let mut raw = std::ptr::null_mut();
        create(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            PCWSTR(name.as_ptr()),
            PCWSTR(clsid.as_ptr()),
            std::ptr::null(),
            0,
            &mut raw,
        )
        .ok()
        .with_context(|| t!("err.vcam_create"))?;
        let camera = IMFVirtualCamera::from_raw(raw);
        if let Err(e) = camera.Start(None) {
            let _ = camera.Remove();
            return Err(e).with_context(|| t!("err.vcam_create"));
        }
        Ok(camera)
    }
}

enum Slot {
    Empty,
    Frame(Vec<u8>),
    Shutdown,
}

struct Mailbox {
    slot: Mutex<Slot>,
    ready: Condvar,
}

pub struct WindowsCamera {
    /// Scratch I420 at the output size, reused per frame; converted to NV12 before handoff.
    i420: Vec<u8>,
    mailbox: Arc<Mailbox>,
    writer: Option<JoinHandle<()>>,
    /// Dropped after the writer thread has stopped (see `Drop`).
    _host: Host,
}

impl WindowsCamera {
    pub fn open() -> Result<Self> {
        // The shared section is attached later, by the writer thread (see `writer_loop`).
        let host = Host::start()?;
        let mailbox = Arc::new(Mailbox { slot: Mutex::new(Slot::Empty), ready: Condvar::new() });
        let thread = {
            let mailbox = mailbox.clone();
            std::thread::spawn(move || writer_loop(&mailbox))
        };
        Ok(Self { i420: vec![0u8; FRAME_LEN], mailbox, writer: Some(thread), _host: host })
    }

    /// Converts the current `i420` scratch to NV12 and hands it to the writer thread, replacing any
    /// frame not yet written (drop-oldest).
    fn publish_i420(&mut self) {
        let mut nv12 = vec![0u8; FRAME_LEN];
        yuv::i420_to_nv12(&self.i420, &mut nv12, WIDTH, HEIGHT);
        let mut slot = self.mailbox.slot.lock().unwrap();
        if !matches!(*slot, Slot::Shutdown) {
            *slot = Slot::Frame(nv12);
            self.mailbox.ready.notify_one();
        }
    }
}

impl Drop for WindowsCamera {
    fn drop(&mut self) {
        {
            let mut slot = self.mailbox.slot.lock().unwrap();
            *slot = Slot::Shutdown;
            self.mailbox.ready.notify_one();
        }
        if let Some(t) = self.writer.take() {
            let _ = t.join();
        }
    }
}

/// Writes frames into the shared section once it can be attached. Without elevation that is only
/// after the DLL has created it, i.e. once an app opens the camera — until then frames are dropped,
/// which costs nothing since nobody is watching. Once attached the handle is kept, so the section
/// outlives the app closing the camera and the next app gets frames at once.
fn writer_loop(mailbox: &Mailbox) {
    const RETRY: Duration = Duration::from_millis(200);
    let mut writer: Option<Writer> = None;
    let mut tried: Option<Instant> = None;
    loop {
        let frame = {
            let mut slot = mailbox.slot.lock().unwrap();
            loop {
                match std::mem::replace(&mut *slot, Slot::Empty) {
                    Slot::Empty => slot = mailbox.ready.wait(slot).unwrap(),
                    Slot::Frame(f) => break f,
                    Slot::Shutdown => return,
                }
            }
        };
        if writer.is_none() && tried.is_none_or(|t| t.elapsed() >= RETRY) {
            tried = Some(Instant::now());
            writer = Writer::open().ok();
        }
        if let Some(w) = writer.as_mut() {
            w.write(&frame);
        }
    }
}

impl VirtualCamera for WindowsCamera {
    fn width(&self) -> usize {
        WIDTH
    }

    fn height(&self) -> usize {
        HEIGHT
    }

    fn write_black(&mut self) -> Result<()> {
        yuv::fill_black(&mut self.i420, WIDTH, HEIGHT);
        self.publish_i420();
        Ok(())
    }

    /// The no-signal picture is already a packed I420 frame at the output size.
    fn write_packed(&mut self, frame: &[u8]) -> Result<()> {
        self.i420.copy_from_slice(frame);
        self.publish_i420();
        Ok(())
    }

    fn write_i420(
        &mut self,
        src: (usize, usize),
        strides: (usize, usize, usize),
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<()> {
        yuv::scale_i420(src, strides, y, u, v, &mut self.i420, WIDTH, HEIGHT);
        self.publish_i420();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::MediaFoundation::{
        IMFActivate, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MFCreateAttributes, MFEnumDeviceSources,
    };
    use windows::Win32::System::Com::CoTaskMemFree;

    /// Friendly names of the cameras Media Foundation lists right now (Windows appends " (Windows
    /// Virtual Camera)" to ours).
    fn cameras() -> Vec<String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1).unwrap();
            let attrs = attrs.unwrap();
            attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).unwrap();
            let (mut list, mut n) = (std::ptr::null_mut::<Option<IMFActivate>>(), 0u32);
            MFEnumDeviceSources(&attrs, &mut list, &mut n).unwrap();
            let names = std::slice::from_raw_parts_mut(list, n as usize)
                .iter_mut()
                .filter_map(|a| a.take())
                .filter_map(|a| {
                    let (mut s, mut len) = (windows::core::PWSTR::null(), 0);
                    a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut s, &mut len).ok()?;
                    let name = s.to_string().ok();
                    CoTaskMemFree(Some(s.0 as _));
                    name
                })
                .collect();
            CoTaskMemFree(Some(list as _));
            names
        }
    }

    /// Needs the vcam-source DLL registered (scripts/windows-vcam-register.ps1 -Register), but not
    /// elevation: `cargo test --bin acam host_adds_and_removes_camera -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn host_adds_and_removes_camera() {
        assert!(!cameras().iter().any(|n| n.starts_with(FRIENDLY_NAME)), "a stale {FRIENDLY_NAME} is already listed");
        let host = Host::start().unwrap();
        let listed = cameras();
        println!("cameras while hosting: {listed:?}");
        assert!(listed.iter().any(|n| n.starts_with(FRIENDLY_NAME)));
        drop(host);
        let listed = cameras();
        println!("cameras after drop: {listed:?}");
        assert!(!listed.iter().any(|n| n.starts_with(FRIENDLY_NAME)));
    }

    /// End to end without elevation: host the camera, open it like an app would (which makes the
    /// Frame Server load the DLL, which creates the shared section), attach a `Writer` as a normal
    /// user, and check a frame we write comes out of the camera. Needs the current DLL installed
    /// (scripts/windows-vcam-register.ps1 -Register, or copy it over the registered one):
    /// `cargo test --bin acam frames_reach_camera_without_admin -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn frames_reach_camera_without_admin() {
        use windows::Win32::Media::MediaFoundation::{IMFMediaSource, MFCreateSourceReaderFromMediaSource};
        // The Frame Server keeps the DLL (and its handle on the section) after a camera app closes, so
        // only the first run after (re)installing the DLL shows the DLL creating the section.
        if Writer::open().is_ok() {
            println!("note: the section already exists (an earlier run, or acam elevated), so this run only checks attaching");
        }
        unsafe {
            let _host = Host::start().unwrap();
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1).unwrap();
            let attrs = attrs.unwrap();
            attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).unwrap();
            let (mut list, mut n) = (std::ptr::null_mut::<Option<IMFActivate>>(), 0u32);
            MFEnumDeviceSources(&attrs, &mut list, &mut n).unwrap();
            let activate = std::slice::from_raw_parts_mut(list, n as usize)
                .iter_mut()
                .filter_map(|a| a.take())
                .find(|a| {
                    let (mut s, mut len) = (windows::core::PWSTR::null(), 0);
                    a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut s, &mut len).is_ok()
                        && s.to_string().is_ok_and(|n| n.starts_with(FRIENDLY_NAME))
                })
                .expect("ACAM Camera not listed");
            let source: IMFMediaSource = activate.ActivateObject().unwrap();
            let reader = MFCreateSourceReaderFromMediaSource(&source, None).unwrap();
            const FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;

            // Reads frames until one's top-left luma is `want`, for up to ~3 s.
            let wait_for = |want: Option<u8>| -> Option<u8> {
                let mut last = None;
                for _ in 0..90 {
                    let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
                    reader.ReadSample(FIRST_VIDEO_STREAM, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)).unwrap();
                    let Some(sample) = sample else { continue };
                    let buf = sample.ConvertToContiguousBuffer().unwrap();
                    let mut p = std::ptr::null_mut();
                    buf.Lock(&mut p, None, None).unwrap();
                    last = Some(*p);
                    buf.Unlock().unwrap();
                    if want.is_none() || last == want {
                        break;
                    }
                }
                last
            };
            // The first sample makes the DLL create the section.
            wait_for(None);

            let mut writer = None;
            for _ in 0..20 {
                writer = Writer::open().ok();
                if writer.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let mut writer = writer.expect("could not attach to the section the DLL should have created");
            let mut frame = vec![128u8; FRAME_LEN];
            frame[..WIDTH * HEIGHT].fill(200);
            for _ in 0..3 {
                writer.write(&frame);
            }
            assert_eq!(wait_for(Some(200)), Some(200), "our frame never came out of the camera");
            println!("frame written by a non-elevated process came out of {FRIENDLY_NAME}");
            let _ = source.Shutdown();
        }
    }
}
