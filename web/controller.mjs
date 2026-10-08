import { INTERVAL_MS, UUIDS, integer, encodeBf, encodeV2Wave } from "./protocol.mjs";
import { WAVES, PRIZES } from "./data.mjs";
import { BleSession, SimSession, pulses, idleFrame, emit } from "./bluetooth.mjs";
import { draw, finalOdds } from "./gacha.mjs";

export class Controller extends EventTarget {
  constructor({ bluetooth = globalThis.navigator?.bluetooth, locks = globalThis.navigator?.locks,
    now = () => performance.now(), sessionFactory = (device, version) => new BleSession(device, version), autoRun = true } = {}) {
    super(); Object.assign(this, { bluetooth, locks, now, sessionFactory, autoRun });
    this.sessions = []; this.connecting = new Set(); this.known = new Map(); this.version = "v3";
    this.config = { limitA: 20, limitB: 20, frequencyA: 160, frequencyB: 160, intensityA: 0, intensityB: 0 };
    this.fakeoutChance = 50; this.channel = "both"; this.desired = [0, 0];
    this.revision = 0; this.stopGeneration = 0; this.active = null; this.pending = null;
    this.timer = null; this.ticking = false; this.lastTick = null; this.releaseLock = null;
  }
  get limit() { return this.channel === "a" ? this.config.limitA : this.channel === "b" ? this.config.limitB : Math.min(this.config.limitA, this.config.limitB); }
  get online() { return this.sessions.length > 0 && this.sessions.every(s => s.ready); }
  notice(message) { emit(this, "notice", message); }
  status() {
    const remaining = this.active?.until === Infinity ? 0 : Math.max(0, (this.active?.until || 0) - this.now());
    return { devices: this.sessions.map(s => s.name), strengths: this.sessions.map(s => s.strengths),
      limit: this.limit, fakeout_chance: this.fakeoutChance, stop_generation: this.stopGeneration,
      playing: this.active?.label || null, remaining_ms: remaining, total_ms: this.active?.duration || 0,
      online: this.online, version: this.version, simulated: this.sessions.some(s => s.simulated) };
  }
  prizes() { const odds = finalOdds(this.fakeoutChance); return PRIZES.map((p, i) => ({ ...p, final_chance_pct: odds[i] * 100 })); }
  requireOnline() {
    if (!this.online) throw new Error("Choose and connect a device, or enable offline preview");
  }
  async acquireLock() {
    if (this.releaseLock) return;
    if (!this.locks) throw new Error("This browser does not support connection ownership; use Chrome over HTTPS or localhost");
    await new Promise((resolve, reject) => {
      this.locks.request("dgnative-bluetooth-owner", { ifAvailable: true }, lock => {
        if (!lock) { reject(new Error("Another tab owns the Bluetooth connection; disconnect it first")); return; }
        return new Promise(release => { this.releaseLock = release; resolve(); });
      }).catch(reject);
    });
  }
  releaseOwnership() {
    if (!this.sessions.some(s => !s.simulated) && !this.connecting.size && this.releaseLock) {
      this.releaseLock(); this.releaseLock = null;
    }
  }
  async choose(version = "v3") {
    if (!this.bluetooth) throw new Error("Web Bluetooth is unavailable; open this page in Chrome, or use offline preview");
    if (this.sessions.some(s => s.simulated || s.version !== version)) throw new Error("Disconnect the current sessions before changing device type");
    const uuids = UUIDS[version];
    // requestDevice is invoked before any await, preserving the click's user activation.
    const choice = this.bluetooth.requestDevice({ filters: [{ namePrefix: version === "v3" ? "47L121000" : "D-LAB ESTIM01" }],
      optionalServices: [uuids.service, uuids.batteryService] });
    const revision = this.revision;
    try {
      const device = await choice;
      if (revision !== this.revision) throw new Error("Device selection cancelled by STOP");
      await this.acquireLock();
      if (revision !== this.revision) throw new Error("Connection cancelled by STOP");
      await this.connect(device, version);
    } finally { this.releaseOwnership(); }
  }
  async reconnect(id) {
    const known = this.known.get(id);
    if (!known) throw new Error("Select the device again after reloading the page");
    if (this.sessions.some(s => s.simulated || s.version !== known.version)) throw new Error("Disconnect the current sessions first");
    const revision = this.revision;
    await this.acquireLock();
    try {
      if (revision !== this.revision) throw new Error("Connection cancelled by STOP");
      await this.connect(known.device, known.version);
    } finally { this.releaseOwnership(); }
  }
  async connect(device, version) {
    if (this.sessions.some(s => s.id === device.id)) return;
    const revision = this.revision;
    const session = this.sessionFactory(device, version);
    this.connecting.add(session);
    session.addEventListener("status", event => {
      emit(this, "status", this.status());
      if (event.detail.message) this.notice(`${session.name}: A${session.strengths[0]} / B${session.strengths[1]}`);
      if (this.sessions.includes(session) && (session.strengths[0] > this.config.limitA || session.strengths[1] > this.config.limitB)) {
        void this.stop("Device strength exceeded the configured limit").catch(error => this.notice(error.message));
      }
    });
    session.addEventListener("notice", event => this.notice(event.detail));
    session.addEventListener("disconnect", () => {
      const existed = this.sessions.includes(session);
      this.sessions = this.sessions.filter(s => s !== session);
      if (existed) {
        this.notice(`${session.name} disconnected`);
        void this.stop("Device disconnected").catch(error => this.notice(error.message));
      }
      this.releaseOwnership(); emit(this, "status", this.status());
    });
    try {
      await session.connect(this.config, () => revision === this.revision);
      if (revision !== this.revision) throw new Error("Connection cancelled");
      this.version = version; this.sessions.push(session); this.known.set(device.id, { device, version });
      this.notice(`${session.name} connected at zero strength${version === "v3" ? `; persistent limits A${this.config.limitA}/B${this.config.limitB} written` : ""}`);
      this.lastTick = null; this.startLoop(); emit(this, "status", this.status());
    } catch (error) { session.disconnect(); throw error; }
    finally { this.connecting.delete(session); this.releaseOwnership(); }
  }
  async preview(version = "v3") {
    if (this.sessions.some(s => !s.simulated)) throw new Error("Disconnect Bluetooth before enabling offline preview");
    await this.disconnectAll(); this.version = version;
    this.sessions = [new SimSession(version)]; this.startLoop();
    this.notice("Offline preview enabled; no Bluetooth device is connected"); emit(this, "status", this.status());
  }
  startLoop() {
    if (this.autoRun && !this.timer) this.timer = setInterval(() => { void this.tick().catch(error => this.notice(error.message)); }, INTERVAL_MS);
  }
  async zeroDevices() {
    const sessions = [...this.sessions];
    const results = await Promise.allSettled(sessions.map(s => s.stop()));
    for (let i = 0; i < results.length; i++) if (results[i].status === "rejected") {
      const session = sessions[i];
      session?.disconnect(); throw results[i].reason;
    }
  }
  halt() {
    ++this.revision; this.pending = null; this.active = null; this.desired = [0, 0];
    emit(this, "status", this.status());
    return this.zeroDevices();
  }
  stop(reason = "STOP") {
    ++this.stopGeneration;
    const result = this.halt();
    for (const session of this.connecting) session.disconnect();
    emit(this, "stop", reason); this.notice(reason);
    return result;
  }
  async disconnect(id) {
    await this.stop("Disconnected by user");
    const session = this.sessions.find(s => s.id === id);
    this.sessions = this.sessions.filter(s => s.id !== id); session?.disconnect();
    this.releaseOwnership(); emit(this, "status", this.status());
  }
  async disconnectAll() {
    try { await this.stop("Disconnected"); }
    finally {
      const sessions = this.sessions; this.sessions = [];
      sessions.forEach(s => s.disconnect()); this.releaseOwnership();
      clearInterval(this.timer); this.timer = null; emit(this, "status", this.status());
    }
  }
  async configure(config) {
    if (this.version === "v3") encodeBf(config);
    else { integer(config.limitA, 0, 2047, "A software limit"); integer(config.limitB, 0, 2047, "B software limit"); }
    const stopped = this.stop("Configuration changed; output stopped");
    const revision = this.revision;
    await stopped;
    if (revision !== this.revision) throw new Error("Configuration cancelled");
    this.config = { ...config };
    await Promise.all(this.sessions.map(s => s.configure(config).catch(error => { s.disconnect(); throw error; })));
    if (revision !== this.revision) throw new Error("Configuration cancelled");
    this.notice(this.version === "v3" ? "Soft limits and balance parameters persist on the device across power cycles" : "V2 limits are enforced by this page only");
    emit(this, "status", this.status());
  }
  async draw() {
    this.requireOnline();
    if (this.version !== "v3") throw new Error("The wheel uses Coyote 3.0 waveforms; V2 supports manual X/Y/Z control");
    const stopped = this.halt(); const revision = this.revision;
    await stopped; this.requireOnline();
    if (revision !== this.revision) throw new Error("Draw cancelled");
    const result = { ...draw(this.fakeoutChance), token: globalThis.crypto.randomUUID() };
    this.pending = { ...result, revision }; return result;
  }
  async start(token) {
    const pending = this.pending;
    if (!pending || pending.token !== token || pending.revision !== this.revision) throw new Error("Draw was cancelled or already used");
    this.requireOnline(); this.pending = null;
    const p = pending.prize, strength = Math.floor(this.limit * p.strength_pct / 100);
    this.activate({ label: p.label, wave: p.wave, strengths: [strength, strength], duration: p.seconds * 1000 });
    await this.tick();
  }
  activate({ label, wave, strengths, duration, v2wave, manual = false }) {
    const a = this.channel !== "b" ? strengths[0] : 0, b = this.channel !== "a" ? strengths[1] : 0;
    integer(a, 0, this.config.limitA, "A strength"); integer(b, 0, this.config.limitB, "B strength");
    const started = this.now();
    this.desired = [a, b]; this.active = { label, wave, v2wave, started, duration, manual, until: duration ? started + duration : Infinity };
    this.sessions.forEach(s => s.setStrength(a, b)); emit(this, "status", this.status());
  }
  async manual({ wave = "breathing", strengths = [0, 0], seconds = 0, v2wave = { x: 1, y: 9, z: 0 }, continuePlayback = false }) {
    this.requireOnline(); integer(seconds, 0, 86400, "Duration");
    integer(strengths[0], 0, this.config.limitA, "A strength"); integer(strengths[1], 0, this.config.limitB, "B strength");
    if (!WAVES.some(w => w.name === wave)) throw new Error("Unknown waveform");
    encodeV2Wave(v2wave);
    if (continuePlayback && this.active?.manual && this.active.wave === wave) {
      this.desired = [this.channel !== "b" ? strengths[0] : 0, this.channel !== "a" ? strengths[1] : 0];
      this.active.v2wave = v2wave;
      this.sessions.forEach(s => s.setStrength(...this.desired)); emit(this, "status", this.status());
      await this.tick(); return;
    }
    const stopped = this.stop("Manual control selected"); const revision = this.revision;
    await stopped;
    if (revision !== this.revision) throw new Error("Playback cancelled");
    this.requireOnline(); this.activate({ label: this.version === "v3" ? WAVES.find(w => w.name === wave).label : "V2 manual", wave, strengths, duration: seconds * 1000, v2wave, manual: true });
    await this.tick();
  }
  async tick() {
    if (this.ticking || !this.online) return;
    const now = this.now();
    if (this.active && this.sessions.some(s => !s.simulated) && this.lastTick !== null && now - this.lastTick > 500) {
      await this.stop("Playback stopped: browser scheduling paused"); this.lastTick = now; return;
    }
    this.lastTick = now;
    if (this.active && now >= this.active.until) { await this.halt(); return; }
    this.ticking = true; const revision = this.revision;
    const active = this.active;
    let frame = idleFrame;
    if (active) {
      const wave = WAVES.find(w => w.name === active.wave);
      const data = pulses(wave, Math.floor((now - active.started) / INTERVAL_MS));
      frame = { pulsesA: this.channel !== "b" ? data : idleFrame.pulsesA,
        pulsesB: this.channel !== "a" ? data : idleFrame.pulsesB,
        waveA: this.channel !== "b" ? active.v2wave : idleFrame.waveA,
        waveB: this.channel !== "a" ? active.v2wave : idleFrame.waveB };
    }
    try {
      await Promise.all(this.sessions.map(s => s.tick(frame, () => revision === this.revision)));
    } catch (error) {
      await this.stop("Bluetooth write failed").catch(() => {}); throw error;
    } finally { this.ticking = false; emit(this, "status", this.status()); }
  }
}
