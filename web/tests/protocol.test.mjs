import test from "node:test";
import assert from "node:assert/strict";
import { encodeB0, encodeBf, parseB1, StrengthQueue, KEEP, encodeV2Wave, encodeV2Strength, decodeV2Strength, UUIDS } from "../protocol.mjs";
import { nearestHigher, finalOdds, draw } from "../gacha.mjs";
import { PRIZES, WAVES } from "../data.mjs";

const hex = bytes => Buffer.from(bytes).toString("hex").toUpperCase();
const pulses = (frequency, intensity) => frequency.map((f, i) => [f, intensity[i]]);
const silent = pulses([0, 0, 0, 0], [0, 0, 0, 101]);

test("V3 commands match all existing official HEX vectors", () => {
  const a = pulses([10, 10, 10, 10], [0, 10, 20, 30]);
  assert.equal(hex(encodeB0({ pulsesA: a, pulsesB: silent })), "B00000000A0A0A0A000A141E0000000000000065");
  assert.equal(hex(encodeB0({ a: { mode: 1, value: 5 }, pulsesA: a, pulsesB: silent })), "B00405000A0A0A0A000A141E0000000000000065");
  assert.equal(hex(encodeB0({ sequence: 1, a: { mode: 1, value: 10 }, pulsesA: pulses([40, 60, 80, 100], [100, 90, 90, 90]), pulsesB: silent })), "B0140A00283C5064645A5A5A0000000000000065");
  assert.equal(hex(encodeB0({ pulsesA: pulses([15, 15, 15, 15], [40, 50, 60, 70]), pulsesB: pulses([10, 10, 10, 10], [10, 10, 10, 10]) })), "B00000000F0F0F0F28323C460A0A0A0A0A0A0A0A");
  assert.equal(hex(encodeBf({ limitA: 150, limitB: 30 })), "BF961EA0A00000");
  assert.deepEqual(parseB1(Uint8Array.of(0xb1, 1, 25, 0)), { sequence: 1, a: 25, b: 0 });
  assert.equal(parseB1(Uint8Array.of(0xbe, 1, 2)), null);
  assert.throws(() => parseB1([])); assert.throws(() => parseB1([0xb1, 1]));
  assert.throws(() => encodeBf({ limitA: 201 }));
  assert.throws(() => encodeB0({ a: { mode: 3, value: 201 } }));
});

test("B1 parsing respects DataView offsets", () => {
  const bytes = Uint8Array.of(0xff, 0xb1, 4, 20, 30, 0xff);
  assert.deepEqual(parseB1(new DataView(bytes.buffer, 1, 4)), { sequence: 4, a: 20, b: 30 });
});

test("strength changes wait for matching B1, accumulate and STOP bypasses confirmation", () => {
  const queue = new StrengthQueue();
  queue.adjust(0, 1); assert.deepEqual(queue.tick(), { sequence: 1, a: { mode: 1, value: 1 }, b: KEEP });
  queue.adjust(0, 3); queue.onB1({ sequence: 7 }); assert.equal(queue.tick().sequence, 0);
  queue.onB1({ sequence: 1 }); assert.equal(queue.tick().a.value, 3);
  queue.set(0, 10); queue.adjust(0, 2); queue.zeroNow();
  const zero = queue.tick(); assert.equal(zero.a.value, 0); assert.equal(zero.b.value, 0); assert.equal(zero.a.mode, 3);
  queue.set(0, 8);
  for (let i = 0; i < 9; i++) assert.equal(queue.tick().sequence, 0);
  assert.equal(queue.tick().a.value, 8);
});

test("V2 packing matches official examples and raw strength roundtrips", () => {
  for (const [x, y, z, expected] of [[1, 9, 0, "210100"], [1, 9, 4, "210102"], [1, 9, 20, "21010A"], [1, 10, 3, "418101"], [1, 15, 13, "E18106"], [1, 34, 20, "41040A"], [1, 41, 13, "218506"]]) {
    assert.equal(hex(encodeV2Wave({ x, y, z })), expected);
  }
  for (const strengths of [[0, 0], [7, 7], [2047, 0], [0, 2047], [700, 1234]]) assert.deepEqual(decodeV2Strength(encodeV2Strength(...strengths)), strengths);
  assert.throws(() => encodeV2Strength(2048, 0)); assert.throws(() => encodeV2Wave({ x: 32, y: 1, z: 0 }));
  assert.equal(UUIDS.v2.strength, "955a1504-0fe2-f5aa-a094-84b8d4f3e8ad");
});

test("fake-stop upgrades use circular distance and preserve final probability", () => {
  assert.equal(nearestHigher(0), 12, "Air upgrades across the pointer to Thunder");
  assert.equal(nearestHigher(12), 12);
  for (let i = 0; i < PRIZES.length - 1; i++) assert.ok(PRIZES[nearestHigher(i)].strength_pct > PRIZES[i].strength_pct);
  for (const chance of [0, 50, 100]) assert.ok(Math.abs(finalOdds(chance).reduce((sum, p) => sum + p, 0) - 1) < 1e-12);
  assert.deepEqual(draw(50, () => 0), { preview_index: 0, index: 12, prize: PRIZES[12] });
  const rolls = [0, 99]; assert.equal(draw(50, () => rolls.shift()).index, 0);
  assert.equal(WAVES.length, 8);
  for (const wave of WAVES) for (const [f, i] of wave.frames) { assert.ok(f >= 10 && f <= 240); assert.ok(i >= 0 && i <= 100); }
});
