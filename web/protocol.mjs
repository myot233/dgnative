export const INTERVAL_MS = 100;
export const KEEP = Object.freeze({ mode: 0, value: 0 });
export const set = value => ({ mode: 3, value });
export const SILENT = Array.from({ length: 4 }, () => [10, 101]);
export const ZERO = Array.from({ length: 4 }, () => [10, 0]);

export function encodeFrequency(input) {
  if (input < 10 || input > 1000) return 10;
  return input <= 100 ? input : input <= 600 ? Math.floor((input - 100) / 5) + 100 : Math.floor((input - 600) / 10) + 200;
}

export function integer(value, min, max, name = "Value") {
  if (!Number.isInteger(value) || value < min || value > max) {
    throw new RangeError(`${name} must be an integer from ${min} to ${max}`);
  }
  return value;
}

export const sig = short => `0000${short.toString(16).padStart(4, "0")}-0000-1000-8000-00805f9b34fb`;
export const v2uuid = short => `955a${short.toString(16).padStart(4, "0")}-0fe2-f5aa-a094-84b8d4f3e8ad`;
export const UUIDS = {
  v3: { service: sig(0x180c), write: sig(0x150a), notify: sig(0x150b), batteryService: sig(0x180a), battery: sig(0x1500) },
  v2: { service: v2uuid(0x180b), strength: v2uuid(0x1504), waveA: v2uuid(0x1505), waveB: v2uuid(0x1506), batteryService: v2uuid(0x180a), battery: v2uuid(0x1500) },
};

export function encodeB0({ sequence = 0, a = KEEP, b = KEEP, pulsesA = ZERO, pulsesB = ZERO } = {}) {
  integer(sequence, 0, 15, "Sequence");
  for (const action of [a, b]) {
    integer(action.mode, 0, 3, "Strength action");
    integer(action.value, 0, 200, "Strength");
  }
  const bytes = new Uint8Array(20);
  bytes.set([0xb0, sequence << 4 | a.mode << 2 | b.mode, a.value, b.value]);
  for (const [pulses, offset] of [[pulsesA, 4], [pulsesB, 12]]) {
    if (pulses.length !== 4) throw new RangeError("A frame must contain four pulse groups");
    pulses.forEach(([frequency, intensity], i) => {
      // Raw invalid pulses are intentional: the device discards that channel.
      bytes[offset + i] = integer(frequency, 0, 255, "Frequency byte");
      bytes[offset + 4 + i] = integer(intensity, 0, 255, "Intensity byte");
    });
  }
  return bytes;
}

export function encodeBf({ limitA = 20, limitB = 20, frequencyA = 160, frequencyB = 160, intensityA = 0, intensityB = 0 } = {}) {
  return Uint8Array.of(0xbf,
    integer(limitA, 0, 200, "A limit"), integer(limitB, 0, 200, "B limit"),
    ...[frequencyA, frequencyB, intensityA, intensityB].map(value => integer(value, 0, 255, "Balance")));
}

export function bytesOf(value) {
  return value instanceof DataView
    ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength) : new Uint8Array(value);
}

export function parseB1(value) {
  const bytes = bytesOf(value);
  if (!bytes.length || bytes[0] === 0xb1 && bytes.length < 4) throw new Error("Invalid B1 notification");
  return bytes[0] === 0xb1 ? { sequence: bytes[1], a: bytes[2], b: bytes[3] } : null;
}

export class StrengthQueue {
  constructor() { this.pending = [null, null]; this.next = 1; this.inflight = null; }
  set(channel, value) { this.pending[channel] = { absolute: integer(value, 0, 200, "Strength") }; }
  adjust(channel, delta) {
    integer(delta, -200, 200, "Adjustment");
    const current = this.pending[channel];
    this.pending[channel] = current && "absolute" in current
      ? { absolute: current.absolute + delta } : { delta: (current?.delta || 0) + delta };
  }
  zeroNow() { this.pending = [{ absolute: 0 }, { absolute: 0 }]; this.inflight = null; }
  clear() { this.pending = [null, null]; this.inflight = null; }
  onB1(message) { if (this.inflight?.sequence === message.sequence) this.inflight = null; }
  tick() {
    if (this.inflight && ++this.inflight.ticks < 10) return { sequence: 0, a: KEEP, b: KEEP };
    this.inflight = null;
    const actions = this.pending.map(p => {
      if (!p || p.delta === 0) return KEEP;
      if ("absolute" in p) return set(Math.max(0, Math.min(200, p.absolute)));
      return { mode: p.delta > 0 ? 1 : 2, value: Math.min(200, Math.abs(p.delta)) };
    });
    if (actions.every(action => action.mode === 0)) return { sequence: 0, a: KEEP, b: KEEP };
    const sequence = this.next;
    this.next = sequence % 15 + 1;
    this.inflight = { sequence, ticks: 0 };
    this.pending = [null, null];
    return { sequence, a: actions[0], b: actions[1] };
  }
}

const pack24 = value => Uint8Array.of(value & 255, value >> 8 & 255, value >> 16 & 255);
export function encodeV2Strength(a, b) {
  return pack24(integer(a, 0, 2047, "A strength") << 11 | integer(b, 0, 2047, "B strength"));
}
export function decodeV2Strength(value) {
  const bytes = bytesOf(value);
  if (bytes.length < 3) throw new Error("Invalid V2 strength notification");
  const packed = bytes[0] | bytes[1] << 8 | bytes[2] << 16;
  return [packed >> 11 & 2047, packed & 2047];
}
export function encodeV2Wave({ x, y, z }) {
  return pack24(integer(z, 0, 31, "Z") << 15 | integer(y, 0, 1023, "Y") << 5 | integer(x, 0, 31, "X"));
}
export function decodeV2Wave(value) {
  const bytes = bytesOf(value);
  if (bytes.length < 3) throw new Error("Invalid V2 waveform");
  const packed = bytes[0] | bytes[1] << 8 | bytes[2] << 16;
  return { x: packed & 31, y: packed >> 5 & 1023, z: packed >> 15 & 31 };
}
export function v2FromFrequency(frequency, z) {
  integer(frequency, 10, 1000, "Frequency");
  const x = Math.floor(Math.sqrt(frequency / 1000) * 15);
  const wave = { x, y: frequency - x, z };
  encodeV2Wave(wave); return wave;
}
