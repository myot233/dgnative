import { Controller } from "./controller.mjs";
import { WAVES } from "./data.mjs";
import { integer } from "./protocol.mjs";
import { mountWheel } from "./wheel.mjs";

const controller = new Controller();
const byId = id => document.getElementById(id);
const number = id => Number(byId(id).value);
const parameters = new URLSearchParams(location.search);
const initialLimit = Number(parameters.get("limit") ?? 20);
if (Number.isInteger(initialLimit) && initialLimit >= 0 && initialLimit <= 200) {
  controller.config.limitA = controller.config.limitB = initialLimit;
}
byId("limitA").value = controller.config.limitA;
byId("limitB").value = controller.config.limitB;
const channel = typeof BroadcastChannel === "function" ? new BroadcastChannel("dgnative-control") : null;
let busy = false;
const lastDesired = [0, 0];

function log(message) {
  const row = document.createElement("li");
  row.textContent = `${new Date().toLocaleTimeString()}  ${message}`;
  byId("events").prepend(row);
  while (byId("events").children.length > 40) byId("events").lastChild.remove();
}
function reportError(error) { byId("message").textContent = error.message; log(error.message); }
async function action(operation) {
  if (busy) return;
  busy = true; render();
  try { await operation(); byId("message").textContent = ""; }
  catch (error) { reportError(error); }
  finally { busy = false; render(); }
}
function requestStop() {
  channel?.postMessage({ type: "stop" });
  void controller.stop().catch(reportError);
}
channel?.addEventListener("message", event => {
  if (event.data?.type === "stop") void controller.stop("STOP from another tab").catch(reportError);
});

function render() {
  const status = controller.status();
  byId("connection").textContent = status.online
    ? status.simulated ? "Offline preview" : `${status.devices.length} device(s) connected` : "No device connected";
  byId("connection").dataset.connected = status.online;
  byId("connect").disabled = busy;
  byId("preview").disabled = busy;
  byId("disconnectAll").disabled = busy || !controller.sessions.length;
  byId("model").disabled = busy || controller.sessions.length > 0;
  byId("play").disabled = busy || !status.online;
  byId("readBattery").disabled = busy || !status.online;
  byId("applyConfig").disabled = busy;
  document.querySelectorAll("[data-nudge]").forEach(button => { button.disabled = busy || !status.online; });
  byId("v2Controls").hidden = controller.version !== "v2";
  byId("v3Controls").hidden = controller.version !== "v3";
  byId("limitNote").textContent = controller.version === "v3"
    ? "Limits and balance parameters are saved on the device across power cycles. Configuration changes stop output."
    : "V2 strengths use raw protocol units (app level x 7). Limits are enforced by this page only.";
  for (const id of ["limitA", "limitB"]) byId(id).max = controller.version === "v3" ? 200 : 2047;
  for (const [i, id] of ["requestedA", "requestedB"].entries()) {
    byId(id).max = i === 0 ? controller.config.limitA : controller.config.limitB;
    if (lastDesired[i] !== controller.desired[i] && document.activeElement !== byId(id)) byId(id).value = controller.desired[i];
    lastDesired[i] = controller.desired[i];
  }
  byId("deviceList").replaceChildren();
  for (const session of controller.sessions) {
    const row = document.createElement("div"); row.className = "device-row";
    const label = document.createElement("span");
    label.textContent = `${session.name} · battery ${session.battery ?? "—"}% · A ${session.strengths[0] ?? "—"} / B ${session.strengths[1] ?? "—"}`;
    const button = document.createElement("button"); button.textContent = "Disconnect"; button.disabled = busy;
    button.onclick = () => { void action(() => controller.disconnect(session.id)); };
    row.append(label, button); byId("deviceList").append(row);
  }
  for (const [id, known] of controller.known) if (!controller.sessions.some(s => s.id === id)) {
    const button = document.createElement("button"); button.textContent = `Reconnect ${known.device.name}`; button.disabled = busy;
    button.onclick = () => { void action(() => controller.reconnect(id)); };
    byId("deviceList").append(button);
  }
}

function config() {
  return Object.fromEntries(["limitA", "limitB", "frequencyA", "frequencyB", "intensityA", "intensityB"].map(id => [id, number(id)]));
}
function manual(continuePlayback = false) {
  return controller.manual({ wave: byId("wave").value, strengths: [number("requestedA"), number("requestedB")],
    seconds: number("duration"), v2wave: { x: number("v2x"), y: number("v2y"), z: number("v2z") }, continuePlayback });
}

WAVES.forEach(wave => {
  const option = document.createElement("option"); option.value = wave.name;
  option.textContent = `${wave.label}${wave.official ? " · official" : ""}`; byId("wave").append(option);
});
byId("connect").onclick = () => { void action(() => controller.choose(byId("model").value)); };
byId("preview").onclick = () => { void action(() => controller.preview(byId("model").value)); };
byId("disconnectAll").onclick = () => { void action(() => controller.disconnectAll()); };
byId("readBattery").onclick = () => { void action(() => Promise.all(controller.sessions.map(s => s.readBattery()))); };
byId("emergencyStop").onclick = requestStop;
byId("play").onclick = () => { void action(manual); };
byId("model").onchange = () => { controller.version = byId("model").value; render(); };
byId("outputChannel").onchange = () => {
  void action(async () => { await controller.stop("Output channel changed"); controller.channel = byId("outputChannel").value; });
};
byId("applyConfig").onclick = () => { void action(async () => {
  const chance = integer(number("fakeoutSetting"), 0, 100, "Fake-stop chance");
  await controller.configure(config()); controller.fakeoutChance = chance;
  controller.dispatchEvent(new Event("status"));
}); };
document.querySelectorAll("[data-nudge]").forEach(button => {
  button.onclick = () => { void action(() => {
    const input = byId(button.dataset.channel === "a" ? "requestedA" : "requestedB");
    input.value = Math.max(0, Math.min(Number(input.max), Number(input.value) + Number(button.dataset.nudge)));
    return manual(true);
  }); };
});

controller.addEventListener("status", render);
controller.addEventListener("notice", event => log(event.detail));
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    clearInterval(controller.timer); controller.timer = null;
    void controller.stop("Page hidden; Auto and output stopped").catch(reportError);
  } else { controller.lastTick = null; controller.startLoop(); }
});
window.addEventListener("pagehide", () => {
  channel?.close(); void controller.disconnectAll().catch(() => {});
});
document.addEventListener("keydown", event => {
  if (event.key === "Escape") { event.preventDefault(); requestStop(); return; }
  if (event.target.closest("input, select, textarea, button")) return;
  if (event.key === " " || event.key === "0") { event.preventDefault(); requestStop(); }
  else if (["ArrowUp", "+", "k", "ArrowDown", "-", "j"].includes(event.key) && controller.online) {
    event.preventDefault();
    const delta = ["ArrowUp", "+", "k"].includes(event.key) ? 1 : -1;
    void action(() => {
      for (const [i, id] of ["requestedA", "requestedB"].entries()) {
        if (controller.channel === "a" && i === 1 || controller.channel === "b" && i === 0) continue;
        byId(id).value = Math.max(0, Math.min(Number(byId(id).max), number(id) + delta));
      }
      return manual(true);
    });
  }
});

mountWheel(controller, requestStop);
render();
if (!globalThis.isSecureContext || !navigator.bluetooth) {
  byId("message").textContent = "Bluetooth requires Chrome and HTTPS or localhost. Offline preview is available in this browser.";
}
