//! Minimal V4L2 output for v4l2loopback: set an I420 format, then write() whole frames.

use rust_i18n::t;
use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;

const VIDIOC_QUERYCAP: libc::c_ulong = 0x8068_5600; // _IOR('V', 0, struct v4l2_capability)
const VIDIOC_S_FMT: libc::c_ulong = 0xc0d0_5605; // _IOWR('V', 5, struct v4l2_format)
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_CAP_VIDEO_OUTPUT: u32 = 0x0000_0002;
const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const FOURCC_YU12: u32 = u32::from_le_bytes(*b"YU12");

#[repr(C)]
struct V4l2Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Default)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
struct V4l2Format {
    type_: u32,
    // The kernel union contains pointers, so it is 8-byte aligned and 200 bytes long.
    fmt: [u64; 25],
}

const _: () = assert!(size_of::<V4l2Capability>() == 104);
const _: () = assert!(size_of::<V4l2Format>() == 208);

pub struct LoopbackOutput {
    file: File,
    pub width: usize,
    pub height: usize,
    frame: Vec<u8>,
}

impl crate::webcam::VirtualCamera for LoopbackOutput {
    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn write_black(&mut self) -> Result<()> {
        LoopbackOutput::write_black(self)
    }

    fn write_packed(&mut self, frame: &[u8]) -> Result<()> {
        LoopbackOutput::write_packed(self, frame)
    }

    fn write_i420(
        &mut self,
        src_size: (usize, usize),
        strides: (usize, usize, usize),
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<()> {
        LoopbackOutput::write_i420(self, src_size, strides, y, u, v)
    }
}

impl LoopbackOutput {
    pub fn open(path: &str, width: usize, height: usize) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| t!("err.v4l2_open", path = path))?;
        let fd = file.as_raw_fd();

        let mut cap: V4l2Capability = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, VIDIOC_QUERYCAP, &mut cap) } < 0 {
            bail!("{}", t!("err.v4l2_querycap", path = path, error = std::io::Error::last_os_error()));
        }
        let caps = if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 { cap.device_caps } else { cap.capabilities };
        let driver = cstr(&cap.driver);
        // Guard against writing into a real webcam node by mistake.
        if !driver.contains("loopback") {
            bail!("{}", t!("err.v4l2_not_loopback", path = path, driver = driver));
        }
        // With exclusive_caps=1 the node only advertises OUTPUT while no producer is attached.
        if caps & V4L2_CAP_VIDEO_OUTPUT == 0 {
            eprintln!("{}", t!("log.v4l2_no_output", path = path));
        }

        let pix = V4l2PixFormat {
            width: width as u32,
            height: height as u32,
            pixelformat: FOURCC_YU12,
            field: V4L2_FIELD_NONE,
            bytesperline: width as u32,
            sizeimage: (width * height * 3 / 2) as u32,
            ..Default::default()
        };
        let mut fmt = V4l2Format { type_: V4L2_BUF_TYPE_VIDEO_OUTPUT, fmt: [0; 25] };
        unsafe {
            std::ptr::copy_nonoverlapping(
                &pix as *const _ as *const u8,
                fmt.fmt.as_mut_ptr() as *mut u8,
                size_of::<V4l2PixFormat>(),
            );
        }
        if unsafe { libc::ioctl(fd, VIDIOC_S_FMT, &mut fmt) } < 0 {
            bail!("{}", t!("err.v4l2_format", path = path, width = width, height = height, error = std::io::Error::last_os_error()));
        }

        let mut frame = vec![0u8; width * height * 3 / 2];
        crate::yuv::fill_black(&mut frame, width, height);
        Ok(Self { file, width, height, frame })
    }

    pub fn write_black(&mut self) -> Result<()> {
        crate::yuv::fill_black(&mut self.frame, self.width, self.height);
        self.file.write_all(&self.frame).context(t!("err.v4l2_write"))
    }

    /// Writes a packed I420 frame that already has the output size (the no-signal picture).
    pub fn write_packed(&mut self, frame: &[u8]) -> Result<()> {
        self.file.write_all(frame).context(t!("err.v4l2_write"))
    }

    /// Fits the source I420 planes into the output size (nearest neighbour, letterboxed) and writes one frame.
    pub fn write_i420(
        &mut self,
        src: (usize, usize),
        strides: (usize, usize, usize),
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<()> {
        crate::yuv::scale_i420(src, strides, y, u, v, &mut self.frame, self.width, self.height);
        self.file.write_all(&self.frame).context(t!("err.v4l2_write"))
    }
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}
