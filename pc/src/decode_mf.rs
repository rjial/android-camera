//! Windows H.264 decoding with the Media Foundation decoder MFT that ships with Windows
//! (`CLSID_MSH264DecoderMFT`). Given a D3D11 device it decodes on the GPU through DXVA, which is the
//! point: software decode on a constrained laptop throttles the whole CPU and latency spirals (see
//! TODO.md). Without one it still decodes in software, multithreaded.
//!
//! The decoder outputs NV12; frames are read back (for GPU frames `IMF2DBuffer::Lock2D` copies the
//! texture to system memory) and de-interleaved into I420, the format the rest of acam works in.
//! `MF_LOW_LATENCY` makes it emit each frame as soon as its access unit is in, instead of holding a
//! few back for reordering — phone encoders don't use B-frames, so nothing is lost.

use crate::decode::{VideoDecoder, Yuv};
use anyhow::{Context, Result, bail};
use std::mem::ManuallyDrop;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11Multithread,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::Interface;

/// 30 fps in 100 ns units. Only used to give samples increasing timestamps; frames are emitted as
/// soon as they decode regardless of the value.
const FRAME_DURATION: i64 = 333_333;

/// Set once a GPU decoder has failed mid-stream, so later decoders go straight to software.
static HARDWARE_FAILED: AtomicBool = AtomicBool::new(false);

pub fn hardware_failed() -> bool {
    HARDWARE_FAILED.load(Ordering::Relaxed)
}

pub struct MfDecoder {
    mft: IMFTransform,
    /// Kept alive for the decoder's lifetime when decoding on the GPU.
    _manager: Option<IMFDXGIDeviceManager>,
    hardware: bool,
    /// Display size (the decoder's buffers may be taller, e.g. 1088 rows for 1080p).
    size: (usize, usize),
    /// Height of the decoder's buffers as reported by the output type.
    coded_height: usize,
    pts: i64,
    /// The last frame, packed I420: Y, then U, then V.
    i420: Vec<u8>,
}

// The MFT is created in the multithreaded apartment and only ever used from one thread at a time.
unsafe impl Send for MfDecoder {}

impl MfDecoder {
    pub fn new(hardware: bool) -> Result<Self> {
        static STARTUP: Once = Once::new();
        unsafe {
            // S_FALSE (already initialised) is fine; the engine threads never enter an STA.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            STARTUP.call_once(|| {
                let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
            });
            let mft: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).context("H.264 decoder MFT")?;
            let attrs = mft.GetAttributes()?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
            let manager = if hardware { Some(attach_d3d11(&mft, &attrs).context("DXVA")?) } else { None };

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &input, 0).context("SetInputType(H.264)")?;

            let mut d = Self {
                mft,
                _manager: manager,
                hardware,
                size: (0, 0),
                coded_height: 0,
                pts: 0,
                i420: Vec::new(),
            };
            // Before the first SPS the decoder may not offer an output type yet; the stream-change
            // notification on the first frame sets it then.
            let _ = d.set_output_type();
            d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(d)
        }
    }

    /// Picks NV12 from the decoder's output types and records the frame geometry.
    unsafe fn set_output_type(&mut self) -> Result<()> {
        unsafe {
            for i in 0.. {
                let t = self.mft.GetOutputAvailableType(0, i).context("no NV12 output type")?;
                if t.GetGUID(&MF_MT_SUBTYPE)? != MFVideoFormat_NV12 {
                    continue;
                }
                self.mft.SetOutputType(0, &t, 0)?;
                let packed = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
                let (w, h) = ((packed >> 32) as usize, (packed & 0xffff_ffff) as usize);
                self.coded_height = h;
                self.size = display_size(&t).unwrap_or((w, h));
                return Ok(());
            }
            unreachable!()
        }
    }

    unsafe fn feed(&mut self, data: &[u8]) -> windows::core::Result<()> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.pts)?;
            sample.SetSampleDuration(FRAME_DURATION)?;
            self.pts += FRAME_DURATION;
            self.mft.ProcessInput(0, &sample, 0)
        }
    }

    /// Pulls one output: Ok(true) if a frame was copied into `i420`, Ok(false) if the decoder wants
    /// more input.
    unsafe fn pull(&mut self) -> Result<bool> {
        unsafe {
            loop {
                let info = self.mft.GetOutputStreamInfo(0)?;
                let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
                let mut out = MFT_OUTPUT_DATA_BUFFER::default();
                if info.dwFlags & provides == 0 {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize)?)?;
                    out.pSample = ManuallyDrop::new(Some(sample));
                }
                let mut status = 0;
                let result = self.mft.ProcessOutput(0, std::slice::from_mut(&mut out), &mut status);
                let sample = ManuallyDrop::take(&mut out.pSample);
                drop(ManuallyDrop::take(&mut out.pEvents));
                match result {
                    Ok(()) => {}
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        // First SPS, or a new resolution: renegotiate and try again.
                        self.set_output_type()?;
                        continue;
                    }
                    Err(e) => return Err(e).context("ProcessOutput"),
                }
                let Some(sample) = sample else { bail!("decoder returned no sample") };
                self.copy_frame(&sample)?;
                return Ok(true);
            }
        }
    }

    /// Reads the NV12 sample back and de-interleaves it into `i420`.
    unsafe fn copy_frame(&mut self, sample: &IMFSample) -> Result<()> {
        unsafe {
            let (w, h) = self.size;
            if w == 0 || h == 0 {
                bail!("frame before output type");
            }
            let buffer = sample.GetBufferByIndex(0)?;
            let (mut base, mut pitch, mut len) = (std::ptr::null_mut(), 0i32, 0u32);
            let locked2d = buffer.cast::<IMF2DBuffer2>().ok();
            let contiguous;
            if let Some(b2) = &locked2d {
                let mut start = std::ptr::null_mut();
                b2.Lock2DSize(MF2DBuffer_LockFlags_Read, &mut base, &mut pitch, &mut start, &mut len)?;
            } else {
                contiguous = sample.ConvertToContiguousBuffer()?;
                contiguous.Lock(&mut base, None, Some(&mut len))?;
                pitch = w as i32;
            }
            let result = (|| {
                if pitch <= 0 {
                    bail!("bottom-up NV12 is not supported");
                }
                let pitch = pitch as usize;
                // Rows in the Y plane (the chroma plane starts after them). A buffer that is exactly
                // one NV12 image tells us directly — a GPU texture can be taller than the output type
                // says; otherwise trust the output type's height.
                let len_rows = (len as usize) * 2 / 3 / pitch;
                let exact = (len as usize) * 2 % (3 * pitch) == 0;
                let rows = if exact { len_rows } else { self.coded_height }.max(h);
                if (len as usize) < pitch * rows + pitch * (h / 2 - 1) + w {
                    bail!("NV12 buffer too small: {len} bytes for {w}x{h} pitch {pitch}");
                }
                let src = std::slice::from_raw_parts(base as *const u8, len as usize);
                self.i420.resize(w * h * 3 / 2, 0);
                let (y, uv) = self.i420.split_at_mut(w * h);
                let (u, v) = uv.split_at_mut(w * h / 4);
                for row in 0..h {
                    y[row * w..][..w].copy_from_slice(&src[row * pitch..][..w]);
                }
                let chroma = &src[pitch * rows..];
                for row in 0..h / 2 {
                    let s = &chroma[row * pitch..][..w];
                    let (ur, vr) = (&mut u[row * w / 2..][..w / 2], &mut v[row * w / 2..][..w / 2]);
                    for (i, pair) in s.chunks_exact(2).enumerate() {
                        ur[i] = pair[0];
                        vr[i] = pair[1];
                    }
                }
                Ok(())
            })();
            match &locked2d {
                Some(b2) => b2.Unlock2D()?,
                None => buffer.Unlock().ok().unwrap_or(()),
            }
            result
        }
    }
}

impl VideoDecoder for MfDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Option<Yuv<'_>>> {
        let result = unsafe { self.decode_inner(data) };
        if result.is_err() && self.hardware {
            HARDWARE_FAILED.store(true, Ordering::Relaxed);
        }
        if !result? {
            return Ok(None);
        }
        let (w, h) = self.size;
        let (y, uv) = self.i420.split_at(w * h);
        let (u, v) = uv.split_at(w * h / 4);
        Ok(Some(Yuv { size: (w, h), strides: (w, w / 2, w / 2), y, u, v }))
    }
}

impl MfDecoder {
    unsafe fn decode_inner(&mut self, data: &[u8]) -> Result<bool> {
        unsafe {
            let mut got = false;
            if let Err(e) = self.feed(data) {
                if e.code() != MF_E_NOTACCEPTING {
                    return Err(e).context("ProcessInput");
                }
                // Full: drain what's ready, then the input fits.
                while self.pull()? {
                    got = true;
                }
                self.feed(data).context("ProcessInput")?;
            }
            // Keep only the newest frame if more than one came out.
            while self.pull()? {
                got = true;
            }
            Ok(got)
        }
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
        }
    }
}

/// The visible area when the decoder pads the frame (`MF_MT_MINIMUM_DISPLAY_APERTURE`, an
/// `MFVideoArea`: two `MFOffset`s then a `SIZE`).
unsafe fn display_size(t: &IMFMediaType) -> Option<(usize, usize)> {
    let mut blob = [0u8; 16];
    let mut n = 0u32;
    unsafe { t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut blob, Some(&mut n)).ok()? };
    let cx = i32::from_le_bytes(blob[8..12].try_into().ok()?);
    let cy = i32::from_le_bytes(blob[12..16].try_into().ok()?);
    (n == 16 && cx > 0 && cy > 0).then_some((cx as usize, cy as usize))
}

/// Gives the decoder a D3D11 device so it decodes with DXVA.
unsafe fn attach_d3d11(mft: &IMFTransform, attrs: &IMFAttributes) -> Result<IMFDXGIDeviceManager> {
    unsafe {
        if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
            bail!("decoder is not D3D11 aware");
        }
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .context("D3D11CreateDevice")?;
        let device = device.context("no D3D11 device")?;
        // The decoder uses the device from its own worker threads.
        let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
        let (mut token, mut manager) = (0u32, None);
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.context("no DXGI device manager")?;
        manager.ResetDevice(&device, token)?;
        mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
            .context("MFT_MESSAGE_SET_D3D_MANAGER")?;
        Ok(manager)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::encoder::Encoder;
    use openh264::formats::{YUVBuffer, YUVSource};

    /// Encodes a few frames with openh264 and checks the Media Foundation decoder reproduces them.
    fn roundtrip(hardware: bool, (w, h): (usize, usize)) {
        let mut decoder = match MfDecoder::new(hardware) {
            Ok(d) => d,
            // No GPU decoder on this machine, or no Media Foundation at all (Windows Server CI
            // runners): nothing to test. Desktop Windows always has the software decoder.
            Err(e) => return eprintln!("skipping {} decode test: {e:#}", if hardware { "GPU" } else { "CPU" }),
        };
        let mut encoder = Encoder::new().unwrap();
        let mut decoded = 0;
        for i in 0..10u8 {
            let mut frame = YUVBuffer::new(w, h);
            // A horizontal luma ramp shifted per frame, so a stale or garbled frame shows.
            let rgb: Vec<u8> =
                (0..w * h).flat_map(|p| [((p % w) as u8).wrapping_add(i * 8); 3]).collect();
            frame.read_rgb(openh264::formats::RgbSliceU8::new(&rgb, (w, h)));
            let au = encoder.encode(&frame).unwrap().to_vec();
            if let Some(f) = decoder.decode(&au).unwrap() {
                decoded += 1;
                assert_eq!(f.size, (w, h));
                // Compare a few luma samples with the source; lossy codec, so allow some slack.
                let src = frame.y();
                for &(x, yy) in &[(10, 10), (w / 2, h / 2), (w - 10, h - 10)] {
                    let (a, b) = (src[yy * w + x] as i32, f.y[yy * f.strides.0 + x] as i32);
                    assert!((a - b).abs() < 24, "frame {i} at ({x},{yy}): {a} vs {b}");
                }
            }
        }
        assert!(decoded >= 8, "only {decoded} of 10 frames came out (low latency mode off?)");
    }

    #[test]
    fn software_roundtrip() {
        roundtrip(false, (640, 480));
    }

    #[test]
    fn software_roundtrip_padded_height() {
        // 1080 isn't a multiple of 16, so the decoder pads its buffers.
        roundtrip(false, (1920, 1080));
    }

    /// `cargo test --release --bin acam decoder_speed -- --ignored --nocapture`: per-frame decode
    /// time of each decoder on this machine, for a 720p stream with motion.
    #[test]
    #[ignore]
    fn decoder_speed() {
        let (w, h) = (1280, 720);
        let mut encoder = Encoder::new().unwrap();
        let stream: Vec<Vec<u8>> = (0..120usize)
            .map(|i| {
                let rgb: Vec<u8> = (0..w * h)
                    .flat_map(|p| {
                        let (x, y) = (p % w, p / w);
                        [(x + i * 4) as u8, (y + i * 2) as u8, ((x ^ y) + i) as u8]
                    })
                    .collect();
                let mut frame = YUVBuffer::new(w, h);
                frame.read_rgb(openh264::formats::RgbSliceU8::new(&rgb, (w, h)));
                encoder.encode(&frame).unwrap().to_vec()
            })
            .collect();
        for (name, mut d) in [
            ("openh264", Box::new(crate::decode::tests::openh264()) as Box<dyn VideoDecoder>),
            ("MF CPU", Box::new(MfDecoder::new(false).unwrap())),
            ("MF GPU", Box::new(MfDecoder::new(true).unwrap())),
        ] {
            let t = std::time::Instant::now();
            let n = stream.iter().filter(|au| d.decode(au).unwrap().is_some()).count();
            println!("{name:>9}: {n} frames, {:.2} ms/frame", t.elapsed().as_secs_f64() * 1000.0 / stream.len() as f64);
        }
    }

    #[test]
    fn hardware_roundtrip() {
        roundtrip(true, (640, 480));
        roundtrip(true, (1920, 1080));
    }
}
