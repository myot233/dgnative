import { WAVES } from "./data.mjs";

export function mountWheel(controller, requestStop) {

// Display labels for the built-in waveforms; must match `builtin::ALL` in the library.
const WAVE_NAMES = Object.fromEntries(WAVES.map(w => [w.name, w.label]));

let prizes = [];
let angles = [];      // [start, end] radians of each segment
let rotation = 0;     // current wheel angle
let spinning = false;
let spinGeneration = 0;
let autoEnabled = false;
let autoDeadline = null;
let playbackUntil = 0;
let lastStatus = null;
let lastStatusAt = 0;
let stopGeneration = null;
let refreshingStatus = false;

const canvas = document.getElementById("wheel");
const ctx = canvas.getContext("2d");

// Segment size follows the initial draw weight; the table accounts for fake-stop upgrades.
function layout() {
  const total = prizes.reduce((s, p) => s + p.weight, 0);
  let at = -Math.PI / 2;   // start straight up (pointer position)
  angles = prizes.map(p => {
    const span = (p.weight / total) * Math.PI * 2;
    const seg = [at, at + span];
    at += span;
    return seg;
  });
}

function drawWheel() {
  const size = canvas.width;
  const r = size / 2;
  ctx.clearRect(0, 0, size, size);
  ctx.save();
  ctx.translate(r, r);
  ctx.rotate(rotation);

  prizes.forEach((p, i) => {
    const [a0, a1] = angles[i];
    ctx.beginPath();
    ctx.moveTo(0, 0);
    ctx.arc(0, 0, r - 6, a0, a1);
    ctx.closePath();
    ctx.fillStyle = p.color;
    ctx.fill();
    ctx.strokeStyle = "rgba(0,0,0,.35)";
    ctx.lineWidth = 2;
    ctx.stroke();

    // Labels run along the radius: text length extends toward the center, so segment
    // width does not limit it; the font size must fit inside the segment's arc width,
    // otherwise text from adjacent segments overlaps
    const span = a1 - a0;
    const labelR = r * 0.72;              // radius of the label center
    const arc = span * labelR;            // arc width available there
    const size = Math.max(17, Math.min(38, arc * 0.5));

    ctx.save();
    ctx.rotate((a0 + a1) / 2);
    ctx.textAlign = "right";
    ctx.textBaseline = "middle";
    ctx.fillStyle = "rgba(255,255,255,.96)";
    ctx.font = `600 ${size}px -apple-system, 'PingFang SC', sans-serif`;

    // Second line only when the arc is tall enough, otherwise keep just the prize name (the table always shows it)
    if (arc > size * 2.5) {
      ctx.fillText(p.label, r - 26, -size * 0.5);
      ctx.font = `500 ${size * 0.62}px -apple-system, 'PingFang SC', sans-serif`;
      ctx.fillStyle = "rgba(255,255,255,.74)";
      ctx.fillText(`${p.strength_pct}% / ${p.seconds}s`, r - 26, size * 0.6);
    } else {
      ctx.fillText(p.label, r - 26, 0);
    }
    ctx.restore();
  });

  ctx.restore();
}

const easeOut = t => 1 - Math.pow(1 - t, 3);
const easeInOut = t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
const easeOutBack = t => {
  const x = t - 1;
  return 1 + 2.70158 * x * x * x + 1.70158 * x * x;
};

function animateRotation(target, ms, ease, cancelled) {
  return new Promise(resolve => {
    const from = rotation, start = performance.now();
    function frame(now) {
      if (cancelled()) { resolve(false); return; }
      const t = Math.min(1, (now - start) / ms);
      rotation = from + (target - from) * ease(t);
      drawWheel();
      if (t < 1) requestAnimationFrame(frame);
      else resolve(true);
    }
    requestAnimationFrame(frame);
  });
}

// Overshoot the target, then recoil to its center before output can begin.
async function spinTo(index, cancelled, shortHop = false) {
  const [a0, a1] = angles[index];
  const center = (a0 + a1) / 2;
  // The initial spin makes five extra turns; a fake-stop hop takes the shortest circular path.
  const twoPi = Math.PI * 2;
  let delta = (-Math.PI / 2 - center - rotation) % twoPi;
  if (delta < 0) delta += twoPi;
  if (shortHop && delta > Math.PI) delta -= twoPi;
  const target = rotation + delta + (shortHop ? 0 : twoPi * 5);

  const overshoot = Math.min(0.14, (a1 - a0) * 0.3) * (Math.sign(delta) || 1);
  if (!await animateRotation(target + overshoot, shortHop ? 850 : 3600,
    shortHop ? easeInOut : easeOut, cancelled)) return false;
  if (!await animateRotation(target, shortHop ? 450 : 600, easeOutBack, cancelled)) return false;
  rotation %= twoPi;
  // Give the final canvas frame time to paint before requesting device output.
  await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  return !cancelled();
}

function renderTable() {
  document.getElementById("prizeTable").innerHTML = prizes.map(p => `
    <tr>
      <td><span class="swatch" style="background:${p.color}"></span>${p.label}</td>
      <td>${WAVE_NAMES[p.wave] || p.wave}</td>
      <td class="n">${p.strength_pct}%</td>
      <td class="n">${p.seconds}s</td>
      <td class="n">${p.final_chance_pct.toFixed(1)}%</td>
    </tr>`).join("");
}

function autoRange() {
  const min = Number(document.getElementById("autoMin").value);
  const max = Number(document.getElementById("autoMax").value);
  if (!Number.isFinite(min) || !Number.isFinite(max) || min < 1 || max > 3600 || max < min) {
    throw new Error("Choose intervals from 1 to 3600 seconds, with max at least min");
  }
  return [min, max];
}

function setAuto(enabled, message) {
  autoEnabled = enabled;
  autoDeadline = null;
  const button = document.getElementById("auto");
  button.setAttribute("aria-pressed", String(enabled));
  button.textContent = enabled ? "Auto: On" : "Auto";
  document.getElementById("autoStatus").textContent = message ||
    (enabled ? "Waiting for the next spin" : "Auto off · intervals start after playback finishes");
}

function autoTick() {
  if (!autoEnabled) return;
  if (!lastStatus || performance.now() - lastStatusAt > 2000) {
    // A background tab may throttle timers; refresh before deciding whether output is idle.
    void refreshStatus();
    return;
  }
  const status = document.getElementById("autoStatus");
  if (spinning) { status.textContent = "Spinning…"; return; }
  if (lastStatus.playing || performance.now() < playbackUntil) {
    autoDeadline = null;
    status.textContent = "Waiting for playback to finish";
    return;
  }
  if (autoDeadline === null) {
    try {
      const [min, max] = autoRange();
      autoDeadline = performance.now() + (min + Math.random() * (max - min)) * 1000;
    } catch (error) { setAuto(false, error.message); return; }
  }
  const remaining = autoDeadline - performance.now();
  if (remaining <= 0) {
    autoDeadline = null;
    void spin();
  } else {
    status.textContent = `Next spin in ${(remaining / 1000).toFixed(1)}s`;
  }
}

async function refreshStatus() {
  if (refreshingStatus) return;
  refreshingStatus = true;
  try {
    const s = controller.status();
    document.getElementById("spin").disabled = spinning || !s.online || s.version !== "v3";
    document.getElementById("auto").disabled = !s.online || s.version !== "v3";
    if (prizes[0]?.applied_chance !== s.fakeout_chance) {
      prizes = controller.prizes().map(p => ({ ...p, applied_chance: s.fakeout_chance }));
      renderTable();
    }
    if (stopGeneration !== null && s.stop_generation !== stopGeneration) {
      ++spinGeneration;
      setAuto(false);
    }
    stopGeneration = s.stop_generation;
    lastStatus = s;
    lastStatusAt = performance.now();
    document.getElementById("devices").textContent =
      s.devices.length ? s.devices.join(" / ") : "none";
    document.getElementById("limit").textContent = s.limit;
    document.getElementById("fakeoutChance").textContent = `${s.fakeout_chance}%`;
    document.getElementById("strength").textContent =
      s.strengths.map(([a, b]) => `A${a} B${b}`).join("  ") || "—";

    // Do not overwrite the hub while spinning, to avoid fighting the result display
    if (!spinning) {
      const bar = document.getElementById("bar");
      if (s.playing) {
        document.getElementById("hubLabel").textContent = s.playing;
        document.getElementById("hubDetail").textContent =
          (s.remaining_ms / 1000).toFixed(1) + "s";
        bar.style.width = s.total_ms
          ? (100 - s.remaining_ms / s.total_ms * 100).toFixed(1) + "%" : "0";
      } else {
        document.getElementById("hubLabel").textContent = "Ready";
        document.getElementById("hubDetail").textContent = "Press the button below";
        bar.style.width = "0";
      }
    }
    if (autoEnabled) autoTick();
  } catch (e) { if (autoEnabled) setAuto(false, "Auto stopped: connection lost"); }
  finally { refreshingStatus = false; }
}

async function spin() {
  if (spinning || !controller.online || controller.version !== "v3") return;
  spinning = true;
  autoDeadline = null;
  playbackUntil = 0;
  const generation = ++spinGeneration;
  const cancelled = () => generation !== spinGeneration;
  const btn = document.getElementById("spin");
  btn.disabled = true;
  document.getElementById("hubLabel").textContent = "…";
  document.getElementById("hubDetail").textContent = "";

  try {
    const r = await controller.draw();
    if (cancelled() || !await spinTo(r.preview_index, cancelled)) return;
    if (r.preview_index !== r.index) {
      const preview = prizes[r.preview_index];
      document.getElementById("hubLabel").textContent = preview.label;
      document.getElementById("hubDetail").textContent =
        `${preview.strength_pct}% / ${preview.seconds}s`;
      // Hold the apparent result without sending any output, then reveal the higher result.
      if (!await animateRotation(rotation, 700, easeOut, cancelled)) return;
      if (!await spinTo(r.index, cancelled, true)) return;
    }
    document.getElementById("hubLabel").textContent = r.prize.label;
    document.getElementById("hubDetail").textContent =
      `${r.prize.strength_pct}% / ${r.prize.seconds}s`;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    if (cancelled()) return;
    await controller.start(r.token);
    if (!cancelled()) playbackUntil = performance.now() + r.prize.seconds * 1000;
  } catch (error) {
    if (!cancelled()) {
      setAuto(false, "Auto stopped: " + error.message);
      document.getElementById("hubLabel").textContent = "Error";
      document.getElementById("hubDetail").textContent = error.message;
    }
  } finally {
    spinning = false;
    btn.disabled = !controller.online || controller.version !== "v3";
  }
}

document.getElementById("spin").onclick = spin;

document.getElementById("auto").onclick = () => {
  if (autoEnabled) { setAuto(false); return; }
  try {
    autoRange();
    if (!lastStatus || performance.now() - lastStatusAt > 2000) throw new Error("Connect a device or enable offline preview before enabling Auto");
    setAuto(true);
    autoTick();
  } catch (error) { setAuto(false, error.message); }
};

for (const id of ["autoMin", "autoMax"]) {
  document.getElementById(id).onchange = () => {
    if (!autoEnabled) return;
    try { autoRange(); } catch (error) { setAuto(false, error.message); return; }
    autoDeadline = null;
    autoTick();
  };
}

document.getElementById("stop").onclick = () => {
  ++spinGeneration;
  setAuto(false);
  playbackUntil = 0;
  requestStop();
};

(async () => {
  prizes = controller.prizes();
  layout();
  drawWheel();
  renderTable();
  refreshStatus();
  setInterval(refreshStatus, 250);
  setInterval(autoTick, 100);
})();

controller.addEventListener("stop", () => {
  ++spinGeneration; setAuto(false); playbackUntil = 0;
  void refreshStatus();
});

}
