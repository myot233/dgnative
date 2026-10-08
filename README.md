# dgnative

Rust implementation of the Bluetooth protocol of the DG-LAB Coyote pulse host (Coyote V2 / V3).

The protocol follows the official open-source documentation [DG-LAB-OPENSOURCE](https://github.com/DG-LAB-OPENSOURCE/DG-LAB-OPENSOURCE);
every codec path is checked against the HEX examples given there as test vectors.

## Structure

This is a cargo workspace; the library, the command-line tool and the desktop UI
are separate crates:

| crate | Contents |
|---|---|
| `crates/dgnative` | Library: protocol codec + BLE transport, the only part published to crates.io |
| `crates/dgnative-cli` | The `dgnative` command-line tool, built solely on the library's public API |
| `crates/dgnative-gui` | `dgnative-gui`, a [gpui](https://www.gpui.rs/) desktop window over the same API |

Inside the library:

| Module | Contents |
|---|---|
| `protocol::v3` | Coyote 3.0: `B0` / `BF` command encoding, `B1` message parsing, strength input state machine |
| `protocol::v3::builtin` | 8 built-in waveforms; the `official` field separates official data from custom data |
| `protocol::v2` | Coyote 2.0: bit packing for PWM_AB2 strength and PWM_A34/B34 waveforms |
| `ble` | Cross-platform transport built on [btleplug](https://github.com/deviceplug/btleplug) (`ble` feature, on by default) |

The `protocol` layer does no IO and works without the `ble` feature, e.g. wired up to
your own BLE stack, a WebSocket relay, or an embedded target:

```toml
dgnative = { version = "0.1", default-features = false }          # protocol only
dgnative = { version = "0.1", default-features = false, features = ["ble"] }
```

## Prebuilt binaries

Every commit on `main` refreshes the `rolling` prerelease with the CLI and the
desktop UI for linux-x86_64, macos-aarch64, macos-x86_64 and windows-x86_64.
They are unsigned, so macOS needs the quarantine flag cleared before the first
run:

```sh
xattr -d com.apple.quarantine dgnative dgnative-gui
```

## Command-line tool

```sh
cargo install --path crates/dgnative-cli    # or cargo run -p dgnative-cli -- <command>
```

```
dgnative scan                    Scan for nearby DG-LAB devices
dgnative info                    Connect and show battery
dgnative config --limit-a 50 --limit-b 50
                                 Write strength soft limits and balance parameters (BF)
dgnative waves                   List built-in waveforms
dgnative play [waveform]         Play a waveform, adjust strength live from the keyboard
dgnative gacha                   Web gacha wheel
dgnative monitor                 Watch strength changes and battery events
dgnative stop                    Zero out both channels immediately
```

Global options:

| Option | Effect |
|---|---|
| `-t/--timeout` | Scan duration in seconds (default 15) |
| `-D/--device` | Device id prefix, the "id" column of `scan`; the first few characters are enough |
| `-A/--all` | Run against every host found at once |

Without `-D` / `-A` the strongest signal wins. If several hosts are in range you get a
notice. Under `-A`, a host that fails to connect does not abort the rest; it is reported
and the run continues.

`play` is the main entry point:

```sh
dgnative play tide --channel both --limit 30 --duration 60
dgnative -A play full --limit 200 --strength 200 --channel both -d 20
```

**`play` writes the soft limit itself**: `--limit` is sent as a BF before playback starts,
so there is no need to run `config` separately. The start strength `--strength` defaults
to 0 and has to be raised by hand — deliberately, so nothing fires the moment you start.

Keys while running:

| Key | Action |
|---|---|
| `↑` / `+` / `k` | Strength +1 |
| `↓` / `-` / `j` | Strength −1 |
| `Space` / `0` | Zero out immediately (no wait for B1 confirmation; takes effect on the next 100ms cycle) |
| `a` / `b` / `o` | Switch the adjustment target to A / B / both channels |
| `q` / `Esc` / `Ctrl-C` | Zero out and exit |

Keyboard input applies to all devices at once. Non-interactive environments (pipes, CI,
unattended batch runs) have no TTY, so keyboard control is switched off automatically,
status is printed once every 2s instead, and the run ends via `--duration`.

An abnormal exit (panic, kill) leaves no time to zero out, but the protocol itself is
fail-safe: waveform data is only valid for 100ms, so the device stops emitting pulses as
soon as B0 stops. Channel strength values stay on the device and must be set again after
reconnecting.

## Gacha wheel

```sh
dgnative gacha --limit 30
```

Open the printed address in a browser; after the wheel finishes spinning and recoils into
place, its prize is played at that prize's strength and duration. By default, 50% of initial
draws with a higher-strength prize available fake-stop on the initial result for 0.7 seconds,
then slide to the nearest higher segment and recoil again. Distance is measured between
segment centers around the circle, including the wrap: "Air" can upgrade to "Thunder".
Output begins only after the final visible result settles. Set `--fakeout-chance 0` to
disable this effect, or choose any percentage from 0 to 100. STOP cancels a pending
draw as well as active output. There are 13 prizes by default, from "Air" (0%, 3s)
to "Thunder" (100%, 5s), covering all 8 waveforms.

**Prize strength is a percentage of the soft limit, not an absolute value** — so the
wheel's output can never exceed `--limit`, and lowering it makes the whole prize pool
lighter. Segments are drawn in proportion to the initial draw weights; the table shows
final probabilities including fake-stop upgrades. A "Stop" button sits permanently on
the page and interrupts at any time, including the apparent stop and the final slide.

The **Auto** button schedules repeated spins. Set the minimum and maximum interval in
seconds (1 to 3600, default 10 to 30); each wait is chosen uniformly from that range.
The first wait starts when Auto is enabled, or after current playback finishes. Later
waits start after each prize finishes playing, so automatic draws do not interrupt it.
Changing the interval range reschedules a pending wait. Press Auto again to cancel future
draws, or STOP to also cancel the current draw/output. STOP in another open page cancels
Auto too. Reloading the page leaves Auto off.

Run the deterministic wheel and Auto regression checks with
`node crates/dgnative-cli/tests/gacha-ui.cjs`; these use a controlled browser clock and
mock API responses without connecting to hardware.

Prizes live in the `PRIZES` table in `crates/dgnative-cli/src/gacha.rs`; edit it and
recompile. Two tests block the usual ways of breaking it: `prize_table_is_valid` checks
that waveform names exist, percentages stay in range and weights are non-zero;
`narrowest_segment_stays_readable` makes sure the weight spread never gets wide enough to
squeeze the smallest segment below readable label width (currently 8.9° at the narrowest).

`--offline` runs only the web page without connecting to a device, for tuning the prize
pool and previewing the wheel:

```sh
dgnative gacha --offline
```

### Exposing it on the LAN

By default only the loopback address is bound. To let others on the same subnet spin:

```sh
dgnative gacha --bind 0.0.0.0 --allow 192.168.1.0/24 --limit 30
```

`--allow` takes a CIDR or a single IP and may be repeated. **Binding a non-loopback
address requires an explicit `--allow`**, otherwise startup is refused — this endpoint
triggers electrical output and has no business being exposed without access control.
Sources outside the allowlist always get a 403, with the source IP printed to the terminal.

Note that this is a subnet-level allowlist with no authentication: anyone inside an
allowed subnet can spin the wheel. Only open it up on networks you trust.

## Desktop UI

```sh
cargo run -p dgnative-gui             # or dgnative-gui from a release archive
cargo run -p dgnative-gui -- --offline
```

A single window: scan, pick a device from the list, then two channel cards with
`-` / `+`, a waveform picker, the soft limit, and a STOP button that zeroes both
channels. Same defaults as the CLI — soft limit 20, both channels start at 0 —
and the same keys: `up`/`down` adjust, `space` zeroes, `a` / `b` / `o` choose
which channels the buttons act on.

Only pulse host 3.0 can be driven; a 2.0 host or a wireless sensor shows up in
the scan list marked `3.0 only`. Disconnecting zeroes the device first.

`--offline` runs the whole flow against a simulator: the scan returns two fake
devices and strengths are echoed back as if the hardware confirmed them. Use it
to work on the window with nothing attached. `-D/--device <prefix>` skips the
picker and connects to a known device at startup.

The UI is built with [gpui](https://www.gpui.rs/), which renders through Metal
on macOS, Vulkan on Linux and DirectX on Windows. Building it on Linux needs
the usual desktop development packages (`libwayland-dev`, `libxkbcommon-dev`,
`libx11-dev`, `libfontconfig1-dev`, `libfreetype6-dev`, `libasound2-dev`); see
`.github/workflows/ci.yml` for the exact list CI installs.

## Web Bluetooth

`web/` is a standalone static app with direct Web Bluetooth control, manual A/B
controls, configuration, battery/status monitoring, offline preview, and the gacha
wheel including fake-stops and Auto. It requires no native BLE service:

```sh
python3 -m http.server 8779 --bind 127.0.0.1 --directory web
```

Open `http://127.0.0.1:8779/` in Chrome, disconnect any native client, then select
the device through the browser picker. Keep the page visible while playing;
switching it to the background stops output and Auto. See [web/README.md](web/README.md)
for the full flow, browser limitations, and tests. The native CLI and GUI remain
available for native scanning, background playback, and LAN control.

## V3 usage

```rust
use dgnative::ble::Coyote3;
use dgnative::protocol::v3::{B0, Bf, Pulse, StrengthAction};
use std::time::Duration;

let coyote = Coyote3::scan_and_connect(Duration::from_secs(10)).await?;

// ⚠️ BF has no reply and persists across power cycles; rewrite the soft limits on every reconnect
coyote.set_config(&Bf::with_limits(50, 50)).await?;

// One B0 every 100ms
coyote.send(&B0 {
    sequence: 1,
    action_a: StrengthAction::Set(10),
    pulses_a: [Pulse::new(10, 50)?; 4],
    ..B0::default()
}).await?;
```

Strength changes have to be handshaked against the device's B1 report (the official
recommendation is that a change with a non-zero sequence number waits for the B1 carrying
the same sequence number before the next one is sent). `StrengthQueue` wraps that rhythm:

```rust
let mut queue = StrengthQueue::new();
queue.adjust_a(1);                        // user pressed "+" once

// when assembling B0 every 100ms
let (sequence, action_a, action_b) = queue.tick();

// when a B1 notification arrives
queue.on_b1(&b1);
```

User input during the wait is accumulated automatically; if the device never reports back
(because the soft limit was reached, for instance), the state machine drops out of the wait
after roughly one second instead of deadlocking.

Emergency stop should use `queue.zero_now()` rather than `set_a(0)`: the former maps to
`strengthZero()` in the official documentation and skips the "wait for B1 confirmation"
throttle, so it goes out on the next cycle; the latter takes up to one confirmation
timeout (about a second) in the worst case.

For a complete control loop see `crates/dgnative/examples/coyote3_demo.rs`:

```sh
cargo run -p dgnative --example coyote3_demo
```

## Protocol notes

**V3**: a single 20-byte B0 command carries strength changes for both channels plus 4
waveform groups per channel (25ms each), and is written once every 100ms. If any of a
channel's 4 groups holds an out-of-range value, the host discards **all** 4 groups for
that channel — the official code uses exactly this to keep one channel silent, which is
why `Pulse` exposes its raw bytes publicly and `Pulse::new` is only there for when you
do want validation.

**V2**: strength persists once written, while waveform parameters are only valid for 0.1s
per group and must be rewritten every 100ms. The 3-byte values of all three
characteristics are sent little-endian.

## Safety notes

The CLI and the examples default to a strength soft limit of 20 and a start strength of 0.
Do not raise them before you have confirmed that the device behaves as expected, and do
not run an automated control loop unsupervised.

The soft limit **persists across power cycles**, and both `config` and `play --limit`
overwrite whatever you set in the official app. Setting it to 200 amounts to disabling
strength protection, and it stays that way until the next write — not just for the current
run. Check the current limit before handing the device over to another program.

## Known uncertainties

In the official V2 documentation table, the "description" column of PWM_A34 / PWM_B34 has
the B / A channels swapped, yet their bit definitions are named Ax/Ay/Az and Bx/By/Bz
respectively. This implementation follows the characteristic names and the bit definitions
(0x1505 = channel A, 0x1506 = channel B). If real hardware turns out to behave the other
way round, swap the writes to the two characteristics.

## Testing

```sh
cargo test --workspace --all-features
```

The protocol codec is fully covered by the HEX test vectors from the official
documentation. Verified on real hardware: scanning, connecting, GATT service discovery,
battery readout. Output commands (B0/BF) are so far only checked against test vectors,
with no verification on real hardware.
