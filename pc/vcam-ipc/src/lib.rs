//! Shared-memory transport for the Windows virtual webcam: carries one decoded video frame from
//! `acam.exe` (the [`Writer`]) to the `vcam-source` DLL (the [`Reader`]), which the Windows Frame
//! Server loads in its own service process.
//!
//! The transport is a **pagefile-backed `Global\` section** (not a disk-backed file): the frame is
//! rewritten ~30x/s, and a disk-backed mapping's write-back saturates disk I/O and stalls the whole
//! producer process (the decode/network loop backs up and latency climbs without bound). Pagefile
//! backing lives in RAM. Creating a `Global\` section needs `SeCreateGlobalPrivilege`, so `acam.exe`
//! must run elevated (Administrator); the Frame Server services (Local Service / Local System) then
//! open it — a null DACL on the section grants them access across the session boundary.
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
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Security::{
        InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        SetSecurityDescriptorDacl,
    };

    /// `SECURITY_DESCRIPTOR_REVISION` from the Windows SDK (not re-exported by this `windows` feature set).
    const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
        OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
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

    /// The frame producer, held by `acam.exe` for the lifetime of a virtual-camera session.
    pub struct Writer {
        handle: HANDLE,
        ptr: *mut u8,
    }

    // SAFETY: `ptr`/`handle` refer to a process-wide-valid shared section; the Writer has a single
    // logical owner (it is moved onto a dedicated writer thread in acam) so there is no aliasing.
    unsafe impl Send for Writer {}

    impl Writer {
        /// Creates the `Global\` section and writes the header. The frame starts all-black so a reader
        /// that attaches before the first real frame shows black, not garbage. Fails (e.g. access
        /// denied) when not running elevated — creating a `Global\` object needs that privilege.
        pub fn create() -> std::io::Result<Self> {
            // Null DACL: grant every account (incl. the Frame Server services in session 0) access.
            let mut sd = SECURITY_DESCRIPTOR::default();
            let psd = PSECURITY_DESCRIPTOR(&mut sd as *mut _ as *mut c_void);
            unsafe {
                InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION).map_err(io_err)?;
                SetSecurityDescriptorDacl(psd, true, None, false).map_err(io_err)?;
            }
            let sa = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd.0,
                bInheritHandle: false.into(),
            };
            let handle = unsafe {
                CreateFileMappingW(INVALID_HANDLE_VALUE, Some(&sa), PAGE_READWRITE, 0, SECTION_LEN as u32, w!("Global\\acam-vcam-video"))
            }
            .map_err(io_err)?;
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, SECTION_LEN) };
            if view.Value.is_null() {
                unsafe { let _ = CloseHandle(handle); }
                return Err(std::io::Error::other("MapViewOfFile (write) returned null"));
            }
            let ptr = view.Value as *mut u8;
            let buf = unsafe { std::slice::from_raw_parts_mut(ptr, SECTION_LEN) };
            write_u32(buf, 0, MAGIC);
            write_u32(buf, 4, WIDTH as u32);
            write_u32(buf, 8, HEIGHT as u32);
            write_u32(buf, 12, FORMAT_NV12);
            write_u32(buf, 16, FRAME_LEN as u32);
            // seq starts at 0 (no frame yet); fill the frame with black NV12 (Y=16, UV=128).
            let (y, uv) = buf[FRAME_OFFSET..].split_at_mut(WIDTH * HEIGHT);
            y.fill(16);
            uv.fill(128);
            Ok(Self { handle, ptr })
        }

        /// Publishes one NV12 frame (must be exactly [`FRAME_LEN`] bytes) tear-free, stamped with the
        /// current wall-clock time so the reader can measure how old the frame is when it serves it.
        pub fn write(&mut self, nv12: &[u8]) {
            assert_eq!(nv12.len(), FRAME_LEN);
            let buf = unsafe { std::slice::from_raw_parts_mut(self.ptr, SECTION_LEN) };
            let s = unsafe { seq(self.ptr) };
            let begin = s.load(Ordering::Relaxed);
            s.store(begin.wrapping_add(1), Ordering::Release); // odd: write in progress
            buf[FRAME_OFFSET..FRAME_OFFSET + FRAME_LEN].copy_from_slice(nv12);
            write_u64(buf, TS_OFFSET, now_ns());
            s.store(begin.wrapping_add(2), Ordering::Release); // even: frame complete
        }
    }

    impl Drop for Writer {
        fn drop(&mut self) {
            unsafe {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr as *mut c_void });
                let _ = CloseHandle(self.handle);
            }
        }
    }

    /// The frame consumer, held by the `vcam-source` DLL. Attaches to an already-created section.
    pub struct Reader {
        handle: HANDLE,
        ptr: *const u8,
    }

    // SAFETY: the section is process-wide valid; the Reader has a single logical owner (it lives
    // behind a Mutex in the DLL).
    unsafe impl Send for Reader {}

    impl Reader {
        /// Opens the section if the producer has created it (correct magic), else `None` — the DLL
        /// retries on the next sample request, so a not-yet-running `acam.exe` just means no frame.
        pub fn open() -> Option<Self> {
            let handle = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, w!("Global\\acam-vcam-video")) }.ok()?;
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, SECTION_LEN) };
            if view.Value.is_null() {
                unsafe { let _ = CloseHandle(handle); }
                return None;
            }
            let ptr = view.Value as *const u8;
            let buf = unsafe { std::slice::from_raw_parts(ptr, SECTION_LEN) };
            if read_u32(buf, 0) != MAGIC {
                unsafe {
                    let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: ptr as *mut c_void });
                    let _ = CloseHandle(handle);
                }
                return None;
            }
            Some(Self { handle, ptr })
        }

        /// Copies the latest complete frame into `out` (must be [`FRAME_LEN`]) and returns the
        /// wall-clock time it was published (ns since epoch), or `None` if no frame has been published
        /// yet or a stable read could not be obtained in a few tries (writer too busy).
        pub fn read(&self, out: &mut [u8]) -> Option<u64> {
            assert_eq!(out.len(), FRAME_LEN);
            let buf = unsafe { std::slice::from_raw_parts(self.ptr, SECTION_LEN) };
            let s = unsafe { seq(self.ptr) };
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

    impl Drop for Reader {
        fn drop(&mut self) {
            unsafe {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr as *mut c_void });
                let _ = CloseHandle(self.handle);
            }
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
        pub fn create() -> std::io::Result<Self> {
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
