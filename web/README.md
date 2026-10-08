# Web Bluetooth app

The `web/` directory is a standalone static app. It connects directly to the
computer's Bluetooth adapter through Web Bluetooth; it makes no requests to the
Rust gacha API and needs no native BLE helper or JavaScript build step.

For local development, run from the repository root:

```sh
python3 -m http.server 8779 --bind 127.0.0.1 --directory web
```

Open `http://127.0.0.1:8779/` in Chrome on macOS, Windows, or Android. Production
hosting must use HTTPS and serve `.mjs` files as JavaScript. All assets are local;
no CDN, account, or remote service is required. The default limit is 20 with zero
starting strength. `?limit=150` preselects a different limit in the page; it is
written only when connecting or applying configuration.

## Flow

1. Disconnect the device from the native app or CLI before using Web Bluetooth.
2. Set the channel limits, then click **Apply configuration**. Changes stop any
   running output. For Coyote 3.0, the limits and balance parameters are saved on
   the device and persist across power cycles.
3. Click **Connect device** and select the host in Chrome's picker. Each connection
   sends zero first, rewrites BF, subscribes to strength reports, and reads battery.
   Repeat the picker to connect additional devices of the same model.
4. Use manual controls or the gacha wheel. Arrow keys and `+` / `-` adjust strengths;
   Space, `0`, and Escape stop output when the page has keyboard focus. Escape also
   works from input fields. The 13 prizes and 8 waveforms match the Rust app.
5. Fake-stop defaults to 50% of eligible draws. It pauses on the initial result for
   700ms, moves to the nearest higher segment around the circle, and recoils again.
   Output starts only after the final result has settled and painted.
6. **Auto** waits a random interval between the configured bounds, starting after
   playback finishes. **STOP** cancels Auto, animations, pending draws, and output.
7. Disconnect sends zero before dropping the link. Reconnect starts at zero and
   discovers fresh GATT characteristics; it does not resume previous output.

**Offline preview** simulates this flow without accessing Bluetooth. It is useful
in browsers that do not support Web Bluetooth. Coyote 2.0 supports battery and
strength monitoring plus manual raw-strength / X/Y/Z waveform control. Its raw
strength is the official app level multiplied by 7. V2 limits are software limits;
the V3 gacha waveforms do not apply to V2.

## Browser behavior

- Initial device selection requires a user click. Native scan IDs, RSSI sorting,
  silent strongest-device selection, and scan-all connection are replaced by the
  browser picker. Reconnect uses previously selected devices within the same page.
- Keep the page visible while playing. Hiding it cancels Auto and pending draws,
  sends zero, and stops the timer. Returning keeps output stopped. A scheduling gap
  over 500ms stops active physical playback instead of sending overdue frames.
- Bluetooth writes are serialized per device. STOP invalidates queued frames and
  strength changes; it follows any already-running GATT operation with zero. A
  stalled GATT operation closes the connection. Waveform frames expire after 100ms,
  but stored channel strength still needs to be cleared on reconnect.
- One tab owns real Bluetooth connections using Web Locks. STOP is broadcast to
  other tabs of the same origin. Each device still needs user selection. There is
  no LAN HTTP control endpoint; remote control would require an additional relay.
- Safari, Firefox and standard iOS browsers do not offer the required native API.
  Installing a PWA does not provide a background BLE service.

Protocol bytes and control logic are checked with official test vectors and mocked
GATT operations. This does not verify physical B0/BF output on a real device.

## Development and checks

Rust remains the source of truth for waveform and prize data. After changing those
tables, regenerate the committed static module:

```sh
python3 scripts/export-web-data.py
python3 scripts/export-web-data.py --check
node --test web/tests/*.test.mjs
node web/tests/wheel-ui.cjs
```

The protocol, transport, playback controller and wheel are separate modules.
No npm dependencies are required. Tests use Node 24 or newer.
