mod adb;
mod controls;
mod decode;
#[cfg(windows)]
mod decode_mf;
mod engine;
mod gui;
mod gui_image;
mod i18n;
mod protocol;
mod nosignal;
mod share;
mod usb;
#[cfg(target_os = "linux")]
mod v4l2out;
#[cfg(windows)]
mod vcam_windows;
mod webcam;
mod yuv;

use anyhow::{Result, bail};
use rust_i18n::t;
use clap::Parser;
use engine::{Engine, EngineConfig, Mode};
use std::io::BufRead;
use std::time::Duration;

rust_i18n::i18n!("locales", fallback = "en");

#[derive(Parser, Debug, Clone)]
#[command(version, about = t!("cli.about").to_string())]
pub struct Args {
    #[arg(help = t!("cli.headless").to_string(), long)]
    headless: bool,
    #[arg(help = t!("cli.host").to_string(), long)]
    host: Option<String>,
    #[arg(long, default_value_t = 5000, help = t!("cli.port").to_string())]
    port: u16,
    #[arg(help = t!("cli.discover").to_string(), long)]
    discover: bool,
    #[arg(help = t!("cli.token").to_string(), long, short)]
    token: Option<u32>,
    #[arg(help = t!("cli.usb").to_string(), long, conflicts_with_all = ["host", "discover"])]
    usb: bool,
    #[arg(help = t!("cli.adb").to_string(), long, conflicts_with_all = ["host", "discover", "usb"])]
    adb: bool,
    #[arg(help = t!("cli.usb_vid").to_string(), long, value_parser = parse_hex_u16, requires = "usb")]
    usb_vid: Option<u16>,
    #[arg(help = t!("cli.device").to_string(), long, default_value_t = default_device())]
    device: String,
    #[arg(help = t!("cli.no_webcam").to_string(), long)]
    no_webcam: bool,
    #[arg(help = t!("cli.dump").to_string(), long)]
    dump: Option<String>,
    #[arg(help = t!("cli.size").to_string(), long, default_value = "1280x720", value_parser = parse_size)]
    size: (u16, u16),
    #[arg(long, default_value_t = 30, help = t!("cli.fps").to_string())]
    fps: u8,
    #[arg(long, default_value_t = 4000, help = t!("cli.bitrate").to_string())]
    bitrate: u32,
    #[arg(help = t!("cli.front").to_string(), long)]
    front: bool,
    #[arg(help = t!("cli.no_obs").to_string(), long)]
    no_obs: bool,
    #[arg(long, help = t!("cli.lang", list = i18n::languages().iter().map(|(c, _)| *c).collect::<Vec<_>>().join(", ")).to_string())]
    lang: Option<String>,
}

impl Args {
    /// Engine config from the connection flags, or None if no connection was requested.
    pub fn engine_config(&self, stats_interval: Duration) -> Result<Option<EngineConfig>> {
        let mode = if self.usb {
            Mode::Usb { vid: self.usb_vid }
        } else if self.adb {
            let Some(token) = self.token else { bail!("{}", t!("cli.need_token")) };
            Mode::Adb { port: self.port, token }
        } else if self.host.is_some() || self.discover {
            let Some(token) = self.token else { bail!("{}", t!("cli.need_token")) };
            Mode::Tcp { host: self.host.clone(), port: self.port, token }
        } else {
            return Ok(None);
        };
        Ok(Some(EngineConfig {
            mode,
            size: self.size,
            fps: self.fps,
            bitrate: self.bitrate,
            front: self.front,
            device: self.device.clone(),
            dump: self.dump.clone(),
            stats_interval,
        }))
    }
}

/// The `--device` default: a v4l2loopback node on Linux, unused elsewhere until a platform
/// `VirtualCamera` impl needs a different kind of identifier (see `webcam.rs`).
#[cfg(target_os = "linux")]
fn default_device() -> String {
    "/dev/video10".to_string()
}

#[cfg(not(target_os = "linux"))]
fn default_device() -> String {
    String::new()
}

pub fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s.split_once('x').ok_or_else(|| t!("cli.size_format").to_string())?;
    let w: u16 = w.parse().map_err(|_| t!("cli.width_invalid").to_string())?;
    let h: u16 = h.parse().map_err(|_| t!("cli.height_invalid").to_string())?;
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
        return Err(t!("cli.size_even").to_string());
    }
    Ok((w, h))
}

pub fn parse_hex_u16(s: &str) -> Result<u16, String> {
    u16::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| t!("cli.hex_invalid").to_string())
}

fn main() -> Result<()> {
    if let Err(e) = i18n::init(i18n::lang_arg().as_deref()) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let args = Args::parse();
    if args.headless {
        headless(&args)
    } else {
        #[cfg(windows)]
        hide_own_console();
        gui::run(args)
    }
}

/// acam is a console program (so `--headless` and `--help` work in a terminal), which makes Windows
/// open a console window when the GUI is started from the Start menu or Explorer. Drop that window —
/// but only if it was made for us, not when acam was started from a terminal.
#[cfg(windows)]
fn hide_own_console() {
    use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0u32; 2];
    if unsafe { GetConsoleProcessList(&mut pids) } == 1 {
        let _ = unsafe { FreeConsole() };
    }
}

fn headless(args: &Args) -> Result<()> {
    let Some(cfg) = args.engine_config(Duration::from_secs(5))? else {
        bail!("{}", t!("cli.need_target"));
    };
    let engine = Engine::new(|msg| println!("{msg}"));
    engine.set_webcam(!args.no_webcam);
    engine.set_obs(!args.no_obs);
    engine.connect(cfg);

    println!("{}", t!("cli.commands"));
    println!("  iso <n|auto>, shutter <1/250|20ms|auto>, ev <-2..2>, focus <cm|inf|auto>, refocus,");
    println!("  wb <auto|daylight|cloudy|shade|incandescent|fluorescent|twilight|3200K>, zoom <x>,");
    println!("  lock ae|awb on|off, reset");
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let (caps, live) = {
            let st = engine.state();
            (st.caps.clone(), st.live.clone())
        };
        match controls::parse_command(&line, caps.as_ref(), &live) {
            Ok(Some(pairs)) => {
                engine.set_controls(&pairs);
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("{e}");
                continue;
            }
        }
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("s"), _) => engine.switch_camera(),
            (Some("k"), _) => engine.request_keyframe(),
            (Some("reset"), _) => engine.reset_controls(),
            (Some("caps"), _) => match caps {
                Some(c) => println!("{}", c.summary()),
                None => eprintln!("{}", t!("cli.caps_missing")),
            },
            (Some("status"), _) => println!("{}", live.summary()),
            (Some("b"), Some(v)) => match v.parse::<u32>() {
                Ok(kbps) => engine.set_bitrate(kbps),
                Err(_) => eprintln!("{}", t!("cli.bitrate_invalid")),
            },
            (None, _) => {}
            _ => eprintln!("{}", t!("cli.unknown_command")),
        }
    }
    // stdin closed (e.g. running as a service): keep streaming.
    loop {
        std::thread::park();
    }
}
