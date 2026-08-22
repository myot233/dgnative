//! Gacha wheel: a local web page wheel; whichever prize it lands on is played at that prize's
//! strength and duration.
//!
//! Prize strength is a **percentage of the soft limit**, not an absolute value -- the wheel's
//! output ceiling always equals the soft limit written to the device by `--limit` at startup, so
//! lowering limit makes everything lighter across the board.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dgnative::protocol::v3::{B0, B0_INTERVAL_MS, B1, Bf, Notification, StrengthQueue, builtin};
use futures::StreamExt;
use ipnet::IpNet;
use rand::RngExt;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::{Channel, Device, SILENT, Target, connect_all};

/// One prize on the wheel.
#[derive(Clone, Copy, Serialize)]
pub struct Prize {
    /// Display name.
    pub label: &'static str,
    /// Waveform name (matches a built-in waveform in `builtin`).
    pub wave: &'static str,
    /// Strength, as a percentage of the soft limit (0-100).
    pub strength_pct: u8,
    /// Duration in seconds.
    pub seconds: u64,
    /// Draw weight; the larger it is, the more likely the prize.
    pub weight: u32,
    /// Wheel segment color.
    pub color: &'static str,
}

/// Default prize pool. The weights need not sum to 100; drawing is by relative proportion.
///
/// Segment size is drawn from the weight, so the weights should not differ too much -- the
/// segment of the smallest weight gets too narrow to even fit its text. Currently min / max is
/// about 1:5, and the narrowest segment still spans about 9°.
///
/// Edit this table to customize the game; the `prize_table_is_valid` test checks that the
/// waveform name exists, the percentage is in range, and the weight is non-zero.
// Laid out as a table so each prize's numbers can be compared vertically
#[rustfmt::skip]
pub const PRIZES: &[Prize] = &[
    // --- Light tier: high weight, most results land here ---
    Prize { label: "Air",         wave: "breathing", strength_pct: 0,  seconds: 3,  weight: 16, color: "#3f4756" },
    Prize { label: "Breeze",      wave: "breathing", strength_pct: 15, seconds: 4,  weight: 15, color: "#46708f" },
    Prize { label: "Caress",      wave: "breathing", strength_pct: 25, seconds: 6,  weight: 15, color: "#4a8fb8" },
    Prize { label: "Ripple",      wave: "tide",      strength_pct: 30, seconds: 8,  weight: 12, color: "#4e9fa8" },
    // --- Mid tier ---
    Prize { label: "Sip",         wave: "breathing", strength_pct: 40, seconds: 8,  weight: 12, color: "#5aa9a0" },
    Prize { label: "Steps",       wave: "steps",     strength_pct: 45, seconds: 10, weight: 10, color: "#6bab7e" },
    Prize { label: "Heartbeat",   wave: "pulse",     strength_pct: 55, seconds: 10, weight: 9,  color: "#9aac52" },
    Prize { label: "Flutter",     wave: "flutter",   strength_pct: 50, seconds: 12, weight: 8,  color: "#c9a227" },
    // --- Heavy tier: low weight ---
    Prize { label: "Surge",       wave: "tide",      strength_pct: 70, seconds: 12, weight: 7,  color: "#d9902f" },
    Prize { label: "Combo",       wave: "staccato",  strength_pct: 65, seconds: 8,  weight: 6,  color: "#d97a34" },
    Prize { label: "Crescendo",   wave: "ramp",      strength_pct: 80, seconds: 16, weight: 5,  color: "#cf6136" },
    Prize { label: "Long Night",  wave: "tide",      strength_pct: 60, seconds: 30, weight: 4,  color: "#8e5bb5" },
    Prize { label: "Thunder",     wave: "full",      strength_pct: 100, seconds: 5, weight: 3,  color: "#c8443c" },
];

/// Draw one prize by weight and return its index.
fn draw() -> usize {
    let total: u32 = PRIZES.iter().map(|p| p.weight).sum();
    let mut roll = rand::rng().random_range(0..total);
    for (i, prize) in PRIZES.iter().enumerate() {
        if roll < prize.weight {
            return i;
        }
        roll -= prize.weight;
    }
    PRIZES.len() - 1
}

/// Runtime state polled by the page.
#[derive(Clone, Serialize, Default)]
struct Status {
    /// Short ids of the connected devices.
    devices: Vec<String>,
    /// Current (A, B) strength of each device.
    strengths: Vec<(u8, u8)>,
    /// Soft limit in effect.
    limit: u8,
    /// Name of the prize being played; null when idle.
    playing: Option<String>,
    /// Milliseconds remaining.
    remaining_ms: u64,
    /// Total duration of this run, in milliseconds.
    total_ms: u64,
}

/// Commands the HTTP handlers send to the device loop.
enum Cmd {
    Play(usize),
    Stop,
}

/// Inputs to the device loop: HTTP commands, or B1 strength reports from a device.
enum Event {
    Cmd(Cmd),
    B1(usize, B1),
}

#[derive(Clone)]
struct AppState {
    tx: mpsc::UnboundedSender<Event>,
    status: Arc<Mutex<Status>>,
    allow: Arc<Vec<IpNet>>,
}

/// Parse an `--allow` value: a CIDR (`192.168.1.0/24`) or a single IP.
pub fn parse_allow(text: &str) -> Result<IpNet, String> {
    if let Ok(net) = text.parse::<IpNet>() {
        return Ok(net);
    }
    // A bare IP is equivalent to /32 (IPv4) or /128 (IPv6)
    match text.parse::<IpAddr>() {
        Ok(ip) => Ok(IpNet::from(ip)),
        Err(_) => Err(format!("{text:?} is neither a CIDR nor an IP address")),
    }
}

/// Take the client IP, unmapping IPv4-mapped addresses back to IPv4.
///
/// Under dual-stack listening, IPv4 clients show up as `::ffff:192.168.1.5`; without unmapping
/// they bypass allow rules written as IPv4 subnets.
fn client_ip(addr: SocketAddr) -> IpAddr {
    match addr.ip() {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        ip => ip,
    }
}

/// Allowlist middleware: any client outside the allowed subnets gets a 403.
async fn guard(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let ip = client_ip(peer);
    if !state.allow.iter().any(|net| net.contains(&ip)) {
        eprintln!("rejected request from {ip} (not in a --allow subnet)");
        return (StatusCode::FORBIDDEN, "forbidden\n").into_response();
    }
    next.run(req).await
}

/// The prize currently playing.
struct Active {
    prize: Prize,
    wave: &'static builtin::Builtin,
    until: Instant,
    total: Duration,
    frame: usize,
}

/// Device loop: fixed 100ms cadence, sending silence frames when idle and the prize waveform
/// while playing.
async fn runner(
    devices: Arc<Vec<Device>>,
    limit: u8,
    channel: Channel,
    mut rx: mpsc::UnboundedReceiver<Event>,
    status: Arc<Mutex<Status>>,
) {
    let mut queues: Vec<StrengthQueue> = devices.iter().map(|_| StrengthQueue::new()).collect();
    let mut ticker = tokio::time::interval(Duration::from_millis(B0_INTERVAL_MS));
    let mut active: Option<Active> = None;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Wrap up first when time is up, then send this frame, so we do not
                // emit an extra 100ms
                if active.as_ref().is_some_and(|a| Instant::now() >= a.until) {
                    active = None;
                    for q in &mut queues {
                        q.zero_now();
                    }
                }

                let pulses = match &mut active {
                    Some(a) => {
                        let p = a.wave.pulses_at(a.frame);
                        a.frame += 1;
                        p
                    }
                    // Idle: both channels get out-of-range values, so the host
                    // drops all waveform data
                    None => SILENT,
                };

                for (device, queue) in devices.iter().zip(queues.iter_mut()) {
                    let (sequence, action_a, action_b) = queue.tick();
                    let cmd = B0 {
                        sequence,
                        action_a,
                        action_b,
                        pulses_a: if channel.uses_a() { pulses } else { SILENT },
                        pulses_b: if channel.uses_b() { pulses } else { SILENT },
                    };
                    if let Err(e) = device.coyote.send(&cmd).await {
                        eprintln!("write to {} failed: {e}", device.tag);
                    }
                }

                if let Ok(mut s) = status.lock() {
                    match &active {
                        Some(a) => {
                            s.playing = Some(a.prize.label.to_string());
                            s.remaining_ms =
                                a.until.saturating_duration_since(Instant::now()).as_millis() as u64;
                            s.total_ms = a.total.as_millis() as u64;
                        }
                        None => {
                            s.playing = None;
                            s.remaining_ms = 0;
                            s.total_ms = 0;
                        }
                    }
                }
            }

            Some(event) = rx.recv() => match event {
                Event::Cmd(Cmd::Play(index)) => {
                    let Some(prize) = PRIZES.get(index).copied() else { continue };
                    let Some(wave) = builtin::by_name(prize.wave) else { continue };

                    // Strength is a percentage of the soft limit, so it inherently
                    // cannot exceed limit
                    let strength = (limit as u32 * prize.strength_pct as u32 / 100) as u8;
                    for q in &mut queues {
                        if channel.uses_a() {
                            q.set_a(strength);
                        }
                        if channel.uses_b() {
                            q.set_b(strength);
                        }
                    }
                    let total = Duration::from_secs(prize.seconds);
                    active = Some(Active {
                        prize,
                        wave,
                        until: Instant::now() + total,
                        total,
                        frame: 0,
                    });
                    println!("drew '{}' at {strength}/{limit} for {}s", prize.label, prize.seconds);
                }

                Event::Cmd(Cmd::Stop) => {
                    active = None;
                    for q in &mut queues {
                        q.zero_now();
                    }
                    println!("emergency stopped");
                }

                Event::B1(index, b1) => {
                    if let Some(q) = queues.get_mut(index) {
                        q.on_b1(&b1);
                    }
                    if let Ok(mut s) = status.lock()
                        && let Some(slot) = s.strengths.get_mut(index)
                    {
                        *slot = (b1.strength_a, b1.strength_b);
                    }
                }
            }
        }
    }
}

async fn page() -> impl IntoResponse {
    Html(include_str!("gacha.html"))
}

async fn prizes() -> impl IntoResponse {
    Json(PRIZES)
}

async fn get_status(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.status.lock().map(|s| s.clone()).unwrap_or_default();
    Json(snapshot)
}

#[derive(Serialize)]
struct SpinResult {
    index: usize,
    prize: Prize,
}

/// Draw: the server decides the result and starts output immediately; the page is responsible for
/// spinning the pointer to the matching segment.
async fn spin(State(state): State<AppState>) -> impl IntoResponse {
    let index = draw();
    let _ = state.tx.send(Event::Cmd(Cmd::Play(index)));
    Json(SpinResult {
        index,
        prize: PRIZES[index],
    })
}

async fn stop(State(state): State<AppState>) -> impl IntoResponse {
    let _ = state.tx.send(Event::Cmd(Cmd::Stop));
    Json(serde_json::json!({ "ok": true }))
}

/// Start the gacha service: connect devices, write the soft limit, run the 100ms loop and the
/// HTTP server.
///
/// With `offline`, no device is connected and only the web page runs -- for tuning the prize pool
/// and previewing the wheel.
pub async fn serve(
    target: &Target,
    bind: IpAddr,
    port: u16,
    allow: Vec<IpNet>,
    limit: u8,
    channel: Channel,
    offline: bool,
) -> Result<()> {
    // Binding to a non-loopback address means other machines can trigger output, so who may
    // access it must be stated explicitly
    if !bind.is_loopback() && allow.is_empty() {
        bail!(
            "--bind {bind} exposes the control interface to the network; you must also use \
             --allow to name the permitted subnets, for example --allow 192.168.1.0/24"
        );
    }
    let allow = if allow.is_empty() {
        // Loopback only: spell it out so the middleware runs one check for every case
        vec!["127.0.0.0/8".parse().unwrap(), "::1/128".parse().unwrap()]
    } else {
        allow
    };

    let devices = Arc::new(if offline {
        println!("offline mode: no device connected, running the web page only.");
        Vec::new()
    } else {
        connect_all(target).await?
    });

    println!();
    for device in devices.iter() {
        device
            .coyote
            .set_config(&Bf::with_limits(limit, limit))
            .await?;
        println!("  {} soft limit A={limit} B={limit}", device.tag);
    }

    let status = Arc::new(Mutex::new(Status {
        devices: devices.iter().map(|d| d.tag.clone()).collect(),
        strengths: vec![(0, 0); devices.len()],
        limit,
        ..Status::default()
    }));

    let (tx, rx) = mpsc::unbounded_channel::<Event>();

    // The B1 reports of every device feed into the same event stream
    for (index, device) in devices.iter().enumerate() {
        let mut events = device.coyote.events().await?;
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if let dgnative::ble::Coyote3Event::Message(Notification::Strength(b1)) = event {
                    let _ = tx.send(Event::B1(index, b1));
                }
            }
        });
    }

    let state = AppState {
        tx: tx.clone(),
        status: Arc::clone(&status),
        allow: Arc::new(allow.clone()),
    };
    let app = Router::new()
        .route("/", get(page))
        .route("/api/prizes", get(prizes))
        .route("/api/status", get(get_status))
        .route("/api/spin", post(spin))
        .route("/api/stop", post(stop))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);

    let addr = SocketAddr::from((bind, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}, try another --port or --bind"))?;

    println!("\ngacha wheel started: http://{addr}");
    println!(
        "allowed subnets: {}",
        allow
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !bind.is_loopback() {
        println!(
            "⚠️  the interface is exposed to the network; anyone in those subnets can trigger output."
        );
    }
    println!(
        "Prize strength is a percentage of the soft limit {limit} and never exceeds it. Ctrl-C to exit.\n"
    );

    let loop_handle = tokio::spawn(runner(
        Arc::clone(&devices),
        limit,
        channel,
        rx,
        Arc::clone(&status),
    ));
    // ConnectInfo needs this make-service to get at the client address
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    let result = server.await;

    // Let the loop send the zeroing frame out first, then stop it and disconnect
    let _ = tx.send(Event::Cmd(Cmd::Stop));
    tokio::time::sleep(Duration::from_millis(300)).await;
    loop_handle.abort();
    for device in devices.iter() {
        if let Err(e) = device.coyote.stop().await {
            eprintln!(
                "{} failed to zero out: {e} -- power that host off manually to be sure",
                device.tag
            );
        }
        let _ = device.coyote.disconnect().await;
    }

    result.context("HTTP service exited unexpectedly")?;
    println!("\nZeroed out and disconnected.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(nets: &[&str], peer: &str) -> bool {
        let nets: Vec<IpNet> = nets.iter().map(|n| parse_allow(n).unwrap()).collect();
        let ip = client_ip(peer.parse().unwrap());
        nets.iter().any(|n| n.contains(&ip))
    }

    #[test]
    fn parse_allow_accepts_cidr_and_bare_ip() {
        assert_eq!(
            parse_allow("192.168.1.0/24").unwrap().to_string(),
            "192.168.1.0/24"
        );
        // A bare IP collapses to a single-address subnet
        assert_eq!(
            parse_allow("192.168.1.5").unwrap().to_string(),
            "192.168.1.5/32"
        );
        assert_eq!(parse_allow("::1").unwrap().to_string(), "::1/128");
        assert!(parse_allow("not-an-ip").is_err());
        assert!(parse_allow("192.168.1.0/99").is_err());
    }

    #[test]
    fn cidr_matching() {
        assert!(allowed(&["192.168.1.0/24"], "192.168.1.5:1234"));
        assert!(!allowed(&["192.168.1.0/24"], "192.168.2.5:1234"));
        assert!(!allowed(&["192.168.1.0/24"], "127.0.0.1:1234"));
        // Any one of several subnets matching is enough
        assert!(allowed(&["127.0.0.0/8", "192.168.1.0/24"], "127.0.0.1:1"));
        // An empty list rejects everything
        assert!(!allowed(&[], "127.0.0.1:1"));
    }

    #[test]
    fn ipv4_mapped_ipv6_is_normalized_before_matching() {
        // Under dual-stack listening, IPv4 clients show up as ::ffff:a.b.c.d;
        // without unmapping they bypass rules written as IPv4 subnets
        assert!(allowed(&["192.168.1.0/24"], "[::ffff:192.168.1.5]:1234"));
        assert!(!allowed(&["192.168.1.0/24"], "[::ffff:10.0.0.5]:1234"));
        assert!(allowed(&["127.0.0.0/8"], "[::ffff:127.0.0.1]:1234"));
        // Real IPv6 addresses are unaffected
        assert!(allowed(&["fd00::/8"], "[fd00::1]:1234"));
        assert!(!allowed(&["192.168.1.0/24"], "[fd00::1]:1234"));
    }

    #[test]
    fn prize_table_is_valid() {
        for prize in PRIZES {
            assert!(
                builtin::by_name(prize.wave).is_some(),
                "prize '{}' references nonexistent waveform {}",
                prize.label,
                prize.wave
            );
            assert!(
                prize.strength_pct <= 100,
                "prize '{}' percentage out of range",
                prize.label
            );
            assert!(
                prize.weight > 0,
                "prize '{}' has weight 0 and can never be drawn",
                prize.label
            );
        }
        assert!(PRIZES.iter().map(|p| p.weight).sum::<u32>() > 0);
    }

    #[test]
    fn narrowest_segment_stays_readable() {
        // Segment size is drawn from the weight; when the weights differ too much the smallest
        // one gets too narrow to fit its text. If this fails while editing the prize pool,
        // either narrow the weight spread or drop a few prizes.
        let total: u32 = PRIZES.iter().map(|p| p.weight).sum();
        let min = PRIZES.iter().map(|p| p.weight).min().unwrap();
        let degrees = 360.0 * f64::from(min) / f64::from(total);
        assert!(
            degrees >= 7.0,
            "narrowest segment is only {degrees:.1}°, too small to fit a prize name"
        );
    }

    #[test]
    fn draw_stays_in_bounds() {
        for _ in 0..2000 {
            assert!(draw() < PRIZES.len());
        }
    }

    #[test]
    fn strength_never_exceeds_limit() {
        // Prize strength is a percentage of the soft limit, so it never goes out of
        // range for any limit
        for limit in [0u8, 1, 20, 99, 200] {
            for prize in PRIZES {
                let strength = (limit as u32 * prize.strength_pct as u32 / 100) as u8;
                assert!(
                    strength <= limit,
                    "{} computed {strength} at limit={limit}",
                    prize.label
                );
            }
        }
    }
}
