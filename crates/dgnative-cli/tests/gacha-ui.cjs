// Run with: node crates/dgnative-cli/tests/gacha-ui.cjs
// A controlled clock checks the real inline script without connecting to hardware.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const html = fs.readFileSync(path.join(__dirname, '../src/gacha.html'), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const prizes = [
  {label: 'Air', wave: 'breathing', strength_pct: 0, seconds: 3, weight: 16, color: '#333', final_chance_pct: 24},
  {label: 'Breeze', wave: 'breathing', strength_pct: 15, seconds: 4, weight: 15, color: '#666', final_chance_pct: 22},
  {label: 'Thunder', wave: 'full', strength_pct: 100, seconds: 5, weight: 3, color: '#999', final_chance_pct: 54},
];

async function flush() { for (let i = 0; i < 20; i++) await Promise.resolve(); }

async function harness({preview = 1, final = preview, delayDraw = false, random = [0.5]} = {}) {
  let now = 0, frames = [], resolveDraw, firstRotation = false;
  let playback = null, playbackEnd = 0, stopGeneration = 0, disconnected = false;
  const calls = [], angles = [], labels = [], elements = new Map(), intervals = [];
  const ctx = new Proxy({
    clearRect() { firstRotation = true; },
    rotate(angle) {
      if (firstRotation) { angles.push({time: now, angle}); firstRotation = false; }
    },
  }, {get: (target, key) => key in target ? target[key] : () => {}});
  const element = id => {
    if (!elements.has(id)) {
      let text = '';
      elements.set(id, {
        style: {}, attributes: {}, innerHTML: '', disabled: false, width: 840,
        value: id === 'autoMin' ? '10' : id === 'autoMax' ? '30' : '',
        getContext: () => ctx,
        setAttribute(key, value) { this.attributes[key] = value; },
        get textContent() { return text; },
        set textContent(value) { text = value; if (id === 'hubLabel') labels.push({time: now, value}); },
      });
    }
    return elements.get(id);
  };
  const testMath = Object.create(Math);
  testMath.random = () => random.length ? random.shift() : 0.5;
  const context = vm.createContext({
    document: {getElementById: element}, Math: testMath,
    performance: {now: () => now},
    requestAnimationFrame: callback => frames.push(callback),
    setInterval(callback, period) { intervals.push({callback, period, next: now + period}); },
    fetch: async (route, options = {}) => {
      calls.push({route, time: now, body: options.body, angle: angles.at(-1)?.angle, label: element('hubLabel').textContent});
      if (disconnected) throw new Error('Disconnected');
      let data;
      if (route === '/api/prizes') data = prizes;
      else if (route === '/api/status') data = {
        devices: [], strengths: [], limit: 150, fakeout_chance: 50, stop_generation: stopGeneration,
        playing: now < playbackEnd ? playback : null,
        remaining_ms: Math.max(0, playbackEnd - now), total_ms: prizes[final].seconds * 1000,
      };
      else if (route === '/api/spin') {
        playbackEnd = 0;
        if (delayDraw) await new Promise(resolve => { resolveDraw = resolve; });
        data = {preview_index: preview, index: final, prize: prizes[final], token: 'reserved-result'};
      } else {
        if (route === '/api/start') { playback = prizes[final].label; playbackEnd = now + prizes[final].seconds * 1000; }
        if (route === '/api/stop') { stopGeneration++; playbackEnd = 0; }
        data = {ok: true};
      }
      return {ok: true, json: async () => data};
    },
  });
  vm.runInContext(script, context);
  await flush();
  return {
    calls, angles, labels, element,
    starts: () => calls.filter(c => c.route === '/api/start'),
    draws: () => calls.filter(c => c.route === '/api/spin'),
    resolveDraw: () => resolveDraw(),
    crossPageStop() { stopGeneration++; playbackEnd = 0; },
    disconnect() { disconnected = true; },
    async refresh() { await vm.runInContext('refreshStatus()', context); },
    async step(ms = 16) {
      now += ms;
      const batch = frames; frames = [];
      for (const timer of intervals) {
        if (now >= timer.next) { timer.next = now + timer.period; timer.callback(); }
      }
      for (const callback of batch) callback(now);
      await flush();
    },
    async advance(ms) { const end = now + ms; while (now < end) await this.step(Math.min(16, end - now)); },
    async until(predicate, timeout = 30000) {
      const end = now + timeout;
      while (!predicate() && now < end) await this.step();
      assert.ok(predicate(), 'condition did not finish within the test deadline');
    },
    get time() { return now; },
  };
}

function alignment(index, rotation) {
  const total = prizes.reduce((sum, p) => sum + p.weight, 0);
  const before = prizes.slice(0, index).reduce((sum, p) => sum + p.weight, 0);
  const center = -Math.PI / 2 + (before + prizes[index].weight / 2) / total * 2 * Math.PI;
  return Math.atan2(Math.sin(rotation + center + Math.PI / 2), Math.cos(rotation + center + Math.PI / 2));
}

async function animationTests() {
  const normal = await harness();
  const normalSpin = normal.element('spin').onclick(); await flush();
  await normal.advance(4200);
  assert.equal(normal.starts().length, 0);
  await normal.until(() => normal.starts().length === 1); await normalSpin;
  const start = normal.starts()[0];
  assert.ok(start.time >= 4240);
  assert.equal(start.label, 'Breeze');
  assert.ok(Math.abs(alignment(1, start.angle)) < 1e-9);
  assert.equal(JSON.parse(start.body).token, 'reserved-result');
  const recoil = normal.angles.filter(a => a.time >= 3600);
  assert.ok(recoil.some(a => a.angle > start.angle + 0.02));
  assert.ok(recoil.some((a, i) => i && a.angle < recoil[i - 1].angle));

  for (const preview of [0, 1]) {
    const fake = await harness({preview, final: 2});
    const spin = fake.element('spin').onclick(); await flush();
    await fake.until(() => fake.labels.some(l => l.value === prizes[preview].label));
    const apparent = fake.labels.find(l => l.value === prizes[preview].label);
    assert.equal(fake.starts().length, 0);
    const pausedAngle = fake.angles.at(-1).angle;
    await fake.advance(650);
    assert.equal(fake.starts().length, 0);
    const pausedDelta = fake.angles.at(-1).angle - pausedAngle;
    assert.ok(Math.abs(Math.atan2(Math.sin(pausedDelta), Math.cos(pausedDelta))) < 1e-9);
    await fake.until(() => fake.starts().length === 1); await spin;
    const finalStart = fake.starts()[0];
    assert.ok(finalStart.time - apparent.time >= 2000);
    assert.equal(finalStart.label, 'Thunder');
    assert.ok(Math.abs(alignment(2, finalStart.angle)) < 1e-9);
    const hop = fake.angles.filter(a => a.time > apparent.time + 700);
    assert.ok(Math.max(...hop.map(a => a.angle)) - Math.min(...hop.map(a => a.angle)) < Math.PI);
    assert.equal(fake.starts().length, 1);
  }

  for (const cancelAt of [1000, 4500, 5300, 6050]) {
    const cancelled = await harness({preview: 0, final: 2});
    const spin = cancelled.element('spin').onclick(); await flush();
    await cancelled.advance(cancelAt);
    cancelled.element('stop').onclick(); await cancelled.step(); await spin;
    await cancelled.advance(8000);
    assert.equal(cancelled.starts().length, 0, `STOP at ${cancelAt}ms must prevent output`);
    assert.equal(cancelled.element('spin').disabled, false);
  }
  const delayed = await harness({delayDraw: true});
  const delayedSpin = delayed.element('spin').onclick(); await flush();
  delayed.element('stop').onclick(); delayed.resolveDraw(); await delayedSpin;
  assert.equal(delayed.starts().length, 0);
  console.log('PASS: normal/fake stops settle and paint before output; STOP cancels every phase.');
}

async function autoTests() {
  const auto = await harness({random: [0.5, 1]});
  auto.element('autoMin').value = '1'; auto.element('autoMax').value = '3';
  auto.element('auto').onclick();
  assert.equal(auto.element('auto').attributes['aria-pressed'], 'true');
  await auto.advance(1950); assert.equal(auto.draws().length, 0);
  await auto.until(() => auto.draws().length === 1);
  assert.ok(auto.draws()[0].time >= 2000 && auto.draws()[0].time < 2120);
  await auto.until(() => auto.starts().length === 1);
  const startedAt = auto.starts()[0].time;
  await auto.advance(6900);
  assert.equal(auto.draws().length, 1, 'wait must begin after four seconds of playback');
  await auto.until(() => auto.draws().length === 2);
  assert.ok(auto.draws()[1].time >= startedAt + 7000);
  auto.element('stop').onclick(); await auto.advance(10000);
  assert.equal(auto.draws().length, 2);
  assert.equal(auto.starts().length, 1);

  const off = await harness();
  off.element('autoMin').value = off.element('autoMax').value = '1';
  off.element('auto').onclick(); off.element('auto').onclick();
  await off.advance(3000); assert.equal(off.draws().length, 0);
  off.element('auto').onclick(); off.element('stop').onclick();
  await off.advance(3000); assert.equal(off.draws().length, 0);

  const edited = await harness({random: [0, 0]});
  edited.element('auto').onclick(); await edited.advance(500);
  edited.element('autoMin').value = edited.element('autoMax').value = '1';
  edited.element('autoMin').onchange();
  await edited.advance(900); assert.equal(edited.draws().length, 0);
  await edited.until(() => edited.draws().length === 1);
  assert.ok(edited.draws()[0].time >= 1500 && edited.draws()[0].time < 1620);
  edited.element('stop').onclick(); await edited.step();

  for (const [min, max] of [['3', '1'], ['0', '1'], ['1', '3601'], ['NaN', '10'], ['1', '']]) {
    const invalid = await harness();
    invalid.element('autoMin').value = min; invalid.element('autoMax').value = max;
    invalid.element('auto').onclick(); await invalid.advance(3000);
    assert.equal(invalid.element('auto').attributes['aria-pressed'], 'false');
    assert.equal(invalid.draws().length, 0);
  }

  for (const external of ['stop', 'disconnect']) {
    const cancelled = await harness();
    cancelled.element('autoMin').value = cancelled.element('autoMax').value = '1';
    cancelled.element('auto').onclick();
    if (external === 'stop') cancelled.crossPageStop(); else cancelled.disconnect();
    await cancelled.refresh(); await cancelled.advance(3000);
    assert.equal(cancelled.draws().length, 0);
    assert.equal(cancelled.element('auto').attributes['aria-pressed'], 'false');
  }

  const background = await harness();
  background.element('autoMin').value = background.element('autoMax').value = '1';
  background.element('auto').onclick();
  await background.step(60000);
  assert.equal(background.element('auto').attributes['aria-pressed'], 'true');
  assert.equal(background.draws().length, 1, 'a throttled tab must refresh status and spin once');
  background.element('stop').onclick(); await background.step();
  console.log('PASS: Auto random bounds, playback waits, edits, toggles, STOP, validation and disconnect.');
}

(async () => { await animationTests(); await autoTests(); })().catch(error => {
  console.error(error); process.exitCode = 1;
});
