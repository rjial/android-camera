//! Camera capabilities / live values reported by the phone, and the text commands of `--headless`.

use rust_i18n::t;
use crate::protocol::{cap, ctl};

/// White balance presets as Camera2 CONTROL_AWB_MODE_* values, with CLI names (also the `wb.*` locale keys).
pub const AWB_PRESETS: [(i64, &str); 8] = [
    (1, "auto"),
    (5, "daylight"),
    (6, "cloudy"),
    (8, "shade"),
    (2, "incandescent"),
    (3, "fluorescent"),
    (4, "warm-fluorescent"),
    (7, "twilight"),
];

/// Translated UI name of a preset from [`AWB_PRESETS`].
pub fn awb_label(cli_name: &str) -> String {
    let key = format!("wb.{cli_name}");
    t!(&key).to_string()
}
pub const AWB_MANUAL: i64 = 0;

/// What the active camera supports. Missing features are `None`/`false`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Caps {
    pub manual_sensor: bool,
    pub iso: Option<(i64, i64)>,
    pub exposure_ns: Option<(i64, i64)>,
    pub ev: Option<(i64, i64)>,
    pub ev_step: f64,
    /// Closest focus in diopters; `None` for fixed-focus cameras.
    pub min_focus_diopter: Option<f64>,
    pub autofocus: bool,
    pub awb_modes: u64,
    pub manual_wb: bool,
    pub zoom: (f64, f64),
    pub ae_lock: bool,
    pub awb_lock: bool,
    pub frame_ns: i64,
}

impl Caps {
    pub fn from_tlv(pairs: &[(u8, i64)]) -> Self {
        let get = |k: u8| pairs.iter().rev().find(|(key, _)| *key == k).map(|(_, v)| *v);
        let range = |a: u8, b: u8| Some((get(a)?, get(b)?));
        let manual_sensor = get(cap::MANUAL_SENSOR) == Some(1);
        Caps {
            manual_sensor,
            iso: range(cap::ISO_MIN, cap::ISO_MAX).filter(|_| manual_sensor),
            exposure_ns: range(cap::EXPOSURE_MIN_NS, cap::EXPOSURE_MAX_NS).filter(|_| manual_sensor),
            ev: range(cap::EV_MIN, cap::EV_MAX).filter(|(a, b)| a != b),
            ev_step: get(cap::EV_STEP_MILLI).unwrap_or(0) as f64 / 1000.0,
            min_focus_diopter: get(cap::MIN_FOCUS_MILLI_DIOPTER).filter(|v| *v > 0).map(|v| v as f64 / 1000.0),
            autofocus: get(cap::AUTOFOCUS) == Some(1),
            awb_modes: get(cap::AWB_MODES).unwrap_or(0) as u64,
            manual_wb: get(cap::MANUAL_WB) == Some(1),
            zoom: (
                get(cap::ZOOM_MIN_MILLI).unwrap_or(1000) as f64 / 1000.0,
                get(cap::ZOOM_MAX_MILLI).unwrap_or(1000) as f64 / 1000.0,
            ),
            ae_lock: get(cap::AE_LOCK) == Some(1),
            awb_lock: get(cap::AWB_LOCK) == Some(1),
            frame_ns: get(cap::FRAME_DURATION_NS).unwrap_or(33_333_333),
        }
    }

    pub fn has_awb(&self, mode: i64) -> bool {
        (0..64).contains(&mode) && self.awb_modes & (1 << mode) != 0
    }

    pub fn zoom_supported(&self) -> bool {
        self.zoom.1 > self.zoom.0 + 0.01
    }

    /// Longest shutter the phone accepts (sensor limit, at most 1 s).
    pub fn max_exposure_ns(&self) -> Option<i64> {
        self.exposure_ns.map(|(_, max)| max.min(1_000_000_000))
    }

    /// Stream fps with the given shutter: a shutter longer than one frame stretches the frame.
    pub fn fps_with_exposure(&self, exposure_ns: i64) -> f64 {
        1e9 / exposure_ns.max(self.frame_ns) as f64
    }

    pub fn summary(&self) -> String {
        let mut s = Vec::new();
        match (self.iso, self.exposure_ns) {
            (Some(iso), Some(_)) => s.push(
                t!(
                    "caps.iso_shutter",
                    min = iso.0,
                    max = iso.1,
                    fast = format_exposure(self.exposure_ns.unwrap().0),
                    slow = format_exposure(self.max_exposure_ns().unwrap())
                )
                .to_string(),
            ),
            _ => s.push(t!("caps.no_manual_sensor").to_string()),
        }
        match self.ev {
            Some((a, b)) => s.push(format!("EV {:+.1}..{:+.1}", a as f64 * self.ev_step, b as f64 * self.ev_step)),
            None => s.push(t!("caps.no_ev").to_string()),
        }
        match self.min_focus_diopter {
            Some(d) => s.push(t!("caps.focus_min", cm = format!("{:.0}", 100.0 / d)).to_string()),
            None => s.push(t!("caps.no_focus").to_string()),
        }
        let presets: Vec<&str> = AWB_PRESETS.iter().filter(|p| self.has_awb(p.0)).map(|p| p.1).collect();
        s.push(format!("WB: {}{}", presets.join("/"), if self.manual_wb { " + Kelvin" } else { "" }));
        if self.zoom_supported() {
            s.push(format!("zoom {:.1}x-{:.1}x", self.zoom.0, self.zoom.1));
        } else {
            s.push(t!("caps.no_zoom").to_string());
        }
        s.join(" | ")
    }
}

/// Actual values from the phone's capture results.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Live {
    pub iso: Option<i64>,
    pub exposure_ns: Option<i64>,
    pub focus_diopter: Option<f64>,
    pub zoom: Option<f64>,
    /// Increments whenever the phone rejected a control change.
    pub rejected: u64,
}

impl Live {
    pub fn apply(&mut self, pairs: &[(u8, i64)]) {
        for &(k, v) in pairs {
            match k {
                ctl::ISO => self.iso = Some(v),
                ctl::EXPOSURE_NS => self.exposure_ns = Some(v),
                ctl::FOCUS_MILLI_DIOPTER => self.focus_diopter = Some(v as f64 / 1000.0),
                ctl::ZOOM_MILLI => self.zoom = Some(v as f64 / 1000.0),
                ctl::ERROR => self.rejected += 1,
                _ => {}
            }
        }
    }

    pub fn summary(&self) -> String {
        let mut s = Vec::new();
        if let Some(i) = self.iso {
            s.push(format!("ISO {i}"));
        }
        if let Some(e) = self.exposure_ns {
            s.push(format_exposure(e));
        }
        if let Some(f) = self.focus_diopter {
            s.push(t!("live.focus", value = format_focus(f)).to_string());
        }
        if let Some(z) = self.zoom.filter(|z| *z > 1.01) {
            s.push(format!("{z:.1}x"));
        }
        s.join(" · ")
    }
}

pub fn format_exposure(ns: i64) -> String {
    let secs = ns as f64 / 1e9;
    if secs >= 0.3 { format!("{secs:.1} s") } else { format!("1/{:.0}", 1.0 / secs) }
}

pub fn format_focus(diopter: f64) -> String {
    if diopter < 0.05 { "∞".into() } else { format!("{:.0} cm", 100.0 / diopter) }
}

/// Parses a `--headless` camera command into SET_CONTROLS pairs.
/// `Ok(None)` means the line is not a camera command.
pub fn parse_command(line: &str, caps: Option<&Caps>, live: &Live) -> Result<Option<Vec<(u8, i64)>>, String> {
    let mut parts = line.split_whitespace();
    let (Some(cmd), arg) = (parts.next(), parts.next()) else { return Ok(None) };
    let arg2 = parts.next();
    let known = ["iso", "shutter", "ev", "focus", "refocus", "wb", "zoom", "lock"];
    if !known.contains(&cmd) {
        return Ok(None);
    }
    let caps = caps.ok_or_else(|| t!("cmd.no_caps").to_string())?;
    let unsupported = || Err(t!("cmd.unsupported").to_string());
    let need = |what: &str| t!("cmd.need_value", example = what).to_string();

    let pairs = match cmd {
        "iso" => match arg.ok_or_else(|| need("iso 400 / iso auto"))? {
            "auto" => vec![(ctl::AE_MODE, 0)],
            v => {
                let (Some((lo, hi)), true) = (caps.iso, caps.manual_sensor) else { return unsupported() };
                let iso: i64 = v.parse().map_err(|_| t!("cmd.iso_number").to_string())?;
                let mut p = vec![(ctl::AE_MODE, 1), (ctl::ISO, iso.clamp(lo, hi))];
                // Manual ISO turns auto exposure off; keep the current shutter instead of a default.
                if let Some(e) = live.exposure_ns {
                    p.push((ctl::EXPOSURE_NS, e));
                }
                p
            }
        },
        "shutter" => match arg.ok_or_else(|| need("shutter 1/250 / shutter auto"))? {
            "auto" => vec![(ctl::AE_MODE, 0)],
            v => {
                let (Some((lo, _)), Some(hi)) = (caps.exposure_ns, caps.max_exposure_ns()) else { return unsupported() };
                let ns = parse_shutter(v)?;
                let mut p = vec![(ctl::AE_MODE, 1), (ctl::EXPOSURE_NS, ns.clamp(lo, hi))];
                if let Some(i) = live.iso {
                    p.push((ctl::ISO, i));
                }
                p
            }
        },
        "ev" => {
            let Some((lo, hi)) = caps.ev else { return unsupported() };
            let ev: f64 = arg.ok_or_else(|| need("ev -1.5"))?.parse().map_err(|_| t!("cmd.ev_number").to_string())?;
            let steps = if caps.ev_step > 0.0 { (ev / caps.ev_step).round() as i64 } else { 0 };
            vec![(ctl::EV_STEPS, steps.clamp(lo, hi))]
        }
        "focus" => match arg.ok_or_else(|| need("focus 30 (cm) / focus inf / focus auto"))? {
            "auto" => {
                if !caps.autofocus {
                    return unsupported();
                }
                vec![(ctl::AF_MODE, 0)]
            }
            v => {
                let Some(max_d) = caps.min_focus_diopter else { return unsupported() };
                let d = if v == "inf" {
                    0.0
                } else {
                    let cm: f64 = v.trim_end_matches("cm").parse().map_err(|_| t!("cmd.focus_cm").to_string())?;
                    if cm <= 0.0 {
                        return Err(t!("cmd.distance_positive").to_string());
                    }
                    100.0 / cm
                };
                vec![(ctl::AF_MODE, 1), (ctl::FOCUS_MILLI_DIOPTER, (d.min(max_d) * 1000.0).round() as i64)]
            }
        },
        "refocus" => {
            if !caps.autofocus {
                return unsupported();
            }
            vec![(ctl::AF_TRIGGER, 1)]
        }
        "wb" => {
            let v = arg.ok_or_else(|| need("wb daylight / wb 3200K"))?;
            if let Some(k) = v.strip_suffix(['K', 'k']) {
                if !caps.manual_wb {
                    return unsupported();
                }
                let k: i64 = k.parse().map_err(|_| t!("cmd.kelvin").to_string())?;
                vec![(ctl::AWB_MODE, AWB_MANUAL), (ctl::WB_KELVIN, k.clamp(2000, 10000))]
            } else {
                let Some(p) = AWB_PRESETS.iter().find(|p| p.1 == v) else {
                    let names: Vec<&str> = AWB_PRESETS.iter().map(|p| p.1).collect();
                    return Err(t!("cmd.wb_presets", list = names.join(", ")).to_string());
                };
                if !caps.has_awb(p.0) {
                    return unsupported();
                }
                vec![(ctl::AWB_MODE, p.0)]
            }
        }
        "zoom" => {
            if !caps.zoom_supported() {
                return unsupported();
            }
            let z: f64 = arg.ok_or_else(|| need("zoom 2"))?.trim_end_matches('x').parse().map_err(|_| t!("cmd.zoom_number").to_string())?;
            vec![(ctl::ZOOM_MILLI, (z.clamp(caps.zoom.0, caps.zoom.1) * 1000.0).round() as i64)]
        }
        "lock" => {
            let on = match arg2 {
                Some("on") => 1,
                Some("off") => 0,
                _ => return Err("lock ae|awb on|off".into()),
            };
            match arg {
                Some("ae") if caps.ae_lock => vec![(ctl::AE_LOCK, on)],
                Some("awb") if caps.awb_lock => vec![(ctl::AWB_LOCK, on)],
                Some("ae" | "awb") => return unsupported(),
                _ => return Err("lock ae|awb on|off".into()),
            }
        }
        _ => unreachable!(),
    };
    Ok(Some(pairs))
}

/// "1/250", "1/30", "20ms", "0.5" (seconds) → nanoseconds.
pub fn parse_shutter(v: &str) -> Result<i64, String> {
    let err = || t!("cmd.shutter_format").to_string();
    let secs = if let Some(d) = v.strip_prefix("1/") {
        1.0 / d.parse::<f64>().map_err(|_| err())?
    } else if let Some(ms) = v.strip_suffix("ms") {
        ms.parse::<f64>().map_err(|_| err())? / 1000.0
    } else {
        v.trim_end_matches('s').parse::<f64>().map_err(|_| err())?
    };
    if secs <= 0.0 || !secs.is_finite() {
        return Err(err());
    }
    Ok((secs * 1e9).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::tlv_decode;
    use crate::protocol::tlv_encode;

    fn s22_back() -> Caps {
        Caps::from_tlv(&tlv_decode(&tlv_encode(&[
            (cap::MANUAL_SENSOR, 1),
            (cap::ISO_MIN, 26),
            (cap::ISO_MAX, 3368),
            (cap::EXPOSURE_MIN_NS, 30_488),
            (cap::EXPOSURE_MAX_NS, 450_999_590),
            (cap::EV_MIN, -20),
            (cap::EV_MAX, 20),
            (cap::EV_STEP_MILLI, 100),
            (cap::MIN_FOCUS_MILLI_DIOPTER, 10_000),
            (cap::AUTOFOCUS, 1),
            (cap::AWB_MODES, 0b1_1111_1111),
            (cap::MANUAL_WB, 1),
            (cap::ZOOM_MIN_MILLI, 1000),
            (cap::ZOOM_MAX_MILLI, 8000),
            (cap::AE_LOCK, 1),
            (cap::AWB_LOCK, 1),
            (cap::FRAME_DURATION_NS, 33_333_333),
        ])))
    }

    fn front() -> Caps {
        Caps::from_tlv(&[
            (cap::MANUAL_SENSOR, 0),
            (cap::EV_MIN, -20),
            (cap::EV_MAX, 20),
            (cap::EV_STEP_MILLI, 100),
            (cap::MIN_FOCUS_MILLI_DIOPTER, 0),
            (cap::AUTOFOCUS, 0),
            (cap::AWB_MODES, 0b1_1111_1110),
            (cap::MANUAL_WB, 0),
            (cap::ZOOM_MIN_MILLI, 1000),
            (cap::ZOOM_MAX_MILLI, 1000),
        ])
    }

    fn cmd(line: &str, caps: &Caps) -> Result<Vec<(u8, i64)>, String> {
        parse_command(line, Some(caps), &Live::default()).map(|o| o.unwrap())
    }

    #[test]
    fn parses_capabilities() {
        let c = s22_back();
        assert!(c.manual_sensor && c.manual_wb && c.autofocus && c.zoom_supported());
        assert_eq!(c.iso, Some((26, 3368)));
        assert_eq!(c.max_exposure_ns(), Some(450_999_590));
        assert_eq!(c.fps_with_exposure(40_000_000).round(), 25.0);
        assert_eq!(c.fps_with_exposure(10_000_000).round(), 30.0);
        assert_eq!(c.min_focus_diopter, Some(10.0));
        assert!(c.has_awb(5) && !c.has_awb(9));
        let f = front();
        assert!(!f.manual_sensor && f.iso.is_none() && f.min_focus_diopter.is_none() && !f.zoom_supported());
    }

    #[test]
    fn commands_follow_capabilities() {
        let c = s22_back();
        assert_eq!(cmd("iso 99999", &c).unwrap(), vec![(ctl::AE_MODE, 1), (ctl::ISO, 3368)]);
        assert_eq!(cmd("shutter 1/250", &c).unwrap(), vec![(ctl::AE_MODE, 1), (ctl::EXPOSURE_NS, 4_000_000)]);
        assert_eq!(cmd("shutter 1", &c).unwrap()[1], (ctl::EXPOSURE_NS, 450_999_590));
        assert_eq!(cmd("shutter 1/25", &c).unwrap()[1], (ctl::EXPOSURE_NS, 40_000_000));
        assert_eq!(cmd("ev -1.5", &c).unwrap(), vec![(ctl::EV_STEPS, -15)]);
        assert_eq!(cmd("focus 50", &c).unwrap(), vec![(ctl::AF_MODE, 1), (ctl::FOCUS_MILLI_DIOPTER, 2000)]);
        assert_eq!(cmd("focus 1", &c).unwrap()[1], (ctl::FOCUS_MILLI_DIOPTER, 10_000));
        assert_eq!(cmd("wb 3200K", &c).unwrap(), vec![(ctl::AWB_MODE, 0), (ctl::WB_KELVIN, 3200)]);
        assert_eq!(cmd("wb cloudy", &c).unwrap(), vec![(ctl::AWB_MODE, 6)]);
        assert_eq!(cmd("zoom 20", &c).unwrap(), vec![(ctl::ZOOM_MILLI, 8000)]);
        assert_eq!(cmd("lock awb on", &c).unwrap(), vec![(ctl::AWB_LOCK, 1)]);

        let f = front();
        for line in ["iso 400", "shutter 1/60", "focus 30", "refocus", "wb 4000K", "zoom 2", "lock ae on"] {
            assert_eq!(cmd(line, &f), Err(t!("cmd.unsupported").to_string()), "{line}");
        }
        assert_eq!(cmd("ev 1", &f).unwrap(), vec![(ctl::EV_STEPS, 10)]);
        assert_eq!(cmd("wb daylight", &f).unwrap(), vec![(ctl::AWB_MODE, 5)]);
    }

    #[test]
    fn non_camera_lines_pass_through() {
        assert_eq!(parse_command("s", None, &Live::default()), Ok(None));
        assert!(parse_command("iso 100", None, &Live::default()).is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(format_exposure(4_000_000), "1/250");
        assert_eq!(format_exposure(500_000_000), "0.5 s");
        assert_eq!(format_focus(0.0), "∞");
        assert_eq!(format_focus(2.0), "50 cm");
    }
}
