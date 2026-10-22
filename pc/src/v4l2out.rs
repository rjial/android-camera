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
        fill_black(&mut frame, width, height);
        Ok(Self { file, width, height, frame })
    }

    pub fn write_black(&mut self) -> Result<()> {
        fill_black(&mut self.frame, self.width, self.height);
        self.file.write_all(&self.frame).context(t!("err.v4l2_write"))
    }

    /// Writes a packed I420 frame that already has the output size (the no-signal picture).
    pub fn write_packed(&mut self, frame: &[u8]) -> Result<()> {
        self.file.write_all(frame).context(t!("err.v4l2_write"))
    }

    /// Fits the source I420 planes into the output size (nearest neighbour, letterboxed) and writes one frame.
    pub fn write_i420(
        &mut self,
        (sw, sh): (usize, usize),
        (ys, us, vs): (usize, usize, usize),
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<()> {
        let (dw, dh) = (self.width, self.height);
        let (y_out, uv_out) = self.frame.split_at_mut(dw * dh);
        let (u_out, v_out) = uv_out.split_at_mut((dw / 2) * (dh / 2));

        if (sw, sh) == (dw, dh) {
            copy_plane(y, ys, y_out, dw, dh);
            copy_plane(u, us, u_out, dw / 2, dh / 2);
            copy_plane(v, vs, v_out, dw / 2, dh / 2);
        } else {
            fill_black(&mut self.frame, dw, dh);
            let (y_out, uv_out) = self.frame.split_at_mut(dw * dh);
            let (u_out, v_out) = uv_out.split_at_mut((dw / 2) * (dh / 2));
            let r = fit(sw, sh, dw, dh);
            scale_plane(y, ys, sw, sh, y_out, dw, r);
            let rc = Rect { x: r.x / 2, y: r.y / 2, w: r.w / 2, h: r.h / 2 };
            scale_plane(u, us, sw / 2, sh / 2, u_out, dw / 2, rc);
            scale_plane(v, vs, sw / 2, sh / 2, v_out, dw / 2, rc);
        }
        self.file.write_all(&self.frame).context(t!("err.v4l2_write"))
    }
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn fill_black(frame: &mut [u8], w: usize, h: usize) {
    let (y, uv) = frame.split_at_mut(w * h);
    y.fill(16);
    uv.fill(128);
}

fn copy_plane(src: &[u8], stride: usize, dst: &mut [u8], w: usize, h: usize) {
    for row in 0..h {
        dst[row * w..row * w + w].copy_from_slice(&src[row * stride..row * stride + w]);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

/// Largest rectangle with the source aspect ratio centred in the destination, even-aligned.
fn fit(sw: usize, sh: usize, dw: usize, dh: usize) -> Rect {
    let (w, h) = if sw * dh > sh * dw { (dw, sh * dw / sw) } else { (sw * dh / sh, dh) };
    let (w, h) = (w & !1, h & !1);
    Rect { x: ((dw - w) / 2) & !1, y: ((dh - h) / 2) & !1, w, h }
}

fn scale_plane(src: &[u8], stride: usize, sw: usize, sh: usize, dst: &mut [u8], dstride: usize, r: Rect) {
    for dy in 0..r.h {
        let srow = &src[(dy * sh / r.h) * stride..];
        let drow = &mut dst[(r.y + dy) * dstride + r.x..];
        for dx in 0..r.w {
            drow[dx] = srow[dx * sw / r.w];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes_portrait_into_landscape() {
        assert_eq!(fit(720, 1280, 1280, 720), Rect { x: 438, y: 0, w: 404, h: 720 });
        assert_eq!(fit(1280, 720, 1280, 720), Rect { x: 0, y: 0, w: 1280, h: 720 });
        assert_eq!(fit(1920, 1080, 1280, 720), Rect { x: 0, y: 0, w: 1280, h: 720 });
    }

    #[test]
    fn scale_plane_downsamples() {
        let src: Vec<u8> = (0..16).collect(); // 4x4
        let mut dst = vec![0u8; 4];
        scale_plane(&src, 4, 4, 4, &mut dst, 2, Rect { x: 0, y: 0, w: 2, h: 2 });
        assert_eq!(dst, vec![0, 2, 8, 10]);
    }
}
