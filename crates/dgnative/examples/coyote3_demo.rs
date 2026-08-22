//! Full Coyote 3.0 control loop example: scan and connect -> write the soft limit -> 100ms waveform loop.
//!
//! ```sh
//! cargo run --example coyote3_demo
//! ```
//!
//! ⚠️ The strength limit in this example is deliberately set very low (20);
//! do not raise it before you have confirmed the device behaves correctly.
//! Pressing Ctrl-C zeroes out the strength before exiting.

use dgnative::ble::{Coyote3, Coyote3Event};
use dgnative::protocol::v3::{B0, B0_INTERVAL_MS, Bf, Notification, Pulse, StrengthQueue};
use futures::StreamExt;
use std::time::Duration;

/// One channel of data (freq, strength) from the official app's built-in "Tide" waveform.
const TIDE: &[(u8, u8)] = &[
    (10, 0),
    (11, 16),
    (13, 33),
    (14, 50),
    (16, 66),
    (18, 83),
    (19, 100),
    (21, 92),
    (22, 84),
    (24, 76),
    (26, 68),
    (26, 0),
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Scanning for Coyote 3.0 (47L121000)...");
    let coyote = Coyote3::scan_and_connect(Duration::from_secs(20)).await?;
    println!("Connected, battery {}%", coyote.battery_level().await?);

    // BF must be rewritten after a reconnect: here both channel soft limits are capped at 20
    coyote.set_config(&Bf::with_limits(20, 20)).await?;
    println!("Soft strength limit set to A=20 B=20");

    // Background task printing the strength changes reported by the device
    let mut events = coyote.events().await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            match event {
                Coyote3Event::Message(Notification::Strength(b1)) => {
                    println!(
                        "strength report seq={} A={} B={}",
                        b1.sequence, b1.strength_a, b1.strength_b
                    );
                    let _ = tx.send(b1);
                }
                Coyote3Event::Message(Notification::Unknown(data)) => {
                    println!("unknown message {data:02X?}");
                }
                Coyote3Event::Battery(level) => println!("battery {level}%"),
            }
        }
    });

    // Use the state machine to turn "set channel A to 5" into a conforming request with a sequence number
    let mut queue = StrengthQueue::new();
    queue.set_a(5);

    let mut ticker = tokio::time::interval(Duration::from_millis(B0_INTERVAL_MS));
    let mut step = 0usize;
    println!("Starting waveform output, Ctrl-C to stop");

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Consume one waveform frame every 100ms (each frame holds 4 groups of 25ms data)
                let (freq, intensity) = TIDE[step % TIDE.len()];
                step += 1;

                let (sequence, action_a, action_b) = queue.tick();
                let cmd = B0 {
                    sequence,
                    action_a,
                    action_b,
                    pulses_a: [Pulse::new(freq, intensity)?; 4],
                    // Keep channel B silent: an out-of-range strength value makes the host discard all data for that channel
                    pulses_b: [Pulse { frequency: 10, intensity: 101 }; 4],
                };
                coyote.send(&cmd).await?;
            }
            Some(b1) = rx.recv() => queue.on_b1(&b1),
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    println!("\nZeroing out strength and disconnecting");
    coyote.stop().await?;
    coyote.disconnect().await?;
    Ok(())
}
