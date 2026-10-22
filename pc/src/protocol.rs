//! ACAM v1 wire format. See ../PROTOCOL.md.

use rust_i18n::t;
use std::io::{self, Read, Write};

pub const MAGIC: &[u8; 4] = b"ACAM";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 14;
pub const MAX_PAYLOAD: u32 = 8 * 1024 * 1024;

pub const T_CODEC_CONFIG: u8 = 0x01;
pub const T_FRAME: u8 = 0x02;
pub const T_REQ_KEYFRAME: u8 = 0x10;
pub const T_SWITCH_CAMERA: u8 = 0x11;
pub const T_SET_BITRATE: u8 = 0x12;
pub const T_BYE: u8 = 0x13;
pub const T_SET_CONTROLS: u8 = 0x14;
pub const T_PING: u8 = 0x20;
pub const T_PONG: u8 = 0x21;
pub const T_CAPABILITIES: u8 = 0x22;
pub const T_CAMERA_STATE: u8 = 0x23;

/// Keys for SET_CONTROLS and CAMERA_STATE (TLV).
pub mod ctl {
    pub const AE_MODE: u8 = 1; // 0 auto, 1 manual
    pub const ISO: u8 = 2;
    pub const EXPOSURE_NS: u8 = 3;
    pub const EV_STEPS: u8 = 4;
    pub const AE_LOCK: u8 = 5;
    pub const AF_MODE: u8 = 6; // 0 continuous, 1 manual
    pub const FOCUS_MILLI_DIOPTER: u8 = 7;
    pub const AF_TRIGGER: u8 = 8;
    pub const AWB_MODE: u8 = 9; // Camera2 CONTROL_AWB_MODE_*; 0 = manual Kelvin
    pub const WB_KELVIN: u8 = 10;
    pub const AWB_LOCK: u8 = 11;
    pub const ZOOM_MILLI: u8 = 12;
    /// CAMERA_STATE only: the phone rejected the last controls (value is unused).
    pub const ERROR: u8 = 99;
}

/// Keys for CAPABILITIES (TLV).
pub mod cap {
    pub const MANUAL_SENSOR: u8 = 100;
    pub const ISO_MIN: u8 = 101;
    pub const ISO_MAX: u8 = 102;
    pub const EXPOSURE_MIN_NS: u8 = 103;
    pub const EXPOSURE_MAX_NS: u8 = 104;
    pub const EV_MIN: u8 = 105;
    pub const EV_MAX: u8 = 106;
    pub const EV_STEP_MILLI: u8 = 107;
    pub const MIN_FOCUS_MILLI_DIOPTER: u8 = 108;
    pub const AWB_MODES: u8 = 109; // bitmask of CONTROL_AWB_MODE_* values
    pub const MANUAL_WB: u8 = 110;
    pub const ZOOM_MIN_MILLI: u8 = 111;
    pub const ZOOM_MAX_MILLI: u8 = 112;
    pub const AE_LOCK: u8 = 113;
    pub const AWB_LOCK: u8 = 114;
    pub const FRAME_DURATION_NS: u8 = 115;
    pub const AUTOFOCUS: u8 = 116;
}

/// Encodes `key u8 | value i64` pairs.
pub fn tlv_encode(pairs: &[(u8, i64)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pairs.len() * 9);
    for &(k, v) in pairs {
        out.push(k);
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// Decodes `key u8 | value i64` pairs; a truncated trailing pair is ignored.
pub fn tlv_decode(b: &[u8]) -> Vec<(u8, i64)> {
    b.chunks_exact(9).map(|c| (c[0], i64::from_be_bytes(c[1..9].try_into().unwrap()))).collect()
}

pub const FLAG_KEYFRAME: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub bitrate_kbps: u32,
    pub camera: u8,
    pub token: u32,
}

impl Hello {
    pub fn encode(&self) -> [u8; 19] {
        let mut b = [0u8; 19];
        b[0..4].copy_from_slice(MAGIC);
        b[4] = VERSION;
        b[5..7].copy_from_slice(&self.width.to_be_bytes());
        b[7..9].copy_from_slice(&self.height.to_be_bytes());
        b[9] = self.fps;
        b[10..14].copy_from_slice(&self.bitrate_kbps.to_be_bytes());
        b[14] = self.camera;
        b[15..19].copy_from_slice(&self.token.to_be_bytes());
        b
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn decode(b: &[u8; 19]) -> io::Result<Self> {
        check_magic(&b[0..5])?;
        Ok(Self {
            width: u16::from_be_bytes([b[5], b[6]]),
            height: u16::from_be_bytes([b[7], b[8]]),
            fps: b[9],
            bitrate_kbps: u32::from_be_bytes(b[10..14].try_into().unwrap()),
            camera: b[14],
            token: u32::from_be_bytes(b[15..19].try_into().unwrap()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub status: u8,
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub codec: u8,
}

impl Config {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn encode(&self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0..4].copy_from_slice(MAGIC);
        b[4] = VERSION;
        b[5] = self.status;
        b[6..8].copy_from_slice(&self.width.to_be_bytes());
        b[8..10].copy_from_slice(&self.height.to_be_bytes());
        b[10] = self.fps;
        b[11] = self.codec;
        b
    }

    pub fn decode(b: &[u8; 12]) -> io::Result<Self> {
        check_magic(&b[0..5])?;
        Ok(Self {
            status: b[5],
            width: u16::from_be_bytes([b[6], b[7]]),
            height: u16::from_be_bytes([b[8], b[9]]),
            fps: b[10],
            codec: b[11],
        })
    }

    pub fn status_text(&self) -> String {
        match self.status {
            0 => "ok".to_string(),
            1 => t!("proto.bad_token").to_string(),
            2 => t!("proto.bad_version").to_string(),
            3 => t!("proto.camera_failed").to_string(),
            _ => t!("proto.unknown_status").to_string(),
        }
    }
}

fn check_magic(b: &[u8]) -> io::Result<()> {
    if &b[0..4] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, t!("err.not_acam").to_string()));
    }
    if b[4] != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            t!("err.version", version = b[4]).to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub kind: u8,
    pub flags: u8,
    pub pts_us: u64,
    pub payload: Vec<u8>,
}

impl Packet {
    pub fn new(kind: u8, pts_us: u64) -> Self {
        Self { kind, flags: 0, pts_us, payload: Vec::new() }
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let mut h = [0u8; HEADER_LEN];
        h[0] = self.kind;
        h[1] = self.flags;
        h[2..6].copy_from_slice(&(self.payload.len() as u32).to_be_bytes());
        h[6..14].copy_from_slice(&self.pts_us.to_be_bytes());
        w.write_all(&h)?;
        w.write_all(&self.payload)?;
        w.flush()
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut h = [0u8; HEADER_LEN];
        r.read_exact(&mut h)?;
        let len = u32::from_be_bytes(h[2..6].try_into().unwrap());
        if len > MAX_PAYLOAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, t!("err.payload_too_big", len = len).to_string()));
        }
        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload)?;
        Ok(Self {
            kind: h[0],
            flags: h[1],
            pts_us: u64::from_be_bytes(h[6..14].try_into().unwrap()),
            payload,
        })
    }

    pub fn is_keyframe(&self) -> bool {
        self.flags & FLAG_KEYFRAME != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn hello_roundtrip() {
        let h = Hello { width: 1280, height: 720, fps: 30, bitrate_kbps: 4000, camera: 1, token: 123456 };
        assert_eq!(Hello::decode(&h.encode()).unwrap(), h);
    }

    #[test]
    fn config_roundtrip() {
        let c = Config { status: 0, width: 1920, height: 1080, fps: 60, codec: 1 };
        assert_eq!(Config::decode(&c.encode()).unwrap(), c);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut b = Config { status: 0, width: 1, height: 1, fps: 1, codec: 1 }.encode();
        b[0] = b'X';
        assert!(Config::decode(&b).is_err());
    }

    #[test]
    fn packet_roundtrip_stream() {
        let a = Packet { kind: T_FRAME, flags: FLAG_KEYFRAME, pts_us: 42, payload: vec![0, 0, 0, 1, 0x65, 9] };
        let b = Packet::new(T_PING, u64::MAX);
        let mut buf = Vec::new();
        a.write_to(&mut buf).unwrap();
        b.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), 2 * HEADER_LEN + 6);
        let mut c = Cursor::new(buf);
        assert_eq!(Packet::read_from(&mut c).unwrap(), a);
        assert_eq!(Packet::read_from(&mut c).unwrap(), b);
        assert!(Packet::read_from(&mut c).is_err());
    }

    #[test]
    fn tlv_roundtrip() {
        let pairs = vec![(ctl::ISO, 1600), (ctl::EV_STEPS, -15), (cap::EXPOSURE_MAX_NS, 450_999_590)];
        let mut b = tlv_encode(&pairs);
        assert_eq!(tlv_decode(&b), pairs);
        b.push(7); // truncated garbage is ignored
        assert_eq!(tlv_decode(&b), pairs);
    }

    #[test]
    fn oversized_payload_rejected() {
        let mut h = [0u8; HEADER_LEN];
        h[2..6].copy_from_slice(&(MAX_PAYLOAD + 1).to_be_bytes());
        assert!(Packet::read_from(&mut Cursor::new(h)).is_err());
    }
}
