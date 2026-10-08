//! `dgnative` command-line tool: control the DG-LAB Coyote pulse host 3.0.

mod gacha;

use std::io::Write;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use ipnet::IpNet;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dgnative::ble::{Coyote3, Coyote3Event, DeviceKind, default_adapter, scan};
use dgnative::protocol::v3::{
    B0, B0_INTERVAL_MS, B1, Bf, MAX_STRENGTH, Notification, Pulse, StrengthQueue, builtin,
};
use futures::StreamExt;

/// Silence frame that makes the host discard all waveform data for this channel
/// (strength 101 is out of range).
const SILENT: [Pulse; 4] = [Pulse {
    frequency: 10,
    intensity: 101,
}; 4];

#[derive(Parser)]
#[command(
    name = "dgnative",
    version,
    about = "Command-line control tool for the DG-LAB Coyote pulse host 3.0",
    long_about = "Command-line control tool for the DG-LAB Coyote pulse host 3.0 (47L121000).\n\
                  The protocol implementation follows the official DG-LAB-OPENSOURCE docs."
)]
struct Cli {
    /// Timeout in seconds for scanning / connecting to a device
    #[arg(long, short = 't', default_value_t = 15, global = true)]
    timeout: u64,

    /// Id of the device to connect to (the "id" shown by `dgnative scan`; a prefix is enough).
    /// When omitted, connects the one with the strongest signal.
    #[arg(long, short = 'D', global = true)]
    device: Option<String>,

    /// Run on every Coyote 3.0 found by the scan (instead of picking just one)
    #[arg(long, short = 'A', global = true, conflicts_with = "device")]
    all: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan for nearby DG-LAB devices
    Scan,

    /// Connect to the device and show its status (battery etc.)
    Info,

    /// Write the channel soft strength limits and balance parameters (BF command)
    Config {
        /// Channel A soft strength limit (0-200)
        #[arg(long)]
        limit_a: u8,
        /// Channel B soft strength limit (0-200)
        #[arg(long)]
        limit_b: u8,
        /// Waveform frequency balance parameter (0-255; higher means stronger low-frequency impact)
        #[arg(long, default_value_t = Bf::DEFAULT_FREQ_BALANCE)]
        freq_balance: u8,
        /// Waveform strength balance parameter (0-255; higher means stronger low-frequency stimulation)
        #[arg(long, default_value_t = Bf::DEFAULT_INTENSITY_BALANCE)]
        intensity_balance: u8,
    },

    /// List the built-in waveforms
    Waves,

    /// Play a waveform; adjust the strength from the keyboard while it runs
    Play {
        /// Waveform name (see `dgnative waves`)
        #[arg(default_value = "breathing")]
        wave: String,
        /// Output channel
        #[arg(long, short, value_enum, default_value_t = Channel::A)]
        channel: Channel,
        /// Starting strength, 0 by default; adjust with ↑/↓ while running
        #[arg(long, short, default_value_t = 0)]
        strength: u8,
        /// Soft strength limit, written to the device with a BF command before starting
        #[arg(long, short = 'L', default_value_t = 20)]
        limit: u8,
        /// Run time in seconds; omit to keep going until stopped manually
        #[arg(long, short)]
        duration: Option<u64>,
    },

    /// Serve the web gacha wheel and output the strength and duration of the prize it lands on
    Gacha {
        /// Port the web page listens on
        #[arg(long, short, default_value_t = 8777)]
        port: u16,
        /// Listen address. Defaults to loopback only; use 0.0.0.0 to let other machines
        /// connect, which then requires --allow as well
        #[arg(long, short = 'b', default_value = "127.0.0.1")]
        bind: IpAddr,
        /// Subnet or IP allowed to connect, repeatable. For example --allow 192.168.1.0/24
        #[arg(long, value_parser = gacha::parse_allow)]
        allow: Vec<IpNet>,
        /// Soft strength limit; prize strength is a percentage of it and output never exceeds it
        #[arg(long, short = 'L', default_value_t = 20)]
        limit: u8,
        /// Output channel
        #[arg(long, short, value_enum, default_value_t = Channel::Both)]
        channel: Channel,
        /// Do not connect any device, just serve the web page (for tuning the prize pool / previewing the wheel)
        #[arg(long)]
        offline: bool,
        /// Chance (%) to fake-stop on the initial prize, then move to the nearest higher segment
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u8).range(0..=100))]
        fakeout_chance: u8,
    },

    /// Watch device events (strength changes, battery)
    Monitor,

    /// Zero out both channels immediately
    Stop,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Channel {
    /// Channel A only
    A,
    /// Channel B only
    B,
    /// Same waveform on both channel A and channel B
    Both,
}

impl Channel {
    fn uses_a(self) -> bool {
        matches!(self, Channel::A | Channel::Both)
    }

    fn uses_b(self) -> bool {
        matches!(self, Channel::B | Channel::Both)
    }

    fn label(self) -> &'static str {
        match self {
            Channel::A => "A",
            Channel::B => "B",
            Channel::Both => "A+B",
        }
    }
}

/// Global connection target: scan timeout plus how the device is selected.
struct Target {
    timeout: Duration,
    device: Option<String>,
    all: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let target = Target {
        timeout: Duration::from_secs(cli.timeout),
        device: cli.device,
        all: cli.all,
    };

    match cli.command {
        Command::Scan => cmd_scan(&target).await,
        Command::Info => cmd_info(&target).await,
        Command::Config {
            limit_a,
            limit_b,
            freq_balance,
            intensity_balance,
        } => {
            cmd_config(
                &target,
                Bf {
                    limit_a,
                    limit_b,
                    freq_balance_a: freq_balance,
                    freq_balance_b: freq_balance,
                    intensity_balance_a: intensity_balance,
                    intensity_balance_b: intensity_balance,
                },
            )
            .await
        }
        Command::Waves => {
            cmd_waves();
            Ok(())
        }
        Command::Play {
            wave,
            channel,
            strength,
            limit,
            duration,
        } => {
            cmd_play(
                &target,
                &wave,
                channel,
                strength,
                limit,
                duration.map(Duration::from_secs),
            )
            .await
        }
        Command::Gacha {
            port,
            bind,
            allow,
            limit,
            channel,
            offline,
            fakeout_chance,
        } => {
            if limit > MAX_STRENGTH {
                bail!("--limit = {limit} is out of range 0-{MAX_STRENGTH}");
            }
            gacha::serve(
                &target,
                bind,
                port,
                allow,
                gacha::Options {
                    limit,
                    channel,
                    offline,
                    fakeout_chance,
                },
            )
            .await
        }
        Command::Monitor => cmd_monitor(&target).await,
        Command::Stop => cmd_stop(&target).await,
    }
}

async fn cmd_scan(target: &Target) -> Result<()> {
    let adapter = default_adapter().await?;
    println!("Scanning ({}s)...", target.timeout.as_secs());
    let devices = scan(&adapter, target.timeout).await?;

    if devices.is_empty() {
        println!(
            "No DG-LAB device found. \
             Make sure the host is powered on and not held by another app."
        );
        return Ok(());
    }

    // Labels are padded to a fixed width so the values below them line up
    println!("\nFound {} device(s):\n", devices.len());
    for (i, d) in devices.iter().enumerate() {
        let rssi = d
            .rssi
            .map(|v| format!("{v} dBm"))
            .unwrap_or_else(|| "unknown".into());
        println!("  [{}] {} - {}", i + 1, d.kind.label(), d.local_name);
        println!("      id      {}", d.id);
        println!("      signal  {rssi}\n");
    }
    Ok(())
}

/// A connected device plus the short tag used in log output.
struct Device {
    /// First 8 chars of the id: enough to tell devices apart without filling the screen.
    tag: String,
    coyote: Coyote3,
}

/// Scan for and connect to the target devices.
///
/// With `--all`, connects to every Coyote 3.0 found; otherwise matches the `--device`
/// prefix, and with neither given picks the strongest signal -- several hosts may be
/// nearby at once, so connecting to "whichever was seen first" is nondeterministic.
async fn connect_all(target: &Target) -> Result<Vec<Device>> {
    let adapter = default_adapter().await?;
    println!("Scanning for a Coyote 3.0 (47L121000)...");
    let found = scan(&adapter, target.timeout).await?;

    let want = target.device.as_deref();
    let mut candidates: Vec<_> = found
        .into_iter()
        .filter(|d| d.kind == DeviceKind::Coyote3)
        .filter(|d| want.is_none_or(|want| d.id.starts_with(want)))
        .collect();

    if candidates.is_empty() {
        match want {
            Some(want) => bail!(
                "No Coyote 3.0 with an id starting with {want:?}. \
                 Run `dgnative scan` to see the devices in range."
            ),
            None => {
                bail!(
                    "No Coyote 3.0 found. Make sure the host is powered on, in range, \
                     and not held by the official app or another program."
                )
            }
        }
    }

    if !target.all {
        // scan() already sorts by descending signal strength
        let extra = candidates.len() - 1;
        candidates.truncate(1);
        if extra > 0 {
            println!(
                "Note: {} hosts are in range, so the strongest signal was picked. \
                 Use -D <id prefix> to pick a device, or -A to control all of them.",
                extra + 1
            );
        }
    }

    let mut devices = Vec::new();
    let mut failures = Vec::new();
    for found in candidates {
        let tag = found.id.chars().take(8).collect::<String>();
        match Coyote3::connect_peripheral(found.peripheral).await {
            Ok(coyote) => {
                println!("Connected {}", found.id);
                devices.push(Device { tag, coyote });
            }
            // Showing up in a scan does not mean it can be connected (signal too weak,
            // already held by another program). With several devices, one failure
            // must not abandon the rest.
            Err(e) => {
                println!("Failed to connect {}: {e}", found.id);
                failures.push(found.id);
            }
        }
    }

    if devices.is_empty() {
        bail!(
            "Scanned {} device(s) but connected to none.",
            failures.len()
        );
    }
    if !failures.is_empty() {
        println!(
            "({} failed to connect, continuing with the {} that did)",
            failures.len(),
            devices.len()
        );
    }
    Ok(devices)
}

async fn cmd_info(target: &Target) -> Result<()> {
    let devices = connect_all(target).await?;
    println!();
    for d in &devices {
        match d.coyote.battery_level().await {
            Ok(level) => println!("  {} battery {level}%", d.tag),
            Err(e) => println!("  {} battery read failed ({e})", d.tag),
        }
    }
    println!(
        "\nNote: the soft strength limit must be written with `dgnative config`, \
         and has to be set again after every reconnect."
    );
    for d in &devices {
        d.coyote.disconnect().await?;
    }
    Ok(())
}

async fn cmd_config(target: &Target, config: Bf) -> Result<()> {
    for (name, v) in [("limit-a", config.limit_a), ("limit-b", config.limit_b)] {
        if v > MAX_STRENGTH {
            bail!("--{name} = {v} is out of range 0-{MAX_STRENGTH}");
        }
    }

    let devices = connect_all(target).await?;
    println!();
    for d in &devices {
        d.coyote.set_config(&config).await?;
        println!(
            "  {} written: soft limit A={} B={}, freq balance={}, strength balance={}",
            d.tag,
            config.limit_a,
            config.limit_b,
            config.freq_balance_a,
            config.intensity_balance_a
        );
    }
    println!(
        "\n(BF has no reply, so the device never reports whether it took effect; \
         the parameters persist across power cycles, but should be rewritten after every reconnect)"
    );
    if config.limit_a == MAX_STRENGTH || config.limit_b == MAX_STRENGTH {
        println!(
            "⚠️  The soft limit is now the maximum 200, which effectively removes the strength \
             protection, and the setting persists across power cycles."
        );
    }
    for d in &devices {
        d.coyote.disconnect().await?;
    }
    Ok(())
}

fn cmd_waves() {
    println!("Built-in waveforms:\n");
    for wave in builtin::ALL {
        let frames = wave.frames.len();
        println!(
            "  {:<11} {:<11} {frames:>2} frame{} / {:>4.1}s cycle   {}",
            wave.name,
            wave.label,
            if frames == 1 { " " } else { "s" },
            wave.cycle_ms() as f64 / 1000.0,
            if wave.official { "official" } else { "custom" }
        );
    }
    println!(
        "\n\"official\" is the original waveform data from the official app, \
         \"custom\" ones were made up by this tool."
    );
}

async fn cmd_stop(target: &Target) -> Result<()> {
    let devices = connect_all(target).await?;
    println!();
    for d in &devices {
        d.coyote.stop().await?;
        println!("  {} zeroed out", d.tag);
    }
    for d in &devices {
        d.coyote.disconnect().await?;
    }
    Ok(())
}

async fn cmd_monitor(target: &Target) -> Result<()> {
    let mut devices = connect_all(target).await?;
    let coyote = devices.remove(0).coyote;
    for d in &devices {
        d.coyote.disconnect().await?;
    }
    let mut events = coyote.events().await?;
    println!("Watching device events, Ctrl-C to exit.\n");

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Coyote3Event::Message(Notification::Strength(b1))) => println!(
                    "Strength change  A={:<4} B={:<4} (seq={})",
                    b1.strength_a, b1.strength_b, b1.sequence
                ),
                Some(Coyote3Event::Message(Notification::Unknown(data))) => {
                    println!("Unknown message  {data:02X?}")
                }
                Some(Coyote3Event::Battery(level)) => println!("Battery          {level}%"),
                None => {
                    println!("\nThe connection dropped.");
                    return Ok(());
                }
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    println!("\nStopped watching.");
    coyote.disconnect().await?;
    Ok(())
}

/// Enter terminal raw mode, restoring it automatically when the value goes out of scope.
///
/// Non-interactive environments (pipes, CI, unattended `--duration` runs) have no TTY;
/// there keyboard control is skipped instead of failing outright.
struct RawMode {
    active: bool,
}

impl RawMode {
    fn enter() -> Self {
        RawMode {
            active: enable_raw_mode().is_ok(),
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
        }
    }
}

/// A newline in raw mode needs an explicit carriage return.
fn line(text: &str) {
    print!("{text}\r\n");
    let _ = std::io::stdout().flush();
}

async fn cmd_play(
    target: &Target,
    wave_name: &str,
    channel: Channel,
    strength: u8,
    limit: u8,
    duration: Option<Duration>,
) -> Result<()> {
    let wave = builtin::by_name(wave_name).with_context(|| {
        format!("unknown waveform {wave_name:?}, run `dgnative waves` to list the available ones")
    })?;
    if limit > MAX_STRENGTH {
        bail!("--limit = {limit} is out of range 0-{MAX_STRENGTH}");
    }
    if strength > limit {
        bail!("starting strength {strength} exceeds the soft limit {limit}");
    }

    let devices = connect_all(target).await?;

    // One strength state machine per device; the B1 report carries the device index for matching
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(usize, B1)>();
    let mut states = Vec::new();
    let mut forwarders = Vec::new();

    println!();
    for (index, device) in devices.iter().enumerate() {
        // BF must be rewritten after a reconnect, otherwise the previous soft limit may linger
        device
            .coyote
            .set_config(&Bf::with_limits(limit, limit))
            .await?;
        println!("  {} soft limit A={limit} B={limit}", device.tag);

        let mut events = device.coyote.events().await?;
        let tx = tx.clone();
        forwarders.push(tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if let Coyote3Event::Message(Notification::Strength(b1)) = event {
                    let _ = tx.send((index, b1));
                }
            }
        }));

        let mut queue = StrengthQueue::new();
        if channel.uses_a() {
            queue.set_a(strength);
        }
        if channel.uses_b() {
            queue.set_b(strength);
        }
        states.push(DeviceState {
            queue,
            strength_a: 0,
            strength_b: 0,
        });
    }
    drop(tx);

    if limit == MAX_STRENGTH {
        println!(
            "\n⚠️  The soft limit is the maximum 200, so strength protection is off, \
             and the setting persists across power cycles."
        );
    }
    println!(
        "\nPlaying \"{}\" on {} device(s), channel {}",
        wave.label,
        devices.len(),
        channel.label()
    );
    println!("  ↑/↓ or +/-  adjust strength      space  zero out now");
    println!("  a / b / o   switch target        q / Ctrl-C  zero out and exit\n");

    let result = play_loop(
        &devices,
        &mut states,
        wave,
        channel,
        limit,
        duration,
        &mut rx,
    )
    .await;

    // Zero out every device first, whether we are exiting normally or after an error
    for f in &forwarders {
        f.abort();
    }
    let mut failures = Vec::new();
    for device in &devices {
        if let Err(e) = device.coyote.stop().await {
            failures.push(format!("{}: {e}", device.tag));
        }
        let _ = device.coyote.disconnect().await;
    }

    result?;
    if !failures.is_empty() {
        bail!(
            "The following devices failed to zero out, power them off manually to be sure:\n  {}",
            failures.join("\n  ")
        );
    }
    println!("All devices zeroed out and disconnected.");
    Ok(())
}

/// State of a single device inside the play loop.
struct DeviceState {
    queue: StrengthQueue,
    strength_a: u8,
    strength_b: u8,
}

/// Main loop of `play`: write one B0 per device every 100ms while responding to the
/// keyboard and to device reports.
async fn play_loop(
    devices: &[Device],
    states: &mut [DeviceState],
    wave: &builtin::Builtin,
    channel: Channel,
    limit: u8,
    duration: Option<Duration>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<(usize, B1)>,
) -> Result<()> {
    let raw = RawMode::enter();
    if !raw.active {
        line(
            "(non-interactive terminal, keyboard control unavailable; \
             will exit once --duration has elapsed)",
        );
        if duration.is_none() {
            line("(no --duration set, so only Ctrl-C can end it)");
        }
    }
    // In a non-interactive environment crossterm has no reader source and building an
    // EventStream panics; swap in a stream that never yields so the keyboard branch never fires.
    let mut keys: futures::stream::BoxStream<'static, std::io::Result<Event>> = if raw.active {
        EventStream::new().boxed()
    } else {
        futures::stream::pending().boxed()
    };
    let mut ticker = tokio::time::interval(Duration::from_millis(B0_INTERVAL_MS));
    let started = Instant::now();

    // Channel the keyboard adjusts, following --channel by default
    let mut target = channel;
    let mut frame = 0usize;

    loop {
        if duration.is_some_and(|d| started.elapsed() >= d) {
            line("");
            line("Reached the configured duration.");
            return Ok(());
        }

        tokio::select! {
            _ = ticker.tick() => {
                let pulses = wave.pulses_at(frame);
                frame += 1;

                for (device, state) in devices.iter().zip(states.iter_mut()) {
                    let (sequence, action_a, action_b) = state.queue.tick();
                    device.coyote.send(&B0 {
                        sequence,
                        action_a,
                        action_b,
                        pulses_a: if channel.uses_a() { pulses } else { SILENT },
                        pulses_b: if channel.uses_b() { pulses } else { SILENT },
                    }).await?;
                }

                let readout = devices
                    .iter()
                    .zip(states.iter())
                    .map(|(d, s)| format!("{} A={:<3} B={:<3}", d.tag, s.strength_a, s.strength_b))
                    .collect::<Vec<_>>()
                    .join(" | ");
                let elapsed = started.elapsed();

                if raw.active {
                    // Interactive terminal: refresh the single status line in place
                    print!(
                        "\r\x1b[2K  {readout} | limit {limit} | target {} | {:>5.1}s",
                        target.label(),
                        elapsed.as_secs_f64()
                    );
                    let _ = std::io::stdout().flush();
                } else if frame.is_multiple_of(20) {
                    // Non-interactive: in-place refresh smears into one line, so print every 2s
                    println!("  [{:>5.1}s] {readout}", elapsed.as_secs_f64());
                }
            }

            Some((index, b1)) = rx.recv() => {
                if let Some(state) = states.get_mut(index) {
                    state.queue.on_b1(&b1);
                    state.strength_a = b1.strength_a;
                    state.strength_b = b1.strength_b;
                }
            }

            Some(Ok(event)) = keys.next() => {
                let Event::Key(key) = event else { continue };
                // On Windows press and release each fire an event, so only take press
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match handle_key(key, target, states) {
                    KeyOutcome::Continue => {}
                    KeyOutcome::Retarget(next) => target = next,
                    KeyOutcome::Quit => {
                        line("");
                        return Ok(());
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => {
                line("");
                return Ok(());
            }
        }
    }
}

enum KeyOutcome {
    Continue,
    Retarget(Channel),
    Quit,
}

/// Keyboard input applies to every device at once.
fn handle_key(key: KeyEvent, target: Channel, states: &mut [DeviceState]) -> KeyOutcome {
    let adjust = |states: &mut [DeviceState], delta: i32| {
        for state in states {
            if target.uses_a() {
                state.queue.adjust_a(delta);
            }
            if target.uses_b() {
                state.queue.adjust_b(delta);
            }
        }
    };

    match key.code {
        KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyOutcome::Quit
        }
        KeyCode::Char('q') | KeyCode::Esc => KeyOutcome::Quit,
        KeyCode::Up | KeyCode::Char('+' | '=' | 'k') => {
            adjust(states, 1);
            KeyOutcome::Continue
        }
        KeyCode::Down | KeyCode::Char('-' | '_' | 'j') => {
            adjust(states, -1);
            KeyOutcome::Continue
        }
        // Emergency stop: do not queue and wait for a B1 confirmation, send on the next 100ms cycle
        KeyCode::Char(' ' | '0') => {
            for state in states {
                state.queue.zero_now();
            }
            KeyOutcome::Continue
        }
        KeyCode::Char('a') => KeyOutcome::Retarget(Channel::A),
        KeyCode::Char('b') => KeyOutcome::Retarget(Channel::B),
        KeyCode::Char('o') => KeyOutcome::Retarget(Channel::Both),
        _ => KeyOutcome::Continue,
    }
}
