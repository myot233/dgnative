import test from "node:test";
import assert from "node:assert/strict";
import { Controller } from "../controller.mjs";
import { BleSession, SerialLane } from "../bluetooth.mjs";
import { UUIDS, encodeB0 } from "../protocol.mjs";

function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
const flush = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

function device(version = "v3") {
  const ops = [], uuids = UUIDS[version];
  class Characteristic extends EventTarget {
    constructor(uuid) { super(); this.uuid = uuid; this.properties = { writeWithoutResponse: true }; this.value = new DataView(Uint8Array.of(97).buffer); }
    async writeValueWithoutResponse(value) {
      ops.push({ type: "write", uuid: this.uuid, bytes: [...value] });
      if (this.gate) { const gate = this.gate; this.gate = null; await gate.promise; }
    }
    async writeValueWithResponse(value) { await this.writeValueWithoutResponse(value); }
    async startNotifications() { ops.push({ type: "subscribe", uuid: this.uuid }); }
    async readValue() { ops.push({ type: "read", uuid: this.uuid }); return this.uuid === uuids.strength ? new DataView(Uint8Array.of(0, 0, 0).buffer) : this.value; }
    notify(bytes) { this.value = new DataView(Uint8Array.from(bytes).buffer); this.dispatchEvent(new Event("characteristicvaluechanged")); }
  }
  const chars = Object.fromEntries(Object.values(uuids).map(uuid => [uuid, new Characteristic(uuid)]));
  const d = new EventTarget(); Object.assign(d, { id: "owned-device", name: version === "v3" ? "47L121000" : "D-LAB ESTIM01", ops, chars });
  d.gatt = {
    connected: false,
    async connect() { if (this.gate) await this.gate.promise; this.connected = true; return this; },
    async getPrimaryService(uuid) { ops.push({ type: "service", uuid }); return { getCharacteristic: async id => chars[id] }; },
    disconnect() { const connected = this.connected; this.connected = false; if (connected) d.dispatchEvent(new Event("gattserverdisconnected")); },
  };
  return d;
}

test("real transport initializes with zero before BF, subscribes and reads battery", async () => {
  const d = device(), session = new BleSession(d);
  await session.connect({ limitA: 20, limitB: 20 });
  const writes = d.ops.filter(o => o.type === "write");
  assert.deepEqual(writes[0].bytes.slice(0, 4), [0xb0, 15, 0, 0]);
  assert.deepEqual(writes[1].bytes, [0xbf, 20, 20, 160, 160, 0, 0]);
  assert.equal(session.battery, 97); assert.equal(session.ready, true);
  d.chars[UUIDS.v3.notify].notify([0xb1, 0, 5, 6]); assert.deepEqual(session.strengths, [5, 6]);
  session.disconnect();
});

test("STOP cancels queued packets and preserves sequence numbers across a delayed write", async () => {
  const d = device(), session = new BleSession(d); await session.connect({ limitA: 150, limitB: 150 });
  d.ops.length = 0;
  const gate = deferred(); d.chars[UUIDS.v3.write].gate = gate;
  session.setStrength(100, 100); const first = session.tick({}); await flush();
  session.setStrength(120, 120); const queued = session.tick({});
  const stop = session.stop(); gate.resolve(); await Promise.all([first, queued, stop]);
  const writes = d.ops.filter(o => o.type === "write");
  assert.equal(writes.length, 2); assert.deepEqual(writes.at(-1).bytes.slice(0, 4), [0xb0, 15, 0, 0]);
  assert.equal(writes.some(w => w.bytes[2] === 120), false);
  session.setStrength(10, 10); await session.tick({});
  assert.equal(d.ops.at(-1).bytes[1] >> 4, 2, "an old sequence-1 reply must not confirm the next request");
  session.disconnect();
});

test("GATT operations never overlap; stalled operations disconnect instead of accumulating packets", async () => {
  let concurrent = 0, peak = 0, timedOut = false;
  const lane = new SerialLane(() => { timedOut = true; }, 30), gate = deferred();
  const operation = async wait => { peak = Math.max(peak, ++concurrent); if (wait) await gate.promise; --concurrent; };
  const first = lane.run(() => operation(true)); const second = lane.run(() => operation(false));
  await flush(); assert.equal(peak, 1); gate.resolve(); await Promise.all([first, second]); assert.equal(peak, 1);
  await assert.rejects(lane.run(() => new Promise(() => {})), /timed out/); assert.equal(timedOut, true);
});

test("V2 transport uses its private UUIDs, raw strength and zero before output", async () => {
  const d = device("v2"), session = new BleSession(d, "v2"); await session.connect({});
  assert.deepEqual(d.ops.find(o => o.type === "write").bytes, [0, 0, 0]);
  session.setStrength(7, 7); await session.tick({ waveA: { x: 1, y: 9, z: 4 }, waveB: { x: 0, y: 0, z: 0 } });
  const writes = d.ops.filter(o => o.type === "write");
  assert.equal(writes.at(-2).uuid, UUIDS.v2.waveA); assert.deepEqual(writes.at(-2).bytes, [0x21, 1, 2]);
  session.disconnect();
});

test("draws produce no output until start; tokens are single-use and STOP invalidates them", async () => {
  const hub = new Controller({ autoRun: false }); await hub.preview(); hub.fakeoutChance = 100;
  const draw = await hub.draw(); assert.deepEqual(hub.sessions[0].strengths, [0, 0]); assert.equal(hub.active, null);
  await hub.start(draw.token); assert.ok(hub.desired[0] > 0); assert.ok(hub.desired[0] <= 20);
  await assert.rejects(hub.start(draw.token), /already used/);
  const next = await hub.draw(); await hub.stop(); await assert.rejects(hub.start(next.token), /cancelled/);
  const old = await hub.draw(), newest = await hub.draw(); await assert.rejects(hub.start(old.token)); await hub.start(newest.token);
  await hub.disconnectAll();
});

test("STOP while the draw awaits zero prevents a late response from rearming output", async () => {
  const hub = new Controller({ autoRun: false }); await hub.preview();
  const gate = deferred(); hub.sessions[0].stop = () => gate.promise;
  const pending = hub.draw(); const stopped = hub.stop(); gate.resolve();
  await assert.rejects(pending, /cancelled/); await stopped; assert.equal(hub.pending, null); assert.equal(hub.active, null);
  await hub.disconnectAll();
});

test("manual changes preserve waveform phase, clamp limits, finish at zero and never resume after STOP", async () => {
  let now = 0; const hub = new Controller({ autoRun: false, now: () => now }); await hub.preview();
  await hub.manual({ strengths: [5, 6], seconds: 2 }); const started = hub.active.started;
  now = 500; await hub.manual({ strengths: [6, 7], seconds: 2, continuePlayback: true }); assert.equal(hub.active.started, started);
  await assert.rejects(hub.manual({ strengths: [21, 0] }), /A strength/);
  now = 2000; await hub.tick(); assert.equal(hub.active, null); assert.deepEqual(hub.sessions[0].strengths, [0, 0]);
  await hub.stop(); now = 3000; await hub.tick(); assert.deepEqual(hub.sessions[0].strengths, [0, 0]);
  await hub.disconnectAll();
});

test("a scheduling gap cancels physical playback instead of sending late bursts", async () => {
  let now = 0; const hub = new Controller({ autoRun: false, now: () => now }); const d = device(); await hub.connect(d, "v3");
  await hub.manual({ strengths: [5, 5], wave: "full", seconds: 60 });
  now = 600; await hub.tick(); assert.equal(hub.active, null); assert.deepEqual(hub.desired, [0, 0]);
  assert.deepEqual(d.ops.filter(o => o.type === "write").at(-1).bytes.slice(0, 4), [0xb0, 15, 0, 0]);
  await hub.disconnectAll();
});

test("STOP during connection closes even a GATT connect that completes late", async () => {
  const d = device(), gate = deferred(); d.gatt.gate = gate;
  const hub = new Controller({ autoRun: false }); const connecting = hub.connect(d, "v3");
  await flush(); await hub.stop(); gate.resolve(); await assert.rejects(connecting, /cancelled/);
  assert.equal(d.gatt.connected, false); assert.equal(hub.sessions.length, 0);
});

test("device selection runs in the click task and a second owning tab is refused", async () => {
  const order = [], d = device();
  const hub = new Controller({ autoRun: false, bluetooth: { requestDevice(options) {
    order.push("picker"); assert.deepEqual(options.filters, [{ namePrefix: "47L121000" }]); return Promise.resolve(d);
  } }, locks: { request(_name, _options, callback) { order.push("lock"); return Promise.resolve(callback(null)); } } });
  await assert.rejects(hub.choose(), /Another tab/); assert.deepEqual(order, ["picker", "lock"]);
  assert.equal(d.gatt.connected, false);
});
