//! Desktop window (gpui + gpui-component): live preview, connection panel and camera controls.

use rust_i18n::t;
use crate::Args;
use crate::controls::Live;
use crate::engine::{self, Engine, EngineConfig, Mode, Stats, Status};
use crate::gui_image::ImageControls;
use crate::i18n;
use anyhow::Result;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable, Root, Selectable, Sizable, Theme, ThemeMode, h_flex, v_flex};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SIZES: [(u16, u16); 3] = [(640, 480), (1280, 720), (1920, 1080)];
const FPS: [u8; 5] = [15, 24, 25, 30, 60];

#[derive(Clone, Copy, PartialEq)]
enum Transport {
    Wifi,
    Usb,
}

struct Found {
    ip: String,
    name: String,
    seen: Instant,
}

struct AcamApp {
    engine: Arc<Engine>,
    transport: Transport,
    host: Entity<InputState>,
    token: Entity<InputState>,
    usb_vid: Entity<InputState>,
    device: Entity<InputState>,
    bitrate: Entity<SliderState>,
    bitrate_kbps: u32,
    size: (u16, u16),
    fps: u8,
    front: bool,
    webcam: bool,
    obs: bool,
    obs_clients: usize,
    preview: bool,
    port: u16,
    dump: Option<String>,
    /// Config of the running connection, if any.
    active: Option<EngineConfig>,
    found: Arc<Mutex<Vec<Found>>>,
    form_error: Option<String>,
    image: Option<Arc<RenderImage>>,
    status: Status,
    stats: Stats,
    webcam_error: Option<String>,
    found_count: usize,
    image_controls: Entity<ImageControls>,
    live: Live,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl AcamApp {
    fn new(args: &Args, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("gui.host_placeholder").to_string()).default_value(args.host.clone().unwrap_or_default())
        });
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("gui.token_placeholder").to_string())
                .default_value(args.token.map(|t| t.to_string()).unwrap_or_default())
        });
        let usb_vid = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("gui.vid_placeholder").to_string())
                .default_value(args.usb_vid.map(|v| format!("{v:04x}")).unwrap_or_default())
        });
        let device = cx.new(|cx| InputState::new(window, cx).default_value(args.device.clone()));
        let bitrate = cx.new(|_| {
            SliderState::new().max(12000.).min(500.).step(250.).default_value(args.bitrate as f32)
        });

        let engine = Arc::new(Engine::new(|msg| eprintln!("{msg}")));
        let webcam = !args.no_webcam;
        engine.set_webcam(webcam);
        engine.set_obs(!args.no_obs);
        engine.set_preview(true);

        let subs = vec![cx.subscribe(&bitrate, |this, _, ev: &SliderEvent, cx| {
            let SliderEvent::Change(v) = ev;
            this.bitrate_kbps = v.start().round() as u32;
            this.engine.set_bitrate(this.bitrate_kbps);
            if let Some(a) = this.active.as_mut() {
                a.bitrate = this.bitrate_kbps;
            }
            cx.notify();
        })];

        let image_controls = cx.new(|_| ImageControls::new(engine.clone()));
        let found = Arc::new(Mutex::new(Vec::new()));
        spawn_discovery_listener(found.clone());

        // Poll the engine at ~60 Hz: new preview frames, status and stats.
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                if this.update_in(cx, |this, window, cx| this.poll(window, cx)).is_err() {
                    break;
                }
            }
        });

        let mut app = Self {
            engine,
            transport: if args.usb { Transport::Usb } else { Transport::Wifi },
            host,
            token,
            usb_vid,
            device,
            bitrate,
            bitrate_kbps: args.bitrate,
            size: args.size,
            fps: args.fps,
            front: args.front,
            webcam,
            obs: !args.no_obs,
            obs_clients: 0,
            preview: true,
            port: args.port,
            dump: args.dump.clone(),
            active: None,
            found,
            form_error: None,
            image: None,
            status: Status::Idle,
            stats: Stats::default(),
            webcam_error: None,
            found_count: 0,
            image_controls,
            live: Live::default(),
            _tasks: vec![poll],
            _subs: subs,
        };
        // Connection flags on the command line connect right away.
        match args.engine_config(Duration::from_secs(1)) {
            Ok(Some(cfg)) => app.start(cfg),
            Ok(None) => {}
            Err(e) => app.form_error = Some(format!("{e:#}")),
        }
        app
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (frame, status, stats, webcam_error, caps, live) = {
            let mut st = self.engine.state();
            (st.frame.take(), st.status.clone(), st.stats, st.webcam_error.clone(), st.caps.clone(), st.live.clone())
        };
        self.image_controls.update(cx, |c, cx| {
            c.set_caps(caps, cx);
            c.set_live(live.clone(), cx);
            c.flush();
        });
        let mut changed = false;
        if let Some(frame) = frame {
            let frame = Arc::try_unwrap(frame).unwrap_or_else(|f| engine::PreviewFrame {
                width: f.width,
                height: f.height,
                bgra: f.bgra.clone(),
            });
            if let Some(buf) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra) {
                let next = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(buf)]));
                // Each RenderImage occupies GPU atlas space until dropped explicitly.
                if let Some(old) = self.image.replace(next) {
                    let _ = window.drop_image(old);
                }
                changed = true;
            }
        }
        if !matches!(status, Status::Connected { .. }) || !self.preview {
            if let Some(old) = self.image.take() {
                let _ = window.drop_image(old);
                changed = true;
            }
        }
        if status != self.status {
            self.status = status;
            changed = true;
        }
        if (stats.fps, stats.kbps, stats.rtt_ms) != (self.stats.fps, self.stats.kbps, self.stats.rtt_ms) {
            self.stats = stats;
            changed = true;
        }
        let obs_clients = self.engine.obs_clients();
        if obs_clients != self.obs_clients {
            self.obs_clients = obs_clients;
            changed = true;
        }
        if live != self.live {
            self.live = live;
            changed = true;
        }
        if webcam_error != self.webcam_error {
            self.webcam_error = webcam_error;
            changed = true;
        }
        let found_count = {
            let mut f = self.found.lock().unwrap();
            f.retain(|d| d.seen.elapsed() < Duration::from_secs(5));
            f.len()
        };
        if found_count != self.found_count {
            self.found_count = found_count;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn form_config(&self, cx: &App) -> Result<EngineConfig, String> {
        let mode = match self.transport {
            Transport::Wifi => {
                let host = self.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return Err(t!("gui.err_host").to_string());
                }
                let token = self.token.read(cx).value().trim().parse::<u32>().map_err(|_| t!("gui.err_token").to_string())?;
                Mode::Tcp { host: Some(host), port: self.port, token }
            }
            Transport::Usb => {
                let vid = self.usb_vid.read(cx).value().trim().to_string();
                let vid = if vid.is_empty() { None } else { Some(crate::parse_hex_u16(&vid)?) };
                Mode::Usb { vid }
            }
        };
        Ok(EngineConfig {
            mode,
            size: self.size,
            fps: self.fps,
            bitrate: self.bitrate_kbps,
            front: self.front,
            device: self.device.read(cx).value().trim().to_string(),
            dump: self.dump.clone(),
            stats_interval: Duration::from_secs(1),
        })
    }

    fn start(&mut self, cfg: EngineConfig) {
        self.form_error = None;
        self.engine.connect(cfg.clone());
        self.active = Some(cfg);
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        match self.form_config(cx) {
            Ok(cfg) => self.start(cfg),
            Err(e) => self.form_error = Some(e),
        }
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.engine.disconnect();
        self.active = None;
        cx.notify();
    }

    /// Resolution/fps/device are negotiated at handshake, so changing them reconnects.
    fn reconnect_if_active(&mut self, cx: &mut Context<Self>) {
        if let Some(mut cfg) = self.active.clone() {
            cfg.size = self.size;
            cfg.fps = self.fps;
            cfg.front = self.front;
            cfg.device = self.device.read(cx).value().trim().to_string();
            self.start(cfg);
        }
        cx.notify();
    }

    /// Switches the interface language; texts built once (input placeholders) are refreshed here.
    fn set_language(&mut self, code: &str, window: &mut Window, cx: &mut Context<Self>) {
        i18n::set(code);
        self.host.update(cx, |s, cx| s.set_placeholder(t!("gui.host_placeholder").to_string(), window, cx));
        self.token.update(cx, |s, cx| s.set_placeholder(t!("gui.token_placeholder").to_string(), window, cx));
        self.usb_vid.update(cx, |s, cx| s.set_placeholder(t!("gui.vid_placeholder").to_string(), window, cx));
        self.image_controls.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let placeholder = match &self.status {
            Status::Idle => t!("gui.preview_idle").to_string(),
            Status::Connecting(target) => t!("gui.status_connecting", target = target).to_string(),
            Status::Connected { .. } if !self.preview => t!("gui.preview_off").to_string(),
            Status::Connected { .. } => t!("gui.preview_waiting").to_string(),
            Status::Error(_) => t!("gui.preview_error").to_string(),
        };
        let overlay = match &self.status {
            Status::Connected { name, width, height, fps } => {
                let mut text = format!(
                    "{name}  ·  {width}x{height} @ {fps}  ·  {:.1} fps  ·  {:.0} kbps  ·  RTT {:.0} ms",
                    self.stats.fps, self.stats.kbps, self.stats.rtt_ms
                );
                let live = self.live.summary();
                if !live.is_empty() {
                    text = format!("{text}\n{live}");
                }
                Some(text)
            }
            _ => None,
        };
        div()
            .relative()
            .flex_1()
            // Without these the image's intrinsic width pushes the side panel out of the window.
            .min_w(px(0.))
            .overflow_hidden()
            .h_full()
            .bg(black())
            .flex()
            .items_center()
            .justify_center()
            .child(match &self.image {
                Some(image) => img(ImageSource::Render(image.clone()))
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => div().text_color(theme.muted_foreground).child(placeholder).into_any_element(),
            })
            .children(overlay.map(|text| {
                div()
                    .absolute()
                    .left_3()
                    .bottom_3()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(hsla(0., 0., 0., 0.6))
                    .text_xs()
                    .text_color(white())
                    .child(text)
            }))
    }

    fn render_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let connected = self.active.is_some();
        let busy = matches!(self.status, Status::Connected { .. });

        let section = |title: std::borrow::Cow<'static, str>| {
            div()
                .pt_2()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(title.to_string())
        };

        let status_line = {
            let (text, color) = match &self.status {
                Status::Idle => (t!("gui.status_idle").to_string(), theme.muted_foreground),
                Status::Connecting(target) => (t!("gui.status_connecting", target = target).to_string(), theme.warning),
                Status::Connected { name, .. } => (t!("gui.status_connected", name = name).to_string(), theme.success),
                Status::Error(e) => (e.clone(), theme.danger),
            };
            div().text_sm().text_color(color).child(text)
        };

        let transport_tabs = h_flex()
            .gap_1()
            .child(
                Button::new("tab-wifi")
                    .label("Wi-Fi")
                    .small()
                    .selected(self.transport == Transport::Wifi)
                    .disabled(connected)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.transport = Transport::Wifi;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("tab-usb")
                    .label("USB")
                    .small()
                    .selected(self.transport == Transport::Usb)
                    .disabled(connected)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.transport = Transport::Usb;
                        cx.notify();
                    })),
            );

        let transport_form = match self.transport {
            Transport::Wifi => {
                let found: Vec<(String, String)> = {
                    let f = self.found.lock().unwrap();
                    f.iter().map(|d| (d.ip.clone(), d.name.clone())).collect()
                };
                v_flex()
                    .gap_2()
                    .child(Input::new(&self.host).disabled(connected))
                    .child(Input::new(&self.token).disabled(connected))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(if found.is_empty() {
                        t!("gui.searching").to_string()
                    } else {
                        t!("gui.found").to_string()
                    }))
                    .children(found.into_iter().enumerate().map(|(i, (ip, name))| {
                        let host = self.host.clone();
                        Button::new(("found", i))
                            .label(format!("{name} — {ip}"))
                            .small()
                            .ghost()
                            .disabled(connected)
                            .on_click(cx.listener(move |_, _, window, cx| {
                                host.update(cx, |s, cx| s.set_value(ip.clone(), window, cx));
                            }))
                    }))
                    .into_any_element()
            }
            Transport::Usb => v_flex()
                .gap_2()
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    t!("gui.usb_hint").to_string(),
                ))
                .child(Input::new(&self.usb_vid).disabled(connected))
                .into_any_element(),
        };

        let connect_button = if connected {
            Button::new("disconnect").label(t!("gui.disconnect").to_string()).danger().on_click(cx.listener(|this, _, _, cx| this.disconnect(cx)))
        } else {
            Button::new("connect").label(t!("gui.connect").to_string()).primary().on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
        };

        let size_buttons = h_flex().gap_1().children(SIZES.iter().map(|&(w, h)| {
            Button::new(SharedString::from(format!("size-{w}")))
                .label(format!("{h}p"))
                .small()
                .selected(self.size == (w, h))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.size = (w, h);
                    this.reconnect_if_active(cx);
                }))
        }));
        let fps_buttons = h_flex().gap_1().children(FPS.iter().map(|&f| {
            Button::new(SharedString::from(format!("fps-{f}")))
                .label(format!("{f}"))
                .small()
                .selected(self.fps == f)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fps = f;
                    this.reconnect_if_active(cx);
                }))
        }));

        let current = i18n::current();
        let language_buttons = h_flex().gap_1().flex_wrap().children(i18n::languages().into_iter().map(|(code, name)| {
            Button::new(SharedString::from(format!("lang-{code}")))
                .label(name)
                .small()
                .selected(current == code)
                .on_click(cx.listener(move |this, _, window, cx| this.set_language(code, window, cx)))
        }));

        v_flex()
            .id("panel")
            .w(px(360.))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .p_4()
            .gap_2()
            .bg(theme.sidebar)
            .border_l_1()
            .border_color(theme.border)
            .child(div().text_lg().font_weight(FontWeight::BOLD).child("ACAM"))
            .child(status_line)
            .children(self.form_error.clone().map(|e| div().text_sm().text_color(theme.danger).child(e)))
            .child(section(t!("gui.section_connection")))
            .child(transport_tabs)
            .child(transport_form)
            .child(connect_button)
            .child(section(t!("gui.section_camera")))
            .child(
                Button::new("switch-camera")
                    .label(if self.front { t!("gui.camera_front") } else { t!("gui.camera_back") }.to_string())
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.front = !this.front;
                        if let Some(a) = this.active.as_mut() {
                            a.front = this.front;
                        }
                        this.engine.switch_camera();
                        cx.notify();
                    })),
            )
            .child(div().text_sm().child(t!("gui.bitrate", kbps = self.bitrate_kbps).to_string()))
            .child(Slider::new(&self.bitrate))
            .child(div().text_sm().child(t!("gui.resolution").to_string()))
            .child(size_buttons)
            .child(div().text_sm().child(t!("gui.fps").to_string()))
            .child(fps_buttons)
            .children(busy.then(|| section(t!("gui.section_image"))))
            .children(busy.then(|| self.image_controls.clone()))
            .child(section(t!("gui.section_output")))
            .child(
                Switch::new("webcam")
                    .label(t!("gui.webcam").to_string())
                    .checked(self.webcam)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        this.webcam = *on;
                        this.engine.set_webcam(*on);
                        cx.notify();
                    })),
            )
            .child(Input::new(&self.device).disabled(busy))
            .children(self.webcam_error.clone().map(|e| div().text_xs().text_color(theme.danger).child(e)))
            .child(
                Switch::new("obs")
                    .label(t!("gui.obs").to_string())
                    .checked(self.obs)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        this.obs = *on;
                        this.engine.set_obs(*on);
                        cx.notify();
                    })),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(match self.engine.obs_socket() {
                Some(path) if self.obs_clients > 0 => {
                    t!("gui.obs_clients", count = self.obs_clients, path = path).to_string()
                }
                Some(path) => t!("gui.obs_hint", path = path).to_string(),
                None => t!("gui.obs_inactive").to_string(),
            }))
            .child(
                Switch::new("preview")
                    .label(t!("gui.preview").to_string())
                    .checked(self.preview)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        this.preview = *on;
                        this.engine.set_preview(*on);
                        cx.notify();
                    })),
            )
            .child(section(t!("gui.section_language")))
            .child(language_buttons)
    }
}

impl Render for AcamApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_preview(cx))
            .child(self.render_panel(cx))
    }
}

/// Collects ACAM announce broadcasts (UDP 5001) for the "HP ditemukan" list.
fn spawn_discovery_listener(found: Arc<Mutex<Vec<Found>>>) {
    std::thread::spawn(move || {
        let sock = loop {
            match UdpSocket::bind(("0.0.0.0", engine::DISCOVERY_PORT)) {
                Ok(s) => break s,
                Err(_) => std::thread::sleep(Duration::from_secs(2)),
            }
        };
        let mut buf = [0u8; 512];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            let Some(name) = engine::parse_announce(&buf[..n]) else { continue };
            let ip = from.ip().to_string();
            let mut f = found.lock().unwrap();
            match f.iter_mut().find(|d| d.ip == ip) {
                Some(d) => d.seen = Instant::now(),
                None => f.push(Found { ip, name, seen: Instant::now() }),
            }
        }
    });
}

pub fn run(args: Args) -> Result<()> {
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        let bounds = Bounds::centered(None, size(px(1180.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("ACAM".into()), ..Default::default() }),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| AcamApp::new(&args, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect(&t!("gui.window_failed"));
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
    Ok(())
}
