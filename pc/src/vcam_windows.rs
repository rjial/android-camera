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
use vcam_ipc::{CLSID_STR, FRAME_LEN, HEIGHT, WIDTH, Writer};
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MF_VERSION, MFCreateVirtualCamera, MFIsVirtualCameraTypeSupported, MFSTARTUP_NOSOCKET, MFShutdown,
    MFStartup, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::HSTRING;

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

unsafe fn create_camera() -> Result<IMFVirtualCamera> {
    unsafe {
        // Windows 10 has no MFCreateVirtualCamera at all.
        if !MFIsVirtualCameraTypeSupported(MFVirtualCameraType_SoftwareCameraSource).is_ok_and(|b| b.as_bool()) {
            bail!("{}", t!("err.vcam_unsupported"));
        }
        let camera = MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            &HSTRING::from(FRIENDLY_NAME),
            &HSTRING::from(CLSID_STR),
            None,
        )
        .with_context(|| t!("err.vcam_create"))?;
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
        // Create the shared section up front so a failure (e.g. not running elevated) is reported here,
        // then move the writer onto its own thread.
        let writer = Writer::create().with_context(|| t!("err.vcam_open", path = r"Global\acam-vcam-video"))?;
        // The camera second: until frames arrive the DLL shows its test pattern.
        let host = Host::start()?;
        let mailbox = Arc::new(Mailbox { slot: Mutex::new(Slot::Empty), ready: Condvar::new() });
        let thread = {
            let mailbox = mailbox.clone();
            std::thread::spawn(move || writer_loop(writer, &mailbox))
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

fn writer_loop(mut writer: Writer, mailbox: &Mailbox) {
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
        writer.write(&frame);
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
}
