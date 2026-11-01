// CQ DJ front end. Talks to the Rust side through Tauri commands and the "station" event.
// Decoded text comes off the air, so it's only ever inserted with textContent.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const BANDS = [
  ["160m", 1_840_000], ["80m", 3_573_000], ["60m", 5_357_000], ["40m", 7_074_000],
  ["30m", 10_136_000], ["20m", 14_074_000], ["17m", 18_100_000], ["15m", 21_074_000],
  ["12m", 24_915_000], ["10m", 28_074_000], ["6m", 50_313_000],
];
const MAX_ROWS = 400;

const $ = (id) => document.getElementById(id);
let settings = null;      // { station: StationConfig, setup_done }
let running = false;
let armedMessage = "";
let work = null;          // station we're working: { call, snr, parity }
let lastParity = null;

// ---------- helpers ----------

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function log(message, level = "info") {
  const li = el("li", level);
  const t = new Date().toISOString().slice(11, 19);
  li.append(el("span", "t", t), document.createTextNode(message));
  $("log").prepend(li);
  while ($("log").children.length > 60) $("log").lastChild.remove();
}

function notice(message, isError = false) {
  const n = $("notice");
  if (!message) { n.hidden = true; return; }
  n.hidden = false;
  n.className = "notice" + (isError ? " error" : "");
  n.textContent = message;
}

const me = () => (settings?.station.callsign || "").toUpperCase();
const grid4 = () => (settings?.station.grid || "").toUpperCase().slice(0, 4);
const fmtReport = (snr) => {
  const v = Math.max(-30, Math.min(30, Math.round(snr)));
  return (v < 0 ? "-" : "+") + String(Math.abs(v)).padStart(2, "0");
};
const fmtUtc = (hhmmss) => `${hhmmss.slice(0, 2)}:${hhmmss.slice(2, 4)}:${hhmmss.slice(4, 6)}`;

// The station that sent a message: "CQ X1ABC FN42" / "CQ POTA X1ABC FN42" / "KN0BLE X1ABC -12".
function senderOf(text) {
  const t = text.trim().split(/\s+/);
  if (t[0] === "CQ") {
    // The callsign is the first token after CQ with both letters and digits ("CQ POTA X1ABC FN42").
    for (const tok of t.slice(1)) if (/\d/.test(tok) && /[A-Z]/.test(tok)) return clean(tok);
    return null;
  }
  return t.length >= 2 ? clean(t[1]) : null;
}
function clean(c) { return c ? c.replace(/[<>]/g, "") : null; }

// What they sent us, and what we should send next.
function nextStep(text) {
  const last = text.trim().split(/\s+/).pop();
  if (/^RR73$|^RRR$/.test(last)) return "73";
  if (last === "73") return null;
  if (/^R[+-]\d\d$/.test(last)) return "rr73";
  if (/^[+-]\d\d$/.test(last)) return "rreport";
  if (/^[A-R]{2}\d\d$/.test(last)) return "report";
  return null;
}

// ---------- settings & setup ----------

async function loadSettings() {
  settings = await invoke("get_settings");
  if (!settings.station) settings.station = {};
  settings.log = Object.assign({ qrz_api_key: "", qrz_upload: false, auto_log: false, tx_power_w: null }, settings.log || {});
  loadWorked();
  renderHeader();
  $("lvl").value = Math.round((settings.station.tx_level ?? 0.3) * 100);
  $("lvl-val").textContent = $("lvl").value + "%";
  $("txhz").value = settings.station.tx_audio_hz ?? 1500;
  $("parity").value = settings.station.tx_parity ?? "even";
  $("gain").value = settings.station.rx_gain_db ?? 0;
  $("gain-val").textContent = `+${$("gain").value} dB`;
  const dial = settings.station.dial_hz ?? 14_074_000;
  $("band").value = String(dial);
  if (!settings.setup_done) openSetup();
}

function renderHeader() {
  $("me").textContent = me() ? `${me()} · ${grid4()}` : "Not set up";
}

async function saveSettings() {
  settings.station.tx_level = Number($("lvl").value) / 100;
  settings.station.tx_audio_hz = Number($("txhz").value);
  settings.station.tx_parity = $("parity").value;
  settings.station.rx_gain_db = Number($("gain").value);
  settings.station.dial_hz = Number($("band").value);
  await invoke("save_settings", { settings });
}

function fillSelect(select, items, current, extra) {
  select.replaceChildren();
  if (extra) select.append(extra);
  for (const name of items) {
    const o = el("option", null, name);
    o.value = name;
    select.append(o);
  }
  if (current !== undefined && current !== null) select.value = current;
}

async function openSetup() {
  $("s-error").hidden = true;
  const s = settings.station;
  $("s-call").value = s.callsign || "";
  $("s-grid").value = s.grid || "";
  const audio = await invoke("list_audio");
  fillSelect($("s-in"), audio.inputs, s.input_device || audio.suggested_input || audio.inputs[0]);
  fillSelect($("s-out"), audio.outputs, s.output_device || audio.suggested_output || audio.outputs[0]);
  const ports = await invoke("list_ports");
  const none = el("option", null, "None — I'll key with VOX and tune by hand");
  none.value = "";
  fillSelect($("s-port"), ports, s.cat_port ?? (ports[0] || ""), none);
  $("s-baud").value = String(s.cat_baud || 38400);
  $("s-qrz").value = settings.log.qrz_api_key || "";
  $("s-qrz-up").checked = !!settings.log.qrz_upload;
  $("s-autolog").checked = !!settings.log.auto_log;
  $("s-pwr").value = settings.log.tx_power_w ?? "";
  $("setup").showModal();
}

$("s-find").addEventListener("click", async () => {
  $("s-find").disabled = true;
  $("s-find-msg").textContent = "Looking for your radio on each port…";
  try {
    const found = await invoke("find_radio");
    if (found) {
      $("s-port").value = found.port;
      $("s-baud").value = String(found.baud);
      $("s-find-msg").textContent = `Found it on ${found.port} at ${found.baud}.`;
    } else {
      $("s-find-msg").textContent = "No radio answered. Is it on, plugged in by USB, and is WSJT-X closed?";
    }
  } catch (e) {
    $("s-find-msg").textContent = String(e);
  }
  $("s-find").disabled = false;
});

$("s-cancel").addEventListener("click", () => $("setup").close());

$("s-qrz-test").addEventListener("click", async () => {
  const key = $("s-qrz").value.trim();
  if (!key) { $("s-qrz-msg").textContent = "Paste your QRZ logbook API key first."; return; }
  $("s-qrz-msg").textContent = "Asking QRZ…";
  try {
    $("s-qrz-msg").textContent = await invoke("test_qrz", { key, myCall: $("s-call").value.trim().toUpperCase() || me() });
    $("s-qrz-up").checked = true;
  } catch (e) {
    $("s-qrz-msg").textContent = String(e);
  }
});

$("setup-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const call = $("s-call").value.trim().toUpperCase();
  const grid = $("s-grid").value.trim().toUpperCase();
  if (!/^[A-Z0-9/]{3,11}$/.test(call)) return setupError("That callsign doesn't look right.");
  if (!/^[A-R]{2}\d{2}([A-X]{2})?$/.test(grid)) return setupError("Grid should look like EL98 (or EL98ab).");
  try {
    await invoke("check_message", { message: `CQ ${call} ${grid.slice(0, 4)}` });
  } catch (e) {
    return setupError("FT8 can't send that callsign as a standard call: " + e);
  }
  Object.assign(settings.station, {
    callsign: call,
    grid,
    input_device: $("s-in").value,
    output_device: $("s-out").value,
    cat_port: $("s-port").value || null,
    cat_baud: Number($("s-baud").value),
  });
  settings.log = {
    qrz_api_key: $("s-qrz").value.trim(),
    qrz_upload: $("s-qrz-up").checked && $("s-qrz").value.trim() !== "",
    auto_log: $("s-autolog").checked,
    tx_power_w: $("s-pwr").value ? Number($("s-pwr").value) : null,
  };
  settings.setup_done = true;
  await saveSettings();
  renderHeader();
  $("setup").close();
  log("Settings saved.");
  if (running) {
    await stopRadio();
    await startRadio();
  }
});

function setupError(msg) {
  $("s-error").textContent = msg;
  $("s-error").hidden = false;
}

$("settings-btn").addEventListener("click", openSetup);

// ---------- start / stop ----------

async function startRadio() {
  if (!settings.setup_done) return openSetup();
  await saveSettings();
  $("power").disabled = true;
  notice("");
  try {
    await invoke("start_station", { config: settings.station });
    running = true;
    log("Radio started.");
  } catch (e) {
    notice("Couldn't start: " + e, true);
    log(String(e), "error");
  }
  $("power").disabled = false;
  renderPower();
}

async function stopRadio() {
  await invoke("stop_station");
  running = false;
  armedMessage = "";
  renderTx(false, false);
  renderPower();
  $("pill").className = "pill";
  $("pill").textContent = "OFF";
  $("cycle-label").textContent = "Not running";
  $("cycle-fill").style.width = "0";
}

function renderPower() {
  $("power").textContent = running ? "Stop radio" : "Start radio";
  $("power").classList.toggle("on", running);
}

$("power").addEventListener("click", () => (running ? stopRadio() : startRadio()));

// ---------- transmit controls ----------

async function cmd(command) {
  try {
    await invoke("station_command", { command });
  } catch (e) {
    log(String(e), "warn");
  }
}

async function arm(message) {
  if (!running) { log("Start the radio first.", "warn"); return; }
  try {
    await invoke("check_message", { message });
  } catch (e) {
    log(String(e), "error");
    return;
  }
  armedMessage = message.toUpperCase();
  if (work && armedMessage.startsWith(work.call + " ")) {
    const last = armedMessage.split(/\s+/).pop();
    if (/^R?[+-]\d\d$/.test(last)) work.rstSent = last.replace(/^R/, "");
    work.timeOn ??= Date.now();
  }
  await cmd({ kind: "tx", enabled: true, message: armedMessage });
  renderTx(true, false);
}

async function halt() {
  armedMessage = "";
  await cmd({ kind: "tx", enabled: false, message: "" });
  renderTx(false, false);
}

function renderTx(armed, transmitting) {
  $("tx-msg").textContent = armed && armedMessage ? armedMessage : "Nothing armed";
  const p = $("parity").value === "even" ? "1st slot (:00 / :30)" : "2nd slot (:15 / :45)";
  $("tx-slot-label").textContent = armed ? (transmitting ? "Transmitting now" : `Sends in the ${p}`) : "";
  const cqOn = armed && armedMessage.startsWith("CQ ");
  $("cq").textContent = cqOn ? "Stop calling CQ" : "Call CQ";
  $("cq").classList.toggle("on", cqOn);
}

$("cq").addEventListener("click", () => {
  if (armedMessage.startsWith("CQ ")) return halt();
  if (!me()) return openSetup();
  arm(`CQ ${me()} ${grid4()}`);
});
$("halt").addEventListener("click", halt);
$("tune").addEventListener("click", () => {
  const on = !$("tune").classList.contains("on");
  cmd({ kind: "tune", on });
});
$("custom-send").addEventListener("click", () => {
  const m = $("custom").value.trim();
  if (m) arm(m);
});
$("custom").addEventListener("keydown", (e) => { if (e.key === "Enter") $("custom-send").click(); });

$("lvl").addEventListener("input", () => {
  $("lvl-val").textContent = $("lvl").value + "%";
  cmd({ kind: "tx_level", level: Number($("lvl").value) / 100 });
});
$("lvl").addEventListener("change", saveSettings);
function setGain(db) {
  db = Math.max(0, Math.min(60, Math.round(db)));
  $("gain").value = db;
  $("gain-val").textContent = `+${db} dB`;
  cmd({ kind: "rx_gain", db });
}
$("gain").addEventListener("input", () => setGain(Number($("gain").value)));
$("gain").addEventListener("change", saveSettings);
// Auto: put the band noise in the middle of the green zone (about -36 dBFS).
let lastRawDb = -120;
$("gain-auto").addEventListener("click", () => {
  if (!running || lastRawDb <= -119) { log("Start the radio first, so CQ DJ can hear the band.", "warn"); return; }
  setGain(-36 - lastRawDb);
  saveSettings();
  log(`Receive gain set to +${$("gain").value} dB.`);
});

$("txhz").addEventListener("change", () => {
  cmd({ kind: "tx_hz", hz: Number($("txhz").value) });
  saveSettings();
});
$("parity").addEventListener("change", () => {
  cmd({ kind: "parity", parity: $("parity").value });
  saveSettings();
  renderTx(!!armedMessage, false);
});

for (const [name, hz] of BANDS) {
  const o = el("option", null, `${name} · ${(hz / 1e6).toFixed(3)}`);
  o.value = String(hz);
  $("band").append(o);
}
$("band").addEventListener("change", () => {
  cmd({ kind: "dial", hz: Number($("band").value) });
  saveSettings();
});

// ---------- logbook ----------

let worked = new Set();       // "CALL|band"
let workedAny = new Set();    // "CALL"
let sessionCount = 0;

const bandName = () => (BANDS.find(([, hz]) => String(hz) === $("band").value) || ["?"])[0];

async function loadWorked() {
  try {
    const list = await invoke("worked_list");
    for (const [call, band] of list) { worked.add(`${call}|${band}`); workedAny.add(call); }
  } catch (_) { /* no log yet */ }
}

function workedTag(call) {
  if (!call) return null;
  if (worked.has(`${call}|${bandName()}`)) return `worked on ${bandName()}`;
  if (workedAny.has(call)) return "worked";
  return null;
}

// Show the log card (or log right away if auto-log is on) once per QSO.
function offerLog() {
  if (!work || work.logOffered) return;
  work.logOffered = true;
  const qso = {
    call: work.call, grid: work.grid || "", rst_sent: work.rstSent || "", rst_rcvd: work.rstRcvd || "",
    time_on: Math.floor((work.timeOn || Date.now()) / 1000), time_off: Math.floor(Date.now() / 1000),
    freq_hz: Number($("band").value) + Number($("txhz").value),
    my_call: me(), my_grid: (settings.station.grid || "").toUpperCase(),
    tx_power_w: settings.log.tx_power_w ?? null, comment: "",
  };
  if (settings.log.auto_log) return saveQso(qso);
  pendingQso = qso;
  $("lg-call").value = qso.call; $("lg-grid").value = qso.grid;
  $("lg-sent").value = qso.rst_sent; $("lg-rcvd").value = qso.rst_rcvd;
  $("logcard").hidden = false;
}
let pendingQso = null;

async function saveQso(qso) {
  try {
    const r = await invoke("log_qso", { qso });
    worked.add(`${qso.call}|${bandName()}`); workedAny.add(qso.call);
    sessionCount++;
    $("log-count").textContent = `${sessionCount} QSO${sessionCount === 1 ? "" : "s"} logged this session.`;
    const li = el("li", "qso");
    const t = new Date(qso.time_off * 1000).toISOString().slice(11, 16);
    li.append(el("span", null, `${t}  ${qso.call.padEnd(8)} ${qso.grid.padEnd(4)} ${qso.rst_sent}/${qso.rst_rcvd}`));
    let status = el("span", "ok", "saved");
    if (r.qrz) {
      if (r.qrz.Ok && (r.qrz.Ok.ok || r.qrz.Ok.duplicate)) status = el("span", "ok", r.qrz.Ok.duplicate ? "already on QRZ" : "saved · QRZ");
      else {
        const why = r.qrz.Err || r.qrz.Ok?.reason || "upload failed";
        status = el("span", "bad", "saved · QRZ failed");
        log(`QRZ upload for ${qso.call} failed: ${why}`, "warn");
      }
    }
    li.append(status);
    $("qsos").prepend(li);
    while ($("qsos").children.length > 15) $("qsos").lastChild.remove();
    log(`Logged ${qso.call} (${qso.rst_sent} / ${qso.rst_rcvd}).`);
  } catch (e) {
    log(String(e), "error");
    notice(String(e), true);
  }
}

$("lg-save").addEventListener("click", () => {
  if (!pendingQso) return;
  const q = Object.assign({}, pendingQso, {
    call: $("lg-call").value.trim().toUpperCase(), grid: $("lg-grid").value.trim().toUpperCase(),
    rst_sent: $("lg-sent").value.trim(), rst_rcvd: $("lg-rcvd").value.trim(),
  });
  $("logcard").hidden = true;
  pendingQso = null;
  saveQso(q);
});
$("lg-skip").addEventListener("click", () => { $("logcard").hidden = true; pendingQso = null; });
$("open-logs").addEventListener("click", async () => {
  try { const dir = await invoke("open_log_folder"); log(`Logs are in ${dir}`); } catch (e) { log(String(e), "error"); }
});

// What a message from the station we're working tells us.
function absorb(text) {
  if (!work) return;
  const last = text.trim().split(/\s+/).pop();
  if (/^[A-R]{2}\d\d$/.test(last) && last !== "RR73") work.grid = last;
  else if (/^[+-]\d\d$/.test(last)) work.rstRcvd = last;
  else if (/^R[+-]\d\d$/.test(last)) work.rstRcvd = last.slice(1);
  else if (last === "RR73" || last === "RRR") work.theyConfirmed = true;
  work.timeOn ??= Date.now();
}

// ---------- working a station ----------

function selectStation(call, snr, parity, text) {
  if (!call || call === me()) return;
  if (work && work.call === call) { work.snr = snr; work.parity = parity; if (text) absorb(text); return renderWorking(null); }
  work = { call, snr, parity };
  if (text) {
    const last = text.trim().split(/\s+/).pop();
    if (/^[A-R]{2}\d\d$/.test(last) && last !== "RR73") work.grid = last;
    if (!text.startsWith("CQ ")) absorb(text);
  }
  $("logcard").hidden = true;
  // Answer in the opposite slot to theirs.
  const ours = parity === "even" ? "odd" : "even";
  $("parity").value = ours;
  cmd({ kind: "parity", parity: ours });
  renderWorking(null);
  log(`Working ${call}. Pick what to send; CQ DJ transmits in the ${ours === "even" ? "1st" : "2nd"} slot.`);
}

function renderWorking(next) {
  if (!work) { $("working").hidden = true; return; }
  $("working").hidden = false;
  $("work-call").textContent = work.call;
  $("work-info").textContent = `heard at ${fmtReport(work.snr)} dB`;
  const r = fmtReport(work.snr);
  const steps = [
    ["grid", "Answer with grid", `${work.call} ${me()} ${grid4()}`],
    ["report", "Send report", `${work.call} ${me()} ${r}`],
    ["rreport", "Roger + report", `${work.call} ${me()} R${r}`],
    ["rr73", "Roger, 73", `${work.call} ${me()} RR73`],
    ["73", "Sign off", `${work.call} ${me()} 73`],
  ];
  const q = $("quick");
  q.replaceChildren();
  for (const [key, what, msg] of steps) {
    const b = el("button", "btn" + (key === next ? " next" : ""));
    b.append(el("span", "what", what), document.createTextNode(msg));
    b.addEventListener("click", () => arm(msg));
    q.append(b);
  }
}

$("work-clear").addEventListener("click", () => { work = null; renderWorking(null); });

// ---------- decodes ----------

function addRow(line, parity, mine = false) {
  const tr = el("tr", "decode" + (line.is_cq ? " cq" : "") + (line.to_me ? " tome" : "") + (mine ? " mine" : ""));
  tr.append(
    el("td", null, fmtUtc(line.utc)),
    el("td", "num", mine ? "TX" : String(line.snr)),
    el("td", "num", mine ? "" : line.dt.toFixed(1)),
    el("td", "num", String(Math.round(line.freq))),
    el("td", "msg", line.text),
  );
  if (!mine) {
    const tag = workedTag(senderOf(line.text));
    if (tag && (line.is_cq || line.to_me)) tr.lastChild.append(el("span", "b4", tag));
  }
  if (!mine) {
    tr.addEventListener("click", () => {
      const call = senderOf(line.text);
      selectStation(call, line.snr, parity, line.text);
      if (line.to_me) renderWorking(nextStep(line.text));
    });
  }
  return tr;
}

function onDecodes(ev) {
  wfLabels = ev.lines.map((l, i) => ({ freq: l.freq, call: senderOf(l.text) || "", toMe: l.to_me, row: i }));
  drawOverlay();
  const body = $("decodes");
  const onlyCq = $("only-cq").checked;
  const sep = el("tr", "sep");
  const td = el("td", null, `${fmtUtc(ev.utc)} · ${ev.lines.length} decoded`);
  td.colSpan = 5;
  sep.append(td);
  const frag = document.createDocumentFragment();
  frag.append(sep);
  const sorted = [...ev.lines].sort((a, b) => (b.to_me - a.to_me) || a.freq - b.freq);
  for (const line of sorted) {
    if (onlyCq && !line.is_cq && !line.to_me) continue;
    frag.append(addRow(line, ev.parity));
  }
  body.prepend(frag);
  while (body.children.length > MAX_ROWS) body.lastChild.remove();

  // Calling you
  const calls = ev.lines.filter((l) => l.to_me);
  if (calls.length) {
    const ul = $("calls");
    ul.querySelector(".empty")?.remove();
    for (const l of calls) {
      const li = el("li", null, `${fmtUtc(l.utc)}  ${String(l.snr).padStart(3)}  ${l.text}`);
      li.addEventListener("click", () => {
        selectStation(senderOf(l.text), l.snr, ev.parity, l.text);
        renderWorking(nextStep(l.text));
      });
      ul.prepend(li);
      while (ul.children.length > 30) ul.lastChild.remove();

      // If it's the station we're working, suggest the next step.
      const from = senderOf(l.text);
      if (work && from === work.call) {
        const next = nextStep(l.text);
        work.snr = l.snr;
        absorb(l.text);
        // They confirmed (RR73/RRR) after our report: the QSO counts. Offer to log it.
        if (work.theyConfirmed && work.rstSent) offerLog();
        renderWorking(next);
        const names = { report: "send their report", rreport: "send Roger + report", rr73: "send RR73", "73": "send 73" };
        log(`${from} sent "${l.text.split(/\s+/).slice(2).join(" ")}".` + (next ? ` Next: ${names[next]}.` : " QSO complete."));
      }
    }
  }
}

// ---------- waterfall ----------

const WF_MAX_HZ = 3000;
const wf = $("wf");
const wfCtx = wf.getContext("2d");
const over = $("wf-over");
const overCtx = over.getContext("2d");
let wfFloor = null;          // smoothed noise floor, dB
let wfLabels = [];           // [{ freq, call }] from the latest decode
const PALETTE = buildPalette();

function buildPalette() {
  // dark navy -> blue -> teal -> yellow -> white
  const stops = [[0, [5, 8, 11]], [0.25, [16, 42, 92]], [0.5, [24, 140, 160]], [0.75, [235, 200, 60]], [1, [255, 255, 240]]];
  const out = [];
  for (let i = 0; i < 256; i++) {
    const t = i / 255;
    let k = 0;
    while (k < stops.length - 2 && t > stops[k + 1][0]) k++;
    const [t0, c0] = stops[k], [t1, c1] = stops[k + 1];
    const f = (t - t0) / (t1 - t0);
    out.push(c0.map((v, j) => Math.round(v + (c1[j] - v) * f)));
  }
  return out;
}

function renderScale() {
  const box = $("wf-scale");
  box.replaceChildren();
  for (let hz = 0; hz <= WF_MAX_HZ; hz += 500) {
    const s = el("span", null, hz === 0 ? "0 Hz" : String(hz));
    s.style.left = `${(hz / WF_MAX_HZ) * 100}%`;
    box.append(s);
  }
}

function onSpectrum(e) {
  const bins = e.line.bins;
  const hzPer = e.line.hz_per_bin;
  const n = Math.min(bins.length, Math.floor(WF_MAX_HZ / hzPer));
  if (wf.width !== n) wf.width = n;

  // Noise floor = 20th percentile of the line, smoothed so the picture doesn't flicker.
  const sorted = Array.from(bins.slice(0, n)).sort((a, b) => a - b);
  const floor = sorted[Math.floor(n * 0.2)] - 200;
  wfFloor = wfFloor === null ? floor : wfFloor * 0.9 + floor * 0.1;

  // Scroll down one row, draw the new line at the top.
  wfCtx.drawImage(wf, 0, 1);
  const row = wfCtx.createImageData(n, 1);
  for (let i = 0; i < n; i++) {
    const db = bins[i] - 200;
    let t = (db - wfFloor + 4) / 34;
    t = Math.max(0, Math.min(1, t));
    let [r, g, b] = PALETTE[Math.round(t * 255)];
    if (e.transmitting) { r = Math.min(255, r * 0.5 + 60); g = g * 0.5 + 20; b = b * 0.4; }
    const o = i * 4;
    row.data[o] = r; row.data[o + 1] = g; row.data[o + 2] = b; row.data[o + 3] = 255;
  }
  wfCtx.putImageData(row, 0, 0);
  if (e.slot_start) {
    wfCtx.fillStyle = "rgba(230, 237, 243, 0.25)";
    wfCtx.fillRect(0, 0, n, 1);
  }
}

function drawOverlay() {
  const box = $("wf-box");
  const dpr = window.devicePixelRatio || 1;
  const w = box.clientWidth, h = box.clientHeight;
  if (over.width !== Math.round(w * dpr)) { over.width = Math.round(w * dpr); over.height = Math.round(h * dpr); }
  overCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
  overCtx.clearRect(0, 0, w, h);
  const x = (hz) => (hz / WF_MAX_HZ) * w;

  // Callsigns from the last decode, at their frequencies.
  overCtx.font = "11px ui-monospace, Menlo, monospace";
  overCtx.textBaseline = "top";
  const label = (text, lx, ly, color) => {
    const tw = overCtx.measureText(text).width;
    overCtx.fillStyle = "rgba(5, 8, 11, 0.75)";
    overCtx.fillRect(lx - 2, ly - 1, tw + 4, 14);
    overCtx.fillStyle = color;
    overCtx.fillText(text, lx, ly);
  };
  for (const l of wfLabels) {
    label(l.call, x(l.freq) + 2, 4 + (l.row % 3) * 15, l.toMe ? "#F5A524" : "#E6EDF3");
  }

  // Our transmit signal: a 50 Hz band starting at the TX frequency.
  const txHz = Number($("txhz").value) || 1500;
  overCtx.fillStyle = "rgba(245, 165, 36, 0.18)";
  overCtx.fillRect(x(txHz), 0, x(txHz + 50) - x(txHz), h);
  overCtx.fillStyle = "#F5A524";
  overCtx.fillRect(x(txHz) - 1, 0, 2, h);
  overCtx.textBaseline = "top";
  label(`TX ${txHz}`, x(txHz) + 4, h - 18, "#F5A524");
}

$("wf-box").addEventListener("click", (ev) => {
  const r = $("wf-box").getBoundingClientRect();
  let hz = Math.round(((ev.clientX - r.left) / r.width) * WF_MAX_HZ / 10) * 10;
  hz = Math.max(200, Math.min(2950, hz));
  $("txhz").value = hz;
  cmd({ kind: "tx_hz", hz });
  saveSettings();
  drawOverlay();
});
$("txhz").addEventListener("input", drawOverlay);
window.addEventListener("resize", drawOverlay);
renderScale();
drawOverlay();

// ---------- status ----------

function onStatus(s) {
  $("utc").textContent = fmtUtc(s.utc);
  const pct = Math.min(100, (s.seconds_into_slot / 15) * 100);
  const ourSlot = s.slot_parity === $("parity").value;
  $("cycle-fill").style.width = pct + "%";
  $("cycle-fill").classList.toggle("tx", s.transmitting);
  $("cycle-label").textContent = s.transmitting
    ? (s.tuning ? "Tuning" : "Transmitting")
    : (ourSlot && s.tx_armed ? "Your transmit slot" : "Listening");
  const pill = $("pill");
  pill.className = "pill " + (s.transmitting ? "tx" : "rx");
  pill.textContent = s.tuning ? "TUNE" : s.transmitting ? "TX" : "RX";
  $("tune").classList.toggle("on", s.tuning);
  $("tune").textContent = s.tuning ? "Stop tune" : "Tune";
  armedMessage = s.tx_armed ? s.tx_message : "";
  renderTx(s.tx_armed, s.transmitting && !s.tuning);

  // RX meter: -80..0 dBFS across the bar; green zone is about -48..-24 dBFS.
  const db = s.rx_dbfs;
  if (!s.transmitting) lastRawDb = s.rx_raw_dbfs;
  $("rx-db").textContent = db > -119 ? `${db.toFixed(0)} dBFS` : "no audio";
  const pos = Math.max(0, Math.min(100, ((db + 80) / 80) * 100));
  $("rx-needle").style.left = `calc(${pos}% - 2px)`;
  lastParity = s.slot_parity;

  // No audio at all for a few seconds usually means macOS blocked the sound card.
  if (db <= -119) {
    silentSince ??= Date.now();
    if (Date.now() - silentSince > 5000 && !silenceWarned) {
      silenceWarned = true;
      notice("No audio from the radio. Check that \"USB Audio CODEC\" is picked in setup, and that CQ DJ is allowed in " +
        "System Settings › Privacy & Security › Microphone.", true);
    }
  } else {
    silentSince = null;
    if (silenceWarned) { silenceWarned = false; notice(""); }
  }
}
let silentSince = null;
let silenceWarned = false;

listen("station", ({ payload: e }) => {
  switch (e.type) {
    case "started":
      log(`Listening on "${e.input}", sending to "${e.output}" (${e.output_rate} Hz). Radio control: ${e.rig}.`);
      break;
    case "clock":
      log(e.message, e.corrected || e.offset_secs === null ? "info" : "error");
      if (e.offset_secs !== null && !e.corrected) notice(e.message, true);
      break;
    case "status":
      onStatus(e);
      break;
    case "decodes":
      onDecodes(e);
      break;
    case "spectrum":
      onSpectrum(e);
      break;
    case "transmit":
      $("decodes").prepend(addRow({ utc: e.utc, snr: 0, dt: 0, freq: e.freq, text: e.message, is_cq: false, to_me: false }, null, true));
      if (work && e.message.startsWith(`${work.call} ${me()} `) && /(RR73|RRR|73)$/.test(e.message) && work.rstRcvd) offerLog();
      break;
    case "log":
      log(e.message, e.level);
      if (e.level === "error") notice(e.message, true);
      break;
    case "stopped":
      running = false;
      renderPower();
      log("Radio stopped.");
      break;
  }
});

// ---------- updates ----------

let pendingUpdate = null;

async function checkForUpdate(manual) {
  try {
    const u = await invoke("check_update");
    if (u) {
      pendingUpdate = u;
      $("update-text").textContent = `CQ DJ ${u.version} is ready (you have ${u.current}). Installing stops the radio for a moment.`;
      $("update").hidden = false;
      if (manual) $("s-version").textContent = `Version ${u.version} is available.`;
    } else if (manual) {
      $("s-version").textContent = `You're up to date (version ${await invoke("app_version")}).`;
    }
  } catch (e) {
    if (manual) $("s-version").textContent = String(e);
  }
}

$("update-later").addEventListener("click", () => { $("update").hidden = true; });
$("update-go").addEventListener("click", async () => {
  $("update-go").disabled = true;
  $("update-later").disabled = true;
  $("update-text").textContent = "Downloading the update…";
  try {
    await invoke("install_update"); // restarts the app when done
  } catch (e) {
    $("update-text").textContent = String(e);
    $("update-go").disabled = false;
    $("update-later").disabled = false;
  }
});
listen("update_progress", ({ payload: p }) => {
  const mb = (p.downloaded / 1e6).toFixed(1);
  const pct = p.total ? ` (${Math.round((p.downloaded / p.total) * 100)}%)` : "";
  $("update-text").textContent = `Downloading the update… ${mb} MB${pct}`;
});
$("s-update").addEventListener("click", () => { $("s-version").textContent = "Checking…"; checkForUpdate(true); });

invoke("app_version").then((v) => { $("s-version").textContent = `Version ${v}`; });
// Check a few seconds after launch, then every 6 hours.
setTimeout(() => checkForUpdate(false), 4000);
setInterval(() => checkForUpdate(false), 6 * 3600 * 1000);

window.addEventListener("beforeunload", () => { invoke("stop_station"); });

loadSettings().catch((e) => notice("Couldn't load settings: " + e, true));
