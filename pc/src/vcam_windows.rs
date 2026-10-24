//! Windows virtual webcam output: the producer half of the Media Foundation virtual camera. It
//! publishes decoded frames into the shared memory the `vcam-source` DLL reads (see `vcam-ipc`).
//! The DLL, loaded by the Windows Frame Server, turns them into the "ACAM Test Camera" device.
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
use anyhow::{Context, Result};
use rust_i18n::t;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use vcam_ipc::{FRAME_LEN, HEIGHT, WIDTH, Writer};

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
}

impl WindowsCamera {
    pub fn open() -> Result<Self> {
        // Create the shared section up front so a failure (e.g. not running elevated) is reported here,
        // then move the writer onto its own thread.
        let writer = Writer::create().with_context(|| t!("err.vcam_open", path = r"Global\acam-vcam-video"))?;
        let mailbox = Arc::new(Mailbox { slot: Mutex::new(Slot::Empty), ready: Condvar::new() });
        let thread = {
            let mailbox = mailbox.clone();
            std::thread::spawn(move || writer_loop(writer, &mailbox))
        };
        Ok(Self { i420: vec![0u8; FRAME_LEN], mailbox, writer: Some(thread) })
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
