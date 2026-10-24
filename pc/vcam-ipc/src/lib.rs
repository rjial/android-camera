//! Shared-memory transport for the Windows virtual webcam: carries one decoded video frame from
//! `acam.exe` (the [`Writer`]) to the `vcam-source` DLL (the [`Reader`]), which the Windows Frame
//! Server loads in its own service process.
//!
//! The transport is a **pagefile-backed `Global\` section** (not a disk-backed file): the frame is
//! rewritten ~30x/s, and a disk-backed mapping's write-back saturates disk I/O and stalls the whole
//! producer process (the decode/network loop backs up and latency climbs without bound). Pagefile
//! backing lives in RAM. It must be `Global\` because the Frame Server runs in session 0 while
//! `acam.exe` runs in the user's session.
//!
//! Creating a `Global\` object needs `SeCreateGlobalPrivilege`, which services have and a normal
//! (non-elevated) user process doesn't — but *opening* one needs no privilege. So whichever side
//! comes first creates the section and the other opens it: in practice the DLL creates it when an app
//! starts using the camera, and a non-elevated `acam.exe` keeps retrying [`Writer::open`] until it
//! exists. (An elevated `acam.exe` can still create it itself.) The section's DACL lets the Frame
//! Server accounts, administrators and interactive users read and write it.
//!
//! One writer, one-or-more readers, lock-free via a seqlock: the writer bumps a sequence counter to
//! odd before touching the frame and to even after, and a reader retries until it reads the same even
//! sequence before and after copying — so it never sees a half-written frame.

use std::time::{SystemTime, UNIX_EPOCH};

/// CLSID of the `vcam-source` media source, in registry string form: the DLL registers itself
/// under it and `acam.exe` passes it to `MFCreateVirtualCamera`.
pub const CLSID_STR: &str = "{ACA40001-0000-4000-8000-000000000001}";

/// The virtual camera streams this fixed size (the `vcam-source` DLL advertises exactly this media
/// type). Frames of other sizes are scaled/letterboxed into it by the writer, mirroring how the
/// Linux v4l2loopback output keeps one fixed size across differently-sized streams.
pub const WIDTH: usize = 1280;
pub const HEIGHT: usize = 720;
/// NV12: a full-res Y plane followed by a half-res interleaved U/V plane.
pub const FRAME_LEN: usize = WIDTH * HEIGHT + WIDTH * HEIGHT / 2;

/// Wall clock in ns since the Unix epoch — the same value in every process on the machine, so the
/// reader can subtract it from its own `now_ns()` to get the producer→consumer age of a frame.
pub fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64
}

#[cfg(windows)]
mod imp {
    use super::{FRAME_LEN, HEIGHT, WIDTH, now_ns};
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
    use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
        PAGE_READWRITE, UnmapViewOfFile,
    };
    use windows::core::w;

    const MAGIC: u32 = u32::from_le_bytes(*b"ACVW");
    const FORMAT_NV12: u32 = 0;
    /// Header at the start of the section; the frame follows at [`FRAME_OFFSET`].
    /// `[magic, width, height, format, frame_bytes, _pad]` as u32, then `seq` (u64) and `publish_ns` (u64).
    const SEQ_OFFSET: usize = 24;
    /// Wall-clock time (ns since epoch) the current frame was published, for latency measurement.
    /// Written inside the seqlock so a reader gets a frame and its timestamp consistently.
    const TS_OFFSET: usize = 32;
    const FRAME_OFFSET: usize = 64;
    const SECTION_LEN: usize = FRAME_OFFSET + FRAME_LEN;

    fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn read_u32(buf: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
    }

    fn write_u64(buf: &mut [u8], offset: usize, value: u64) {
        buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn read_u64(buf: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
    }

    /// The seqlock counter lives inside the shared section so it is shared across processes.
    unsafe fn seq(ptr: *const u8) -> &'static AtomicU64 {
        unsafe { &*(ptr.add(SEQ_OFFSET) as *const AtomicU64) }
    }

    fn io_err(e: impl std::fmt::Display) -> std::io::Error {
        std::io::Error::other(e.to_string())
    }

    /// A mapped view of the section, read-write for both sides (the reader may be the one that
    /// creates it and must write the header).
    struct Section {
        handle: HANDLE,
        ptr: *mut u8,
    }

    impl Section {
        /// Opens the section, or creates it if it doesn't exist yet and this process may create
        /// `Global\` objects. A fresh (all-zero) section gets its header and a black frame.
        fn attach() -> std::io::Result<Self> {
            let name = w!("Global\\acam-vcam-video");
            let access = FILE_MAP_READ | FILE_MAP_WRITE;
            let handle = match unsafe { OpenFileMappingW(access.0, false, name) } {
                Ok(h) => h,
                Err(_) => create(name)?,
            };
            let view = unsafe { MapViewOfFile(handle, access, 0, 0, SECTION_LEN) };
            if view.Value.is_null() {
                let e = std::io::Error::last_os_error();
                unsafe { let _ = CloseHandle(handle); }
                return Err(e);
            }
            let s = Self { handle, ptr: view.Value as *mut u8 };
            let buf = unsafe { std::slice::from_raw_parts_mut(s.ptr, SECTION_LEN) };
            if read_u32(buf, 0) != MAGIC {
                // Fresh section. Two sides attaching at once both write the same values; the seq
                // counter is left alone (0 = no frame yet). Magic goes last so nobody skips this
                // while it is half done.
                write_u32(buf, 4, WIDTH as u32);
                write_u32(buf, 8, HEIGHT as u32);
                write_u32(buf, 12, FORMAT_NV12);
                write_u32(buf, 16, FRAME_LEN as u32);
                let (y, uv) = buf[FRAME_OFFSET..].split_at_mut(WIDTH * HEIGHT);
                y.fill(16);
                uv.fill(128);
                write_u32(buf, 0, MAGIC);
            }
            Ok(s)
        }

        fn buf(&self) -> &mut [u8] {
            unsafe { std::slice::from_raw_parts_mut(self.ptr, SECTION_LEN) }
        }
    }

    impl Drop for Section {
        fn drop(&mut self) {
            unsafe {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr as *mut c_void });
                let _ = CloseHandle(self.handle);
            }
        }
    }

    /// Creates the section with a DACL that lets both sides in: SYSTEM and Local Service (the Frame
    /// Server and Frame Server Monitor services), administrators, and interactive users (`acam.exe`,
    /// not elevated). Fails with access denied without `SeCreateGlobalPrivilege`.
    fn create(name: windows::core::PCWSTR) -> std::io::Result<HANDLE> {
        let mut psd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GRGW;;;IU)"),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
            .map_err(io_err)?;
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd.0,
            bInheritHandle: false.into(),
        };
        let handle = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, Some(&sa), PAGE_READWRITE, 0, SECTION_LEN as u32, name) };
        unsafe { LocalFree(Some(HLOCAL(psd.0))) };
        handle.map_err(io_err)
    }

    /// The frame producer, held by `acam.exe` for the lifetime of a virtual-camera session.
    pub struct Writer {
        section: Section,
    }

    // SAFETY: the section is process-wide valid; the Writer has a single logical owner (it is moved
    // onto a dedicated writer thread in acam) so there is no aliasing.
    unsafe impl Send for Writer {}

    impl Writer {
        /// Attaches to the section. Without elevation this only succeeds once the DLL has created it
        /// (i.e. an app is using the camera), so callers retry.
        pub fn open() -> std::io::Result<Self> {
            Ok(Self { section: Section::attach()? })
        }

        /// Publishes one NV12 frame (must be exactly [`FRAME_LEN`] bytes) tear-free, stamped with the
        /// current wall-clock time so the reader can measure how old the frame is when it serves it.
        pub fn write(&mut self, nv12: &[u8]) {
            assert_eq!(nv12.len(), FRAME_LEN);
            let buf = self.section.buf();
            let s = unsafe { seq(self.section.ptr) };
            let begin = s.load(Ordering::Relaxed);
            s.store(begin.wrapping_add(1), Ordering::Release); // odd: write in progress
            buf[FRAME_OFFSET..FRAME_OFFSET + FRAME_LEN].copy_from_slice(nv12);
            write_u64(buf, TS_OFFSET, now_ns());
            s.store(begin.wrapping_add(2), Ordering::Release); // even: frame complete
        }
    }

    /// The frame consumer, held by the `vcam-source` DLL.
    pub struct Reader {
        section: Section,
    }

    // SAFETY: the section is process-wide valid; the Reader has a single logical owner (it lives
    // behind a Mutex in the DLL).
    unsafe impl Send for Reader {}

    impl Reader {
        /// Attaches to the section, creating it if needed (the Frame Server may create `Global\`
        /// objects). `None` on failure — the DLL retries on the next sample request.
        pub fn open() -> Option<Self> {
            Section::attach().ok().map(|section| Self { section })
        }

        /// Copies the latest complete frame into `out` (must be [`FRAME_LEN`]) and returns the
        /// wall-clock time it was published (ns since epoch), or `None` if no frame has been published
        /// yet or a stable read could not be obtained in a few tries (writer too busy).
        pub fn read(&self, out: &mut [u8]) -> Option<u64> {
            assert_eq!(out.len(), FRAME_LEN);
            let buf = self.section.buf();
            let s = unsafe { seq(self.section.ptr) };
            for _ in 0..8 {
                let s1 = s.load(Ordering::Acquire);
                if s1 == 0 || s1 & 1 == 1 {
                    // 0: no frame yet; odd: a write is in progress.
                    std::hint::spin_loop();
                    continue;
                }
                out.copy_from_slice(&buf[FRAME_OFFSET..FRAME_OFFSET + FRAME_LEN]);
                let ts = read_u64(buf, TS_OFFSET);
                if s.load(Ordering::Acquire) == s1 {
                    return Some(ts);
                }
            }
            None
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// In one (non-service) process: whoever can, creates; a frame written is read back. Run
        /// elevated, or while an app is using the camera, or it is skipped.
        #[test]
        fn writer_to_reader() {
            let Ok(mut w) = Writer::open() else { return eprintln!("skipped: cannot create Global\\ section") };
            let r = Reader::open().unwrap();
            let frame: Vec<u8> = (0..FRAME_LEN).map(|i| (i % 251) as u8).collect();
            w.write(&frame);
            let mut out = vec![0u8; FRAME_LEN];
            r.read(&mut out).unwrap();
            assert!(out == frame);
        }
    }
}

/// Non-Windows stub so the workspace still builds on Linux (CI). This transport is Windows-only; the
/// types exist only to satisfy the compiler and are never constructed off-Windows.
#[cfg(not(windows))]
mod imp {
    pub struct Writer;
    pub struct Reader;

    impl Writer {
        pub fn open() -> std::io::Result<Self> {
            unreachable!("vcam-ipc is Windows-only")
        }
        pub fn write(&mut self, _nv12: &[u8]) {
            unreachable!("vcam-ipc is Windows-only")
        }
    }

    impl Reader {
        pub fn open() -> Option<Self> {
            None
        }
        pub fn read(&self, _out: &mut [u8]) -> Option<u64> {
            unreachable!("vcam-ipc is Windows-only")
        }
    }
}

pub use imp::{Reader, Writer};
