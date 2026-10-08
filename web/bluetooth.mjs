import { UUIDS, StrengthQueue, encodeB0, encodeBf, parseB1, bytesOf, set, ZERO, SILENT, encodeV2Strength, decodeV2Strength, encodeV2Wave } from "./protocol.mjs";

export const emit = (target, type, detail) => target.dispatchEvent(new CustomEvent(type, { detail }));

// Cancel queued work without allowing GATT operations to overlap.
export class SerialLane {
  constructor(onTimeout = () => {}, timeout = 2500) {
    this.tail = Promise.resolve(); this.revision = 0; this.onTimeout = onTimeout; this.timeout = timeout;
  }
  cancel() { ++this.revision; }
  run(operation) {
    const revision = this.revision;
    const result = this.tail.then(async () => {
      if (revision !== this.revision) return false;
      let timer;
      try {
        return await Promise.race([
          Promise.resolve().then(operation),
          new Promise((_, reject) => { timer = setTimeout(() => {
            this.cancel(); this.onTimeout(); reject(new Error("Bluetooth operation timed out; connection closed"));
          }, this.timeout); }),
        ]);
      } finally { clearTimeout(timer); }
    });
    this.tail = result.catch(() => {});
    return result;
  }
}

export class BleSession extends EventTarget {
  constructor(device, version = "v3") {
    super(); this.device = device; this.version = version; this.id = device.id; this.name = device.name;
    this.queue = new StrengthQueue(); this.strengths = [null, null]; this.battery = null;
    this.ready = false; this.closed = false; this.pendingV2 = null; this.listeners = [];
    this.lane = new SerialLane(() => device.gatt.disconnect());
    this.onDisconnect = () => {
      this.ready = false; this.lane.cancel(); emit(this, "disconnect", this.id);
    };
    device.addEventListener("gattserverdisconnected", this.onDisconnect);
  }
  async write(characteristic, bytes, withResponse = false) {
    if (!characteristic || !this.device.gatt.connected) throw new Error("Device is disconnected");
    if (!withResponse && characteristic.properties.writeWithoutResponse) {
      await characteristic.writeValueWithoutResponse(bytes);
    } else { await characteristic.writeValueWithResponse(bytes); }
  }
  async subscribe(characteristic, handler) {
    const listener = event => handler(event.target.value);
    characteristic.addEventListener("characteristicvaluechanged", listener);
    this.listeners.push([characteristic, listener]);
    await this.lane.run(() => characteristic.startNotifications());
  }
  async connect(config, valid = () => true) {
    const check = () => { if (this.closed || !valid()) throw new Error("Connection cancelled"); };
    let timer;
    try {
      const connection = (async () => {
        const server = await this.device.gatt.connect();
        if (this.closed || !valid()) { this.device.gatt.disconnect(); check(); }
        const uuids = UUIDS[this.version];
        const service = await server.getPrimaryService(uuids.service);
        check();
        if (this.version === "v3") {
          this.command = await service.getCharacteristic(uuids.write);
          check();
          await this.stop(); // Saved strengths are cleared before configuration or playback.
          check();
          await this.lane.run(() => this.write(this.command, encodeBf(config)));
          check();
          const notify = await service.getCharacteristic(uuids.notify);
          await this.subscribe(notify, value => {
            try {
              const message = parseB1(value);
              if (message) {
                this.queue.onB1(message); this.strengths = [message.a, message.b];
                emit(this, "status", { message });
              }
            } catch (error) { emit(this, "notice", error.message); }
          });
        } else {
          this.command = await service.getCharacteristic(uuids.strength);
          this.waveA = await service.getCharacteristic(uuids.waveA);
          this.waveB = await service.getCharacteristic(uuids.waveB);
          check(); await this.stop(); check();
          await this.subscribe(this.command, value => {
            this.strengths = decodeV2Strength(value); emit(this, "status", { message: "V2 strength" });
          });
          this.strengths = decodeV2Strength(await this.lane.run(() => this.command.readValue()));
        }
        check();
        try {
          const batteryService = await server.getPrimaryService(uuids.batteryService);
          this.batteryChar = await batteryService.getCharacteristic(uuids.battery);
          try { await this.subscribe(this.batteryChar, value => {
            this.battery = bytesOf(value)[0]; emit(this, "status", { battery: this.battery });
          }); } catch { /* Battery notifications are optional. */ }
          await this.readBattery();
        } catch { /* A missing battery service does not prevent control. */ }
        check(); this.ready = true; emit(this, "status", {});
      })();
      await Promise.race([connection, new Promise((_, reject) => {
        timer = setTimeout(() => { this.disconnect(); reject(new Error("Connection timed out")); }, 10000);
      })]);
    } catch (error) { this.disconnect(); throw error; }
    finally { clearTimeout(timer); }
  }
  async readBattery() {
    if (!this.batteryChar) return;
    const value = await this.lane.run(() => this.batteryChar.readValue());
    if (value !== false) { this.battery = bytesOf(value)[0]; emit(this, "status", {}); }
  }
  async configure(config) {
    if (this.version === "v3") await this.lane.run(() => this.write(this.command, encodeBf(config)));
  }
  setStrength(a, b) {
    if (this.version === "v3") { this.queue.set(0, a); this.queue.set(1, b); }
    else this.pendingV2 = [a, b];
  }
  async tick(frame, valid = () => true) {
    if (!this.ready) return;
    return this.lane.run(async () => {
      if (!valid() || !this.ready) return false;
      if (this.version === "v3") {
        await this.write(this.command, encodeB0({ ...this.queue.tick(), ...frame }));
      } else {
        if (this.pendingV2) {
          const strengths = this.pendingV2; this.pendingV2 = null;
          await this.write(this.command, encodeV2Strength(...strengths), true);
        }
        if (!valid()) return false;
        await this.write(this.waveA, encodeV2Wave(frame.waveA));
        if (!valid()) return false;
        await this.write(this.waveB, encodeV2Wave(frame.waveB));
      }
    });
  }
  async stop() {
    this.queue.clear(); this.pendingV2 = null; this.lane.cancel();
    if (!this.command || !this.device.gatt.connected) return;
    return this.lane.run(() => this.version === "v3"
      ? this.write(this.command, encodeB0({ a: set(0), b: set(0), pulsesA: ZERO, pulsesB: ZERO }))
      : this.write(this.command, encodeV2Strength(0, 0), true));
  }
  disconnect() {
    this.closed = true; this.ready = false; this.lane.cancel();
    this.device.gatt.disconnect();
    this.device.removeEventListener("gattserverdisconnected", this.onDisconnect);
    for (const [characteristic, listener] of this.listeners) characteristic.removeEventListener("characteristicvaluechanged", listener);
  }
}

export class SimSession extends EventTarget {
  constructor(version = "v3", id = "preview") {
    super(); this.version = version; this.id = id; this.name = `Simulated Coyote ${version === "v3" ? "3.0" : "2.0"}`;
    this.strengths = [0, 0]; this.battery = 97; this.ready = true; this.simulated = true;
  }
  async connect() {}
  async configure() {}
  async readBattery() { emit(this, "status", {}); }
  setStrength(a, b) { this.strengths = [a, b]; emit(this, "status", {}); }
  async tick() {}
  async stop() { this.strengths = [0, 0]; emit(this, "status", {}); }
  disconnect() { this.ready = false; emit(this, "disconnect", this.id); }
}

export function pulses(wave, index) {
  const [frequency, intensity] = wave.frames[index % wave.frames.length];
  return Array.from({ length: 4 }, () => [frequency, intensity]);
}
export const idleFrame = { pulsesA: SILENT, pulsesB: SILENT, waveA: { x: 0, y: 0, z: 0 }, waveB: { x: 0, y: 0, z: 0 } };
