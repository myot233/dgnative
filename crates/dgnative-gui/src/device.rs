//! Device worker: owns a tokio runtime on its own thread and runs scanning plus
//! the 100ms B0 control loop, so the UI thread never blocks on BLE.
//!
//! The UI sends [`Command`]s and receives [`Update`]s; nothing else crosses the
//! boundary. The worker is a small state machine: idle -> scanning -> connected
//! -> idle.

use std::thread;
use std::time::Duration;

use dgnative::ble::{Coyote3, Coyote3Event, DeviceKind, Discovered, default_adapter, scan};
use dgnative::protocol::v3::{B0, B0_INTERVAL_MS, Bf, Notification, StrengthQueue, builtin};
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender as UpdateSender;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// One entry of the scan result, as shown in the picker.
#[derive(Debug, Clone)]
pub struct Found {
    /// Full peripheral id; what [`Command::Connect`] is matched against.
    pub id: String,
    /// First 8 characters of the id, which is all the screen has room for.
    pub tag: String,
    pub name: String,
    pub kind: &'static str,
    pub rssi: Option<i16>,
    /// Only Coyote 3.0 hosts can be driven by this app.
    pub supported: bool,
}

/// UI -> device.
#[derive(Debug)]
pub enum Command {
    /// Look for devices; answered with [`Update::Found`].
    Scan,
    /// Connect to a device from the last scan.
    Connect(String),
    /// Drop the connection and go back to idle.
    Disconnect,
    /// Requested strength for both channels.
    Strength { a: u8, b: u8 },
    /// Switch the waveform being played.
    Wave(&'static str),
    /// Rewrite the soft limit (BF).
    Limit(u8),
    /// Emergency stop: zero both channels on the next cycle.
    Zero,
}

/// Device -> UI.
#[derive(Debug)]
pub enum Update {
    Idle,
    Scanning,
    Found(Vec<Found>),
    Connecting(String),
    Connected(String),
    Offline,
    Failed(String),
    /// Strength confirmed by the device (B1).
    Reported {
        a: u8,
        b: u8,
    },
    Battery(u8),
}

pub struct Options {
    pub offline: bool,
    /// Connect to the first device whose id starts with this, without waiting
    /// for the user to pick one.
    pub device: Option<String>,
    pub timeout: Duration,
    pub limit: u8,
}

/// Handle held by the UI. Dropping it stops the worker.
pub struct Device {
    tx: UnboundedSender<Command>,
}

impl Device {
    pub fn send(&self, command: Command) {
        // The worker only goes away when the app is shutting down.
        let _ = self.tx.send(command);
    }
}

/// Start the worker thread.
pub fn spawn(options: Options, updates: UpdateSender<Update>) -> Device {
    let (tx, rx) = unbounded_channel();
    thread::Builder::new()
        .name("dgnative-device".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = updates.unbounded_send(Update::Failed(err.to_string()));
                    return;
                }
            };
            runtime.block_on(run(options, rx, updates));
        })
        .expect("spawn device thread");
    Device { tx }
}

async fn run(
    options: Options,
    mut commands: UnboundedReceiver<Command>,
    updates: UpdateSender<Update>,
) {
    if options.offline {
        let _ = updates.unbounded_send(Update::Offline);
        let _ = updates.unbounded_send(Update::Battery(88));
        simulate(&mut commands, &updates).await;
        return;
    }

    let mut known: Vec<Discovered> = Vec::new();
    let mut limit = options.limit;
    let mut autoconnect = options.device.clone();

    // A device filter on the command line means "connect without asking".
    let mut pending = if autoconnect.is_some() {
        Some(Command::Scan)
    } else {
        let _ = updates.unbounded_send(Update::Idle);
        None
    };

    loop {
        let command = match pending.take() {
            Some(command) => command,
            None => match commands.recv().await {
                Some(command) => command,
                None => return,
            },
        };

        match command {
            Command::Scan => {
                log(format_args!("scanning for {:?}", options.timeout));
                let _ = updates.unbounded_send(Update::Scanning);
                match discover(options.timeout).await {
                    Ok(found) => {
                        log(format_args!("scan found {} device(s)", found.len()));
                        for device in &found {
                            log(format_args!(
                                "  {} {} rssi={:?} supported={}",
                                device.id,
                                device.local_name,
                                device.rssi,
                                is_supported(device)
                            ));
                        }
                        known = found;
                        let _ = updates
                            .unbounded_send(Update::Found(known.iter().map(describe).collect()));
                        if let Some(prefix) = autoconnect.take() {
                            match known
                                .iter()
                                .find(|d| d.id.starts_with(&prefix) && is_supported(d))
                            {
                                Some(hit) => pending = Some(Command::Connect(hit.id.clone())),
                                None => log(format_args!("no device matches -D {prefix}")),
                            }
                        }
                    }
                    Err(err) => {
                        log(format_args!("scan failed: {err}"));
                        let _ = updates.unbounded_send(Update::Failed(err.to_string()));
                        let _ = updates.unbounded_send(Update::Idle);
                    }
                }
            }
            Command::Connect(id) => {
                let Some(target) = known.iter().find(|d| d.id == id).cloned() else {
                    log(format_args!("{id} is no longer in the scan results"));
                    let _ = updates.unbounded_send(Update::Failed("device is gone".into()));
                    continue;
                };
                log(format_args!("connecting to {id}"));
                let _ = updates.unbounded_send(Update::Connecting(short(&target.id)));
                match Coyote3::connect_peripheral(target.peripheral.clone()).await {
                    Ok(coyote) => {
                        session(coyote, limit, &mut commands, &updates, &mut limit).await;
                        log(format_args!("session with {id} ended"));
                        let _ = updates.unbounded_send(Update::Idle);
                    }
                    Err(err) => {
                        log(format_args!("connect failed: {err}"));
                        let _ = updates.unbounded_send(Update::Failed(err.to_string()));
                        let _ = updates.unbounded_send(Update::Idle);
                    }
                }
            }
            // Only meaningful while connected; ignore otherwise, but keep the
            // soft limit so the next connection writes the value on screen.
            Command::Limit(value) => limit = value,
            Command::Disconnect | Command::Strength { .. } | Command::Wave(_) | Command::Zero => {}
        }
    }
}

/// Drive one connection until it is dropped or fails.
async fn session(
    coyote: Coyote3,
    limit: u8,
    commands: &mut UnboundedReceiver<Command>,
    updates: &UpdateSender<Update>,
    limit_out: &mut u8,
) {
    if let Err(err) = coyote.set_config(&Bf::with_limits(limit, limit)).await {
        log(format_args!("BF write failed: {err}"));
        let _ = updates.unbounded_send(Update::Failed(err.to_string()));
        return;
    }
    log(format_args!("connected, soft limit {limit} written"));

    let _ = updates.unbounded_send(Update::Connected(short(&coyote.id())));
    if let Ok(battery) = coyote.battery_level().await {
        let _ = updates.unbounded_send(Update::Battery(battery));
    }

    let mut events = match coyote.events().await {
        Ok(events) => events,
        Err(err) => {
            let _ = updates.unbounded_send(Update::Failed(err.to_string()));
            return;
        }
    };

    let mut queue = StrengthQueue::new();
    let mut wave = builtin::ALL[0];
    let mut phase = 0usize;
    let mut requested = (0u8, 0u8);
    let mut ticker = tokio::time::interval(Duration::from_millis(B0_INTERVAL_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let (sequence, action_a, action_b) = queue.tick();
                let pulses = wave.pulses_at(phase);
                phase = phase.wrapping_add(1);
                // Strength 0 already silences a channel, so both carry the waveform.
                let cmd = B0 {
                    sequence,
                    action_a,
                    action_b,
                    pulses_a: pulses,
                    pulses_b: pulses,
                };
                if let Err(err) = coyote.send(&cmd).await {
                    let _ = updates.unbounded_send(Update::Failed(err.to_string()));
                    return;
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    let _ = coyote.stop().await;
                    let _ = coyote.disconnect().await;
                    return;
                };
                match command {
                    Command::Strength { a, b } => {
                        if a != requested.0 {
                            queue.set_a(a);
                        }
                        if b != requested.1 {
                            queue.set_b(b);
                        }
                        requested = (a, b);
                    }
                    Command::Zero => {
                        queue.zero_now();
                        requested = (0, 0);
                    }
                    Command::Wave(name) => {
                        if let Some(found) = builtin::by_name(name) {
                            wave = *found;
                            phase = 0;
                        }
                    }
                    Command::Limit(value) => {
                        *limit_out = value;
                        if let Err(err) = coyote.set_config(&Bf::with_limits(value, value)).await {
                            let _ = updates.unbounded_send(Update::Failed(err.to_string()));
                        }
                    }
                    Command::Disconnect => {
                        // Leave the device silent behind us.
                        let _ = coyote.stop().await;
                        let _ = coyote.disconnect().await;
                        return;
                    }
                    // A scan needs the connection gone first.
                    Command::Scan | Command::Connect(_) => {}
                }
            }
            Some(event) = events.next() => {
                match event {
                    Coyote3Event::Message(Notification::Strength(b1)) => {
                        queue.on_b1(&b1);
                        let _ = updates.unbounded_send(Update::Reported {
                            a: b1.strength_a,
                            b: b1.strength_b,
                        });
                    }
                    Coyote3Event::Battery(level) => {
                        let _ = updates.unbounded_send(Update::Battery(level));
                    }
                    Coyote3Event::Message(Notification::Unknown(_)) => {}
                }
            }
        }
    }
}

async fn discover(timeout: Duration) -> anyhow::Result<Vec<Discovered>> {
    let adapter = default_adapter().await?;
    Ok(scan(&adapter, timeout).await?)
}

fn describe(device: &Discovered) -> Found {
    Found {
        id: device.id.clone(),
        tag: short(&device.id),
        name: device.local_name.clone(),
        kind: device.kind.label(),
        rssi: device.rssi,
        supported: is_supported(device),
    }
}

fn is_supported(device: &Discovered) -> bool {
    matches!(device.kind, DeviceKind::Coyote3)
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The window has no console of its own, so the BLE side reports to stderr.
/// Run the binary from a terminal to see why a scan or a connection failed.
fn log(args: std::fmt::Arguments) {
    eprintln!("[device] {args}");
}

/// Offline stand-in: fake scan results plus an echo of the requested strengths,
/// so the whole flow can be driven with no hardware attached.
async fn simulate(commands: &mut UnboundedReceiver<Command>, updates: &UpdateSender<Update>) {
    let catalogue = [
        Found {
            id: "SIM-47L1".into(),
            tag: "SIM-47L1".into(),
            name: "47L121000".into(),
            kind: "Pulse host 3.0",
            rssi: Some(-46),
            supported: true,
        },
        Found {
            id: "SIM-D-LA".into(),
            tag: "SIM-D-LA".into(),
            name: "D-LAB ESTIM01".into(),
            kind: "Pulse host 2.0",
            rssi: Some(-77),
            supported: false,
        },
    ];

    while let Some(command) = commands.recv().await {
        match command {
            Command::Scan => {
                let _ = updates.unbounded_send(Update::Scanning);
                tokio::time::sleep(Duration::from_millis(700)).await;
                let _ = updates.unbounded_send(Update::Found(catalogue.to_vec()));
            }
            Command::Connect(id) => {
                let _ = updates.unbounded_send(Update::Connecting(id.clone()));
                tokio::time::sleep(Duration::from_millis(400)).await;
                let _ = updates.unbounded_send(Update::Connected(id));
                let _ = updates.unbounded_send(Update::Battery(88));
            }
            Command::Disconnect => {
                let _ = updates.unbounded_send(Update::Reported { a: 0, b: 0 });
                let _ = updates.unbounded_send(Update::Idle);
            }
            Command::Strength { a, b } => {
                let _ = updates.unbounded_send(Update::Reported { a, b });
            }
            Command::Zero => {
                let _ = updates.unbounded_send(Update::Reported { a: 0, b: 0 });
            }
            Command::Wave(_) | Command::Limit(_) => {}
        }
    }
}
