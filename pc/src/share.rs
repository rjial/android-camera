//! Shares decoded frames with local consumers (the OBS "ACAM Camera" source) over a unix socket.
//! Wire format ("ACVF") is documented in PROTOCOL.md.

use rust_i18n::t;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::nosignal::Reason;

pub const HEADER_LEN: usize = 24;
const COLOR_BT601: u8 = 0;
const COLOR_BT709: u8 = 1;
const RANGE_LIMITED: u8 = 0;

type Logger = Arc<dyn Fn(&str) + Send + Sync>;

pub struct FrameShare {
    path: Option<PathBuf>,
    clients: Arc<Mutex<Vec<SyncSender<Arc<Vec<u8>>>>>>,
    count: Arc<AtomicUsize>,
}

/// `$ACAM_SOCKET`, else `$XDG_RUNTIME_DIR/acam/video.sock`, else `/tmp/acam-<uid>/video.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ACAM_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("acam"),
        None => PathBuf::from(format!("/tmp/acam-{}", unsafe { libc::getuid() })),
    };
    dir.join("video.sock")
}

impl FrameShare {
    /// Starts listening; on failure (e.g. another acam already shares) sharing is simply off.
    pub fn start(log: Logger) -> Self {
        let clients: Arc<Mutex<Vec<SyncSender<Arc<Vec<u8>>>>>> = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let path = socket_path();
        let listener = match bind(&path) {
            Ok(l) => l,
            Err(e) => {
                log(&t!("log.obs_disabled", error = e));
                return Self { path: None, clients, count };
            }
        };
        {
            let (clients, count, log) = (clients.clone(), count.clone(), log.clone());
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    // A large send buffer lets a whole frame go out in one go.
                    let size: libc::c_int = 8 << 20;
                    unsafe {
                        libc::setsockopt(
                            stream.as_raw_fd(),
                            libc::SOL_SOCKET,
                            libc::SO_SNDBUF,
                            &size as *const _ as *const libc::c_void,
                            size_of::<libc::c_int>() as libc::socklen_t,
                        );
                    }
                    // Two frames of slack: a slow consumer loses frames instead of delaying everyone.
                    let (tx, rx) = sync_channel::<Arc<Vec<u8>>>(2);
                    let mut c = clients.lock().unwrap();
                    c.push(tx);
                    count.store(c.len(), Ordering::Relaxed);
                    drop(c);
                    log(&t!("log.obs_connected"));
                    let log = log.clone();
                    thread::spawn(move || {
                        let mut stream: UnixStream = stream;
                        while let Ok(frame) = rx.recv() {
                            if stream.write_all(&frame).is_err() {
                                break;
                            }
                        }
                        log(&t!("log.obs_disconnected"));
                    });
                }
            });
        }
        log(&t!("log.obs_sharing", path = path.display()));
        Self { path: Some(path), clients, count }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn client_count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    /// Sends one I420 frame to every connected consumer. Costs nothing without consumers.
    pub fn publish(&self, (w, h): (usize, usize), strides: (usize, usize, usize), y: &[u8], u: &[u8], v: &[u8]) {
        if self.client_count() == 0 {
            return;
        }
        self.broadcast(Arc::new(pack_i420((w, h), strides, y, u, v, monotonic_ns())));
    }

    /// Tells consumers there is no video and why, so they show the no-signal picture.
    pub fn publish_idle(&self, reason: Reason) {
        if self.client_count() > 0 {
            let mut h = header(0, 0, monotonic_ns(), COLOR_BT601);
            h[22] = reason as u8;
            self.broadcast(Arc::new(h.to_vec()));
        }
    }

    fn broadcast(&self, frame: Arc<Vec<u8>>) {
        let mut c = self.clients.lock().unwrap();
        c.retain(|tx| match tx.try_send(frame.clone()) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
        self.count.store(c.len(), Ordering::Relaxed);
    }
}

impl Drop for FrameShare {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                t!("err.socket_in_use", path = path.display()).to_string(),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Same clock as OBS's `os_gettime_ns()`, so the plugin can measure socket latency.
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn header(w: u32, h: u32, pts_ns: u64, color: u8) -> [u8; HEADER_LEN] {
    let mut b = [0u8; HEADER_LEN];
    b[0..4].copy_from_slice(b"ACVF");
    b[4..8].copy_from_slice(&w.to_le_bytes());
    b[8..12].copy_from_slice(&h.to_le_bytes());
    b[12..20].copy_from_slice(&pts_ns.to_le_bytes());
    b[20] = color;
    b[21] = RANGE_LIMITED;
    b
}

/// Header plus tightly packed I420 planes (the decoder's planes carry row padding).
pub fn pack_i420((w, h): (usize, usize), (ys, us, vs): (usize, usize, usize), y: &[u8], u: &[u8], v: &[u8], pts_ns: u64) -> Vec<u8> {
    // Phones encode HD with BT.709 and SD with BT.601, the usual convention.
    let color = if h >= 720 { COLOR_BT709 } else { COLOR_BT601 };
    let (cw, ch) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(HEADER_LEN + w * h + 2 * cw * ch);
    out.extend_from_slice(&header(w as u32, h as u32, pts_ns, color));
    for (plane, stride, pw, ph) in [(y, ys, w, h), (u, us, cw, ch), (v, vs, cw, ch)] {
        for row in 0..ph {
            out.extend_from_slice(&plane[row * stride..row * stride + pw]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_strided_planes() {
        // 4x2 luma with stride 6, 2x1 chroma with stride 3.
        let y = [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        let u = [9, 10, 0];
        let v = [11, 12, 0];
        let out = pack_i420((4, 2), (6, 3, 3), &y, &u, &v, 42);
        assert_eq!(&out[0..4], b"ACVF");
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()), 2);
        assert_eq!(u64::from_le_bytes(out[12..20].try_into().unwrap()), 42);
        assert_eq!(out[20], COLOR_BT601);
        assert_eq!(&out[HEADER_LEN..], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn hd_is_bt709_and_idle_is_empty() {
        let y = vec![0u8; 1280 * 720];
        let c = vec![0u8; 640 * 360];
        let out = pack_i420((1280, 720), (1280, 640, 640), &y, &c, &c, 0);
        assert_eq!(out[20], COLOR_BT709);
        assert_eq!(out.len(), HEADER_LEN + 1280 * 720 * 3 / 2);
        assert_eq!(&header(0, 0, 0, 0)[4..12], &[0u8; 8]);
    }

    #[test]
    fn delivers_to_socket_clients() {
        let dir = std::env::temp_dir().join(format!("acam-test-{}", std::process::id()));
        let path = dir.join("video.sock");
        unsafe { std::env::set_var("ACAM_SOCKET", &path) };
        let share = FrameShare::start(Arc::new(|_| {}));
        let mut client = UnixStream::connect(&path).unwrap();
        while share.client_count() == 0 {
            thread::sleep(std::time::Duration::from_millis(5));
        }
        share.publish((2, 2), (2, 1, 1), &[1, 2, 3, 4], &[5], &[6]);
        let mut buf = vec![0u8; HEADER_LEN + 6];
        std::io::Read::read_exact(&mut client, &mut buf).unwrap();
        assert_eq!(&buf[HEADER_LEN..], &[1, 2, 3, 4, 5, 6]);
        drop(share);
        assert!(!path.exists());
        let _ = std::fs::remove_dir(dir);
    }
}
