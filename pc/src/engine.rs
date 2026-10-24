//! Connection engine shared by the CLI (`--headless`) and the GUI: connects over TCP or USB,
//! reconnects with backoff, decodes H.264, feeds v4l2loopback and keeps a preview frame.

use rust_i18n::t;
use crate::controls::{Caps, Live};
use crate::nosignal::{self, Reason};
use crate::protocol::*;
use crate::share::FrameShare;
use crate::usb;
use crate::webcam::{self, VirtualCamera};
use anyhow::{Context, Result, bail};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DISCOVERY_PORT: u16 = 5001;
/// No frame for this long counts as lost signal. Above the 1 s maximum shutter, which spaces frames ~1 s apart.
const SIGNAL_TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone)]
pub enum Mode {
    /// `host: None` means find the phone with UDP discovery first.
    Tcp { host: Option<String>, port: u16, token: u32 },
    Usb { vid: Option<u16> },
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub mode: Mode,
    pub size: (u16, u16),
    pub fps: u8,
    pub bitrate: u32,
    pub front: bool,
    /// v4l2loopback node; whether it is written to is controlled by [`Engine::set_webcam`].
    pub device: String,
    pub dump: Option<String>,
    /// How often stats are published (and logged).
    pub stats_interval: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Connecting(String),
    Connected { name: String, width: u16, height: u16, fps: u8 },
    Error(String),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub fps: f64,
    pub kbps: f64,
    pub rtt_ms: f64,
}

pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct EngineState {
    pub status: Status,
    pub stats: Stats,
    pub frame: Option<Arc<PreviewFrame>>,
    pub frame_seq: u64,
    pub webcam_error: Option<String>,
    /// Capabilities of the active camera; `None` until the phone reports them.
    pub caps: Option<Caps>,
    pub live: Live,
}

type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// Write half of the current connection, used for control packets (ping, switch camera, ...).
type Control = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

struct Shared {
    state: Mutex<EngineState>,
    ctrl: Control,
    preview: AtomicBool,
    webcam: AtomicBool,
    /// Lives across reconnects so apps using the virtual camera don't lose it.
    output: Mutex<Webcam>,
    /// Manual camera controls, re-sent after every reconnect.
    controls: Mutex<BTreeMap<u8, i64>>,
    /// Frames for the OBS plugin over a local socket.
    share: FrameShare,
    obs: AtomicBool,
    /// When the last frame was delivered (reset on connect), for the no-signal watchdog.
    last_frame: Mutex<Instant>,
    log: Logger,
}

struct Worker {
    cancel: Arc<AtomicBool>,
    /// Unblocks a TCP read immediately on disconnect.
    shutdown: Arc<Mutex<Option<TcpStream>>>,
    handle: JoinHandle<()>,
}

pub struct Engine {
    shared: Arc<Shared>,
    worker: Mutex<Option<Worker>>,
}

impl Engine {
    pub fn new(log: impl Fn(&str) + Send + Sync + 'static) -> Self {
        let log: Logger = Arc::new(log);
        let engine = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(EngineState {
                    status: Status::Idle,
                    stats: Stats::default(),
                    frame: None,
                    frame_seq: 0,
                    webcam_error: None,
                    caps: None,
                    live: Live::default(),
                }),
                ctrl: Arc::new(Mutex::new(None)),
                preview: AtomicBool::new(false),
                webcam: AtomicBool::new(true),
                controls: Mutex::new(BTreeMap::new()),
                output: Mutex::new(Webcam { device: String::new(), size: (0, 0), out: None, failed: false }),
                share: FrameShare::start(log.clone()),
                obs: AtomicBool::new(true),
                last_frame: Mutex::new(Instant::now()),
                log,
            }),
            worker: Mutex::new(None),
        };
        let weak = Arc::downgrade(&engine.shared);
        thread::spawn(move || watchdog(weak));
        engine
    }

    /// Starts (or restarts) connecting with `cfg`; keeps reconnecting until [`Engine::disconnect`].
    pub fn connect(&self, cfg: EngineConfig) {
        // The old session must release the USB interface and socket before a new one starts.
        if let Some(handle) = self.stop_worker() {
            let _ = handle.join();
        }
        {
            let mut w = self.shared.output.lock().unwrap();
            // The webcam keeps its first size; other stream sizes are scaled into it.
            if w.device != cfg.device || w.size == (0, 0) {
                w.close();
                w.device = cfg.device.clone();
                w.size = (cfg.size.0 as usize, cfg.size.1 as usize);
                w.failed = false;
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(Mutex::new(None));
        let handle = {
            let (shared, cancel, shutdown) = (self.shared.clone(), cancel.clone(), shutdown.clone());
            thread::spawn(move || run(cfg, &shared, &cancel, &shutdown))
        };
        *self.worker.lock().unwrap() = Some(Worker { cancel, shutdown, handle });
    }

    pub fn disconnect(&self) {
        if let Some(handle) = self.stop_worker() {
            // Don't block the caller (the GUI); the worker exits within a few hundred ms.
            thread::spawn(move || {
                let _ = handle.join();
            });
        }
    }

    fn stop_worker(&self) -> Option<JoinHandle<()>> {
        let w = self.worker.lock().unwrap().take()?;
        w.cancel.store(true, Ordering::Relaxed);
        if let Some(s) = w.shutdown.lock().unwrap().as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
        Some(w.handle)
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, EngineState> {
        self.shared.state.lock().unwrap()
    }

    pub fn set_preview(&self, on: bool) {
        self.shared.preview.store(on, Ordering::Relaxed);
        if !on {
            self.state().frame = None;
        }
    }

    pub fn set_obs(&self, on: bool) {
        self.shared.obs.store(on, Ordering::Relaxed);
        if !on {
            self.shared.share.publish_idle(Reason::SharingOff);
        }
    }

    pub fn obs_clients(&self) -> usize {
        self.shared.share.client_count()
    }

    pub fn obs_socket(&self) -> Option<String> {
        self.shared.share.path().map(|p| p.display().to_string())
    }

    pub fn set_webcam(&self, on: bool) {
        self.shared.webcam.store(on, Ordering::Relaxed);
    }

    pub fn switch_camera(&self) {
        // The other camera supports different things; it starts automatic and reports new capabilities.
        self.shared.controls.lock().unwrap().clear();
        {
            let mut st = self.state();
            st.caps = None;
            st.live = Live::default();
        }
        send(&self.shared.ctrl, Packet::new(T_SWITCH_CAMERA, 0));
    }

    /// Sends SET_CONTROLS and remembers the values for later reconnects.
    pub fn set_controls(&self, pairs: &[(u8, i64)]) {
        {
            let mut c = self.shared.controls.lock().unwrap();
            for &(k, v) in pairs {
                if k != ctl::AF_TRIGGER {
                    c.insert(k, v);
                }
            }
            // A trigger means "focus once"; don't restore a manual distance on reconnect.
            if pairs.iter().any(|(k, _)| *k == ctl::AF_TRIGGER) {
                c.remove(&ctl::AF_MODE);
            }
        }
        let mut p = Packet::new(T_SET_CONTROLS, 0);
        p.payload = tlv_encode(pairs);
        send(&self.shared.ctrl, p);
    }

    pub fn reset_controls(&self) {
        let defaults = [
            (ctl::AE_MODE, 0),
            (ctl::EV_STEPS, 0),
            (ctl::AE_LOCK, 0),
            (ctl::AF_MODE, 0),
            (ctl::AWB_MODE, 1),
            (ctl::AWB_LOCK, 0),
            (ctl::ZOOM_MILLI, 1000),
        ];
        let mut p = Packet::new(T_SET_CONTROLS, 0);
        p.payload = tlv_encode(&defaults);
        send(&self.shared.ctrl, p);
        self.shared.controls.lock().unwrap().clear();
    }

    pub fn request_keyframe(&self) {
        send(&self.shared.ctrl, Packet::new(T_REQ_KEYFRAME, 0));
    }

    pub fn set_bitrate(&self, kbps: u32) {
        let mut p = Packet::new(T_SET_BITRATE, 0);
        p.payload = kbps.to_be_bytes().to_vec();
        send(&self.shared.ctrl, p);
    }
}

fn send(ctrl: &Control, p: Packet) {
    if let Some(s) = ctrl.lock().unwrap().as_mut() {
        let _ = p.write_to(s);
    }
}

fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64
}

fn set_status(shared: &Shared, status: Status) {
    shared.state.lock().unwrap().status = status;
}

/// Sleeps in small steps; returns false if cancelled meanwhile.
fn sleep_unless(cancel: &AtomicBool, d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    !cancel.load(Ordering::Relaxed)
}

/// Owns the platform virtual webcam output; opened lazily while the webcam switch is on.
struct Webcam {
    device: String,
    size: (usize, usize),
    out: Option<Box<dyn VirtualCamera>>,
    failed: bool,
}

impl Webcam {
    fn close(&mut self) {
        if let Some(mut o) = self.out.take() {
            let _ = o.write_black();
        }
    }

    fn sync(&mut self, shared: &Shared) -> Option<&mut (dyn VirtualCamera + 'static)> {
        let on = shared.webcam.load(Ordering::Relaxed);
        if !on {
            if let Some(mut o) = self.out.take() {
                let _ = o.write_black();
            }
            if self.failed {
                self.failed = false;
                shared.state.lock().unwrap().webcam_error = None;
            }
            return None;
        }
        if self.out.is_none() && !self.failed {
            match webcam::open(&self.device, self.size.0, self.size.1) {
                Ok(mut o) => {
                    let _ = o.write_black();
                    (shared.log)(&t!("log.webcam_opened", device = self.device, width = o.width(), height = o.height()));
                    shared.state.lock().unwrap().webcam_error = None;
                    self.out = Some(o);
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    (shared.log)(&msg);
                    shared.state.lock().unwrap().webcam_error = Some(msg);
                    self.failed = true;
                }
            }
        }
        self.out.as_deref_mut()
    }

}

/// Shows the no-signal picture while there is no video: on the webcam at 10 fps (v4l2loopback readers
/// wait for writes), and to OBS as a small idle message every 500 ms so newly attached sources get it too.
fn watchdog(shared: Weak<Shared>) {
    // Keyed by caption too, so a language switch redraws it.
    let mut picture: Option<((usize, usize), String, Vec<u8>)> = None;
    let mut tick = 0u32;
    while let Some(shared) = shared.upgrade() {
        let connected = matches!(shared.state.lock().unwrap().status, Status::Connected { .. });
        let lost = shared.last_frame.lock().unwrap().elapsed() > SIGNAL_TIMEOUT;
        let reason = match (connected, lost) {
            (true, false) => None,
            (true, true) => Some(Reason::Lost),
            (false, _) => Some(Reason::Disconnected),
        };
        if let Some(reason) = reason {
            let mut w = shared.output.lock().unwrap();
            // Before the first connect the webcam has no size yet. (Checking the size, not the
            // device path, because the Windows virtual camera has no device path — it is always empty.)
            if w.size != (0, 0)
                && let Some(o) = w.sync(&shared)
            {
                let (size, caption) = ((o.width(), o.height()), reason.caption());
                if picture.as_ref().is_none_or(|(s, c, _)| *s != size || *c != caption) {
                    let frame = nosignal::render(size.0, size.1, &caption);
                    picture = Some((size, caption, frame));
                }
                let _ = o.write_packed(&picture.as_ref().unwrap().2);
            }
        }
        if tick % 5 == 0 {
            if !shared.obs.load(Ordering::Relaxed) {
                shared.share.publish_idle(Reason::SharingOff);
            } else if let Some(reason) = reason {
                shared.share.publish_idle(reason);
            }
        }
        tick = tick.wrapping_add(1);
        drop(shared);
        thread::sleep(Duration::from_millis(100));
    }
}

fn run(cfg: EngineConfig, shared: &Arc<Shared>, cancel: &Arc<AtomicBool>, shutdown: &Mutex<Option<TcpStream>>) {
    shared.output.lock().unwrap().sync(shared);
    let mut dump = match &cfg.dump {
        Some(p) => match File::create(p) {
            Ok(f) => Some(f),
            Err(e) => {
                (shared.log)(&t!("log.dump_failed", path = p, error = e));
                None
            }
        },
        None => None,
    };

    let mut backoff = Duration::from_millis(500);
    let mut usb_failures = 0u32;
    while !cancel.load(Ordering::Relaxed) {
        let target = match &cfg.mode {
            Mode::Usb { .. } => "USB".to_string(),
            Mode::Tcp { host: Some(h), .. } => h.clone(),
            Mode::Tcp { host: None, .. } => {
                set_status(shared, Status::Connecting(t!("status.searching").to_string()));
                match discover(Duration::from_secs(10), cancel, &shared.log) {
                    Ok(h) => h,
                    Err(e) => {
                        (shared.log)(&format!("discovery: {e:#}"));
                        set_status(shared, Status::Error(format!("discovery: {e:#}")));
                        continue;
                    }
                }
            }
        };
        set_status(shared, Status::Connecting(target.clone()));

        let started = Instant::now();
        let link = match &cfg.mode {
            Mode::Usb { vid } => {
                // If the phone sits in accessory mode but the app never answers (e.g. it was closed),
                // resetting the device drops it out of accessory mode so the next switch relaunches the app.
                // `ACAM_USB_NO_RESET` disables that so the accessory device stays enumerated — needed on
                // Windows while installing its WinUSB driver (Zadig), which takes longer than the reset window.
                let reset = usb_failures >= 2 && std::env::var_os("ACAM_USB_NO_RESET").is_none();
                if reset {
                    usb_failures = 0;
                }
                connect_usb(*vid, reset, cancel.clone())
            }
            Mode::Tcp { port, .. } => connect_tcp(&target, *port, shutdown),
        };
        let result = link.and_then(|link| session(&cfg, link, shared, cancel, &mut dump));
        *shared.ctrl.lock().unwrap() = None;
        *shutdown.lock().unwrap() = None;
        {
            let mut st = shared.state.lock().unwrap();
            st.frame = None;
            st.stats = Stats::default();
            st.caps = None;
            st.live = Live::default();
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match result {
            Ok(()) => usb_failures = 0,
            Err(e) => {
                let msg = t!("log.connect_failed", target = target, error = format!("{e:#}")).to_string();
                (shared.log)(&msg);
                set_status(shared, Status::Error(msg));
                if matches!(cfg.mode, Mode::Usb { .. }) && started.elapsed() < Duration::from_secs(15) {
                    usb_failures += 1;
                }
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            backoff = Duration::from_millis(500);
        }
        (shared.log)(&t!("log.reconnect_in", secs = format!("{:.1}", backoff.as_secs_f32())));
        if !sleep_unless(cancel, backoff) {
            break;
        }
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
    set_status(shared, Status::Idle);
}

/// One established byte stream to the phone, over TCP or USB.
struct Link {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    name: String,
}

fn connect_tcp(host: &str, port: u16, shutdown: &Mutex<Option<TcpStream>>) -> Result<Link> {
    let addr = (host, port).to_socket_addrs()?.next().context(t!("err.bad_address"))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(6)))?;
    *shutdown.lock().unwrap() = Some(stream.try_clone()?);
    Ok(Link { writer: Box::new(stream.try_clone()?), reader: Box::new(stream), name: addr.to_string() })
}

fn connect_usb(extra_vid: Option<u16>, reset: bool, cancel: Arc<AtomicBool>) -> Result<Link> {
    let l = usb::connect(extra_vid, reset, cancel)?;
    Ok(Link { reader: Box::new(l.reader), writer: Box::new(l.writer), name: l.name })
}

fn session(
    cfg: &EngineConfig,
    link: Link,
    shared: &Arc<Shared>,
    cancel: &AtomicBool,
    dump: &mut Option<File>,
) -> Result<()> {
    let Link { reader, mut writer, name } = link;
    let mut reader = BufReader::with_capacity(1 << 20, reader);

    let token = match cfg.mode {
        Mode::Tcp { token, .. } => token,
        Mode::Usb { .. } => 0,
    };
    let hello = Hello {
        width: cfg.size.0,
        height: cfg.size.1,
        fps: cfg.fps,
        bitrate_kbps: cfg.bitrate,
        camera: cfg.front as u8,
        token,
    };
    writer.write_all(&hello.encode())?;
    writer.flush()?;
    let mut buf = [0u8; 12];
    sync_to_magic(&mut reader)?;
    buf[..4].copy_from_slice(MAGIC);
    reader.read_exact(&mut buf[4..])?;
    let cfg_reply = Config::decode(&buf)?;
    if cfg_reply.status != 0 {
        bail!("{}", t!("err.rejected", reason = cfg_reply.status_text()));
    }
    if cfg_reply.codec != 1 {
        bail!("{}", t!("err.codec", codec = cfg_reply.codec));
    }
    (shared.log)(&t!(
        "log.connected",
        name = name,
        width = cfg_reply.width,
        height = cfg_reply.height,
        fps = cfg_reply.fps
    ));
    *shared.last_frame.lock().unwrap() = Instant::now();
    set_status(
        shared,
        Status::Connected { name, width: cfg_reply.width, height: cfg_reply.height, fps: cfg_reply.fps },
    );

    *shared.ctrl.lock().unwrap() = Some(writer);
    let saved: Vec<(u8, i64)> = shared.controls.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect();
    if !saved.is_empty() {
        let mut p = Packet::new(T_SET_CONTROLS, 0);
        p.payload = tlv_encode(&saved);
        send(&shared.ctrl, p);
    }
    let alive = Arc::new(AtomicBool::new(true));
    let pinger = {
        let (ctrl, alive) = (shared.ctrl.clone(), alive.clone());
        thread::spawn(move || {
            while alive.load(Ordering::Relaxed) {
                send(&ctrl, Packet::new(T_PING, now_us()));
                thread::sleep(Duration::from_secs(2));
            }
        })
    };
    let result = receive(cfg, &mut reader, shared, cancel, dump);
    alive.store(false, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        // Lets the phone free the camera and accept the next handshake right away.
        send(&shared.ctrl, Packet::new(T_BYE, 0));
    }
    let _ = pinger.join();
    result
}

fn receive(
    cfg: &EngineConfig,
    stream: &mut BufReader<Box<dyn Read + Send>>,
    shared: &Arc<Shared>,
    cancel: &AtomicBool,
    dump: &mut Option<File>,
) -> Result<()> {
    let mut decoder: Option<Decoder> = None;
    let mut waiting_keyframe = true;
    let (mut frames, mut bytes, mut stat_at) = (0u32, 0usize, Instant::now());
    let mut rtt_ms = 0.0f64;
    // TEMP latency bring-up: where does the read loop spend time (decode vs deliver)?
    let (mut dec_us, mut dec_max, mut del_us, mut del_max) = (0u128, 0u128, 0u128, 0u128);

    while !cancel.load(Ordering::Relaxed) {
        let p = Packet::read_from(stream)?;
        match p.kind {
            T_CODEC_CONFIG => {
                // New SPS/PPS may carry a new resolution (e.g. after switching cameras), so start fresh.
                let mut d = Decoder::new()?;
                if let Some(f) = dump.as_mut() {
                    f.write_all(&p.payload)?;
                }
                // Some encoders (e.g. Qualcomm) put SPS+PPS+IDR in one buffer flagged as codec config.
                waiting_keyframe = !has_idr(&p.payload);
                if let Ok(Some(yuv)) = d.decode(&p.payload) {
                    frames += 1;
                    deliver(&yuv, shared)?;
                }
                decoder = Some(d);
            }
            T_FRAME => {
                bytes += p.payload.len();
                if waiting_keyframe && !p.is_keyframe() && !has_idr(&p.payload) {
                    continue;
                }
                waiting_keyframe = false;
                if let Some(f) = dump.as_mut() {
                    f.write_all(&p.payload)?;
                }
                // Encoders that only send SPS/PPS inline with keyframes never emit CODEC_CONFIG.
                let d = match decoder.as_mut() {
                    Some(d) => d,
                    None => decoder.insert(Decoder::new()?),
                };
                let t_dec = Instant::now();
                let decoded = d.decode(&p.payload);
                let d_us = t_dec.elapsed().as_micros();
                dec_us += d_us;
                dec_max = dec_max.max(d_us);
                match decoded {
                    Ok(Some(yuv)) => {
                        frames += 1;
                        let t_del = Instant::now();
                        deliver(&yuv, shared)?;
                        let dl_us = t_del.elapsed().as_micros();
                        del_us += dl_us;
                        del_max = del_max.max(dl_us);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        (shared.log)(&t!("log.decode_error", error = e));
                        waiting_keyframe = true;
                        send(&shared.ctrl, Packet::new(T_REQ_KEYFRAME, 0));
                    }
                }
            }
            T_CAPABILITIES => {
                let caps = Caps::from_tlv(&tlv_decode(&p.payload));
                (shared.log)(&t!("log.camera", caps = caps.summary()));
                shared.state.lock().unwrap().caps = Some(caps);
            }
            T_CAMERA_STATE => {
                let pairs = tlv_decode(&p.payload);
                if pairs.iter().any(|(k, _)| *k == ctl::ERROR) {
                    (shared.log)(&t!("log.controls_rejected"));
                }
                shared.state.lock().unwrap().live.apply(&pairs);
            }
            T_PING => send(&shared.ctrl, Packet::new(T_PONG, p.pts_us)),
            T_PONG => rtt_ms = now_us().saturating_sub(p.pts_us) as f64 / 1000.0,
            other => (shared.log)(&t!("log.packet_ignored", kind = format!("{other:02x}"))),
        }

        let dt = stat_at.elapsed();
        if dt >= cfg.stats_interval {
            let secs = dt.as_secs_f64();
            let stats = Stats { fps: frames as f64 / secs, kbps: bytes as f64 * 8.0 / 1000.0 / secs, rtt_ms };
            shared.state.lock().unwrap().stats = stats;
            let n = frames.max(1) as u128;
            (shared.log)(&format!(
                "{:.1} fps, {:.0} kbps, rtt {:.0} ms | decode avg {}us max {}us, deliver avg {}us max {}us",
                stats.fps, stats.kbps, stats.rtt_ms, dec_us / n, dec_max, del_us / n, del_max
            ));
            (frames, bytes, stat_at) = (0, 0, Instant::now());
            (dec_us, dec_max, del_us, del_max) = (0, 0, 0, 0);
        }
    }
    Ok(())
}

/// Sends a decoded frame to the virtual webcam and, if enabled, to the preview.
fn deliver(yuv: &openh264::decoder::DecodedYUV<'_>, shared: &Shared) -> Result<()> {
    *shared.last_frame.lock().unwrap() = Instant::now();
    if let Some(o) = shared.output.lock().unwrap().sync(shared) {
        o.write_i420(yuv.dimensions(), yuv.strides(), yuv.y(), yuv.u(), yuv.v())?;
    }
    if shared.obs.load(Ordering::Relaxed) {
        shared.share.publish(yuv.dimensions(), yuv.strides(), yuv.y(), yuv.u(), yuv.v());
    }
    if shared.preview.load(Ordering::Relaxed) {
        let (w, h) = yuv.dimensions();
        let mut bgra = vec![0u8; w * h * 4];
        yuv.write_rgba8(&mut bgra);
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let mut st = shared.state.lock().unwrap();
        st.frame = Some(Arc::new(PreviewFrame { width: w as u32, height: h as u32, bgra }));
        st.frame_seq += 1;
    }
    Ok(())
}

/// Skips bytes until "ACAM" has been read. Over USB, frames from the previous session may still be queued.
fn sync_to_magic<R: Read>(r: &mut R) -> Result<()> {
    let (mut matched, mut skipped) = (0usize, 0usize);
    let mut b = [0u8; 1];
    while matched < MAGIC.len() {
        r.read_exact(&mut b)?;
        matched = if b[0] == MAGIC[matched] { matched + 1 } else if b[0] == MAGIC[0] { 1 } else { 0 };
        skipped += 1;
        if skipped > 16 * 1024 * 1024 {
            bail!("{}", t!("err.no_reply"));
        }
    }
    Ok(())
}

/// True if the Annex-B buffer contains an IDR slice (NAL type 5).
fn has_idr(data: &[u8]) -> bool {
    data.windows(4).any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] & 0x1f == 5)
}

/// Parses an ACAM announce broadcast into the phone's name.
pub fn parse_announce(b: &[u8]) -> Option<String> {
    // "ACAM" | ver u8 | tcp_port u16 | name_len u8 | name
    if b.len() >= 8 && &b[0..4] == MAGIC && b[4] == VERSION {
        let name_len = b[7] as usize;
        return Some(String::from_utf8_lossy(b.get(8..8 + name_len).unwrap_or_default()).into_owned());
    }
    None
}

/// Waits for an ACAM announce broadcast and returns the sender's IP.
fn discover(timeout: Duration, cancel: &AtomicBool, log: &Logger) -> Result<String> {
    let sock = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)).context("bind UDP 5001")?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    log(&t!("log.searching"));
    let end = Instant::now() + timeout;
    let mut buf = [0u8; 512];
    while Instant::now() < end && !cancel.load(Ordering::Relaxed) {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        if let Some(name) = parse_announce(&buf[..n]) {
            log(&t!("log.found", name = name, ip = from.ip()));
            return Ok(from.ip().to_string());
        }
    }
    bail!("{}", t!("err.none_found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_idr_after_sps_pps() {
        assert!(has_idr(&[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3]));
        assert!(!has_idr(&[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]));
        assert!(!has_idr(&[0, 0, 0, 1, 0x41, 9]));
    }

    #[test]
    fn announce_roundtrip() {
        let mut b = b"ACAM\x01\x13\x88".to_vec();
        b.push(5);
        b.extend_from_slice(b"S22 U");
        assert_eq!(parse_announce(&b).as_deref(), Some("S22 U"));
        assert_eq!(parse_announce(b"XXXX\x01\x13\x88\x00"), None);
    }

    #[test]
    fn sync_skips_garbage() {
        let mut c = std::io::Cursor::new(b"\x20\x00AACAM\x01rest".to_vec());
        sync_to_magic(&mut c).unwrap();
        let mut rest = [0u8; 1];
        c.read_exact(&mut rest).unwrap();
        assert_eq!(rest[0], 1);
    }
}
