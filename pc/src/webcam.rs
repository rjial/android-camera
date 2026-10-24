//! Cross-platform virtual webcam output. The Linux implementation (v4l2loopback) lives in
//! `v4l2out.rs`; other platforms don't have one yet (see TODO.md).

use anyhow::Result;

/// Sink for decoded video frames, backed by whatever the OS offers as a virtual camera.
pub trait VirtualCamera: Send {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn write_black(&mut self) -> Result<()>;
    /// Writes a packed I420 frame that already has the output size (the no-signal picture).
    fn write_packed(&mut self, frame: &[u8]) -> Result<()>;
    /// Fits the source I420 planes into the output size (nearest neighbour, letterboxed) and writes one frame.
    fn write_i420(
        &mut self,
        src_size: (usize, usize),
        strides: (usize, usize, usize),
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<()>;
}

#[cfg(target_os = "linux")]
pub fn open(path: &str, width: usize, height: usize) -> Result<Box<dyn VirtualCamera>> {
    Ok(Box::new(crate::v4l2out::LoopbackOutput::open(path, width, height)?))
}

#[cfg(windows)]
pub fn open(_path: &str, _width: usize, _height: usize) -> Result<Box<dyn VirtualCamera>> {
    // The Windows virtual camera is a fixed size (see vcam_windows / vcam-ipc), so the requested
    // device path and size are ignored; phone frames are scaled into it.
    Ok(Box::new(crate::vcam_windows::WindowsCamera::open()?))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn open(_path: &str, _width: usize, _height: usize) -> Result<Box<dyn VirtualCamera>> {
    use rust_i18n::t;
    anyhow::bail!("{}", t!("err.webcam_unsupported"))
}
