//! "GAMBAR" section of the side panel: exposure, focus, white balance and zoom.
//! Every control is built from the capabilities the phone reports for the active camera,
//! so unsupported features simply don't appear.

use rust_i18n::t;
use crate::controls::{AWB_MANUAL, AWB_PRESETS, Caps, Live, awb_label, format_exposure, format_focus};
use crate::engine::Engine;
use crate::protocol::ctl;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::slider::{Slider, SliderEvent, SliderScale, SliderState};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Selectable, Sizable, h_flex, v_flex};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Slider drags are sent at most this often; the last value is always flushed.
const SEND_INTERVAL: Duration = Duration::from_millis(66);

/// Common shutter speeds (1/x s); 1/24 and 1/25 match 24/25 fps film and PAL video.
const SHUTTER_PRESETS: [i64; 7] = [24, 25, 30, 50, 60, 120, 250];

#[derive(Clone, Copy)]
struct Values {
    ae_manual: bool,
    iso: f32,
    exposure_ns: f32,
    ev: f32,
    ae_lock: bool,
    af_manual: bool,
    focus_d: f32,
    awb: i64,
    kelvin: f32,
    awb_lock: bool,
    zoom: f32,
}

impl Default for Values {
    fn default() -> Self {
        Values {
            ae_manual: false,
            iso: 400.,
            exposure_ns: 16_666_667.,
            ev: 0.,
            ae_lock: false,
            af_manual: false,
            focus_d: 0.,
            awb: 1,
            kelvin: 5500.,
            awb_lock: false,
            zoom: 1.,
        }
    }
}

struct Sliders {
    iso: Option<Entity<SliderState>>,
    exposure: Option<Entity<SliderState>>,
    ev: Option<Entity<SliderState>>,
    focus: Option<Entity<SliderState>>,
    kelvin: Option<Entity<SliderState>>,
    zoom: Option<Entity<SliderState>>,
}

pub struct ImageControls {
    engine: Arc<Engine>,
    caps: Option<Caps>,
    live: Live,
    v: Values,
    sliders: Sliders,
    pending: BTreeMap<u8, i64>,
    last_sent: Instant,
    _subs: Vec<Subscription>,
}

impl ImageControls {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            caps: None,
            live: Live::default(),
            v: Values::default(),
            sliders: Sliders { iso: None, exposure: None, ev: None, focus: None, kelvin: None, zoom: None },
            pending: BTreeMap::new(),
            last_sent: Instant::now(),
            _subs: Vec::new(),
        }
    }

    /// New camera (or none): rebuild sliders for its ranges and start from automatic settings.
    pub fn set_caps(&mut self, caps: Option<Caps>, cx: &mut Context<Self>) {
        if caps == self.caps {
            return;
        }
        self.caps = caps;
        self.v = Values::default();
        self.pending.clear();
        self._subs.clear();
        self.sliders = Sliders { iso: None, exposure: None, ev: None, focus: None, kelvin: None, zoom: None };
        if let Some(c) = self.caps.clone() {
            if let (Some((lo, hi)), true) = (c.iso, c.manual_sensor) {
                self.sliders.iso = Some(self.slider(lo as f32, hi as f32, 1., true, 400., cx, |this, v| {
                    this.v.iso = v;
                    vec![(ctl::ISO, v.round() as i64)]
                }));
            }
            if let (Some((lo, _)), Some(hi)) = (c.exposure_ns, c.max_exposure_ns()) {
                let start = (hi as f32 / 2.).max(lo as f32);
                self.v.exposure_ns = start;
                self.sliders.exposure = Some(self.slider(lo as f32, hi as f32, 1., true, start, cx, |this, v| {
                    this.v.exposure_ns = v;
                    vec![(ctl::EXPOSURE_NS, v.round() as i64)]
                }));
            }
            if let Some((lo, hi)) = c.ev {
                let step = c.ev_step as f32;
                self.sliders.ev = Some(self.slider(lo as f32 * step, hi as f32 * step, step, false, 0., cx, move |this, v| {
                    this.v.ev = v;
                    vec![(ctl::EV_STEPS, (v / step).round() as i64)]
                }));
            }
            if let Some(max_d) = c.min_focus_diopter {
                // 0 diopter = infinity on the left, closest focus on the right.
                self.sliders.focus = Some(self.slider(0., max_d as f32, 0.05, false, 0., cx, |this, v| {
                    this.v.focus_d = v;
                    vec![(ctl::FOCUS_MILLI_DIOPTER, (v * 1000.).round() as i64)]
                }));
            }
            if c.manual_wb {
                self.sliders.kelvin = Some(self.slider(2000., 10000., 100., false, 5500., cx, |this, v| {
                    this.v.kelvin = v;
                    vec![(ctl::WB_KELVIN, v.round() as i64)]
                }));
            }
            if c.zoom_supported() {
                self.sliders.zoom = Some(self.slider(c.zoom.0 as f32, c.zoom.1 as f32, 0.1, false, 1., cx, |this, v| {
                    this.v.zoom = v;
                    vec![(ctl::ZOOM_MILLI, (v * 1000.).round() as i64)]
                }));
            }
        }
        cx.notify();
    }

    pub fn set_live(&mut self, live: Live, cx: &mut Context<Self>) {
        if live != self.live {
            self.live = live;
            cx.notify();
        }
    }

    /// Sends queued slider values, rate-limited. Called from the window's poll loop.
    pub fn flush(&mut self) {
        if !self.pending.is_empty() && self.last_sent.elapsed() >= SEND_INTERVAL {
            let pairs: Vec<(u8, i64)> = std::mem::take(&mut self.pending).into_iter().collect();
            self.engine.set_controls(&pairs);
            self.last_sent = Instant::now();
        }
    }

    fn queue(&mut self, pairs: Vec<(u8, i64)>) {
        self.pending.extend(pairs);
    }

    /// Buttons and switches go out immediately, together with anything still queued.
    fn send_now(&mut self, pairs: Vec<(u8, i64)>) {
        self.pending.extend(pairs);
        self.last_sent = Instant::now() - SEND_INTERVAL;
        self.flush();
    }

    #[allow(clippy::too_many_arguments)]
    fn slider(
        &mut self,
        min: f32,
        max: f32,
        step: f32,
        log: bool,
        value: f32,
        cx: &mut Context<Self>,
        on_change: impl Fn(&mut Self, f32) -> Vec<(u8, i64)> + 'static,
    ) -> Entity<SliderState> {
        let state = cx.new(|_| {
            let s = SliderState::new().max(max).min(min).step(step);
            let s = if log { s.scale(SliderScale::Logarithmic) } else { s };
            s.default_value(value.clamp(min, max))
        });
        self._subs.push(cx.subscribe(&state, move |this, _, ev: &SliderEvent, cx| {
            let SliderEvent::Change(v) = ev;
            let pairs = on_change(this, v.start());
            this.queue(pairs);
            cx.notify();
        }));
        state
    }

    fn set_slider(slider: &Option<Entity<SliderState>>, value: f32, window: &mut Window, cx: &mut App) {
        if let Some(s) = slider {
            s.update(cx, |s, cx| s.set_value(value, window, cx));
        }
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.v = Values::default();
        self.pending.clear();
        self.engine.reset_controls();
        Self::set_slider(&self.sliders.ev, 0., window, cx);
        Self::set_slider(&self.sliders.zoom, 1., window, cx);
        Self::set_slider(&self.sliders.kelvin, 5500., window, cx);
        cx.notify();
    }
}

fn label_row(label: impl Into<SharedString>, value: String) -> Div {
    h_flex().justify_between().text_sm().child(label.into()).child(div().font_family("monospace").child(value))
}

impl Render for ImageControls {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let note = move |text: std::borrow::Cow<'static, str>| div().text_xs().text_color(muted).child(text.to_string());
        let heading = |text: std::borrow::Cow<'static, str>| div().pt_1().text_sm().font_weight(FontWeight::SEMIBOLD).child(text.to_string());

        let Some(caps) = self.caps.clone() else {
            return v_flex().child(note(t!("img.waiting")));
        };
        let v = self.v;
        let mut col = v_flex().gap_2();

        // Exposure
        col = col.child(heading(t!("img.exposure")));
        if caps.manual_sensor || caps.ev.is_some() {
            if caps.manual_sensor {
                col = col.child(
                    h_flex()
                        .gap_1()
                        .child(Button::new("ae-auto").label(t!("img.auto").to_string()).small().selected(!v.ae_manual).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.v.ae_manual = false;
                                this.send_now(vec![(ctl::AE_MODE, 0)]);
                                cx.notify();
                            },
                        )))
                        .child(Button::new("ae-manual").label(t!("img.manual").to_string()).small().selected(v.ae_manual).on_click(
                            cx.listener(|this, _, window, cx| {
                                // Start manual from what auto exposure is doing right now.
                                // Auto exposure may use digital gain beyond the manual ISO range, so clamp.
                                let caps = this.caps.clone().unwrap_or_default();
                                let (iso_lo, iso_hi) = caps.iso.unwrap_or((1, i64::MAX));
                                let (exp_lo, _) = caps.exposure_ns.unwrap_or((1, i64::MAX));
                                let exp_hi = caps.max_exposure_ns().unwrap_or(i64::MAX);
                                let iso = this.live.iso.map(|i| i as f32).unwrap_or(this.v.iso);
                                let iso = iso.clamp(iso_lo as f32, iso_hi as f32);
                                let exp = this.live.exposure_ns.map(|e| e as f32).unwrap_or(this.v.exposure_ns);
                                let exp = exp.clamp(exp_lo as f32, exp_hi as f32);
                                this.v.ae_manual = true;
                                this.v.iso = iso;
                                this.v.exposure_ns = exp;
                                Self::set_slider(&this.sliders.iso, iso, window, cx);
                                Self::set_slider(&this.sliders.exposure, exp, window, cx);
                                this.send_now(vec![
                                    (ctl::AE_MODE, 1),
                                    (ctl::ISO, iso.round() as i64),
                                    (ctl::EXPOSURE_NS, exp.round() as i64),
                                ]);
                                cx.notify();
                            }),
                        )),
                );
            }
            if v.ae_manual {
                if let Some(s) = &self.sliders.iso {
                    col = col.child(label_row(t!("img.iso").to_string(), format!("{:.0}", v.iso))).child(Slider::new(s));
                }
                if let Some(s) = &self.sliders.exposure {
                    let (lo, _) = caps.exposure_ns.unwrap_or_default();
                    let hi = caps.max_exposure_ns().unwrap_or_default();
                    let mut presets = h_flex().gap_1().flex_wrap();
                    for den in SHUTTER_PRESETS {
                        let ns = 1_000_000_000 / den;
                        if ns < lo || ns > hi {
                            continue;
                        }
                        let selected = (v.exposure_ns as i64 - ns).abs() < ns / 100;
                        presets = presets.child(
                            Button::new(SharedString::from(format!("shutter-{den}")))
                                .label(format!("1/{den}"))
                                .small()
                                .selected(selected)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.v.exposure_ns = ns as f32;
                                    Self::set_slider(&this.sliders.exposure, ns as f32, window, cx);
                                    this.send_now(vec![(ctl::EXPOSURE_NS, ns)]);
                                    cx.notify();
                                })),
                        );
                    }
                    col = col
                        .child(label_row(t!("img.shutter").to_string(), format_exposure(v.exposure_ns as i64)))
                        .child(Slider::new(s))
                        .child(presets);
                    let fps = caps.fps_with_exposure(v.exposure_ns as i64);
                    if fps < 1e9 / caps.frame_ns as f64 - 0.5 {
                        col = col.child(
                            div()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(t!("img.shutter_slow", fps = format!("{fps:.0}")).to_string()),
                        );
                    }
                }
            } else {
                if let Some(s) = &self.sliders.ev {
                    col = col.child(label_row(t!("img.ev").to_string(), format!("{:+.1}", v.ev))).child(Slider::new(s));
                }
                if caps.ae_lock {
                    col = col.child(Switch::new("ae-lock").label(t!("img.ae_lock").to_string()).checked(v.ae_lock).on_click(
                        cx.listener(|this, on: &bool, _, cx| {
                            this.v.ae_lock = *on;
                            this.send_now(vec![(ctl::AE_LOCK, *on as i64)]);
                            cx.notify();
                        }),
                    ));
                }
            }
            if !caps.manual_sensor {
                col = col.child(note(t!("img.no_manual_sensor")));
            }
        } else {
            col = col.child(note(t!("img.unsupported")));
        }

        // Focus
        col = col.child(heading(t!("img.focus")));
        if caps.autofocus || caps.min_focus_diopter.is_some() {
            let mut row = h_flex().gap_1();
            if caps.autofocus {
                row = row.child(Button::new("af-auto").label(t!("img.auto").to_string()).small().selected(!v.af_manual).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.v.af_manual = false;
                        this.send_now(vec![(ctl::AF_MODE, 0)]);
                        cx.notify();
                    },
                )));
            }
            if caps.min_focus_diopter.is_some() {
                row = row.child(Button::new("af-manual").label(t!("img.manual").to_string()).small().selected(v.af_manual).on_click(
                    cx.listener(|this, _, window, cx| {
                        let d = this.live.focus_diopter.map(|d| d as f32).unwrap_or(this.v.focus_d);
                        this.v.af_manual = true;
                        this.v.focus_d = d;
                        Self::set_slider(&this.sliders.focus, d, window, cx);
                        this.send_now(vec![(ctl::AF_MODE, 1), (ctl::FOCUS_MILLI_DIOPTER, (d * 1000.).round() as i64)]);
                        cx.notify();
                    }),
                ));
            }
            if caps.autofocus {
                row = row.child(Button::new("af-once").label(t!("img.refocus").to_string()).small().ghost().on_click(cx.listener(
                    |this, _, _, cx| {
                        this.v.af_manual = false;
                        this.send_now(vec![(ctl::AF_TRIGGER, 1)]);
                        cx.notify();
                    },
                )));
            }
            col = col.child(row);
            if v.af_manual {
                if let Some(s) = &self.sliders.focus {
                    col = col.child(label_row(t!("img.distance").to_string(), format_focus(v.focus_d as f64))).child(Slider::new(s));
                }
            }
            if caps.min_focus_diopter.is_none() {
                col = col.child(note(t!("img.no_manual_focus")));
            }
        } else {
            col = col.child(note(t!("img.fixed_focus")));
        }

        // White balance
        col = col.child(heading(t!("img.wb")));
        let mut presets = h_flex().gap_1().flex_wrap();
        for (mode, name) in AWB_PRESETS.iter().copied().filter(|p| caps.has_awb(p.0)) {
            presets = presets.child(
                Button::new(SharedString::from(format!("awb-{mode}")))
                    .label(awb_label(name))
                    .small()
                    .selected(v.awb == mode)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.v.awb = mode;
                        this.send_now(vec![(ctl::AWB_MODE, mode)]);
                        cx.notify();
                    })),
            );
        }
        if caps.manual_wb {
            presets = presets.child(
                Button::new("awb-kelvin").label(t!("img.kelvin").to_string()).small().selected(v.awb == AWB_MANUAL).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.v.awb = AWB_MANUAL;
                        let k = this.v.kelvin.round() as i64;
                        this.send_now(vec![(ctl::AWB_MODE, AWB_MANUAL), (ctl::WB_KELVIN, k)]);
                        cx.notify();
                    },
                )),
            );
        }
        col = col.child(presets);
        if v.awb == AWB_MANUAL {
            if let Some(s) = &self.sliders.kelvin {
                col = col.child(label_row(t!("img.color_temp").to_string(), format!("{:.0} K", v.kelvin))).child(Slider::new(s));
            }
        }
        if caps.awb_lock && v.awb != AWB_MANUAL {
            col = col.child(Switch::new("awb-lock").label(t!("img.awb_lock").to_string()).checked(v.awb_lock).on_click(
                cx.listener(|this, on: &bool, _, cx| {
                    this.v.awb_lock = *on;
                    this.send_now(vec![(ctl::AWB_LOCK, *on as i64)]);
                    cx.notify();
                }),
            ));
        }
        if caps.awb_modes == 0 {
            col = col.child(note(t!("img.unsupported")));
        }

        // Zoom
        col = col.child(heading(t!("img.zoom")));
        if let Some(s) = &self.sliders.zoom {
            col = col
                .child(
                    h_flex()
                        .justify_between()
                        .text_sm()
                        .child(format!("{:.1}×", v.zoom))
                        .child(Button::new("zoom-reset").label("1×").small().ghost().on_click(cx.listener(
                            |this, _, window, cx| {
                                this.v.zoom = 1.;
                                Self::set_slider(&this.sliders.zoom, 1., window, cx);
                                this.send_now(vec![(ctl::ZOOM_MILLI, 1000)]);
                                cx.notify();
                            },
                        ))),
                )
                .child(Slider::new(s));
        } else {
            col = col.child(note(t!("img.unsupported")));
        }

        col.child(
            Button::new("controls-reset")
                .label(t!("img.reset").to_string())
                .small()
                .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
        )
    }
}
