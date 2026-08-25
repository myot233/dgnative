//! The window: a gpui view over [`UiState`], wired to the device worker.
//!
//! Layout is a single column of full-width blocks: status bar, the two channel
//! cards, the strength controls, one settings panel whose rows share a label
//! column, and the key hints. Every row's content starts at the same x, and
//! wrapping chips wrap inside their own column instead of under the label.

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    App, Context, Div, ElementId, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Stateful, Styled, Task, Window, actions, div, prelude::*, px, relative, rgb,
};

use dgnative::protocol::v3::builtin;

use crate::device::{Command, Device, Update};
use crate::state::{LIMIT_STEP, Link, Target, UiState};

const BG: u32 = 0x14171d;
const PANEL: u32 = 0x1d222b;
const LINE: u32 = 0x2c333f;
const TEXT: u32 = 0xe6e9ef;
const MUTED: u32 = 0x8b95a7;
const ACCENT: u32 = 0xd97a34;
const DANGER: u32 = 0xc8443c;
const OK: u32 = 0x5aa9a0;

/// Width of the label column shared by every settings row.
const LABEL_W: f32 = 88.;
/// Height shared by buttons and chips so rows line up.
const CONTROL_H: f32 = 32.;

actions!(
    dgnative,
    [Louder, Quieter, ZeroNow, TargetA, TargetB, TargetBoth, Quit]
);

/// Key bindings, installed once at startup.
pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("up", Louder, None),
        KeyBinding::new("=", Louder, None),
        KeyBinding::new("k", Louder, None),
        KeyBinding::new("down", Quieter, None),
        KeyBinding::new("-", Quieter, None),
        KeyBinding::new("j", Quieter, None),
        KeyBinding::new("space", ZeroNow, None),
        KeyBinding::new("0", ZeroNow, None),
        KeyBinding::new("a", TargetA, None),
        KeyBinding::new("b", TargetB, None),
        KeyBinding::new("o", TargetBoth, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

pub struct Dashboard {
    state: UiState,
    device: Device,
    focus: FocusHandle,
    _updates: Task<()>,
}

impl Dashboard {
    pub fn new(
        state: UiState,
        device: Device,
        updates: UnboundedReceiver<Update>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);

        let task = cx.spawn(async move |this, cx| {
            let mut updates = updates;
            while let Some(update) = updates.next().await {
                let applied = this.update(cx, |this: &mut Dashboard, cx| {
                    this.apply(update);
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });

        Dashboard {
            state,
            device,
            focus,
            _updates: task,
        }
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Idle => {
                self.state.link = Link::Idle;
                self.state.battery = None;
                self.state.zero();
                self.state.reported_a = 0;
                self.state.reported_b = 0;
            }
            Update::Scanning => self.state.link = Link::Scanning,
            Update::Found(devices) => {
                self.state.devices = devices;
                if self.state.link == Link::Scanning {
                    self.state.link = Link::Idle;
                }
            }
            Update::Connecting(tag) => self.state.link = Link::Connecting(tag),
            Update::Connected(tag) => {
                self.state.link = Link::Connected(tag);
                self.state.error = None;
            }
            Update::Offline => self.state.simulated = true,
            Update::Failed(message) => self.state.link = Link::Failed(message),
            Update::Reported { a, b } => {
                self.state.reported_a = a;
                self.state.reported_b = b;
            }
            Update::Battery(level) => self.state.battery = Some(level),
        }
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        self.state.link = Link::Scanning;
        self.device.send(Command::Scan);
        cx.notify();
    }

    fn connect(&mut self, id: String, cx: &mut Context<Self>) {
        self.state.link = Link::Connecting(id.clone());
        self.device.send(Command::Connect(id));
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.state.zero();
        self.device.send(Command::Disconnect);
        self.state.link = Link::Idle;
        cx.notify();
    }

    fn push_strength(&self) {
        self.device.send(Command::Strength {
            a: self.state.requested_a,
            b: self.state.requested_b,
        });
    }

    fn adjust(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.state.adjust(delta);
        self.push_strength();
        cx.notify();
    }

    fn zero(&mut self, cx: &mut Context<Self>) {
        self.state.zero();
        self.device.send(Command::Zero);
        cx.notify();
    }

    fn nudge_limit(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.state.nudge_limit(delta);
        self.device.send(Command::Limit(self.state.limit));
        // A lower limit may have pulled the requested strengths down with it.
        self.push_strength();
        cx.notify();
    }

    fn select_wave(&mut self, name: &'static str, cx: &mut Context<Self>) {
        self.state.wave = name;
        self.device.send(Command::Wave(name));
        cx.notify();
    }

    fn set_target(&mut self, target: Target, cx: &mut Context<Self>) {
        self.state.target = target;
        cx.notify();
    }

    fn on_louder(&mut self, _: &Louder, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust(1, cx);
    }

    fn on_quieter(&mut self, _: &Quieter, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust(-1, cx);
    }

    fn on_zero(&mut self, _: &ZeroNow, _: &mut Window, cx: &mut Context<Self>) {
        self.zero(cx);
    }

    fn on_target_a(&mut self, _: &TargetA, _: &mut Window, cx: &mut Context<Self>) {
        self.set_target(Target::A, cx);
    }

    fn on_target_b(&mut self, _: &TargetB, _: &mut Window, cx: &mut Context<Self>) {
        self.set_target(Target::B, cx);
    }

    fn on_target_both(&mut self, _: &TargetBoth, _: &mut Window, cx: &mut Context<Self>) {
        self.set_target(Target::Both, cx);
    }

    /// Status bar: connection dot + text on the left, battery and the
    /// scan/disconnect action on the right.
    fn status_bar(&self, cx: &mut Context<Self>) -> Div {
        let (dot, label) = match &self.state.link {
            Link::Idle if self.state.devices.is_empty() => {
                (MUTED, "no device, scan to find one".to_string())
            }
            Link::Idle => (MUTED, "not connected".to_string()),
            Link::Scanning => (ACCENT, "scanning...".to_string()),
            Link::Connecting(tag) => (ACCENT, format!("connecting to {tag}")),
            Link::Connected(tag) => (OK, format!("connected  {tag}")),
            Link::Failed(message) => (DANGER, message.clone()),
        };

        let action = if self.state.link.is_connected() {
            button("disconnect", "disconnect")
                .px_3()
                .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx)))
        } else if self.state.link.is_busy() {
            button("scanning", "scanning...").px_3()
        } else {
            button("scan", "scan")
                .px_3()
                .on_click(cx.listener(|this, _, _, cx| this.scan(cx)))
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .pb_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(8.)).rounded_full().bg(rgb(dot)))
                    .child(div().text_sm().text_color(rgb(MUTED)).child(label))
                    .children(self.state.simulated.then(|| {
                        div()
                            .px_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(LINE))
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("simulator")
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(match self.state.battery {
                                Some(level) if self.state.link.is_connected() => {
                                    format!("battery {level}%")
                                }
                                _ => "battery --".to_string(),
                            }),
                    )
                    .child(action),
            )
    }

    /// Device picker, shown until a connection is up.
    fn picker(&self, cx: &mut Context<Self>) -> Div {
        let mut list = div().flex().flex_col().gap_2();

        if self.state.devices.is_empty() {
            list = list.child(
                div()
                    .h(px(96.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(match self.state.link {
                        Link::Scanning => "looking for DG-LAB devices...",
                        _ => "no devices yet - press scan",
                    }),
            );
        }

        for (index, device) in self.state.devices.iter().enumerate() {
            let id = device.id.clone();
            let busy = self.state.link.is_busy();
            let connect = button(("connect", index), "connect").px_3().when(
                device.supported && !busy,
                |this| {
                    this.on_click(cx.listener(move |this, _, _, cx| this.connect(id.clone(), cx)))
                },
            );

            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(div().text_color(rgb(TEXT)).child(device.name.clone()))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child(format!("{}   {}", device.kind, device.id)),
                            ),
                    )
                    .child(div().w(px(72.)).text_sm().text_color(rgb(MUTED)).child(
                        match device.rssi {
                            Some(rssi) => format!("{rssi} dBm"),
                            None => "-- dBm".to_string(),
                        },
                    ))
                    .child(if device.supported {
                        connect
                    } else {
                        button(("unsupported", index), "3.0 only")
                            .px_3()
                            .text_color(rgb(MUTED))
                    }),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("devices in range"),
            )
            .child(list)
    }

    /// One channel card: name, big requested value, bar, device readback.
    fn channel(&self, name: &'static str, requested: u8, reported: u8, active: bool) -> Div {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(if active { ACCENT } else { LINE }))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if active { ACCENT } else { MUTED }))
                            .child(format!("channel {name}")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(format!("reported {reported}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_3xl()
                            .text_color(rgb(TEXT))
                            .child(requested.to_string()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(format!("/ {}", self.state.limit)),
                    ),
            )
            .child(
                div().h(px(6.)).w_full().rounded_full().bg(rgb(LINE)).child(
                    div()
                        .h_full()
                        .w(relative(self.state.fraction(requested)))
                        .rounded_full()
                        .bg(rgb(ACCENT)),
                ),
            )
    }

    /// `-` / `+` on the left, STOP filling the rest. One height for all three.
    fn strength_controls(&self, cx: &mut Context<Self>) -> Div {
        const H: f32 = 44.;
        div()
            .flex()
            .gap_2()
            .child(
                button("quieter", "-")
                    .w(px(72.))
                    .h(px(H))
                    .text_xl()
                    .on_click(cx.listener(|this, _, _, cx| this.adjust(-1, cx))),
            )
            .child(
                button("louder", "+")
                    .w(px(72.))
                    .h(px(H))
                    .text_xl()
                    .on_click(cx.listener(|this, _, _, cx| this.adjust(1, cx))),
            )
            .child(
                div()
                    .id("zero")
                    .flex_1()
                    .h(px(H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded_md()
                    .bg(rgb(DANGER))
                    .text_color(rgb(0xffffff))
                    .hover(|this| this.bg(rgb(0xd85a52)))
                    .child("STOP")
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xf3d3d1))
                            .child("both channels to zero"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.zero(cx))),
            )
    }

    fn target_row(&self, cx: &mut Context<Self>) -> Div {
        let mut chips = row_content();
        for (index, target) in [Target::A, Target::B, Target::Both].into_iter().enumerate() {
            let selected = self.state.target == target;
            chips = chips.child(
                chip(("target", index), target.label(), selected)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_target(target, cx))),
            );
        }
        labeled_row("adjusts", chips)
    }

    fn wave_row(&self, cx: &mut Context<Self>) -> Div {
        let mut chips = row_content();
        for (index, wave) in builtin::ALL.iter().enumerate() {
            let selected = self.state.wave == wave.name;
            let name = wave.name;
            chips = chips.child(
                chip(("wave", index), wave.label, selected)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_wave(name, cx))),
            );
        }
        labeled_row("waveform", chips)
    }

    fn limit_row(&self, cx: &mut Context<Self>) -> Div {
        let step = i32::from(LIMIT_STEP);
        let content = row_content()
            .child(
                button("limit-down", "-")
                    .w(px(40.))
                    .on_click(cx.listener(move |this, _, _, cx| this.nudge_limit(-step, cx))),
            )
            .child(
                div()
                    .w(px(40.))
                    .text_center()
                    .text_color(rgb(TEXT))
                    .child(self.state.limit.to_string()),
            )
            .child(
                button("limit-up", "+")
                    .w(px(40.))
                    .on_click(cx.listener(move |this, _, _, cx| this.nudge_limit(step, cx))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(if self.state.limit_is_high() {
                        DANGER
                    } else {
                        MUTED
                    }))
                    .child(if self.state.limit_is_high() {
                        "high, and the device keeps it after power off"
                    } else {
                        "written with BF, kept after power off"
                    }),
            );
        labeled_row("soft limit", content)
    }

    /// The live control surface, shown once a device is connected.
    fn controls(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let target = self.state.target;
        vec![
            div()
                .flex()
                .gap_3()
                .child(self.channel(
                    "A",
                    self.state.requested_a,
                    self.state.reported_a,
                    target.uses_a(),
                ))
                .child(self.channel(
                    "B",
                    self.state.requested_b,
                    self.state.reported_b,
                    target.uses_b(),
                )),
            self.strength_controls(cx),
            div()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_lg()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(LINE))
                .child(self.target_row(cx))
                .child(self.wave_row(cx))
                .child(self.limit_row(cx)),
        ]
    }
}

impl Focusable for Dashboard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Dashboard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("dashboard")
            .track_focus(&self.focus)
            .key_context("Dashboard")
            .on_action(cx.listener(Self::on_louder))
            .on_action(cx.listener(Self::on_quieter))
            .on_action(cx.listener(Self::on_zero))
            .on_action(cx.listener(Self::on_target_a))
            .on_action(cx.listener(Self::on_target_b))
            .on_action(cx.listener(Self::on_target_both))
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.status_bar(cx))
            .children(if self.state.link.is_connected() {
                self.controls(cx)
            } else {
                vec![self.picker(cx)]
            })
            // Absorbs any window height the content does not use, so the hint
            // line sits on the bottom edge instead of floating mid-window.
            .child(div().flex_1().min_h(px(8.)))
            .child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(if self.state.link.is_connected() {
                        "up / down adjust      space zeroes both      a  b  o  retarget"
                    } else {
                        "scan, then connect a pulse host 3.0 to get the controls"
                    }),
            )
    }
}

/// A settings row: fixed label column, then everything else in one column that
/// wraps inside itself.
fn labeled_row(label: &'static str, content: Div) -> Div {
    div()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .w(px(LABEL_W))
                .flex_none()
                .h(px(CONTROL_H))
                .flex()
                .items_center()
                .text_sm()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(content)
}

fn row_content() -> Div {
    div().flex_1().flex().flex_wrap().items_center().gap_2()
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(CONTROL_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(PANEL))
        .text_color(rgb(TEXT))
        .hover(|this| this.border_color(rgb(ACCENT)).text_color(rgb(ACCENT)))
        .active(|this| this.bg(rgb(ACCENT)).text_color(rgb(BG)))
        .child(label.into())
}

fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(CONTROL_H))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(rgb(if selected { ACCENT } else { LINE }))
        .bg(rgb(if selected { ACCENT } else { PANEL }))
        .text_color(rgb(if selected { BG } else { MUTED }))
        .text_sm()
        .hover(|this| this.border_color(rgb(ACCENT)))
        .child(label.into())
}
