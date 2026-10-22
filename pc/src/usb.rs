//! Android Open Accessory (AOA) transport: switches a phone into accessory mode and exposes
//! its bulk endpoints as Read/Write, so the ACAM protocol runs over the cable unchanged.

use rust_i18n::t;
use anyhow::{Context, Result, bail};
use rusb::{Device, DeviceHandle, Direction, GlobalContext, TransferType};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const GOOGLE_VID: u16 = 0x18d1;
const ACCESSORY_PIDS: [u16; 2] = [0x2d00, 0x2d01]; // accessory, accessory+adb

const AOA_GET_PROTOCOL: u8 = 51;
const AOA_SEND_STRING: u8 = 52;
const AOA_START: u8 = 53;

/// Identity strings; must match android/app/src/main/res/xml/accessory_filter.xml.
const MANUFACTURER: &str = "ACAM";
const MODEL: &str = "ACAM Webcam";
const DESCRIPTION: &str = "Kamera Android sebagai webcam";
const VERSION: &str = "1";
const URI: &str = "";
const SERIAL: &str = "0";

/// Vendors probed for AOA support by default. Probing is limited to phone vendors so we
/// never send vendor control requests to unrelated devices (keyboards, hubs, ...).
const ANDROID_VIDS: &[u16] = &[
    0x04e8, // Samsung
    0x18d1, // Google
    0x2717, // Xiaomi
    0x22d9, // OPPO / realme
    0x2d95, // vivo
    0x2a70, // OnePlus
    0x22b8, // Motorola
    0x12d1, // Huawei
    0x0fce, // Sony
    0x1004, // LG
    0x0b05, // ASUS
    0x0bb4, // HTC
    0x2e04, // HMD / Nokia
    0x05c6, // Qualcomm reference
    0x0e8d, // MediaTek reference
    0x1ebf, // Coolpad
    0x2a45, // Meizu
    0x29a9, // Smartisan
    0x19d2, // ZTE
    0x17ef, // Lenovo
];

const TIMEOUT_CTRL: Duration = Duration::from_secs(1);
const TIMEOUT_READ: Duration = Duration::from_secs(6);
// Bulk reads are issued in short slices so a cancelled session lets go of the device quickly.
const READ_SLICE: Duration = Duration::from_millis(200);
const TIMEOUT_WRITE: Duration = Duration::from_secs(2);

pub struct UsbLink {
    pub reader: UsbReader,
    pub writer: UsbWriter,
    pub name: String,
}

/// Finds a phone already in accessory mode, or switches one into it, and opens its bulk endpoints.
pub fn connect(extra_vid: Option<u16>, reset: bool, cancel: Arc<AtomicBool>) -> Result<UsbLink> {
    if reset {
        if let Some(dev) = find_accessory()? {
            println!("{}", t!("log.usb_reset"));
            if let Ok(h) = dev.open() {
                let _ = h.reset();
            }
            wait_until(Duration::from_secs(5), || Ok(find_accessory()?.is_none()))?;
            // Give the phone time to re-enumerate in its normal mode.
            thread::sleep(Duration::from_secs(1));
        }
    }
    if find_accessory()?.is_none() {
        let dev = find_android(extra_vid)?;
        switch_to_accessory(&dev)?;
        if !wait_until(Duration::from_secs(5), || Ok(find_accessory()?.is_some()))? {
            bail!("{}", t!("err.usb_no_accessory"));
        }
    }
    let dev = find_accessory()?.context(t!("err.usb_accessory_gone"))?;
    open_accessory(dev, cancel)
}

fn wait_until(limit: Duration, mut cond: impl FnMut() -> Result<bool>) -> Result<bool> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if cond()? {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(250));
    }
    Ok(false)
}

fn find_accessory() -> Result<Option<Device<GlobalContext>>> {
    for dev in rusb::devices()?.iter() {
        let d = dev.device_descriptor()?;
        if d.vendor_id() == GOOGLE_VID && ACCESSORY_PIDS.contains(&d.product_id()) {
            return Ok(Some(dev));
        }
    }
    Ok(None)
}

fn find_android(extra_vid: Option<u16>) -> Result<Device<GlobalContext>> {
    let mut seen = Vec::new();
    for dev in rusb::devices()?.iter() {
        let d = dev.device_descriptor()?;
        let vid = d.vendor_id();
        if !(ANDROID_VIDS.contains(&vid) || Some(vid) == extra_vid) {
            continue;
        }
        seen.push(format!("{:04x}:{:04x}", vid, d.product_id()));
        let handle = match dev.open() {
            Ok(h) => h,
            Err(rusb::Error::Access) => bail!("{}", t!("err.usb_permission", id = format!("{:04x}:{:04x}", vid, d.product_id()))),
            Err(_) => continue,
        };
        if aoa_protocol(&handle).unwrap_or(0) >= 1 {
            return Ok(dev);
        }
    }
    if seen.is_empty() {
        bail!("{}", t!("err.usb_no_phone"));
    }
    bail!("{}", t!("err.usb_no_aoa", devices = seen.join(", ")))
}

fn aoa_protocol(h: &DeviceHandle<GlobalContext>) -> rusb::Result<u16> {
    let mut buf = [0u8; 2];
    let rt = rusb::request_type(Direction::In, rusb::RequestType::Vendor, rusb::Recipient::Device);
    h.read_control(rt, AOA_GET_PROTOCOL, 0, 0, &mut buf, TIMEOUT_CTRL)?;
    Ok(u16::from_le_bytes(buf))
}

fn switch_to_accessory(dev: &Device<GlobalContext>) -> Result<()> {
    let h = dev.open()?;
    let rt = rusb::request_type(Direction::Out, rusb::RequestType::Vendor, rusb::Recipient::Device);
    for (i, s) in [MANUFACTURER, MODEL, DESCRIPTION, VERSION, URI, SERIAL].iter().enumerate() {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        h.write_control(rt, AOA_SEND_STRING, 0, i as u16, &bytes, TIMEOUT_CTRL)
            .context(t!("err.aoa_send"))?;
    }
    h.write_control(rt, AOA_START, 0, 0, &[], TIMEOUT_CTRL)
        .context(t!("err.aoa_start"))?;
    println!("{}", t!("log.usb_switched"));
    Ok(())
}

fn open_accessory(dev: Device<GlobalContext>, cancel: Arc<AtomicBool>) -> Result<UsbLink> {
    let config = dev.active_config_descriptor()?;
    // Interface 0 is the accessory interface; 0x2d01 adds ADB as interface 1, which we leave alone.
    let iface = config.interfaces().next().context(t!("err.usb_no_interface"))?;
    let desc = iface.descriptors().next().context(t!("err.usb_no_descriptor"))?;
    let (mut ep_in, mut ep_out) = (None, None);
    for ep in desc.endpoint_descriptors() {
        if ep.transfer_type() == TransferType::Bulk {
            match ep.direction() {
                Direction::In => ep_in = Some(ep.address()),
                Direction::Out => ep_out = Some(ep.address()),
            }
        }
    }
    let (ep_in, ep_out) = (ep_in.context(t!("err.usb_no_bulk_in"))?, ep_out.context(t!("err.usb_no_bulk_out"))?);

    let handle = dev.open().context(t!("err.usb_open"))?;
    let _ = handle.set_auto_detach_kernel_driver(true);
    handle.claim_interface(desc.interface_number())?;
    // Discard whatever the phone queued for a previous session.
    let mut junk = vec![0u8; 256 * 1024];
    while let Ok(n) = handle.read_bulk(ep_in, &mut junk, Duration::from_millis(50)) {
        if n == 0 {
            break;
        }
    }
    let handle = Arc::new(handle);

    let name = format!("USB {:03}-{:03}", dev.bus_number(), dev.address());
    Ok(UsbLink {
        reader: UsbReader { handle: handle.clone(), ep: ep_in, buf: vec![0; 256 * 1024], pos: 0, len: 0, cancel },
        writer: UsbWriter { handle, ep: ep_out },
        name,
    })
}

fn to_io(e: rusb::Error) -> io::Error {
    let kind = match e {
        rusb::Error::Timeout => io::ErrorKind::TimedOut,
        rusb::Error::NoDevice | rusb::Error::Pipe | rusb::Error::Io => io::ErrorKind::ConnectionReset,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("USB: {e}"))
}

pub struct UsbReader {
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep: u8,
    // Bulk reads must be at least one max-packet long, so always read into our own large buffer.
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    cancel: Arc<AtomicBool>,
}

impl Read for UsbReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let started = Instant::now();
        while self.pos == self.len {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, t!("err.cancelled").to_string()));
            }
            self.pos = 0;
            self.len = match self.handle.read_bulk(self.ep, &mut self.buf, READ_SLICE) {
                Ok(n) => n,
                Err(rusb::Error::Timeout) if started.elapsed() < TIMEOUT_READ => 0,
                Err(e) => return Err(to_io(e)),
            };
        }
        let n = out.len().min(self.len - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

pub struct UsbWriter {
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep: u8,
}

impl Write for UsbWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.handle.write_bulk(self.ep, data, TIMEOUT_WRITE).map_err(to_io)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

