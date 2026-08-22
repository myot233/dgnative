---
name: dgnative
description: >
  Work on or with dgnative, the Rust implementation of the DG-LAB Coyote
  V2/V3 pulse host BLE protocol and its `dgnative` command-line tool. Use this
  skill when driving the CLI (scan, info, config, waves, play, gacha, monitor,
  stop), when writing Rust against the `dgnative` library (B0/BF encoding, B1
  parsing, StrengthQueue, built-in waveforms, the btleplug transport), when
  adding a waveform or a gacha prize, or when touching anything in
  `crates/dgnative` / `crates/dgnative-cli`. Also covers the safety rules that
  apply to every strength value this project can emit.
---

# dgnative

Rust implementation of the DG-LAB Coyote V2/V3 pulse host Bluetooth protocol, plus a CLI.
Cargo workspace, two crates:

| Crate | Contents |
|---|---|
| `crates/dgnative` | Library. `protocol::v2` / `protocol::v3` codecs (no IO) + `ble` transport behind the optional `ble` feature (on by default). |
| `crates/dgnative-cli` | Binary `dgnative`. `main.rs` (commands) + `gacha.rs` / `gacha.html` (web wheel). Depends only on the library's public API. |

Protocol source of truth: [DG-LAB-OPENSOURCE](https://github.com/DG-LAB-OPENSOURCE/DG-LAB-OPENSOURCE).
Every V3/V2 codec path is checked against the HEX vectors from those docs.

## Safety rails

This drives current through a person. These are not style preferences.

- **Never raise a strength value the user did not ask for.** CLI defaults are soft limit `20` and starting strength `0`; examples, docs and generated code keep those defaults. `--limit 200` disables strength protection entirely.
- **The soft limit persists across power cycles.** `config` and `play --limit` overwrite whatever the official app had set, and the new value survives until something rewrites it — including for other programs that talk to the device later. Say so whenever you change it.
- **BF returns nothing and must be rewritten after every reconnect.** Without it the device keeps the previous soft limit, which may not be the one the current session assumes.
- **Fail-safe, but only partly.** Waveform data is valid for 100 ms, so if the B0 stream stops (panic, kill, disconnect) pulse output stops within one cycle. Channel *strength* values stay on the device and must be re-zeroed or reset on reconnect.
- **`gacha` on a non-loopback bind requires `--allow` and still has no authentication.** Anyone inside an allowed subnet can trigger output. Do not widen the bind or the allowlist on your own initiative.
- Hardware-verified so far: scan, connect, GATT discovery, battery read. Output commands (B0/BF) are covered by test vectors only — never claim real-device verification for them.

## CLI

```sh
cargo run -p dgnative-cli -- <command>      # in-tree
cargo install --path crates/dgnative-cli    # installs as `dgnative`
```

| Command | Purpose |
|---|---|
| `scan` | List nearby DG-LAB devices with id and signal |
| `info` | Connect and show battery |
| `config --limit-a N --limit-b N` | Write soft limits + balance parameters (BF) |
| `waves` | List the 8 built-in waveforms |
| `play [WAVE]` | Play a waveform, adjust strength from the keyboard |
| `gacha` | Serve the web gacha wheel |
| `monitor` | Watch strength-change and battery events |
| `stop` | Zero out both channels immediately |

Global: `-t/--timeout` (default 15s), `-D/--device <id prefix>`, `-A/--all` (every Coyote 3.0 found; conflicts with `-D`). Without either, the strongest signal wins.

`play` is the main entry point and writes the soft limit itself:

```sh
dgnative play tide --channel both --limit 30 --duration 60
```

Keys while running: `↑`/`+`/`k` up, `↓`/`-`/`j` down, `Space`/`0` zero now, `a`/`b`/`o` retarget A/B/both, `q`/`Esc`/`Ctrl-C` zero and exit. With no TTY (pipes, CI) keyboard control switches off and status prints every 2 s, so pass `--duration`.

`gacha --offline` serves the page with no device attached — use it for prize-pool and UI work.

## Library

```rust
use dgnative::ble::Coyote3;
use dgnative::protocol::v3::{B0, B0_INTERVAL_MS, Bf, Pulse, StrengthAction, StrengthQueue, builtin};
```

Control loop, in order:

1. `Coyote3::scan_and_connect(timeout)` (or `connect_peripheral` from a `scan()` result).
2. `coyote.set_config(&Bf::with_limits(a, b))` — required after every connect.
3. Every 100 ms (`B0_INTERVAL_MS`), send one `B0`; sending nothing means no output.
4. Feed `coyote.events()` back in: `Coyote3Event::Message(Notification::Strength(b1))` → `queue.on_b1(&b1)`, `Coyote3Event::Battery(pct)`.

`StrengthQueue` owns the strength handshake — the official docs require a non-zero-sequence change to wait for the matching `B1` before the next one:

```rust
let mut queue = StrengthQueue::new();
queue.adjust_a(1);                                  // user pressed "+"
let (sequence, action_a, action_b) = queue.tick();  // once per 100ms tick
```

- Input during a pending change accumulates; if no `B1` ever arrives (e.g. already at the soft limit) the wait clears itself after ~1 s.
- Emergency stop is `queue.zero_now()`, not `set_a(0)`: it skips the throttle and goes out on the next cycle. `set_a(0)` can take a full confirmation timeout.
- `Coyote3::stop()` zeroes both channels directly, bypassing the queue.

Key types: `B0 { sequence, action_a, action_b, pulses_a, pulses_b }` (20-byte encode), `Bf` (7-byte, soft limits + balance), `B1 { sequence, strength_a, strength_b }`, `StrengthAction::{Keep, Increase, Decrease, Set}`, `Pulse { frequency, intensity }`, `builtin::{ALL, by_name}` with `pulses_at(index)` and `cycle_ms()`.

## Protocol invariants

- One `B0` carries both channels: strength action plus 4 pulse groups of 25 ms each = 100 ms. Sequence is 4 bits (0..=15); `0` means "no report wanted".
- **Out-of-range values in any of a channel's 4 groups make the device discard all 4 groups of that channel.** This is the official way to silence one channel — hence the CLI's `SILENT` constant (`intensity: 101`) and why `Pulse`'s fields are public raw bytes with `Pulse::new` as the opt-in validator.
- Ranges: `MAX_STRENGTH` 200, frequency 10..=240 (`FREQ_MIN`/`FREQ_MAX`, use `encode_frequency` for the official 10..=1000 input conversion), intensity 0..=100.
- V2: strength persists once written; waveform parameters expire after 0.1 s and must be rewritten every 100 ms. All three characteristics take 3-byte little-endian values.
- V2 caveat worth preserving: the official doc's description column swaps A/B for PWM_A34/PWM_B34. This implementation follows the characteristic UUIDs and bit names (0x1505 = A, 0x1506 = B).

## Working in this repo

- **English only.** Comments, docs, strings, CLI help, HTML, commit messages. No CJK anywhere.
- Add a waveform: `crates/dgnative/src/protocol/v3/builtin.rs` (`Builtin` const + `ALL`). Set `official: false` unless the frame data really comes from the official app. `WAVE_NAMES` in `gacha.html` must list the same display label.
- Add a gacha prize: the `PRIZES` table in `crates/dgnative-cli/src/gacha.rs`. Strength is a **percentage of the soft limit**, never an absolute value. Two tests guard it: `prize_table_is_valid` (waveform exists, percentage in range, non-zero weight) and `narrowest_segment_stays_readable` (thinnest wheel segment stays ≥ 7°).
- Before finishing: `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features`, `cargo test --workspace --all-features`. Also build `cargo build -p dgnative --no-default-features` — the protocol layer must stay IO-free.
- Verify UI changes for real: `dgnative gacha --offline` and load the page. Verify CLI text changes by running the command, not by reading the source.
- `examples/coyote3_demo.rs` in the library crate is the reference control loop; keep it working when the API changes.
