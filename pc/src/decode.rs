//! H.264 decoding behind one interface so the engine doesn't care which decoder runs. openh264
//! (software) works everywhere; on Windows the Media Foundation decoder (`decode_mf.rs`) is tried
//! first because it can decode on the GPU (DXVA), which keeps the CPU free on slow laptops where
//! sustained software decode throttles the whole machine.
//!
//! `ACAM_DECODER` overrides the choice: `openh264`, `mf` (Media Foundation, GPU if possible) or
//! `mf-sw` (Media Foundation, software only).

use anyhow::Result;
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use rust_i18n::t;

/// One decoded I420 frame, borrowed from the decoder until its next `decode` call.
pub struct Yuv<'a> {
    pub size: (usize, usize),
    pub strides: (usize, usize, usize),
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
}

pub trait VideoDecoder: Send {
    /// Feeds one Annex-B access unit (or SPS/PPS); returns a frame once one is ready.
    fn decode(&mut self, data: &[u8]) -> Result<Option<Yuv<'_>>>;
}

/// Opens the best available decoder and logs which one it is.
pub fn open(log: &dyn Fn(&str)) -> Result<Box<dyn VideoDecoder>> {
    let choice = std::env::var("ACAM_DECODER").unwrap_or_default();
    #[cfg(windows)]
    if choice != "openh264" {
        for hardware in [true, false] {
            if hardware && (choice == "mf-sw" || crate::decode_mf::hardware_failed()) {
                continue;
            }
            match crate::decode_mf::MfDecoder::new(hardware) {
                Ok(d) => {
                    let name = if hardware { "Media Foundation (GPU)" } else { "Media Foundation (CPU)" };
                    log(&t!("log.decoder", name = name));
                    return Ok(Box::new(d));
                }
                Err(e) => log(&t!("log.decoder_unavailable", name = "Media Foundation", error = format!("{e:#}"))),
            }
        }
    }
    let _ = choice;
    log(&t!("log.decoder", name = "openh264 (CPU)"));
    Ok(Box::new(OpenH264(Decoder::new()?)))
}

struct OpenH264(Decoder);

impl VideoDecoder for OpenH264 {
    fn decode(&mut self, data: &[u8]) -> Result<Option<Yuv<'_>>> {
        // SAFETY: `DecodedYUV` only borrows its planes from itself, but they point into buffers the
        // openh264 decoder owns and keeps until its next `decode` call — which `Yuv<'_>` borrowing
        // `&mut self` already rules out.
        fn extend<'a>(s: &[u8]) -> &'a [u8] {
            unsafe { std::slice::from_raw_parts(s.as_ptr(), s.len()) }
        }
        Ok(self.0.decode(data)?.map(|yuv| Yuv {
            size: yuv.dimensions(),
            strides: yuv.strides(),
            y: extend(yuv.y()),
            u: extend(yuv.u()),
            v: extend(yuv.v()),
        }))
    }
}

/// Converts an I420 frame to packed BGRA (BT.601 limited range) for the GUI preview.
pub fn i420_to_bgra(f: &Yuv<'_>, out: &mut [u8]) {
    let (w, h) = f.size;
    for row in 0..h {
        let yr = &f.y[row * f.strides.0..][..w];
        let ur = &f.u[(row / 2) * f.strides.1..];
        let vr = &f.v[(row / 2) * f.strides.2..];
        let dst = &mut out[row * w * 4..][..w * 4];
        for (x, px) in dst.chunks_exact_mut(4).enumerate() {
            let c = 298 * (yr[x] as i32 - 16);
            let (d, e) = (ur[x / 2] as i32 - 128, vr[x / 2] as i32 - 128);
            px[0] = ((c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
            px[1] = ((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            px[2] = ((c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            px[3] = 255;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn openh264() -> impl VideoDecoder {
        OpenH264(Decoder::new().unwrap())
    }

    #[test]
    fn bgra_of_grey_and_red() {
        // 2x2: Y=128 everywhere, neutral chroma → mid grey.
        let (y, u, v) = ([128u8; 4], [128u8], [128u8]);
        let f = Yuv { size: (2, 2), strides: (2, 1, 1), y: &y, u: &u, v: &v };
        let mut out = [0u8; 16];
        i420_to_bgra(&f, &mut out);
        assert_eq!(&out[..4], &[130, 130, 130, 255]);
        // BT.601 red is roughly Y=81, U=90, V=240.
        let (y, u, v) = ([81u8; 4], [90u8], [240u8]);
        let f = Yuv { size: (2, 2), strides: (2, 1, 1), y: &y, u: &u, v: &v };
        i420_to_bgra(&f, &mut out);
        assert!(out[2] > 240 && out[1] < 10 && out[0] < 10, "{:?}", &out[..4]);
    }
}
