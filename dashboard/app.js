import {
  decodeBatch,
  decodeForecast,
  featureLeaf,
  fetchAccount,
  hashLeaf,
  proof,
  readingBytes,
  rootFromLeaves,
  sha256Hex,
  decodePublicAlert,
  decodeOutlook,
  decodeVault,
  decodePayout,
  decodeAlertReceipt,
  base58,
  decodePayoutV2,
  decodeRecord,
  decodeRegistry,
  fromHex,
  mmrPeaks,
  mmrProof,
  mmrVerify,
  toHex,
  verifyInclusion,
} from "./verify.js";

const $ = (s) => document.querySelector(s);
const SVG = "http://www.w3.org/2000/svg";

// ---- idioma ---------------------------------------------------------------

let lang = "pt";
// Text in the active language: L("português", "English").
const L = (pt, en) => (lang === "en" ? en : pt);
let nf0, nf1, nf2, nf3;
function setFormats() {
  const loc = L("pt-BR", "en-US");
  nf0 = new Intl.NumberFormat(loc, { maximumFractionDigits: 0 });
  nf1 = new Intl.NumberFormat(loc, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
  nf2 = new Intl.NumberFormat(loc, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  nf3 = new Intl.NumberFormat(loc, { maximumFractionDigits: 3 });
}
setFormats();
const pct = (x) => `${nf0.format(x * 100)}%`;
const m3 = (milli) => (milli == null ? "—" : nf0.format(milli / 1000));
const short = (h, n = 8) => `${h.slice(0, n)}…${h.slice(-4)}`;
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const STATUS = {
  signed: { cls: "st-good", icon: "✓", label: ["assinou", "signed"] },
  absent: { cls: "st-warning", icon: "–", label: ["ausente", "missing"] },
  disagreed: { cls: "st-critical", icon: "✕", label: ["discordou", "disagreed"] },
  invalid_signature: { cls: "st-critical", icon: "!", label: ["assinatura inválida", "invalid signature"] },
};
const REASONS = {
  unavailable: ["indisponível", "unavailable"],
  stuck: ["sensor travado", "stuck sensor"],
  jump: ["salto impossível", "impossible jump"],
  disagreement: ["divergência entre nós", "nodes disagree"],
  stale: ["sem dados recentes", "no recent data"],
};
const reason = (k) => L(...(REASONS[k] ?? [k, k]));
const UNIT = { precipitation: "mm", river_discharge: "m³/s", river_stage: "m" };

const state = { place: null, h: 24, sel: 0, station: "", batches: new Map(), activeReading: null, tamper: false };
let B; // bundle

// ---- date helpers ---------------------------------------------------------

const MONTHS = {
  pt: ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"],
  en: ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"],
};
const dm = (iso) => L(`${iso.slice(8, 10)}/${iso.slice(5, 7)}`, `${+iso.slice(8, 10)} ${MONTHS.en[+iso.slice(5, 7) - 1]}`);
const dmy = (iso) => L(`${dm(iso)}/${iso.slice(0, 4)}`, `${dm(iso)} ${iso.slice(0, 4)}`);
const addDays = (iso, n) => new Date(Date.parse(iso + "T00:00:00Z") + n * 864e5).toISOString().slice(0, 10);
const utc = (ts) => new Date(ts * 1000).toISOString().replace("T", " ").slice(0, 16) + " UTC";
const explorerAddr = (a) => `https://explorer.solana.com/address/${a}?cluster=${B.meta.cluster}`;
const nodeName = (pk) => `node-${B.meta.members.indexOf(pk) + 1}`;

// ---- tooltip --------------------------------------------------------------

const tip = $("#tooltip");
function showTip(html, x, y) {
  tip.innerHTML = html;
  tip.hidden = false;
  const r = tip.getBoundingClientRect();
  const left = Math.min(x + 14, window.innerWidth - r.width - 8);
  const top = y + r.height + 20 > window.innerHeight ? y - r.height - 12 : y + 14;
  tip.style.left = `${Math.max(8, left)}px`;
  tip.style.top = `${Math.max(8, top)}px`;
}
const hideTip = () => (tip.hidden = true);
const row = (k, v) => `<div class="row"><span>${k}</span><span>${v}</span></div>`;

function el(tag, attrs = {}, parent) {
  const n = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
  parent?.appendChild(n);
  return n;
}

function niceMax(v) {
  const p = 10 ** Math.floor(Math.log10(v));
  return Math.ceil(v / p / 0.5) * 0.5 * p;
}

// ---- derivados ------------------------------------------------------------

function derive() {
  B.byTarget = { 24: new Map(), 48: new Map() };
  B.issueByDay = new Map();
  for (const is of B.issues) {
    B.issueByDay.set(is.issue_day, is);
    for (const h of is.horizons) B.byTarget[h.horizon_hours].set(h.target_day, { issue: is, f: h });
  }
  B.dayIndex = new Map(B.days.map((d, i) => [d.day, i]));
  const thr = B.meta.alert_threshold_milli;
  B.stats = {};
  for (const h of [24, 48]) {
    let hit = 0, fa = 0, miss = 0, inside = 0, n = 0;
    for (const { f } of B.byTarget[h].values()) {
      if (f.observed_milli == null) continue;
      n++;
      const ev = f.observed_milli >= thr;
      if (f.alert && ev) hit++;
      else if (f.alert) fa++;
      else if (ev) miss++;
      if (f.inside_p10_p90) inside++;
    }
    B.stats[h] = { n, hit, fa, miss, coverage: inside / n, pod: hit / (hit + miss), far: fa / (hit + fa) };
  }
  // Observed crossings and the first alert that pointed at that day.
  B.crossings = [];
  B.days.forEach((d, i) => {
    const prev = B.days[i - 1];
    if (d.guaiba_milli >= thr && (!prev || prev.guaiba_milli < thr)) {
      const alerts = [24, 48].map((h) => B.byTarget[h].get(d.day)).filter((x) => x?.f.alert);
      const first = alerts.sort((a, b) => a.issue.issued_at - b.issue.issued_at)[0];
      B.crossings.push({ day: d.day, lead: first ? (Date.parse(d.day) - Date.parse(first.issue.issue_day)) / 36e5 : null });
    }
  });
}

// ---- indicadores ----------------------------------------------------------

function renderKpis() {
  const a = B.metrics.audit;
  const partial = B.days.filter((d) => d.members.some((m) => m.status !== "signed")).length;
  const degr = B.days.reduce((n, d) => n + d.degradations.length, 0);
  const leads = B.crossings.map((c) => (c.lead == null ? L("sem alerta", "no alert") : `${c.lead} h`));
  const tiles = [
    [L("Lotes ancorados", "Batches anchored"), a.batches, L(`${partial} com algum nó ausente ou discordante`, `${partial} with a node missing or disagreeing`)],
    [L("Previsões antes da observação", "Forecasts before observation"), `${a.forecasts_recorded_before_observation}/${a.forecasts_observed}`, L("auditado pelos slots da Solana", "audited against Solana slots")],
    [L("Alertas on-chain", "On-chain alerts"), a.alerts, L(`${a.alerts_recorded_before_observation} gravados antes da observação`, `${a.alerts_recorded_before_observation} recorded before observation`)],
    [L("Cruzamentos do limiar", "Threshold crossings"), B.crossings.length, L(`antecedência do 1º alerta: ${leads.join(" e ")}`, `lead time of first alert: ${leads.join(" and ")}`)],
    [L("Cobertura P10–P90", "P10–P90 coverage"), `${pct(B.stats[24].coverage)} · ${pct(B.stats[48].coverage)}`, L("1 dia · 2 dias antes, todo o replay on-chain (meta ≈ 80%)", "1 day · 2 days ahead, whole on-chain replay (target ≈ 80%)")],
    [L("Fontes degradadas", "Degraded sources"), degr, L("eventos SourceDegraded", "SourceDegraded events")],
  ];
  $("#kpis").innerHTML = tiles
    .map(([l, v, n]) => `<div class="kpi"><div class="label">${l}</div><div class="value">${v}</div><div class="note">${n}</div></div>`)
    .join("");
}

// ---- flow chart -----------------------------------------------------------

function flowSeries() {
  const first = B.days[0].day;
  const last = addDays(B.days.at(-1).day, 2);
  const dates = [];
  for (let d = first; d <= last; d = addDays(d, 1)) dates.push(d);
  return dates.map((day) => {
    const obs = B.days[B.dayIndex.get(day)]?.guaiba_milli ?? null;
    const fc = B.byTarget[state.h].get(day);
    return { day, obs, fc: fc?.f ?? null, issue: fc?.issue ?? null };
  });
}

// Chart of observed flow × forecast band. Reused by the replay and by live mode.
function renderFlow(opts = {}) {
  const {
    box = $("#flow-chart"),
    data = flowSeries(),
    selDay = B.days[state.sel].day,
    h = state.h,
    onPick = (day) => B.dayIndex.has(day) && selectDay(B.dayIndex.get(day)),
    legend = $("#flow-legend"),
    table = true,
  } = opts;
  const W = Math.max(320, box.clientWidth);
  const H = W < 560 ? 260 : 330;
  const m = { l: 52, r: 18, t: 14, b: 26 };
  const thr = B.meta.alert_threshold_milli;
  const ymax = niceMax(Math.max(...data.map((d) => Math.max(d.obs ?? 0, d.fc?.p90 ?? 0)), thr) * 1.05);
  const x = (i) => m.l + (i / (data.length - 1)) * (W - m.l - m.r);
  const y = (v) => H - m.b - (v / ymax) * (H - m.t - m.b);

  const svg = el("svg", { viewBox: `0 0 ${W} ${H}`, width: W, height: H });
  // grid and y axis
  const step = niceMax(ymax / 5);
  for (let v = 0; v <= ymax + 1; v += step) {
    el("line", { x1: m.l, x2: W - m.r, y1: y(v), y2: y(v), class: v === 0 ? "baseline" : "gridline" }, svg);
    el("text", { x: m.l - 6, y: y(v) + 4, "text-anchor": "end", class: "tick-label" }, svg).textContent = nf0.format(v / 1000);
  }
  // x axis: one label every ~5 days
  const every = Math.max(1, Math.ceil(data.length / (W < 560 ? 5 : 8)));
  data.forEach((d, i) => {
    if (i % every === 0) el("text", { x: x(i), y: H - 6, "text-anchor": "middle", class: "tick-label" }, svg).textContent = dm(d.day);
  });

  // selected day
  const si = data.findIndex((d) => d.day === selDay);
  const colW = (W - m.l - m.r) / Math.max(1, data.length - 1);
  if (si >= 0) el("rect", { x: x(si) - colW / 2, y: m.t, width: colW, height: H - m.t - m.b, fill: "var(--surface-2)" }, svg);

  // P10–P90 band and P50
  const fcPts = data.map((d, i) => (d.fc ? { i, ...d.fc } : null)).filter(Boolean);
  const band = fcPts.map((p) => `${x(p.i)},${y(p.p90)}`).concat([...fcPts].reverse().map((p) => `${x(p.i)},${y(p.p10)}`));
  el("polygon", { points: band.join(" "), fill: "var(--band)" }, svg);
  el("polyline", { points: fcPts.map((p) => `${x(p.i)},${y(p.p50)}`).join(" "), fill: "none", stroke: "var(--forecast)", "stroke-width": 2, "stroke-dasharray": "5 4" }, svg);

  // limiar
  el("line", { x1: m.l, x2: W - m.r, y1: y(thr), y2: y(thr), stroke: "var(--critical)", "stroke-width": 1, "stroke-dasharray": "2 3" }, svg);
  el("text", { x: m.l + 6, y: y(thr) - 5, class: "tick-label" }, svg).textContent = L(`limiar de alerta · ${m3(thr)} m³/s`, `alert threshold · ${m3(thr)} m³/s`);

  // observada
  const obsPts = data.map((d, i) => (d.obs != null ? `${x(i)},${y(d.obs)}` : null)).filter(Boolean);
  el("polyline", { points: obsPts.join(" "), fill: "none", stroke: "var(--observed)", "stroke-width": 2, "stroke-linejoin": "round" }, svg);

  // alerts: triangle above the target day's P90
  for (const p of fcPts) {
    if (!p.alert) continue;
    const cx = x(p.i), cy = y(p.p90) - 9;
    el("path", { d: `M${cx - 5},${cy - 4} L${cx + 5},${cy - 4} L${cx},${cy + 4} Z`, fill: "var(--critical)", stroke: "var(--surface)", "stroke-width": 1.5 }, svg);
  }

  // direct labels at the line ends
  const lastObs = data.findLastIndex((d) => d.obs != null);
  if (lastObs >= 0)
    el("text", { x: x(lastObs) - 4, y: y(data[lastObs].obs) - 8, "text-anchor": "end", class: "tick-label", fill: "var(--text-2)" }, svg).textContent = L("observada", "observed");
  const lastFc = fcPts.at(-1);
  if (lastFc) el("text", { x: x(lastFc.i), y: y(lastFc.p90) - 8, "text-anchor": "end", class: "tick-label" }, svg).textContent = "P10–P90";

  // interaction layer
  const hover = el("g", { "pointer-events": "none" }, svg);
  const overlay = el("rect", { x: m.l - colW / 2, y: m.t, width: W - m.l - m.r + colW, height: H - m.t - m.b, fill: "transparent", style: "cursor:pointer" }, svg);
  const pick = (ev) => {
    const r = svg.getBoundingClientRect();
    const px = ((ev.clientX - r.left) / r.width) * W;
    return Math.max(0, Math.min(data.length - 1, Math.round(((px - m.l) / (W - m.l - m.r)) * (data.length - 1))));
  };
  overlay.addEventListener("pointermove", (ev) => {
    const i = pick(ev);
    const d = data[i];
    hover.replaceChildren();
    el("line", { x1: x(i), x2: x(i), y1: m.t, y2: H - m.b, stroke: "var(--axis)", "stroke-width": 1 }, hover);
    if (d.obs != null) el("circle", { cx: x(i), cy: y(d.obs), r: 4.5, fill: "var(--observed)", stroke: "var(--surface)", "stroke-width": 2 }, hover);
    if (d.fc) el("circle", { cx: x(i), cy: y(d.fc.p50), r: 4.5, fill: "var(--forecast)", stroke: "var(--surface)", "stroke-width": 2 }, hover);
    const hd = h / 24;
    showTip(
      `<div class="t-title">${dmy(d.day)}</div>` +
        row(L("Observada", "Observed"), d.obs == null ? L("ainda não ancorada", "not anchored yet") : `${m3(d.obs)} m³/s`) +
        (d.fc
          ? row(L(`Prevista em ${dm(d.issue.issue_day)} (${hd} d)`, `Forecast on ${dm(d.issue.issue_day)} (${hd} d ahead)`), "") +
            row("P10 · P50 · P90", `${m3(d.fc.p10)} · ${m3(d.fc.p50)} · ${m3(d.fc.p90)}`) +
            row(L("Alerta", "Alert"), d.fc.alert ? L("▼ sim (P90 ≥ limiar)", "▼ yes (P90 ≥ threshold)") : L("não", "no"))
          : row(L("Previsão", "Forecast"), L("sem previsão neste horizonte", "no forecast at this lead time"))),
      ev.clientX,
      ev.clientY,
    );
  });
  overlay.addEventListener("pointerleave", () => {
    hover.replaceChildren();
    hideTip();
  });
  overlay.addEventListener("click", (ev) => onPick(data[pick(ev)].day));
  box.replaceChildren(svg);

  legend.innerHTML =
    `<span class="key" style="color:var(--observed)"><span class="swatch-line"></span><span style="color:var(--text-2)">${L("Observada (GloFAS)", "Observed (GloFAS)")}</span></span>` +
    `<span class="key" style="color:var(--forecast)"><span class="swatch-dash"></span><span style="color:var(--text-2)">${L("Previsão P50", "P50 forecast")}</span></span>` +
    `<span class="key"><span class="swatch-band"></span>${L("Faixa P10–P90", "P10–P90 band")}</span>` +
    `<span class="key"><span class="swatch-threshold"></span>${L("Limiar de cheia", "Flood threshold")}</span>` +
    `<span class="key"><span style="color:var(--critical)">▼</span>${L("Alerta emitido para o dia (P90 ≥ limiar)", "Alert issued for the day (P90 ≥ threshold)")}</span>`;

  if (table) renderFlowTable(data);
}

function renderFlowTable(data) {
  const rows = data
    .map(
      (d) =>
        `<tr><td>${dmy(d.day)}</td><td class="num">${m3(d.obs)}</td><td class="num">${m3(d.fc?.p10)}</td><td class="num">${m3(d.fc?.p50)}</td><td class="num">${m3(d.fc?.p90)}</td><td>${d.fc?.alert ? L("▼ alerta", "▼ alert") : ""}</td><td>${d.fc?.inside_p10_p90 == null ? "" : d.fc.inside_p10_p90 ? L("dentro", "inside") : L("fora", "outside")}</td></tr>`,
    )
    .join("");
  $("#flow-table").innerHTML =
    `<table class="data"><thead><tr><th>${L("Dia", "Day")}</th><th class="num">${L("Observada", "Observed")}</th><th class="num">P10</th><th class="num">P50</th><th class="num">P90</th><th>${L("Alerta", "Alert")}</th><th>${L("Faixa", "Band")}</th></tr></thead><tbody>${rows}</tbody></table>`;
}

function renderRain() {
  const box = $("#rain-chart");
  const W = Math.max(320, box.clientWidth);
  const H = 120;
  const m = { l: 52, r: 18, t: 22, b: 22 };
  const n = B.days.length + 2; // same x axis as the flow chart
  const vals = B.days.map((d) => {
    const v = Object.entries(d.rain_milli).filter(([s]) => (d.rain_hours[s] ?? 0) >= 20).map(([, x]) => x / 1000);
    return v.length ? v.reduce((a, b) => a + b, 0) / v.length : null;
  });
  const ymax = niceMax(Math.max(...vals.filter((v) => v != null)));
  const x = (i) => m.l + (i / (n - 1)) * (W - m.l - m.r);
  const y = (v) => H - m.b - (v / ymax) * (H - m.t - m.b);
  const bw = Math.max(3, ((W - m.l - m.r) / (n - 1)) * 0.6);
  const svg = el("svg", { viewBox: `0 0 ${W} ${H}`, width: W, height: H });
  el("text", { x: m.l, y: 12, class: "tick-label", fill: "var(--text-2)" }, svg).textContent = L("Chuva média diária nas estações do INMET (mm)", "Average daily rain at INMET stations (mm)");
  for (const v of [0, ymax / 2, ymax]) {
    el("line", { x1: m.l, x2: W - m.r, y1: y(v), y2: y(v), class: v === 0 ? "baseline" : "gridline" }, svg);
    el("text", { x: m.l - 6, y: y(v) + 4, "text-anchor": "end", class: "tick-label" }, svg).textContent = nf0.format(v);
  }
  vals.forEach((v, i) => {
    if (v == null || v <= 0) return;
    const r = el("rect", { x: x(i) - bw / 2, y: y(v), width: bw, height: Math.max(1, y(0) - y(v)), rx: 2, fill: "var(--seq-3)" }, svg);
    r.style.cursor = "pointer";
    const d = B.days[i];
    r.addEventListener("pointermove", (ev) =>
      showTip(
        `<div class="t-title">${dmy(d.day)} · ${L("média", "average")} ${nf1.format(v)} mm</div>` +
          Object.keys(d.rain_milli)
            .sort()
            .map((s) => row(s, `${nf1.format(d.rain_milli[s] / 1000)} mm (${d.rain_hours[s]} h)`))
            .join(""),
        ev.clientX,
        ev.clientY,
      ),
    );
    r.addEventListener("pointerleave", hideTip);
    r.addEventListener("click", () => selectDay(i));
  });
  box.replaceChildren(svg);
}

// ---- timeline -------------------------------------------------------------

function renderTimeline() {
  const n = B.days.length;
  const cols = `minmax(86px,auto) repeat(${n}, minmax(18px, 1fr))`;
  const cell = (i, cls, txt, title) =>
    `<button type="button" class="tl-cell ${cls}${i === state.sel ? " selected" : ""}" data-i="${i}" title="${esc(title)}" aria-label="${esc(title)}">${txt}</button>`;
  let html = `<div class="tl-grid" style="grid-template-columns:${cols}"><div></div>`;
  html += B.days.map((d, i) => `<button type="button" class="tl-cell tl-day${i === state.sel ? " selected" : ""}" data-i="${i}">${dm(d.day)}</button>`).join("");
  B.meta.members.forEach((pk, k) => {
    html += `<div class="tl-label">node-${k + 1}</div>`;
    html += B.days
      .map((d, i) => {
        const st = STATUS[d.members.find((m) => m.member === pk)?.status ?? "absent"];
        return cell(i, st.cls, st.icon, `${dmy(d.day)} · node-${k + 1} ${L(...st.label)}`);
      })
      .join("");
  });
  html += `<div class="tl-label">${L("Alerta emitido", "Alert issued")}</div>`;
  html += B.days
    .map((d, i) => {
      const is = B.issueByDay.get(d.day);
      const al = is?.horizons.filter((h) => h.alert) ?? [];
      return al.length
        ? cell(i, "st-critical", "▼", L(`${dmy(d.day)} · alerta para ${al.map((h) => dm(h.target_day)).join(" e ")}`, `${dmy(d.day)} · alert for ${al.map((h) => dm(h.target_day)).join(" and ")}`))
        : cell(i, "st-forecast", "", L(`${dmy(d.day)} · previsão sem alerta`, `${dmy(d.day)} · forecast, no alert`));
    })
    .join("");
  html += `<div class="tl-label">${L("Fonte degradada", "Degraded source")}</div>`;
  html += B.days
    .map((d, i) =>
      d.degradations.length
        ? cell(i, "st-warning", "!", `${dmy(d.day)} · ${d.degradations.map((g) => `${g.station_id} (${reason(g.reason)})`).join(", ")}`)
        : cell(i, "empty", "", dmy(d.day)),
    )
    .join("");
  html += "</div>";
  const box = $("#timeline");
  box.innerHTML = html;
  box.querySelectorAll("[data-i]").forEach((b) => b.addEventListener("click", () => selectDay(+b.dataset.i)));

  $("#timeline-legend").innerHTML = [
    ["st-good", "✓", L("assinou", "signed")],
    ["st-warning", "–", L("ausente", "missing")],
    ["st-critical", "✕", L("discordou (assinou outra raiz)", "disagreed (signed a different root)")],
    ["st-critical", "▼", L("alerta emitido no fim do dia", "alert issued at the end of the day")],
    ["st-warning", "!", "SourceDegraded"],
  ]
    .map(([c, i, l]) => `<span class="key"><span class="dot ${c}">${i}</span>${l}</span>`)
    .join("");
}

// ---- selected day ---------------------------------------------------------

async function loadBatch(d) {
  const file = d.batch_file ?? `batches/${String(d.batch_id).padStart(5, "0")}.json`;
  if (!state.batches.has(file)) {
    const res = await fetch(`data/${file}`);
    state.batches.set(file, await res.json());
  }
  return state.batches.get(file);
}

function selectDay(i) {
  state.sel = Math.max(0, Math.min(B.days.length - 1, i));
  state.activeReading = null;
  $("#verify-panel").hidden = true;
  renderFlow();
  renderTimeline();
  renderDay();
}

function renderDay() {
  const d = B.days[state.sel];
  $("#day-title").textContent = `${L("Lote", "Batch")} ${d.batch_id} · ${dmy(d.day)}`;
  $("#day-sub").textContent = L(
    `Janela ${utc(d.t_start)} → ${utc(d.t_end)} (tempo do replay). Ancorado na devnet em ${utc(d.committed_at)}, slot ${nf0.format(d.slot)}.`,
    `Window ${utc(d.t_start)} → ${utc(d.t_end)} (replay time). Anchored on devnet at ${utc(d.committed_at)}, slot ${nf0.format(d.slot)}.`,
  );
  $("#prev-day").disabled = state.sel === 0;
  $("#next-day").disabled = state.sel === B.days.length - 1;

  const signers = d.members
    .map((m) => {
      const st = STATUS[m.status];
      return `<div class="status"><span class="dot ${st.cls}">${st.icon}</span>${nodeName(m.member)} ${L(...st.label)} <a class="mono" href="${explorerAddr(m.member)}" target="_blank" rel="noopener">${short(m.member, 6)}</a></div>`;
    })
    .join("");
  const events = [
    ...d.degradations.map(
      (g) => `<li><span class="dot st-warning">!</span> <strong>SourceDegraded</strong> ${g.station_id}: ${reason(g.reason)}, C = ${nf2.format(g.score_milli / 1000)} · <a href="${g.explorer}" target="_blank" rel="noopener">${L("transação", "transaction")}</a></li>`,
    ),
    ...d.dissents.map(
      (x) =>
        L(
          `<li><span class="dot st-critical">✕</span> ${nodeName(x.member)} informou ${x.station_id} às ${utc(x.ts_utc).slice(11, 16)} = ${x.value_milli == null ? "nada" : nf1.format(x.value_milli / 1000)}; mediana dos nós = ${x.median_milli == null ? "nada" : nf1.format(x.median_milli / 1000)} (MAD)</li>`,
          `<li><span class="dot st-critical">✕</span> ${nodeName(x.member)} reported ${x.station_id} at ${utc(x.ts_utc).slice(11, 16)} = ${x.value_milli == null ? "nothing" : nf1.format(x.value_milli / 1000)}; median of the nodes = ${x.median_milli == null ? "nothing" : nf1.format(x.median_milli / 1000)} (MAD)</li>`,
        ),
    ),
  ];
  $("#day-batch").innerHTML = `<div class="panel">
    <h3>${L("Lote ancorado", "Anchored batch")}</h3>
    <dl class="kv">
      <dt>${L("Raiz Merkle", "Merkle root")}</dt><dd class="hash" title="${d.merkle_root}">${d.merkle_root}</dd>
      <dt>${L("Raiz anterior", "Previous root")}</dt><dd class="hash" title="${d.prev_batch_root}">${short(d.prev_batch_root, 16)}</dd>
      <dt>${L("Leituras", "Readings")}</dt><dd>${d.leaf_count}</dd>
      <dt>${L("Assinaturas", "Signatures")}</dt><dd>${signers}</dd>
      <dt>${L("Na cadeia", "On-chain")}</dt><dd><a href="${d.explorer}" target="_blank" rel="noopener">${L("transação", "transaction")}</a> · ${d.protocol === "v2" ? chainLink(d) : `<a href="${explorerAddr(d.batch_account)}" target="_blank" rel="noopener">${L("conta do lote", "batch account")}</a>`}</dd>
    </dl>
    ${events.length ? `<ul class="events">${events.join("")}</ul>` : ""}
    <p><button type="button" class="ghost" id="verify-batch">${L("Verificar o lote inteiro na devnet", "Verify the whole batch on devnet")}</button></p>
  </div>`;
  $("#verify-batch").addEventListener("click", () => verifyBatch(d));

  // Who knew what: forecasts that pointed at this day and the one issued at its end.
  const before = [48, 24]
    .map((h) => B.byTarget[h].get(d.day))
    .filter(Boolean)
    .map(
      ({ issue, f }) =>
        `<tr><td>${dm(issue.issue_day)} (${f.horizon_hours / 24} ${L("d antes", "d ahead")})</td><td>${m3(f.p10)}</td><td>${m3(f.p50)}</td><td>${m3(f.p90)}</td><td>${f.alert ? `<span class="crit-text">${L("▼ alerta", "▼ alert")}</span>` : "—"}</td><td>${f.inside_p10_p90 == null ? "—" : f.inside_p10_p90 ? `<span class="good-text">${L("dentro", "inside")}</span>` : L("fora", "outside")}</td></tr>`,
    )
    .join("");
  const is = B.issueByDay.get(d.day);
  const issued = is
    ? is.horizons
        .map(
          (f) =>
            `<tr><td>${dm(f.target_day)} (+${f.horizon_hours} h)</td><td>${m3(f.p10)}</td><td>${m3(f.p50)}</td><td>${m3(f.p90)}</td><td>${f.alert ? `<a class="crit-text" href="${f.alert.explorer}" target="_blank" rel="noopener">${L("▼ alerta", "▼ alert")}</a>` : "—"}</td><td><a href="${f.explorer}" target="_blank" rel="noopener">slot ${nf0.format(f.slot)}</a></td></tr>`,
        )
        .join("")
    : "";
  const obsTxt = d.guaiba_milli == null ? L("sem leitura", "no reading") : `${m3(d.guaiba_milli)} m³/s`;
  $("#day-forecast").innerHTML = `<div class="panel">
    <h3>${L(`O que já se previa para ${dm(d.day)}`, `What had been forecast for ${dm(d.day)}`)}</h3>
    <p class="sub">${L("Observada (Guaíba, GloFAS)", "Observed (Guaíba, GloFAS)")}: <strong>${obsTxt}</strong></p>
    ${before ? `<table class="fc-table"><thead><tr><th>${L("Emitida em", "Issued on")}</th><th>P10</th><th>P50</th><th>P90</th><th>${L("Alerta", "Alert")}</th><th>${L("Observada", "Observed")}</th></tr></thead><tbody>${before}</tbody></table>` : `<p class="sub">${L("Nenhuma previsão apontava para este dia.", "No forecast targeted this day.")}</p>`}
    <h3 style="margin-top:14px">${L("Previsão emitida no fim do dia", "Forecast issued at the end of the day")}</h3>
    ${
      is
        ? `<table class="fc-table"><thead><tr><th>${L("Alvo", "Target")}</th><th>P10</th><th>P50</th><th>P90</th><th>${L("Alerta", "Alert")}</th><th>${L("Gravada", "Recorded")}</th></tr></thead><tbody>${issued}</tbody></table>
           <p class="sub">${L(`Baseada no lote ${is.based_on_batch_id} (raiz ${short(is.based_on_root)})`, `Based on batch ${is.based_on_batch_id} (root ${short(is.based_on_root)})`)}, features_root ${short(is.features_root)}.</p>
           <p><button type="button" class="ghost" id="verify-forecast">${L("Verificar previsão na devnet", "Verify forecast on devnet")}</button></p>`
        : `<p class="sub">${L("Sem previsão (dados insuficientes).", "No forecast (not enough data).")}</p>`
    }
  </div>`;
  $("#verify-forecast")?.addEventListener("click", () => verifyForecast(is));

  renderReadings(d);
}

async function renderReadings(d) {
  const b = await loadBatch(d);
  if (B.days[state.sel] !== d) return;
  const sel = $("#station-filter");
  const ids = [...new Set(b.readings.map((r) => r.station_id))].sort();
  if (!ids.includes(state.station)) state.station = "";
  sel.innerHTML = `<option value="">${L("todas", "all")} (${b.readings.length})</option>` + ids.map((s) => `<option${s === state.station ? " selected" : ""}>${s}</option>`).join("");
  const rows = b.readings
    .map((r, i) => ({ r, i }))
    .filter(({ r }) => !state.station || r.station_id === state.station)
    .map(
      ({ r, i }) =>
        `<tr data-i="${i}"${state.activeReading === i ? ' class="active"' : ""}><td>${r.station_id}</td><td>${utc(r.ts_utc).slice(0, 16)}</td><td class="num">${(r.variable === "precipitation" ? nf1 : nf0).format(r.value_milli / 1000)}</td><td>${UNIT[r.variable]}</td><td class="hash">${short(r.raw_payload_sha256, 10)}</td><td><button type="button" class="link-btn" data-v="${i}">${L("verificar", "verify")}</button></td></tr>`,
    )
    .join("");
  $("#readings").innerHTML =
    `<table class="data"><thead><tr><th>${L("Estação", "Station")}</th><th>${L("Instante", "Time")}</th><th class="num">${L("Valor", "Value")}</th><th>${L("Unid.", "Unit")}</th><th>${L("SHA-256 do dado bruto", "SHA-256 of raw data")}</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
  $("#readings").querySelectorAll("[data-v]").forEach((btn) => btn.addEventListener("click", () => verifyReading(d, +btn.dataset.v)));
}

// ---- verification ---------------------------------------------------------

const OWNED = () => L("A conta pertence ao programa do Cota", "The account belongs to the Cota program");

// ---- where a record lives: its own account (program v1) or a leaf of the accumulator (program v2) ----

const programOf = (item) => B.meta.programs?.[item.protocol || "v1"]?.program ?? B.meta.program;
let accState = null;

/** Program v2's registry, read live, and the dashboard's copy of the leaves rebuilt into peaks. Cached for a minute. */
async function accumulator() {
  if (accState && Date.now() - accState.at < 60_000) return accState.value;
  const meta = B.meta.programs.v2;
  const value = (async () => {
    const [acc, file] = await Promise.all([fetchAccount(B.meta.rpc, meta.registry), fetch(`data/${meta.leaves_file}`).then((r) => r.json())]);
    if (acc.owner !== meta.program) throw new Error(L(`a conta do registro pertence a ${acc.owner}`, `the registry account belongs to ${acc.owner}`));
    const reg = decodeRegistry(acc.data);
    const leaves = file.leaves.map(fromHex);
    const peaks = await mmrPeaks(leaves);
    const same = reg.leaf_count === leaves.length && peaks.every((x, i) => x === reg.peaks[i]);
    return { reg, leaves, same, slot: acc.slot };
  })();
  accState = { at: Date.now(), value };
  value.catch(() => (accState = null));
  return value;
}

/** The on-chain version of an item, with the same field names in both programs, plus how it was proven. */
async function onchain(item, address, decodeV1) {
  if (item.protocol === "v2") {
    const a = await accumulator();
    const bytes = fromHex(item.record);
    const leaf = await hashLeaf(bytes);
    const known = item.record_index < a.leaves.length && toHex(a.leaves[item.record_index]) === toHex(leaf);
    const { siblings } = known ? await mmrProof(a.leaves, item.record_index) : { siblings: [] };
    const v = await mmrVerify(a.reg.peaks, a.reg.leaf_count, leaf, item.record_index, siblings);
    return {
      ...decodeRecord(bytes),
      contextSlot: a.slot,
      owned: v.ok,
      stale: !v.ok && a.reg.leaf_count > a.leaves.length,
      proven: { index: item.record_index, count: a.reg.leaf_count, depth: siblings.length, registry: B.meta.programs.v2.registry },
    };
  }
  const acc = await fetchAccount(B.meta.rpc, address);
  return { ...decodeV1(acc.data), contextSlot: acc.slot, owned: acc.owner === programOf(item), lamports: acc.lamports };
}

/** The check that the record is the program's own: its account (v1) or its place in the tree (v2). */
const OWNED_BY = (c) =>
  c.proven
    ? L(
        `Registro nº ${nf0.format(c.proven.index)} de ${nf0.format(c.proven.count)} na árvore do programa: subindo ${c.proven.depth} níveis chega-se ao pico guardado na <a href="${explorerAddr(c.proven.registry)}" target="_blank" rel="noopener">conta do registro</a>`,
        `Record #${nf0.format(c.proven.index)} of ${nf0.format(c.proven.count)} in the program's tree: ${c.proven.depth} levels up it reaches the peak stored in the <a href="${explorerAddr(c.proven.registry)}" target="_blank" rel="noopener">registry account</a>`,
      )
    : OWNED();
const STALE = () =>
  L("O painel está alguns minutos atrás da cadeia (registros novos ainda não publicados). Tente de novo em instantes.", "The dashboard is a few minutes behind the chain (new records not published yet). Try again shortly.");

/** Link to where the item lives on-chain. */
const chainLink = (item) =>
  item.protocol === "v2"
    ? `<a href="${explorerAddr(B.meta.programs.v2.registry)}" target="_blank" rel="noopener">${L(`registro nº ${nf0.format(item.record_index)}`, `record #${nf0.format(item.record_index)}`)}</a>`
    : `<a href="${explorerAddr(item.batch_account ?? item.account ?? item.record)}" target="_blank" rel="noopener">${L("conta", "account")}</a>`;
const ATTESTED = (n) => L(`Atestado por ${n} de ${B.meta.members.length} nós (limiar ${B.meta.threshold})`, `Attested by ${n} of ${B.meta.members.length} nodes (threshold ${B.meta.threshold})`);
const CHECKED = (slot) => L(`conferido contra a devnet (slot atual ${nf0.format(slot)})`, `checked against devnet (current slot ${nf0.format(slot)})`);
const MISMATCH = () => L("✕ Divergência encontrada.", "✕ Mismatch found.");
const CANT = () => L("Não foi possível verificar", "Could not verify");

function panel(cls, html, target = "#verify-panel") {
  const p = $(target);
  p.className = `verify ${cls}`;
  p.innerHTML = html;
  p.hidden = false;
  return p;
}

async function onchainBatch(d) {
  const c = await onchain(d, d.batch_account, decodeBatch);
  if (!c.owned) throw new Error(c.stale ? STALE() : L("o lote não é um registro do programa do Cota", "the batch is not a record of the Cota program"));
  return c;
}

async function verifyReading(d, index) {
  state.activeReading = index;
  $("#readings").querySelectorAll("tr[data-i]").forEach((tr) => tr.classList.toggle("active", +tr.dataset.i === index));
  const b = await loadBatch(d);
  const original = b.readings[index];
  const r = state.tamper ? { ...original, value_milli: original.value_milli + 1 } : original;
  panel("", `<h3>${L("Verificando a leitura", "Verifying reading")} ${original.station_id} · ${utc(original.ts_utc)}…</h3>`);
  try {
    const bytes = readingBytes(r);
    const leaves = await Promise.all(b.readings.map((x) => hashLeaf(readingBytes(x))));
    const leaf = await hashLeaf(bytes);
    const path = await proof(leaves, index);
    const res = await verifyInclusion(leaf, index, leaves.length, path);
    let chain, chainErr;
    try {
      chain = await onchainBatch(d);
    } catch (e) {
      chainErr = e.message;
    }
    const computed = toHex(res.root);
    const ok = res.ok && chain && computed === chain.merkle_root;
    const steps = res.steps
      .map((s, k) => `<li>${L(`nível ${k + 1}: irmão à ${s.side === "left" ? "esquerda" : "direita"}`, `level ${k + 1}: sibling on the ${s.side}`)} <span class="hash">${short(toHex(s.sibling), 12)}</span> → <span class="hash">${short(toHex(s.result), 12)}</span></li>`)
      .join("");
    const p = panel(
      ok ? "ok" : "fail",
      `<h3>${L("Prova de inclusão", "Inclusion proof")} — ${original.station_id}, ${utc(original.ts_utc)}${state.tamper ? L(" (adulterada: +0,001)", " (tampered: +0.001)") : ""}</h3>
      <ol>
        <li>${L("Bytes canônicos", "Canonical bytes")} (Borsh, ${bytes.length} B): <span class="hash">${short(toHex(bytes), 24)}</span></li>
        <li>${L("Folha", "Leaf")} = SHA-256(0x00 ‖ bytes) = <span class="hash">${toHex(leaf)}</span></li>
        <li>${L(`Caminho de ${path.length} irmãos (posição ${index} de ${leaves.length})`, `Path of ${path.length} siblings (position ${index} of ${leaves.length})`)}:<ol>${steps}</ol></li>
        <li>${L("Raiz recalculada no navegador", "Root recomputed in the browser")}: <span class="hash">${computed}</span></li>
        <li>${L("Raiz on-chain, lida agora da devnet", "On-chain root, read just now from devnet")}${chain ? ` (${chainLink(d)}, slot ${nf0.format(chain.contextSlot)})` : ""}: <span class="hash">${chain ? chain.merkle_root : `${L("não foi possível consultar", "could not query")}: ${esc(chainErr)}`}</span></li>
      </ol>
      <div class="verdict ${ok ? "good-text" : "crit-text"}">${
        ok
          ? L("✓ A leitura está no lote que o comitê assinou e a Solana registrou.", "✓ The reading is in the batch the committee signed and Solana recorded.")
          : chain
            ? L("✕ Não confere: esta leitura não é a que foi ancorada.", "✕ Mismatch: this reading is not the one that was anchored.")
            : L("Não foi possível comparar com a cadeia agora (a raiz do ledger local é ", "Could not compare with the chain right now (the local ledger root is ") + d.merkle_root.slice(0, 12) + "…)."
      }</div>
      <label class="tamper"><input type="checkbox" id="tamper"${state.tamper ? " checked" : ""}/> ${L("Simular adulteração (+0,001 no valor)", "Simulate tampering (+0.001 on the value)")}</label>`,
    );
    p.querySelector("#tamper").addEventListener("change", (e) => {
      state.tamper = e.target.checked;
      verifyReading(d, index);
    });
  } catch (e) {
    panel("fail", `<h3>${L("Erro na verificação", "Verification error")}</h3><p>${esc(e.message)}</p>`);
  }
}

async function verifyBatch(d) {
  panel("", `<h3>${L("Verificando o lote", "Verifying batch")} ${d.batch_id}…</h3>`);
  try {
    const b = await loadBatch(d);
    const root = toHex(await rootFromLeaves(await Promise.all(b.readings.map((x) => hashLeaf(readingBytes(x))))));
    const c = await onchainBatch(d);
    const prev = B.days[state.sel - 1];
    const checks = [
      [L(`Raiz recalculada das ${b.readings.length} leituras = raiz on-chain`, `Root recomputed from the ${b.readings.length} readings = on-chain root`), root === c.merkle_root],
      [L("Encadeia na raiz do lote anterior", "Chains to the previous batch's root"), c.prev_batch_root === (prev ? prev.merkle_root : "0".repeat(64))],
      [L("Quantidade de folhas", "Leaf count"), c.leaf_count === b.readings.length],
      [
        L(
          `Assinantes on-chain (bitmap ${c.signers.toString(2).padStart(3, "0")}): ${c.signer_count} de ${B.meta.members.length}, limiar ${B.meta.threshold}`,
          `On-chain signers (bitmap ${c.signers.toString(2).padStart(3, "0")}): ${c.signer_count} of ${B.meta.members.length}, threshold ${B.meta.threshold}`,
        ),
        c.signer_count >= B.meta.threshold,
      ],
      [OWNED_BY(c), c.owned],
    ];
    const ok = checks.every(([, v]) => v);
    panel(
      ok ? "ok" : "fail",
      `<h3>${L(`Lote ${d.batch_id} conferido contra a devnet`, `Batch ${d.batch_id} checked against devnet`)} (slot ${nf0.format(c.contextSlot)})</h3><ol>${checks
        .map(([t, v]) => `<li>${v ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol><div class="verdict ${ok ? "good-text" : "crit-text"}">${ok ? L("✓ Lote íntegro.", "✓ Batch intact.") : MISMATCH()}</div>`,
    );
  } catch (e) {
    panel("fail", `<h3>${L("Não foi possível consultar a devnet", "Could not query devnet")}</h3><p>${esc(e.message)}</p>`);
  }
}

async function verifyForecast(is) {
  panel("", `<h3>${L("Verificando a previsão emitida em", "Verifying the forecast issued on")} ${dmy(is.issue_day)}…</h3>`);
  try {
    const froot = toHex(await rootFromLeaves(await Promise.all(is.features.map((f) => hashLeaf(featureLeaf(f))))));
    const items = [];
    let ok = froot === is.features_root;
    items.push([L(`features_root recalculada de ${is.features.length} features = ${short(froot)}`, `features_root recomputed from ${is.features.length} features = ${short(froot)}`), ok]);
    for (const f of is.horizons) {
      const c = await onchain(f, f.account, decodeForecast);
      const checks = [
        c.owned,
        c.p10 === f.p10 && c.p50 === f.p50 && c.p90 === f.p90,
        c.features_root === froot,
        c.model_sha256 === B.meta.model_sha256,
        c.based_on_root === is.based_on_root,
      ];
      const target = B.days[B.dayIndex.get(f.target_day)];
      const before = target ? c.slot < target.slot : null;
      const pass = checks.every(Boolean) && before !== false;
      ok &&= pass;
      items.push([
        L(
          `+${f.horizon_hours} h → ${dm(f.target_day)}: quantis, modelo e features conferem; gravada no slot ${nf0.format(c.slot)}` +
            (target ? `, ${before ? "antes" : "DEPOIS"} do lote da observação (slot ${nf0.format(target.slot)})` : ", observação ainda não ancorada"),
          `+${f.horizon_hours} h → ${dm(f.target_day)}: quantiles, model and features match; recorded at slot ${nf0.format(c.slot)}` +
            (target ? `, ${before ? "before" : "AFTER"} the observation's batch (slot ${nf0.format(target.slot)})` : ", observation not anchored yet"),
        ),
        pass,
      ]);
    }
    panel(
      ok ? "ok" : "fail",
      `<h3>${L(`Previsão de ${dmy(is.issue_day)} conferida contra a devnet`, `Forecast of ${dmy(is.issue_day)} checked against devnet`)}</h3><ol>${items
        .map(([t, v]) => `<li>${v ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol><p class="sub">${L("Modelo", "Model")} <span class="hash">${B.meta.model_sha256}</span></p>`,
    );
  } catch (e) {
    panel("fail", `<h3>${L("Não foi possível consultar a devnet", "Could not query devnet")}</h3><p>${esc(e.message)}</p>`);
  }
}

// ---- qualidade ------------------------------------------------------------

function seqClass(score) {
  if (score >= 0.995) return 0;
  if (score >= 0.95) return 1;
  if (score >= 0.85) return 2;
  if (score >= 0.7) return 3;
  if (score >= 0.6) return 4;
  return 5;
}

function renderQuality() {
  const ids = B.stations.map((s) => s.id);
  const n = B.days.length;
  let html = `<div class="hm-grid" style="grid-template-columns:minmax(120px,auto) repeat(${n}, minmax(16px,1fr))"><div></div>`;
  html += B.days.map((d) => `<div class="tl-cell tl-day">${dm(d.day)}</div>`).join("");
  for (const id of ids) {
    html += `<div class="tl-label">${id}</div>`;
    html += B.days
      .map((d, i) => {
        const q = d.quality.find((x) => x.station_id === id);
        if (!q) return `<div></div>`;
        const k = seqClass(q.score);
        const ink = `var(--seq-ink-${k})`;
        const mark = q.degraded ? "!" : q.flags.length ? "•" : "";
        return `<button type="button" class="hm-cell${q.degraded ? " degraded" : ""}" style="background:var(--seq-${k});color:${ink}" data-s="${id}" data-i="${i}" aria-label="${id} ${dmy(d.day)}: C ${nf2.format(q.score)}">${mark}</button>`;
      })
      .join("");
  }
  html += "</div>";
  const box = $("#quality");
  box.innerHTML = html;
  box.querySelectorAll(".hm-cell").forEach((c) => {
    const d = B.days[+c.dataset.i];
    const q = d.quality.find((x) => x.station_id === c.dataset.s);
    c.addEventListener("pointermove", (ev) =>
      showTip(
        `<div class="t-title">${q.station_id} · ${dmy(d.day)}</div>` +
          row(L("C<sub>s</sub> (média móvel)", "C<sub>s</sub> (moving average)"), nf2.format(q.score)) +
          row(L("C do dia", "Day's C"), nf2.format(q.raw)) +
          row(L("Disponibilidade", "Availability"), `${q.present}/${q.expected}`) +
          row("Flags", q.flags.length ? q.flags.map(reason).join(", ") : L("nenhuma", "none")) +
          row(L("Degradada", "Degraded"), q.degraded ? reason(q.degraded) : L("não", "no")),
        ev.clientX,
        ev.clientY,
      ),
    );
    c.addEventListener("pointerleave", hideTip);
    c.addEventListener("click", () => {
      state.station = q.station_id;
      selectDay(+c.dataset.i);
      $("#day-section").scrollIntoView({ behavior: "smooth" });
    });
  });
  const cuts = [0.995, 0.95, 0.85, 0.7, 0.6];
  const fmt = (v) => new Intl.NumberFormat(L("pt-BR", "en-US"), { minimumFractionDigits: 2, maximumFractionDigits: 3 }).format(v);
  $("#quality-legend").innerHTML =
    `<span class="key">${L("Índice C", "C index")} <span class="ramp">${[0, 1, 2, 3, 4, 5].map((k) => `<span style="background:var(--seq-${k})"></span>`).join("")}</span> ≥ ${fmt(cuts[0])} → &lt; ${fmt(cuts[4])}</span>` +
    `<span class="key"><span class="dot" style="background:var(--seq-5);box-shadow:inset 0 0 0 2px var(--critical)">!</span>SourceDegraded on-chain</span>` +
    `<span class="key"><span class="dot" style="background:var(--seq-0);color:var(--text)">•</span>${L("flag de sensor (travado/salto)", "sensor flag (stuck/jump)")}</span>`;
}

function renderMetrics() {
  const t = B.metrics.test;
  const rows = ["h1", "h2"]
    .map((k, j) => {
      const r = t[k];
      return `<tr><td>${j + 1} ${L(j ? "dias" : "dia", j ? "days" : "day")}</td><td class="num">${nf2.format(r.nse_p50)}</td><td class="num">${nf2.format(r.nse_persistence)}</td><td class="num">${nf2.format(r.kge_p50)}</td><td class="num">${nf0.format(r.crps)}</td><td class="num">${pct(r.coverage_p10_p90)}</td><td class="num">${nf2.format(r.alerts.pod)}</td><td class="num">${nf2.format(r.alerts.far)}</td><td class="num">${nf2.format(r.alerts.csi)}</td></tr>`;
    })
    .join("");
  $("#metrics").innerHTML =
    `<table class="data"><thead><tr><th>${L("Avaliação do modelo (alvos 26/04–25/05)", "Model evaluation (targets Apr 26 – May 25)")}</th><th class="num">NSE P50</th><th class="num">${L("NSE persistência", "NSE persistence")}</th><th class="num">KGE</th><th class="num">CRPS (m³/s)</th><th class="num">${L("Cobertura P10–P90", "P10–P90 coverage")}</th><th class="num">POD</th><th class="num">FAR</th><th class="num">CSI</th></tr></thead><tbody>${rows}</tbody></table>`;
  document.querySelectorAll(".model-sha").forEach((n) => (n.textContent = `model_sha256 ${B.meta.model_sha256.slice(0, 16)}…`));
}

// ---- live -----------------------------------------------------------------

// Brasília time in both languages; in English, "25 Oct 2026", like dm/dmy.
const br = (ts) =>
  new Date(ts * 1000).toLocaleString(L("pt-BR", "en-GB"), { timeZone: "America/Sao_Paulo", dateStyle: L("short", "medium"), timeStyle: "short" });
const SEVERITY = {
  3: { cls: "st-critical", label: ["Grande perigo (vermelho)", "Great danger (red)"] },
  2: { style: "background:var(--serious)", label: ["Perigo (laranja)", "Danger (orange)"] },
  1: { cls: "st-warning", label: ["Perigo potencial (amarelo)", "Potential danger (yellow)"] },
  0: { style: "background:var(--muted)", label: ["Outro", "Other"] },
};

function liveSeries() {
  const LV = B.live;
  const byTarget = new Map();
  for (const is of LV.issues) for (const f of is.horizons) if (f.horizon_hours === 24) byTarget.set(f.target_day, { issue: is, f });
  const obs = new Map(LV.days.map((d) => [d.day, d.guaiba_milli]));
  const rows = [];
  for (let d = LV.days[0].day; d <= addDays(LV.days.at(-1).day, 2); d = addDays(d, 1)) {
    const fc = byTarget.get(d);
    rows.push({ day: d, obs: obs.get(d) ?? null, fc: fc?.f ?? null, issue: fc?.issue ?? null });
  }
  // The +48 h target of the latest issue also goes into the band.
  const last = LV.issues.at(-1);
  const h48 = last?.horizons.find((f) => f.horizon_hours === 48);
  const tail = rows.find((r) => r.day === h48?.target_day);
  if (tail && !tail.fc) Object.assign(tail, { fc: h48, issue: last });
  return rows;
}

function renderLiveChart() {
  if (!B.live.days.length) return;
  renderFlow({ box: $("#live-chart"), data: liveSeries(), selDay: null, h: 24, onPick: () => {}, legend: $("#live-legend"), table: false });
}

function renderLive() {
  const LV = B.live;
  const thr = B.meta.alert_threshold_milli;
  if (!LV.days.length) {
    $("#live-sub").textContent = L("Modo ao vivo ainda não iniciado: nenhum lote depois do replay.", "Live mode not started yet: no batch after the replay.");
  } else {
    const d = LV.days.at(-1);
    renderStage();
    renderBasin();
    $("#live-sub").textContent = L(
      `Último lote: ${dmy(d.day)} (lote ${d.batch_id}), ancorado em ${br(d.committed_at)} (horário de Brasília). ` +
        `${LV.days.length} dia(s) desde ${dmy(LV.days[0].day)}. Nível medido nas réguas da ANA; vazão modelada pelo GloFAS; a chuva do INMET entra com ~1 mês de atraso.`,
      `Latest batch: ${dmy(d.day)} (batch ${d.batch_id}), anchored ${br(d.committed_at)} (Brasília time). ` +
        `${LV.days.length} day(s) since ${dmy(LV.days[0].day)}. Level measured on ANA gauges; flow modelled by GloFAS; INMET rain comes in about 1 month late.`,
    );
    // Each day's batch goes in on the robot's first run after the day closes (00:00 UTC), within a few hours.
    // A day closed more than 12 h ago and still missing means the robot or a source stopped.
    const nextEnd = Date.parse(d.day) + 2 * 864e5;
    const behind = Math.floor((Date.now() - nextEnd + 864e5) / 864e5);
    if (Date.now() > nextEnd + 12 * 3600e3) {
      $("#live-sub").insertAdjacentHTML(
        "afterbegin",
        `<strong class="crit-text">${L(
          `⚠ O registro está ${behind} dia(s) atrasado: o último dia gravado é ${dmy(d.day)}. `,
          `⚠ The record is ${behind} day(s) behind: the latest recorded day is ${dmy(d.day)}. `,
        )}</strong>`,
      );
    }
    const is = LV.issues.at(-1);
    const fc = (h) => is?.horizons.find((f) => f.horizon_hours === h);
    const alerting = is?.horizons.filter((f) => f.alert) ?? [];
    const red = LV.public_alerts.filter((a) => a.severity === 3).length;
    const tiles = [
      [L(`Vazão em ${dm(d.day)}`, `Flow on ${dm(d.day)}`), `${m3(d.guaiba_milli)}`, "m³/s, Guaíba (GloFAS)"],
      ...[24, 48]
        .map((h) => fc(h))
        .filter(Boolean)
        .map((f) => [L(`Previsão para ${dm(f.target_day)}`, `Forecast for ${dm(f.target_day)}`), m3(f.p50), `P10–P90: ${m3(f.p10)}–${m3(f.p90)} m³/s`]),
      [
        L("Situação do modelo", "Model status"),
        alerting.length ? L("▼ alerta", "▼ alert") : L("sem alerta", "no alert"),
        alerting.length
          ? L(`P90 ≥ ${m3(thr)} para ${alerting.map((f) => dm(f.target_day)).join(" e ")}`, `P90 ≥ ${m3(thr)} for ${alerting.map((f) => dm(f.target_day)).join(" and ")}`)
          : L(`P90 abaixo de ${m3(thr)} m³/s`, `P90 below ${m3(thr)} m³/s`),
      ],
      [
        L("Avisos oficiais registrados", "Official warnings recorded"),
        LV.public_alerts.length,
        L(`${red} vermelho(s) · desde o início do monitoramento`, `${red} red · since monitoring began`),
      ],
    ];
    $("#live-kpis").innerHTML = tiles
      .map(([l, v, n]) => `<div class="kpi"><div class="label">${l}</div><div class="value">${v}</div><div class="note">${n}</div></div>`)
      .join("");
    renderLiveChart();
  }

  if (!LV.public_alerts.length) {
    $("#live-alerts").innerHTML = `<div class="empty-note">${L("Nenhum aviso do INMET para o RS registrado até agora.", "No INMET warning for Rio Grande do Sul recorded so far.")}</div>`;
    return;
  }
  const rows = LV.public_alerts
    .map((a, i) => {
      const s = SEVERITY[a.severity] ?? SEVERITY[0];
      const n = a.signers.toString(2).split("").filter((b) => b === "1").length;
      return `<tr><td>${br(a.committed_at)}<br><span class="hash">slot ${nf0.format(a.slot)}</span></td>
        <td>${esc(a.description)}<br><span class="hash" title="${esc(a.alert_id)}">${esc(a.alert_id.split(".").slice(-2).join("."))}</span></td>
        <td><span class="sev"><span class="dot ${s.cls ?? ""}" style="${s.style ?? ""}">!</span>${L(...s.label)}</span></td>
        <td>${br(a.onset)} → ${br(a.expires)}</td>
        <td class="num">${n}/${B.meta.members.length}</td>
        <td><button type="button" class="link-btn" data-a="${i}">${L("verificar", "verify")}</button>${a.explorer ? ` · <a href="${a.explorer}" target="_blank" rel="noopener">tx</a>` : ""}</td></tr>`;
    })
    .join("");
  $("#live-alerts").innerHTML =
    `<table class="data"><thead><tr><th>${L("Gravado on-chain", "Recorded on-chain")}</th><th>${L("Aviso (texto do INMET)", "Warning (INMET text, Portuguese)")}</th><th>${L("Severidade", "Severity")}</th><th>${L("Vigência", "In force")}</th><th class="num">${L("Nós", "Nodes")}</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
  $("#live-alerts").querySelectorAll("[data-a]").forEach((b) => b.addEventListener("click", () => verifyPublicAlert(LV.public_alerts[+b.dataset.a])));
}

// ---- river level (ANA gauge) ----------------------------------------------
// Reference levels used by Porto Alegre's civil defense on the Cais Mauá gauge.

const STAGE_FLOOD = 3000;
const STAGE_NEAR = 2500;
const STAGE_RECORD = { milli: 5350, day: "2024-05-05" };
const meters = (milli) => nf2.format(milli / 1000);
const cm = (milli) => nf0.format(Math.abs(milli) / 10);
// Brasília time (UTC−3) of an instant.
const brHour = (ts) => new Date((ts - 3 * 3600) * 1000).toISOString();
const riverName = (sid) => {
  const n = (B.live.stage_stations.find((s) => s.id === sid)?.name ?? sid).replace(/ \(ANA\)$/, "");
  return L(n, n.replace(" em ", " at "));
};

// `sameDay`: without the previous day, the change is between the day's own first and last reading.
function stageChange(milli, sameDay = false) {
  const span = sameDay ? L("ao longo do dia", "during the day") : L("em 24 h", "in 24 h");
  if (milli === 0) return L(`estável ${span}`, `steady ${span}`);
  return milli > 0 ? L(`▲ subiu ${cm(milli)} cm ${span}`, `▲ up ${cm(milli)} cm ${span}`) : L(`▼ desceu ${cm(milli)} cm ${span}`, `▼ down ${cm(milli)} cm ${span}`);
}

// A gauge's change: against the end of the previous day, or within the day itself if that is
// missing.
function gaugeChange(cur, prev) {
  if (prev?.last != null) return { ch: cur.last - prev.last, sameDay: false };
  if (cur.first != null && cur.hours > 1) return { ch: cur.last - cur.first, sameDay: true };
  return null;
}

function renderStage() {
  const LV = B.live;
  const ser = LV.stage_series ?? [];
  $("#stage").hidden = !ser.length;
  if (!ser.length) return;

  const [ts, v] = ser.at(-1);
  const prev = ser.find(([t]) => t === ts - 86_400);
  const iso = brHour(ts);
  const when = L(`${iso.slice(11, 13)}h de ${dm(iso.slice(0, 10))}`, `${iso.slice(11, 13)}:00 on ${dm(iso.slice(0, 10))}`);
  const state =
    v >= STAGE_FLOOD
      ? { icon: "▲", cls: "alert", h: L("Guaíba acima da cota de inundação", "Guaíba above flood level") }
      : v >= STAGE_NEAR
        ? { icon: "!", cls: "warn", h: L("Guaíba perto da cota de inundação", "Guaíba close to flood level") }
        : { icon: "✓", cls: "ok", h: L("Guaíba em nível normal", "Guaíba at a normal level") };
  const max = Math.max(6000, v * 1.1);
  const at = (x) => `${Math.min(100, (x / max) * 100)}%`;
  const day = LV.days.findLast((d) => d.stage);

  const status = `<div class="now-status">
      <span class="now-icon ${state.cls}" aria-hidden="true">${state.icon}</span>
      <div>
        <h3>${state.h}</h3>
        <p>${L(
          `O nível no Cais Mauá era de <strong>${meters(v)} m</strong> às ${when}${prev ? ` (${stageChange(v - prev[1])})` : ""}. Porto Alegre começa a alagar a partir de ${meters(STAGE_FLOOD)} m; em 2024 o rio chegou a ${meters(STAGE_RECORD.milli)} m.`,
          `The level at Cais Mauá was <strong>${meters(v)} m</strong> at ${when}${prev ? ` (${stageChange(v - prev[1])})` : ""}. Porto Alegre starts to flood from ${meters(STAGE_FLOOD)} m; in 2024 the river reached ${meters(STAGE_RECORD.milli)} m.`,
        )}</p>
        <div class="meter" role="img" aria-label="${L(`${meters(v)} m; inundação a partir de ${meters(STAGE_FLOOD)} m`, `${meters(v)} m; flooding from ${meters(STAGE_FLOOD)} m`)}">
          <div class="meter-track"><div class="meter-fill${v >= STAGE_FLOOD ? " alert" : ""}" style="width:${at(v)}"></div><span class="meter-mark" style="left:${at(STAGE_FLOOD)}"></span><span class="meter-mark soft" style="left:${at(STAGE_RECORD.milli)}"></span></div>
          <div class="meter-labels"><span style="left:0">0 m</span><span style="left:${at(STAGE_FLOOD)}">${L("inundação", "flooding")} · ${meters(STAGE_FLOOD)} m</span><span class="rec" style="left:${at(STAGE_RECORD.milli)}">2024 · ${meters(STAGE_RECORD.milli)} m</span></div>
        </div>
        <p class="sub">${L(
          `Medido na régua da ANA (estação ${LV.stage_main.replace("ANA-", "")}), hora a hora, e gravado na devnet no lote de ${dmy(day.day)}.`,
          `Measured on the ANA gauge (station ${LV.stage_main.replace("ANA-", "")}), every hour, and recorded on devnet in the batch for ${dmy(day.day)}.`,
        )}</p>
      </div>
    </div>`;

  // Tributaries: each gauge has its own zero, so what can be compared is the change.
  // Only compares with the day right before: with a day missing, the "change in 24 h" would be over 48 h.
  const before = LV.days.find((d) => d.day === addDays(day.day, -1) && d.stage);
  const rivers = Object.entries(day.stage)
    .filter(([sid]) => sid !== LV.stage_main)
    .map(([sid, s]) => {
      const g = gaugeChange(s, before?.stage?.[sid]);
      const ch = g ? stageChange(g.ch, g.sameDay) : L("sem leitura do dia anterior", "no reading from the day before");
      return `<div><dt>${esc(riverName(sid))}</dt><dd>${meters(s.last)} m<small>${ch}</small></dd></div>`;
    })
    .join("");
  $("#stage-now").innerHTML =
    status +
    (rivers
      ? `<div><p class="facts-title">${L("Rios que chegam ao Guaíba", "Rivers flowing into the Guaíba")}</p><dl class="facts compact">${rivers}</dl>
         <p class="sub">${L("Fim do dia anterior. Cada régua tem o seu próprio zero: compare a subida, não a altura.", "End of the previous day. Each gauge has its own zero: compare the rise, not the height.")}</p></div>`
      : "");
  renderStageChart(ser);
}

function renderStageChart(ser) {
  const box = $("#stage-chart");
  const W = Math.max(320, box.clientWidth);
  const H = W < 560 ? 220 : 280;
  const m = { l: 44, r: 18, t: 14, b: 26 };
  const t0 = ser[0][0], t1 = ser.at(-1)[0];
  const ymax = Math.max(3500, Math.ceil((Math.max(...ser.map((p) => p[1])) * 1.1) / 500) * 500);
  const x = (t) => m.l + ((t - t0) / Math.max(1, t1 - t0)) * (W - m.l - m.r);
  const y = (v) => H - m.b - (v / ymax) * (H - m.t - m.b);
  const svg = el("svg", { viewBox: `0 0 ${W} ${H}`, width: W, height: H });

  for (let v = 0; v <= ymax; v += 500) {
    if (v % 1000 && ymax > 4000) continue;
    el("line", { x1: m.l, x2: W - m.r, y1: y(v), y2: y(v), class: v === 0 ? "baseline" : "gridline" }, svg);
    el("text", { x: m.l - 6, y: y(v) + 4, "text-anchor": "end", class: "tick-label" }, svg).textContent = `${nf1.format(v / 1000)} m`;
  }
  // one label per day (Brasília), spaced according to the width
  const days = [];
  for (let t = Math.ceil((t0 + 3 * 3600) / 86_400) * 86_400 - 3 * 3600; t <= t1; t += 86_400) days.push(t);
  const every = Math.max(1, Math.ceil(days.length / (W < 560 ? 4 : 8)));
  days.forEach((t, i) => {
    if (i % every === 0) el("text", { x: x(t), y: H - 6, "text-anchor": "middle", class: "tick-label" }, svg).textContent = dm(brHour(t).slice(0, 10));
  });

  el("line", { x1: m.l, x2: W - m.r, y1: y(STAGE_FLOOD), y2: y(STAGE_FLOOD), stroke: "var(--critical)", "stroke-width": 1, "stroke-dasharray": "2 3" }, svg);
  el("text", { x: m.l + 6, y: y(STAGE_FLOOD) - 5, class: "tick-label" }, svg).textContent = L(`cota de inundação · ${meters(STAGE_FLOOD)} m`, `flood level · ${meters(STAGE_FLOOD)} m`);

  // Gaps longer than 6 h (gauge not transmitting) break the line instead of joining the points;
  // an odd missing hour, common in telemetry, does not.
  let d = "";
  ser.forEach(([t, v], i) => (d += `${i && t - ser[i - 1][0] <= 6 * 3600 ? "L" : "M"}${x(t).toFixed(1)},${y(v).toFixed(1)}`));
  el("path", { d, fill: "none", stroke: "var(--observed)", "stroke-width": 2, "stroke-linejoin": "round" }, svg);

  const hover = el("g", { "pointer-events": "none" }, svg);
  const overlay = el("rect", { x: m.l, y: m.t, width: W - m.l - m.r, height: H - m.t - m.b, fill: "transparent" }, svg);
  const move = (ev) => {
    const r = svg.getBoundingClientRect();
    const t = t0 + ((ev.clientX - r.left) * (W / r.width) - m.l) / (W - m.l - m.r) * (t1 - t0);
    let best = ser[0];
    for (const p of ser) if (Math.abs(p[0] - t) < Math.abs(best[0] - t)) best = p;
    hover.innerHTML = "";
    el("line", { x1: x(best[0]), x2: x(best[0]), y1: m.t, y2: H - m.b, stroke: "var(--muted)", "stroke-width": 1 }, hover);
    el("circle", { cx: x(best[0]), cy: y(best[1]), r: 4, fill: "var(--observed)", stroke: "var(--surface)", "stroke-width": 2 }, hover);
    const iso = brHour(best[0]);
    showTip(`<strong>${dmy(iso.slice(0, 10))}, ${iso.slice(11, 13)}h</strong>${row(L("Nível", "Level"), `${meters(best[1])} m`)}`, ev.clientX, ev.clientY);
  };
  overlay.addEventListener("pointermove", move);
  overlay.addEventListener("pointerleave", () => {
    hover.innerHTML = "";
    hideTip();
  });
  box.replaceChildren(svg);
  $("#stage-legend").innerHTML = `<span class="key" style="color:var(--observed)"><span class="swatch-line"></span><span style="color:var(--text-2)">${L(
    `Nível medido no Cais Mauá, hora a hora (últimos ${Math.round((t1 - t0) / 86_400)} dias gravados)`,
    `Level measured at Cais Mauá, hour by hour (last ${Math.round((t1 - t0) / 86_400)} recorded days)`,
  )}</span></span>` +
    `<span class="key" style="color:var(--critical)"><span class="swatch-dash"></span><span style="color:var(--text-2)">${L("Cota de inundação de Porto Alegre", "Porto Alegre flood level")}</span></span>`;
}

// ---- basin map -------------------------------------------------------------
// Our own drawing (scripts/build_map.py): basin outline, rivers and land in already
// projected coordinates. The stations use the same projection. Clicking a point shows the latest
// recorded value.

const basin = { map: null, zoom: "all", sel: null, wired: false };
// "Near Porto Alegre" frame (lon/lat), where the stations crowd together.
const POA_FRAME = [-52.08, -30.32, -50.72, -29.4];
const PORTO_ALEGRE = [-51.23, -30.03];

function mapXY([lon, lat]) {
  const { frame, k, scale } = basin.map.projection;
  return [(lon - frame[0]) * k * scale, (frame[3] - lat) * scale];
}

// Latest live day with data for the station, and the day right before it.
function lastWith(get) {
  const days = B.live.days;
  for (let i = days.length - 1; i >= 0; i--) {
    const v = get(days[i]);
    if (v != null) {
      const prev = days.find((d) => d.day === addDays(days[i].day, -1));
      return { day: days[i], v, prev: prev ? get(prev) : null };
    }
  }
  return null;
}

function basinPoints() {
  const pts = [];
  // Bundles generated before the map have no `live.stations`: fall back to the replay catalog
  // (without the gauges).
  for (const s of B.live.stations ?? B.stations.filter((x) => x.source === "open_meteo_glofas")) {
    if (s.source === "ana") {
      const found = lastWith((d) => d.stage?.[s.id]);
      const last = found && { ...found, v: found.v.last };
      const g = found ? gaugeChange(found.v, found.prev) : null;
      const trend = !last ? "none" : !g || Math.abs(g.ch) < 20 ? "steady" : g.ch > 0 ? "up" : "down";
      pts.push({ ...s, kind: "ana", last, ch: g?.ch ?? null, sameDay: g?.sameDay ?? false, trend });
    } else if (s.source === "open_meteo_glofas") {
      pts.push({ ...s, kind: "glofas", last: lastWith((d) => d.flow?.[s.id]) });
    }
  }
  for (const id of B.outlook.rule.points) {
    const s = B.stations.find((x) => x.id === id);
    if (s) pts.push({ ...s, kind: "rain" });
  }
  return pts;
}

const TREND = {
  up: { color: "var(--serious)", label: ["subindo", "rising"] },
  down: { color: "var(--brand)", label: ["descendo", "falling"] },
  steady: { color: "var(--muted)", label: ["estável", "steady"] },
  none: { color: "var(--surface)", label: ["sem leitura gravada ainda", "no reading recorded yet"] },
};
const cleanName = (n) => {
  const t = n.replace(/ \((ANA|GloFAS)\)$/, "");
  return L(t, t.replace(" em ", " at "));
};

async function renderBasin() {
  if (!basin.map) {
    try {
      basin.map = await (await fetch("basin-map.json")).json();
    } catch {
      $("#basin").hidden = true;
      return;
    }
  }
  if (!basin.wired) {
    basin.wired = true;
    $("#basin-zoom").querySelectorAll("button").forEach((b) =>
      b.addEventListener("click", () => {
        basin.zoom = b.dataset.zoom;
        $("#basin-zoom").querySelectorAll("button").forEach((x) => x.setAttribute("aria-checked", x === b));
        drawBasin();
      }),
    );
  }
  const pts = basinPoints();
  basin.sel ??= pts.find((p) => p.id === B.live.stage_main && p.last)?.id ?? "GLOFAS-GUAIBA";
  // Same marks as on the map, at legend size.
  const icon = (inner) => `<svg class="mk" viewBox="-8 -8 16 16" width="16" height="16" aria-hidden="true">${inner}</svg>`;
  const dot = (fill, stroke = "var(--surface)") => icon(`<circle r="6" fill="${fill}" stroke="${stroke}" stroke-width="2"/>`);
  $("#basin-legend").innerHTML = [
    [dot("var(--serious)"), L("Régua de nível (ANA), subindo", "River gauge (ANA), rising")],
    [dot("var(--brand)"), L("Régua de nível, descendo", "River gauge, falling")],
    [dot("var(--muted)"), L("Régua de nível, estável", "River gauge, steady")],
    [dot("var(--surface)", "var(--muted)"), L("Régua ainda sem leitura gravada", "Gauge with no reading recorded yet")],
    [icon('<path d="M0,-5L5,0L0,5L-5,0Z" fill="var(--text-2)"/>'), L("Vazão calculada por satélite e modelo (GloFAS)", "Flow computed from satellite and model (GloFAS)")],
    [icon('<path d="M-5,-3L5,-3L0,5Z" fill="var(--surface)" stroke="var(--text-2)" stroke-width="1.5" stroke-linejoin="round"/>'), L("Ponto onde se lê a chuva prevista", "Point where forecast rain is read")],
  ]
    .map(([m, t]) => `<li>${m}${t}</li>`)
    .join("");
  drawBasin();
}

function drawBasin() {
  const M = basin.map;
  const box = $("#basin-map");
  const W = Math.max(300, box.clientWidth);
  let vb = [0, 0, M.width, M.height];
  if (basin.zoom === "poa") {
    const [x0, y0] = mapXY([POA_FRAME[0], POA_FRAME[3]]);
    const [x1, y1] = mapXY([POA_FRAME[2], POA_FRAME[1]]);
    vb = [x0, y0, x1 - x0, y1 - y0];
  }
  const H = Math.round((W * vb[3]) / vb[2]);
  const u = vb[2] / W; // map units per screen pixel
  const svg = el("svg", { viewBox: vb.join(" "), width: W, height: H, class: "basin-svg" });

  el("rect", { x: vb[0], y: vb[1], width: vb[2], height: vb[3], fill: "var(--map-water)" }, svg);
  el("path", { d: M.land, fill: "var(--map-land)" }, svg);
  el("path", { d: M.basin, fill: "var(--map-basin)", stroke: "var(--brand)", "stroke-width": 1.2 * u, "stroke-linejoin": "round" }, svg);
  for (const d of Object.values(M.groups)) el("path", { d, fill: "none", stroke: "var(--brand)", "stroke-opacity": 0.35, "stroke-width": u, "stroke-dasharray": `${3 * u} ${3 * u}` }, svg);
  el("path", { d: M.state, fill: "none", stroke: "var(--axis)", "stroke-width": u, "stroke-dasharray": `${4 * u} ${3 * u}` }, svg);
  const rw = { 1: 0.8, 2: 1.6, 3: 3 };
  for (const [c, d] of Object.entries(M.rivers)) el("path", { d, fill: "none", stroke: "var(--map-river)", "stroke-width": rw[c] * u, "stroke-linecap": "round", "stroke-linejoin": "round" }, svg);
  for (const l of M.labels) {
    const t = el("text", { x: l.x, y: l.y, class: "map-label", "font-size": 12 * u }, svg);
    t.textContent = l.name;
  }
  const [px, py] = mapXY(PORTO_ALEGRE);
  el("rect", { x: px - 4 * u, y: py - 4 * u, width: 8 * u, height: 8 * u, fill: "var(--text)", stroke: "var(--surface)", "stroke-width": 1.5 * u }, svg);
  const pl = el("text", { x: px - 9 * u, y: py + 22 * u, "text-anchor": "end", class: "map-city", "font-size": 13 * u }, svg);
  pl.textContent = "Porto Alegre";

  // Drawing order: rain, GloFAS and, on top, the gauges.
  const pts = basinPoints().sort((a, b) => ["rain", "glofas", "ana"].indexOf(a.kind) - ["rain", "glofas", "ana"].indexOf(b.kind));
  for (const p of pts) {
    const [x, y] = mapXY([p.lon, p.lat]);
    // Not drawn outside the frame: an invisible point must not receive keyboard focus.
    if (x < vb[0] || x > vb[0] + vb[2] || y < vb[1] || y > vb[1] + vb[3]) continue;
    const g = el("g", { class: `mk-g${p.id === basin.sel ? " sel" : ""}`, tabindex: 0, role: "button", "aria-label": `${cleanName(p.name)}: ${pointSummary(p)}`, "aria-pressed": p.id === basin.sel }, svg);
    if (p.id === basin.sel) el("circle", { cx: x, cy: y, r: 13 * u, fill: "none", stroke: "var(--text)", "stroke-width": 2 * u }, g);
    if (p.kind === "ana") {
      const t = TREND[p.trend];
      el("circle", { cx: x, cy: y, r: 7 * u, fill: t.color, stroke: p.trend === "none" ? "var(--muted)" : "var(--surface)", "stroke-width": 2 * u }, g);
    } else if (p.kind === "glofas") {
      const r = 5 * u;
      el("path", { d: `M${x},${y - r}L${x + r},${y}L${x},${y + r}L${x - r},${y}Z`, fill: "var(--text-2)", stroke: "var(--surface)", "stroke-width": 1.5 * u }, g);
    } else {
      const r = 5 * u;
      el("path", { d: `M${x - r},${y - r * 0.6}L${x + r},${y - r * 0.6}L${x},${y + r}Z`, fill: "var(--surface)", stroke: "var(--text-2)", "stroke-width": 1.5 * u, "stroke-linejoin": "round" }, g);
    }
    // Touch area larger than the drawing.
    el("circle", { cx: x, cy: y, r: 14 * u, fill: "transparent" }, g);
    const pick = () => {
      basin.sel = p.id;
      drawBasin();
      $("#basin-map").querySelector(".mk-g.sel")?.focus();
    };
    g.addEventListener("click", pick);
    g.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter" || ev.key === " ") {
        ev.preventDefault();
        pick();
      }
    });
    g.addEventListener("pointermove", (ev) => showTip(`<strong>${esc(cleanName(p.name))}</strong><div>${pointSummary(p)}</div>`, ev.clientX, ev.clientY));
    g.addEventListener("pointerleave", hideTip);
  }
  box.replaceChildren(svg);
  renderBasinDetail(pts.find((p) => p.id === basin.sel));
}

function pointSummary(p) {
  if (p.kind === "ana") return p.last ? `${meters(p.last.v)} m · ${L(...TREND[p.trend].label)}` : L(...TREND.none.label);
  if (p.kind === "glofas") return p.last ? `${m3(p.last.v)} m³/s` : L("sem dado", "no data");
  return L("chuva prevista", "forecast rain");
}

function renderBasinDetail(p) {
  if (!p) return;
  const kinds = {
    ana: L("Régua de nível · ANA", "River gauge · ANA"),
    glofas: L("Vazão calculada · GloFAS", "Computed flow · GloFAS"),
    rain: L("Ponto da chuva prevista", "Forecast rain point"),
  };
  let body = "";
  if (p.kind === "rain") {
    const o = B.outlook.live[0];
    const R = B.outlook.rule;
    body = `<p>${L(
      `Um dos ${R.points.length} pontos onde o Cota lê a chuva que três modelos meteorológicos preveem para os próximos ${R.window_days} dias. A média desses pontos é o que acende o alerta antecipado.`,
      `One of the ${R.points.length} points where Cota reads the rain three weather models forecast for the next ${R.window_days} days. The average of these points is what triggers the early warning.`,
    )}</p>${
      o
        ? `<div class="detail-value">${mm(o.rain_mean_milli)} mm</div><p class="sub">${L(
            `média da bacia na previsão de ${dmy(o.day)}; o alerta acende a partir de ${R.threshold_mm} mm`,
            `basin average in the forecast from ${dmy(o.day)}; the warning goes on from ${R.threshold_mm} mm`,
          )}</p>`
        : ""
    }`;
  } else if (!p.last) {
    body = `<p>${L(
      "Ainda não há leitura desta estação gravada na cadeia. As réguas da ANA entram nos lotes a partir de novembro de 2026.",
      "No reading from this station has been recorded on-chain yet. ANA gauges join the batches from November 2026.",
    )}</p>`;
  } else {
    const d = p.last.day;
    const q = d.quality?.find((x) => x.station_id === p.id);
    const value = p.kind === "ana" ? `${meters(p.last.v)} m` : `${m3(p.last.v)} m³/s`;
    let change = "";
    if (p.kind === "ana") change = p.ch == null ? L("sem leitura do dia anterior", "no reading from the day before") : stageChange(p.ch, p.sameDay);
    else if (p.last.prev) {
      const pc = (p.last.v / p.last.prev - 1) * 100;
      change = Math.abs(pc) < 1 ? L("estável em 24 h", "steady over 24 h") : pc > 0 ? L(`▲ ${nf0.format(pc)}% em 24 h`, `▲ ${nf0.format(pc)}% in 24 h`) : L(`▼ ${nf0.format(-pc)}% em 24 h`, `▼ ${nf0.format(-pc)}% in 24 h`);
    }
    const health = !q
      ? ""
      : q.degraded
        ? `<p class="crit-text">⚠ ${L("Fonte com problema", "Source has a problem")}: ${reason(q.degraded)}</p>`
        : `<p class="good-text">✓ ${L("Funcionando normalmente", "Working normally")} <span class="sub">(${L("nota de qualidade", "quality score")} ${nf2.format(q.score)})</span></p>`;
    body = `<div class="detail-value">${value}</div><p>${change}</p>${health}
      <p class="sub">${L(
        p.kind === "ana" ? `Última leitura de ${dmy(d.day)}, gravada no lote ${d.batch_id}.` : `Valor do dia ${dmy(d.day)}, gravado no lote ${d.batch_id}.`,
        p.kind === "ana" ? `Last reading of ${dmy(d.day)}, recorded in batch ${d.batch_id}.` : `Value for ${dmy(d.day)}, recorded in batch ${d.batch_id}.`,
      )} <a href="${d.explorer}" target="_blank" rel="noopener">${L("ver transação", "view transaction")} ↗</a></p>`;
    if (p.kind === "glofas")
      body += `<p class="sub">${L(
        "Vazão estimada por um modelo global a partir de satélite e previsão do tempo, não medida no rio.",
        "Flow estimated by a global model from satellite and weather data, not measured in the river.",
      )}</p>`;
  }
  $("#basin-detail").innerHTML = `<p class="detail-kind">${kinds[p.kind]}</p><h4>${esc(cleanName(p.name))}</h4>${body}`;
}

// ---- copy in Bitcoin (OpenTimestamps) -------------------------------------
// Once a day the accumulator's root is timestamped in Bitcoin, so the record survives a devnet reset.

function renderStamp() {
  const box = $("#btc-stamp");
  const t = (B.timestamps ?? [])[0];
  box.hidden = !t;
  if (!t) return;
  const when = br(Date.parse(t.stamped) / 1000);
  const status = t.bitcoin_block
    ? L(
        `<span class="good-text">✓ confirmado no bloco <a href="https://mempool.space/block/${t.bitcoin_block}" target="_blank" rel="noopener">${nf0.format(t.bitcoin_block)}</a> do Bitcoin</span>`,
        `<span class="good-text">✓ confirmed in Bitcoin block <a href="https://mempool.space/block/${t.bitcoin_block}" target="_blank" rel="noopener">${nf0.format(t.bitcoin_block)}</a></span>`,
      )
    : L("aguardando o próximo bloco do Bitcoin (leva algumas horas)", "waiting for the next Bitcoin block (takes a few hours)");
  box.innerHTML = `<h3>${L("Cópia de segurança no Bitcoin", "Backup copy in Bitcoin")}</h3>
    <p>${L(
      "Todo dia a raiz da árvore com todos os registros do Cota é carimbada no Bitcoin pelo OpenTimestamps, de graça. Mesmo que a rede de testes da Solana seja apagada, dá para provar que estes registros já existiam naquela data.",
      "Every day the root of the tree holding all of Cota's records is timestamped in Bitcoin through OpenTimestamps, for free. Even if Solana's test network is wiped, it can be proven that these records already existed on that date.",
    )}</p>
    <p class="sub">${L(
      `Último carimbo: ${nf0.format(t.records)} registros, raiz <span class="hash">${short(t.root, 12)}</span>, enviado em ${when} · ${status}.`,
      `Latest stamp: ${nf0.format(t.records)} records, root <span class="hash">${short(t.root, 12)}</span>, sent ${when} · ${status}.`,
    )}
    <a href="data/timestamps/${t.file}" download>${L("arquivo", "file")}</a> · <a href="data/timestamps/${t.file}.ots" download>${L("prova .ots", ".ots proof")}</a> · <a href="https://opentimestamps.org" target="_blank" rel="noopener">${L("conferir em opentimestamps.org", "check on opentimestamps.org")} ↗</a></p>${
      B.archive
        ? `<p class="sub">${L(
            `Os dados completos também têm cópia permanente fora do GitHub: ${nf0.format(B.archive.files)} arquivos no Arweave (${nf0.format(B.archive.bytes / 1024)} KiB comprimidos) e o repositório no Software Heritage. Qualquer cópia pode ser conferida contra a cadeia.`,
            `The complete data also has permanent copies outside GitHub: ${nf0.format(B.archive.files)} files on Arweave (${nf0.format(B.archive.bytes / 1024)} KiB compressed) and the repository on Software Heritage. Any copy can be checked against the chain.`,
          )} <a href="https://github.com/ThallesOwnsIt/Cota/blob/main/data/ledger/archive.json" target="_blank" rel="noopener">${L("lista no Arweave", "Arweave list")} ↗</a> · <a href="https://archive.softwareheritage.org/browse/origin/?origin_url=https://github.com/ThallesOwnsIt/Cota" target="_blank" rel="noopener">Software Heritage ↗</a></p>`
        : ""
    }`;
}

// ---- who can change the program ----------------------------------------------------------------
// The upgrade authority of program v2 is a Squads multisig vault. The browser reads, live from devnet, the
// program's authority and the multisig's own account (threshold, members, wait): the static config only
// says where to look. One query per page load; every redraw (language switch) reuses it.

let upgradeState = null;

/** Squads v4 `Multisig`: create_key, config_authority, threshold u16, time_lock u32, …, members Vec. */
function decodeSquads(d) {
  const v = new DataView(d.buffer, d.byteOffset, d.byteLength);
  let o = 8 + 32 + 32;
  const threshold = v.getUint16(o, true);
  const timeLock = v.getUint32(o + 2, true);
  o += 2 + 4 + 8 + 8;
  o += 1 + (d[o] ? 32 : 0) + 1; // rent_collector: Option<Pubkey>, bump
  const members = v.getUint32(o, true);
  return { threshold, timeLock, members };
}

function upgradeFacts() {
  const u = B.upgrade;
  upgradeState ??= Promise.all([fetchAccount(B.meta.rpc, u.program_data), fetchAccount(B.meta.rpc, u.multisig)]).then(([pd, ms]) => ({
    authority: pd.data[12] === 1 ? base58(pd.data.subarray(13, 45)) : null,
    squads: ms.owner === u.squads_program ? decodeSquads(ms.data) : null,
  }));
  upgradeState.catch(() => (upgradeState = null));
  return upgradeState;
}

async function renderUpgrade() {
  const box = $("#upgrade-auth");
  const u = B.upgrade;
  box.hidden = !u;
  if (!u) return;
  const title = `<h3>${L("Quem pode alterar o programa", "Who can change the program")}</h3>`;
  box.innerHTML = title + `<p class="sub">${L("Conferindo na devnet…", "Checking on devnet…")}</p>`;
  let facts;
  try {
    facts = await upgradeFacts();
  } catch (e) {
    box.innerHTML = title + `<p class="sub">${L("Não foi possível consultar a devnet agora", "Could not query devnet right now")}: ${esc(e.message)}</p>`;
    return;
  }
  // Drawn in the language active now, whatever language was active when the query started.
  const { authority, squads } = facts;
  const ok = authority === u.vault && squads != null;
  const wait = (s) => (s >= 3600 && s % 3600 === 0 ? `${s / 3600} h` : `${nf0.format(s)} s`);
  const body = squads
    ? L(
        `Nenhuma chave sozinha. Uma atualização do código precisa de ${squads.threshold} de ${squads.members} assinaturas numa multisig (Squads) e ainda espera ${wait(squads.timeLock)} antes de valer; durante a espera, a proposta fica pública. Por enquanto as ${squads.members} chaves estão com o mesmo responsável: o próximo passo é entregá-las a pessoas diferentes.`,
        `No single key. A code update needs ${squads.threshold} of ${squads.members} signatures on a multisig (Squads) and then waits ${wait(squads.timeLock)} before it takes effect; while it waits, the proposal is public. For now the ${squads.members} keys are held by the same person: the next step is handing them to different people.`,
      )
    : "";
  box.innerHTML =
    title +
    (body ? `<p>${body}</p>` : "") +
    `<p class="sub">${
      ok
        ? `<span class="good-text">✓ ${L("conferido agora na devnet", "checked just now on devnet")}</span>: ${L("a autoridade de upgrade do programa é o", "the program's upgrade authority is the")} <a href="${explorerAddr(u.vault)}" target="_blank" rel="noopener">${L("cofre da multisig", "multisig vault")}</a>${L(", e as regras acima foram lidas da própria multisig", ", and the rules above were read from the multisig itself")}`
        : `<span class="crit-text">✕ ${L("a autoridade de upgrade on-chain é", "the on-chain upgrade authority is")} ${esc(authority ?? L("nenhuma", "none"))}</span>`
    } · <a href="${explorerAddr(u.multisig)}" target="_blank" rel="noopener">multisig</a></p>`;
}

async function verifyPublicAlert(a) {
  panel("", `<h3>${L("Verificando o aviso", "Verifying warning")} ${esc(a.alert_id)}…</h3>`, "#live-verify");
  try {
    const text = await (await fetch(`data/public_alerts/${a.content_sha256}.txt`)).text();
    const local = await sha256Hex(text);
    const r = await onchain(a, a.record, decodePublicAlert);
    const acc = { slot: r.contextSlot };
    const checks = [
      [OWNED_BY(r), r.owned],
      [
        L(
          `SHA-256 do conteúdo guardado (${nf0.format(text.length)} caracteres), calculado no navegador = hash on-chain`,
          `SHA-256 of the stored content (${nf0.format(text.length)} characters), computed in the browser = on-chain hash`,
        ),
        local === r.content_sha256,
      ],
      [L("Código, severidade e vigência conferem", "Code, severity and validity period match"), r.alert_id === a.alert_id && r.severity === a.severity && r.onset === a.onset && r.expires === a.expires],
      [ATTESTED(r.signer_count), r.signer_count >= B.meta.threshold],
    ];
    const ok = checks.every(([, v]) => v);
    panel(
      ok ? "ok" : "fail",
      `<h3>${esc(a.description)}: ${CHECKED(acc.slot)}</h3><ol>${checks
        .map(([t, v]) => `<li>${v ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol>
      <p class="sub">Hash <span class="hash">${r.content_sha256}</span></p>
      <div class="verdict ${ok ? "good-text" : "crit-text"}">${
        ok
          ? L(`✓ Esta versão do aviso existia, no máximo, no slot ${nf0.format(r.slot)} (${br(r.committed_at)}).`, `✓ This version of the warning existed by slot ${nf0.format(r.slot)} (${br(r.committed_at)}) at the latest.`)
          : MISMATCH()
      }</div>
      <details><summary>${L("Conteúdo guardado", "Stored content")}</summary><pre class="hash" style="white-space:pre-wrap">${esc(text.slice(0, 4000))}${text.length > 4000 ? "…" : ""}</pre></details>`,
      "#live-verify",
    );
  } catch (e) {
    panel("fail", `<h3>${CANT()}</h3><p>${esc(e.message)}</p>`, "#live-verify");
  }
}

// ---- early warning (forecast rain) ----------------------------------------

const mm = (milli) => nf1.format(milli / 1000);
const monthLabel = (iso) => L(`${MONTHS.pt[+iso.slice(5, 7) - 1]}/${iso.slice(2, 4)}`, `${MONTHS.en[+iso.slice(5, 7) - 1]} '${iso.slice(2, 4)}`);

function renderOutlook() {
  const O = B.outlook;
  const R = O.rule;
  const bt = O.backtest;
  const cal = R.calibration;
  const last = O.live[0];
  const leads = bt.floods.map((f) => f.lead_days).filter((x) => x != null);
  const warned = bt.floods.filter((f) => f.first_alert).length;
  const faDays = bt.contingency.false_alarms;

  $("#outlook-sub").innerHTML =
    L(
      `Regra fixada on-chain antes de qualquer registro ao vivo: se a chuva prevista para os ${R.window_days} dias seguintes, média dos ` +
        `${R.points.length} pontos da bacia e dos modelos ${R.models.join(", ")}, chegar a <strong>${R.threshold_mm} mm</strong>, ` +
        `o Guaíba pode cruzar ${nf0.format(R.target.threshold_m3s)} m³/s em até ${R.target.within_days} dias. ` +
        `Calibrada só com ${cal.years[0]}–${cal.years[1]} (chuva observada, CSI ${nf2.format(cal.csi)}). `,
      `Rule fixed on-chain before any live record: if the rain forecast for the next ${R.window_days} days, averaged over the ` +
        `${R.points.length} basin points and the models ${R.models.join(", ")}, reaches <strong>${R.threshold_mm} mm</strong>, ` +
        `the Guaíba may cross ${nf0.format(R.target.threshold_m3s)} m³/s within ${R.target.within_days} days. ` +
        `Calibrated only on ${cal.years[0]}–${cal.years[1]} (observed rain, CSI ${nf2.format(cal.csi)}). `,
    ) +
    (R.account ? `${L("Conta", "Account")} <a href="${explorerAddr(R.account)}" target="_blank" rel="noopener">${short(R.account)}</a> · ` : "") +
    `<span class="hash" title="rule_sha256">${short(R.sha256, 12)}</span>`;

  const tiles = [
    last
      ? [
          `${L("Chuva prevista", "Forecast rain")} ${dm(addDays(last.day, 1))}–${dm(addDays(last.day, R.window_days))}`,
          `${mm(last.rain_mean_milli)} mm`,
          `${L("modelos", "models")}: ${mm(last.rain_min_milli)}–${mm(last.rain_max_milli)} mm · ${
            last.triggered ? L("▲ alerta antecipado", "▲ early warning") : L(`abaixo de ${R.threshold_mm} mm`, `below ${R.threshold_mm} mm`)
          }`,
        ]
      : [L("Chuva prevista", "Forecast rain"), "—", L("nenhuma perspectiva registrada ainda", "no outlook recorded yet")],
    [
      L("Cheias com alerta prévio", "Floods warned in advance"),
      `${warned} ${L("de", "of")} ${bt.floods.length}`,
      leads.length ? L(`antecedência de ${leads.join(", ")} dias`, `${leads.join(", ")} days ahead`) : "—",
    ],
    [
      L("Dias com alarme falso", "False-alarm days"),
      `${faDays}`,
      L(`${bt.false_alarm_runs.length} episódios em ${nf0.format(bt.contingency.days)} dias avaliados`, `${bt.false_alarm_runs.length} episodes in ${nf0.format(bt.contingency.days)} days scored`),
    ],
    [
      L("CSI fora da amostra", "Out-of-sample CSI"),
      nf2.format(bt.contingency.csi),
      L(
        `calibração: ${nf2.format(cal.csi)} · acertos ${bt.contingency.hits}, falhas ${bt.contingency.misses}`,
        `calibration: ${nf2.format(cal.csi)} · hits ${bt.contingency.hits}, misses ${bt.contingency.misses}`,
      ),
    ],
  ];
  $("#outlook-kpis").innerHTML = tiles
    .map(([l, v, n]) => `<div class="kpi"><div class="label">${l}</div><div class="value">${v}</div><div class="note">${n}</div></div>`)
    .join("");

  $("#outlook-bt-sub").textContent = L(
    `Para cada dia, a chuva que os modelos previam no fim daquele dia (Open-Meteo Previous Runs). Para o dia alvo D+k usa-se a previsão ` +
      `feita (k+1)×24 h antes: só rodadas já publicadas, um dia mais velhas que as do modo ao vivo. Vazão: GloFAS. ` +
      `Um alerta conta como acerto se a vazão cruzar o limiar em até ${R.target.within_days} dias.`,
    `For each day, the rain the models were forecasting at the end of that day (Open-Meteo Previous Runs). For target day D+k we use the ` +
      `forecast made (k+1)×24 h earlier: only runs already published, one day older than the ones live mode uses. Flow: GloFAS. ` +
      `An alert counts as a hit if the flow crosses the threshold within ${R.target.within_days} days.`,
  );

  const fl = bt.floods
    .map(
      (f) => `<tr><td>${dmy(f.first_day)} – ${dmy(f.last_day)}</td><td class="num">${nf0.format(f.peak_m3s)}</td>
      <td>${f.first_alert ? dmy(f.first_alert) : L("sem alerta", "no alert")}</td><td class="num">${f.lead_days ?? "—"}</td></tr>`,
    )
    .join("");
  const fa = bt.false_alarm_runs.map(([a, b]) => (a === b ? dmy(a) : `${dmy(a)} – ${dmy(b)}`)).join(" · ");
  $("#outlook-bt-table").innerHTML =
    `<table class="data"><thead><tr><th>${L("Cheia", "Flood")} (${L("vazão", "flow")} ≥ ${nf0.format(R.target.threshold_m3s)} m³/s)</th><th class="num">${L("Pico", "Peak")} (m³/s)</th>` +
    `<th>${L("Primeiro alerta", "First alert")}</th><th class="num">${L("Antecedência (d)", "Lead time (d)")}</th></tr></thead><tbody>${fl}</tbody></table>` +
    `<p class="sub">${L(`Alarmes falsos (alerta sem cheia em ${R.target.within_days} dias)`, `False alarms (alert with no flood within ${R.target.within_days} days)`)}: ${fa}</p>`;

  renderOutlookNow();
  renderOutlookChart();

  if (!O.live.length) {
    $("#outlook-live").innerHTML = `<div class="empty-note">${L("Nenhuma perspectiva registrada até agora.", "No outlook recorded so far.")}</div>`;
    return;
  }
  const rows = O.live
    .map((o, i) => {
      const n = o.signers.toString(2).split("").filter((b) => b === "1").length;
      const res = o.triggered
        ? `<span class="sev"><span class="dot" style="background:var(--serious)">▲</span>${L("alerta antecipado", "early warning")}</span>`
        : `<span class="sev"><span class="dot" style="background:var(--good)">✓</span>${L("abaixo da regra", "below the rule")}</span>`;
      return `<tr><td>${L("fim de", "end of")} ${dmy(o.day)}<br><span class="hash">${L("janela", "window")} ${dm(addDays(o.day, 1))}–${dm(addDays(o.day, R.window_days))}</span></td>
        <td class="num">${mm(o.rain_mean_milli)} mm<br><span class="hash">${mm(o.rain_min_milli)}–${mm(o.rain_max_milli)} · ${o.models} ${L("modelos", "models")}</span></td>
        <td>${res}</td>
        <td>${br(o.committed_at)}<br><span class="hash">slot ${nf0.format(o.slot)}</span></td>
        <td class="num">${n}/${B.meta.members.length}</td>
        <td><button type="button" class="link-btn" data-o="${i}">${L("verificar", "verify")}</button>${o.explorer ? ` · <a href="${o.explorer}" target="_blank" rel="noopener">tx</a>` : ""}</td></tr>`;
    })
    .join("");
  $("#outlook-live").innerHTML =
    `<table class="data"><thead><tr><th>${L("Emissão", "Issued")}</th><th class="num">${L(`Chuva prevista em ${R.window_days} d`, `Forecast rain over ${R.window_days} d`)}</th><th>${L("Regra", "Rule")}</th><th>${L("Gravado on-chain", "Recorded on-chain")}</th><th class="num">${L("Nós", "Nodes")}</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
  $("#outlook-live").querySelectorAll("[data-o]").forEach((b) => b.addEventListener("click", () => verifyOutlook(O.live[+b.dataset.o])));
}

// Today's situation in plain language; the technical numbers stay in the collapsed block.
function renderOutlookNow() {
  const O = B.outlook;
  const R = O.rule;
  const bt = O.backtest;
  const last = O.live[0];
  const leads = bt.floods.map((f) => f.lead_days).filter((x) => x != null);
  const warned = bt.floods.filter((f) => f.first_alert).length;
  const lastAlert = [O.live.find((o) => o.triggered)?.day, bt.days.findLast((d) => d.alert)?.day].filter(Boolean).sort().at(-1);

  let status;
  if (!last) {
    status = `<div class="now-status"><span class="now-icon ok" aria-hidden="true">·</span><div>
      <h3>${L("Ainda sem previsão registrada", "No forecast recorded yet")}</h3></div></div>`;
  } else {
    const v = last.rain_mean_milli / 1000;
    const win = `${dm(addDays(last.day, 1))}–${dm(addDays(last.day, R.window_days))}`;
    const max = Math.max(R.threshold_mm * 1.5, v * 1.1);
    const at = (x) => `${Math.min(100, (x / max) * 100)}%`;
    status = `<div class="now-status">
        <span class="now-icon ${last.triggered ? "alert" : "ok"}" aria-hidden="true">${last.triggered ? "▲" : "✓"}</span>
        <div>
          <h3>${last.triggered ? L("Alerta antecipado", "Early warning") : L("Sem alerta antecipado", "No early warning")}</h3>
          <p>${
            last.triggered
              ? L(
                  `A chuva prevista para a bacia de ${win} é de ${mm(last.rain_mean_milli)} mm, acima dos ${R.threshold_mm} mm. O Guaíba pode passar do nível de cheia nos próximos ${R.target.within_days} dias.`,
                  `The rain forecast for the basin for ${win} is ${mm(last.rain_mean_milli)} mm, above ${R.threshold_mm} mm. The Guaíba may go over flood level within the next ${R.target.within_days} days.`,
                )
              : L(
                  `A chuva prevista para a bacia de ${win} é de ${mm(last.rain_mean_milli)} mm, abaixo dos ${R.threshold_mm} mm que acendem o alerta.`,
                  `The rain forecast for the basin for ${win} is ${mm(last.rain_mean_milli)} mm, below the ${R.threshold_mm} mm that would trigger a warning.`,
                )
          }</p>
          <div class="meter" role="img" aria-label="${L(`${mm(last.rain_mean_milli)} mm de ${R.threshold_mm} mm`, `${mm(last.rain_mean_milli)} mm of ${R.threshold_mm} mm`)}">
            <div class="meter-track"><div class="meter-fill${last.triggered ? " alert" : ""}" style="width:${at(v)}"></div><span class="meter-mark" style="left:${at(R.threshold_mm)}"></span></div>
            <div class="meter-labels"><span style="left:0">0 mm</span><span style="left:${at(R.threshold_mm)}">${L("alerta", "warning")} · ${R.threshold_mm} mm</span></div>
          </div>
          <p class="sub">${L(`Previsão do fim de ${dmy(last.day)}, gravada em ${br(last.committed_at)}.`, `Forecast from the end of ${dmy(last.day)}, recorded ${br(last.committed_at)}.`)}</p>
        </div>
      </div>`;
  }
  const fact = (dt, dd, small) => `<div><dt>${dt}</dt><dd>${dd}<small>${small}</small></dd></div>`;
  $("#outlook-now").innerHTML =
    status +
    `<dl class="facts">${
      fact(
        L("Cheias avisadas com antecedência desde 2024", "Floods warned in advance since 2024"),
        `${warned} ${L("de", "of")} ${bt.floods.length}`,
        leads.length ? L(`de ${Math.min(...leads)} a ${Math.max(...leads)} dias antes`, `${Math.min(...leads)} to ${Math.max(...leads)} days ahead`) : "",
      ) +
      fact(L("Último alerta antecipado", "Latest early warning"), lastAlert ? dmy(lastAlert) : "—", L(`o alerta vale para os ${R.target.within_days} dias seguintes`, `a warning covers the next ${R.target.within_days} days`)) +
      fact(
        L("Alarmes falsos no teste", "False alarms in the test"),
        L(`${bt.contingency.false_alarms} dias`, `${bt.contingency.false_alarms} days`),
        L(`em ${bt.false_alarm_runs.length} ocasiões, de fev/2024 a out/2026`, `on ${bt.false_alarm_runs.length} occasions, Feb 2024 to Oct 2026`),
      )
    }</dl>`;
}

// ---- early-warning chart, with zoom ---------------------------------------
// Visible window [i0, i1] in backtest.days indexes. The range buttons, the flood shortcuts
// and the navigation strip below (dragging the window or its edges) change the window; only the top chart is redrawn.

const ov = { i0: null, i1: null, nav: null };
const OV_MIN = 14;
const OV_RANGES = [
  [30, "1M", "1M"],
  [91, "3M", "3M"],
  [182, "6M", "6M"],
  [365, "1A", "1Y"],
  [0, "Tudo", "All"],
];
const monthYear = (iso) =>
  new Intl.DateTimeFormat(L("pt-BR", "en-US"), { month: "long", year: "numeric", timeZone: "UTC" }).format(new Date(iso + "T00:00:00Z"));

// Floods close together (like the two in May 2024) become a single shortcut.
function floodGroups() {
  const { floods } = B.outlook.backtest;
  const groups = [];
  for (const f of floods) {
    const g = groups.at(-1);
    if (g && Date.parse(f.first_day) - Date.parse(g.last_day) <= 20 * 864e5) g.last_day = f.last_day;
    else groups.push({ first_day: f.first_day, last_day: f.last_day, first_alert: f.first_alert });
  }
  return groups;
}

function setOvRange(i0, i1) {
  const n = B.outlook.backtest.days.length;
  i0 = Math.round(i0);
  i1 = Math.round(i1);
  if (i1 - i0 < OV_MIN) {
    const c = (i0 + i1) / 2;
    i0 = Math.round(c - OV_MIN / 2);
    i1 = i0 + OV_MIN;
  }
  if (i0 < 0) [i0, i1] = [0, i1 - i0];
  if (i1 > n - 1) [i0, i1] = [Math.max(0, i0 - (i1 - (n - 1))), n - 1];
  ov.i0 = i0;
  ov.i1 = i1;
  drawOutlookMain();
  updateOvNav();
  syncOvControls();
}

function syncOvControls() {
  const n = B.outlook.backtest.days.length;
  const span = ov.i1 - ov.i0;
  document.querySelectorAll("#ov-range button").forEach((b) => {
    const d = +b.dataset.days;
    const on = d === 0 ? ov.i0 === 0 && ov.i1 === n - 1 : ov.i1 === n - 1 && span === Math.min(d, n - 1);
    b.setAttribute("aria-checked", String(on));
  });
  document.querySelectorAll("#ov-jump [data-g]").forEach((b) => b.setAttribute("aria-pressed", String(+b.dataset.i0 === ov.i0 && +b.dataset.i1 === ov.i1)));
}

function renderOutlookChart() {
  const data = B.outlook.backtest.days;
  const n = data.length;
  const idx = new Map(data.map((d, i) => [d.day, i]));
  if (ov.i0 == null) {
    ov.i1 = n - 1;
    ov.i0 = Math.max(0, n - 1 - 182);
  }

  $("#ov-range").innerHTML = OV_RANGES.map(
    ([d, pt, en]) => `<button type="button" role="radio" data-days="${d}" aria-checked="false">${L(pt, en)}</button>`,
  ).join("");
  $("#ov-range")
    .querySelectorAll("button")
    .forEach((b) =>
      b.addEventListener("click", () => {
        const d = +b.dataset.days;
        setOvRange(d === 0 ? 0 : n - 1 - d, n - 1);
      }),
    );

  $("#ov-jump").innerHTML =
    `<span>${L("Ir para:", "Jump to:")}</span>` +
    floodGroups()
      .map((g) => {
        const a = Math.max(0, (idx.get(g.first_alert ?? g.first_day) ?? 0) - 14);
        const b = Math.min(n - 1, (idx.get(g.last_day) ?? n - 1) + 10);
        return `<button type="button" class="jump-btn" data-g data-i0="${a}" data-i1="${b}" aria-pressed="false">${L("Cheia de", "Flood of")} ${monthYear(g.first_day)}</button>`;
      })
      .join("");
  $("#ov-jump")
    .querySelectorAll("[data-g]")
    .forEach((b) => b.addEventListener("click", () => setOvRange(+b.dataset.i0, +b.dataset.i1)));

  buildOvNav();
  drawOutlookMain();
  updateOvNav();
  syncOvControls();

  const R = B.outlook.rule;
  $("#outlook-legend").innerHTML =
    `<span class="key" style="color:var(--text-2)"><span class="swatch-line"></span>${L("Chuva prevista (5 dias)", "Forecast rain (5 days)")}</span>` +
    `<span class="key"><span class="dot-key" style="background:var(--serious)"></span>${L("Dia de alerta antecipado", "Early-warning day")} (≥ ${R.threshold_mm} mm)</span>` +
    `<span class="key" style="color:var(--observed)"><span class="swatch-line"></span><span style="color:var(--text-2)">${L("Vazão do rio", "River flow")}</span></span>` +
    `<span class="key"><span class="swatch-flood"></span>${L("Cheia", "Flood")}</span>`;
}

function drawOutlookMain() {
  const O = B.outlook;
  const data = O.backtest.days;
  const R = O.rule;
  const { i0, i1 } = ov;
  const span = i1 - i0;
  const box = $("#outlook-chart");
  const W = Math.max(320, box.clientWidth);
  const small = W < 560;
  const hA = small ? 120 : 150;
  const hB = small ? 100 : 120;
  const m = { l: 46, r: 12, t: 22, b: 24, gap: 30 };
  const H = m.t + hA + m.gap + hB + m.b;
  const pw = W - m.l - m.r;
  const x = (i) => m.l + ((i - i0) / span) * pw;
  const vis = data.slice(i0, i1 + 1);
  const rmax = niceMax(Math.max(R.threshold_mm * 1.3, ...vis.map((d) => d.rain ?? 0)));
  const qmax = niceMax(Math.max(R.target.threshold_m3s * 1.2, ...vis.map((d) => d.q ?? 0)));
  const yA0 = m.t + hA;
  const yA = (v) => yA0 - (v / rmax) * hA;
  const yB0 = m.t + hA + m.gap + hB;
  const yB = (v) => yB0 - (v / qmax) * hB;
  const svg = el("svg", { viewBox: `0 0 ${W} ${H}`, width: W, height: H });
  const clip = el("clipPath", { id: "ov-clip" }, el("defs", {}, svg));
  el("rect", { x: m.l, y: 0, width: pw, height: H }, clip);

  // floods: band on both panels
  const idx = new Map(data.map((d, i) => [d.day, i]));
  const plot = el("g", { "clip-path": "url(#ov-clip)" });
  for (const f of O.backtest.floods) {
    const a = idx.get(f.first_day), b = idx.get(f.last_day);
    if (a == null || b == null || b < i0 || a > i1) continue;
    el("rect", { x: x(a - 0.5), y: m.t, width: Math.max(3, x(b + 0.5) - x(a - 0.5)), height: yB0 - m.t, fill: "var(--critical)", "fill-opacity": 0.14 }, plot);
  }

  // axes and grids
  el("text", { x: m.l, y: 12, class: "tick-label", fill: "var(--text-2)" }, svg).textContent =
    L(`Chuva prevista para os ${R.window_days} dias seguintes (mm)`, `Rain forecast for the next ${R.window_days} days (mm)`);
  for (const v of [0, rmax / 2, rmax]) {
    el("line", { x1: m.l, x2: W - m.r, y1: yA(v), y2: yA(v), class: v === 0 ? "baseline" : "gridline" }, svg);
    el("text", { x: m.l - 6, y: yA(v) + 4, "text-anchor": "end", class: "tick-label" }, svg).textContent = nf0.format(v);
  }
  el("text", { x: m.l, y: yA0 + m.gap - 8, class: "tick-label", fill: "var(--text-2)" }, svg).textContent = L("Vazão do Guaíba (m³/s)", "Guaíba river flow (m³/s)");
  for (const v of [0, qmax / 2, qmax]) {
    el("line", { x1: m.l, x2: W - m.r, y1: yB(v), y2: yB(v), class: v === 0 ? "baseline" : "gridline" }, svg);
    el("text", { x: m.l - 6, y: yB(v) + 4, "text-anchor": "end", class: "tick-label" }, svg).textContent = nf0.format(v);
  }
  svg.appendChild(plot);

  // date labels according to the zoom: weeks, months or quarters
  const perLabel = span / (small ? 4 : 8);
  const tick =
    perLabel <= (small ? 12 : 9)
      ? (d) => ["01", "08", "15", "22"].includes(d.day.slice(8)) && dm(d.day)
      : (d) => {
          if (d.day.slice(8) !== "01") return false;
          const every = perLabel <= 35 ? 1 : perLabel <= 70 ? 2 : perLabel <= 100 ? 3 : 6;
          return (+d.day.slice(5, 7) - 1) % every === 0 && monthLabel(d.day);
        };
  for (let i = i0; i <= i1; i++) {
    const t = tick(data[i]);
    if (t) el("text", { x: x(i), y: H - 6, "text-anchor": "middle", class: "tick-label" }, svg).textContent = t;
  }

  // limiares
  el("line", { x1: m.l, x2: W - m.r, y1: yA(R.threshold_mm), y2: yA(R.threshold_mm), stroke: "var(--serious)", "stroke-width": 1, "stroke-dasharray": "2 3" }, svg);
  el("text", { x: m.l + 6, y: yA(R.threshold_mm) - 5, class: "tick-label" }, svg).textContent = `${L("alerta", "warning")} · ${R.threshold_mm} mm`;
  el("line", { x1: m.l, x2: W - m.r, y1: yB(R.target.threshold_m3s), y2: yB(R.target.threshold_m3s), stroke: "var(--critical)", "stroke-width": 1, "stroke-dasharray": "2 3" }, svg);
  el("text", { x: W - m.r, y: yB(R.target.threshold_m3s) - 5, "text-anchor": "end", class: "tick-label" }, svg).textContent =
    `${L("cheia", "flood")} · ${nf0.format(R.target.threshold_m3s)} m³/s`;

  // series (broken where data is missing); one extra day at each end so the line reaches the edge
  const lo = Math.max(0, i0 - 1), hi = Math.min(data.length - 1, i1 + 1);
  const line = (key, y, color, width) => {
    let seg = [];
    const flush = () => {
      if (seg.length > 1) el("polyline", { points: seg.join(" "), fill: "none", stroke: color, "stroke-width": width, "stroke-linejoin": "round" }, plot);
      seg = [];
    };
    for (let i = lo; i <= hi; i++) data[i][key] == null ? flush() : seg.push(`${x(i)},${y(data[i][key])}`);
    flush();
  };
  line("rain", yA, "var(--text-2)", span > 400 ? 1.2 : 1.6);
  line("q", yB, "var(--observed)", span > 400 ? 1.4 : 2);
  const r = span > 400 ? 3 : span > 120 ? 4 : 5;
  for (let i = i0; i <= i1; i++)
    if (data[i].alert) el("circle", { cx: x(i), cy: yA(data[i].rain), r, fill: "var(--serious)", stroke: "var(--surface)", "stroke-width": 1.5 }, plot);

  // interaction
  const hover = el("g", { "pointer-events": "none" }, svg);
  const overlay = el("rect", { x: m.l, y: m.t, width: pw, height: yB0 - m.t, fill: "transparent" }, svg);
  overlay.addEventListener("pointermove", (ev) => {
    const rc = svg.getBoundingClientRect();
    const px = ((ev.clientX - rc.left) / rc.width) * W;
    const i = Math.max(i0, Math.min(i1, i0 + Math.round(((px - m.l) / pw) * span)));
    const d = data[i];
    hover.replaceChildren();
    el("line", { x1: x(i), x2: x(i), y1: m.t, y2: yB0, stroke: "var(--axis)", "stroke-width": 1 }, hover);
    if (d.rain != null) el("circle", { cx: x(i), cy: yA(d.rain), r: 4.5, fill: d.alert ? "var(--serious)" : "var(--text-2)", stroke: "var(--surface)", "stroke-width": 2 }, hover);
    if (d.q != null) el("circle", { cx: x(i), cy: yB(d.q), r: 4.5, fill: "var(--observed)", stroke: "var(--surface)", "stroke-width": 2 }, hover);
    showTip(
      `<div class="t-title">${L("Fim de", "End of")} ${dmy(d.day)}</div>` +
        row(`${L("Chuva prevista", "Forecast rain")} ${dm(addDays(d.day, 1))}–${dm(addDays(d.day, R.window_days))}`, d.rain == null ? L("sem dados", "no data") : `${nf1.format(d.rain)} mm`) +
        Object.entries(d.models).map(([k, v]) => row(`&nbsp;&nbsp;${k}`, `${nf1.format(v)} mm`)).join("") +
        row(L("Alerta antecipado", "Early warning"), d.alert ? L("▲ sim", "▲ yes") : L("não", "no")) +
        row(L("Vazão no dia", "Flow that day"), d.q == null ? "—" : `${nf0.format(d.q)} m³/s`) +
        row(L(`Cheia em até ${R.target.within_days} dias`, `Flood within ${R.target.within_days} days`), d.flood ? L("sim", "yes") : L("não", "no")),
      ev.clientX,
      ev.clientY,
    );
  });
  overlay.addEventListener("pointerleave", () => {
    hover.replaceChildren();
    hideTip();
  });
  box.replaceChildren(svg);
}

// Navigation strip: the flow over the whole period, with the visible window on top.
function buildOvNav() {
  const data = B.outlook.backtest.days;
  const n = data.length;
  const box = $("#outlook-nav");
  const W = Math.max(320, box.clientWidth || $("#outlook-chart").clientWidth);
  const m = { l: 46, r: 12 };
  const H = 52;
  const pw = W - m.l - m.r;
  const x = (i) => m.l + (i / (n - 1)) * pw;
  const qmax = Math.max(...data.map((d) => d.q ?? 0));
  const y = (v) => H - 14 - (v / qmax) * (H - 20);
  const svg = el("svg", { viewBox: `0 0 ${W} ${H}`, width: W, height: H, tabindex: 0, role: "slider" });
  svg.setAttribute("aria-label", L("Período visível do gráfico (setas para mover)", "Visible chart period (arrow keys to move)"));
  el("rect", { x: m.l, y: 4, width: pw, height: H - 18, rx: 6, fill: "var(--surface-2)" }, svg);
  const idx = new Map(data.map((d, i) => [d.day, i]));
  for (const f of B.outlook.backtest.floods) {
    const a = idx.get(f.first_day), b = idx.get(f.last_day);
    if (a != null && b != null) el("rect", { x: x(a), y: 4, width: Math.max(2, x(b) - x(a)), height: H - 18, fill: "var(--critical)", "fill-opacity": 0.25 }, svg);
  }
  const pts = data.map((d, i) => (d.q == null ? null : `${x(i)},${y(d.q)}`)).filter(Boolean);
  el("polyline", { points: pts.join(" "), fill: "none", stroke: "var(--observed)", "stroke-width": 1, opacity: 0.8 }, svg);
  data.forEach((d, i) => {
    if (d.day.slice(5) === "01-01" || i === 0)
      el("text", { x: x(i) + (i === 0 ? 0 : 0), y: H - 2, class: "tick-label", "text-anchor": i === 0 ? "start" : "middle" }, svg).textContent = d.day.slice(0, 4);
  });
  const shadeL = el("rect", { class: "nav-shade", y: 4, height: H - 18 }, svg);
  const shadeR = el("rect", { class: "nav-shade", y: 4, height: H - 18 }, svg);
  const win = el("rect", { class: "nav-window", y: 4, height: H - 18, rx: 4, "data-role": "win" }, svg);
  const hl = el("rect", { class: "nav-handle", y: 12, width: 8, height: H - 34, rx: 3, "data-role": "l" }, svg);
  const hr = el("rect", { class: "nav-handle", y: 12, width: 8, height: H - 34, rx: 3, "data-role": "r" }, svg);
  ov.nav = { svg, W, m, pw, x, shadeL, shadeR, win, hl, hr };

  const toI = (ev) => {
    const rc = svg.getBoundingClientRect();
    const px = ((ev.clientX - rc.left) / rc.width) * W;
    return Math.max(0, Math.min(n - 1, Math.round(((px - m.l) / pw) * (n - 1))));
  };
  let drag = null;
  svg.addEventListener("pointerdown", (ev) => {
    drag = { role: ev.target.dataset?.role ?? "new", i: toI(ev), i0: ov.i0, i1: ov.i1, moved: false };
    svg.setPointerCapture(ev.pointerId);
    ev.preventDefault();
  });
  svg.addEventListener("pointermove", (ev) => {
    if (!drag) return;
    const i = toI(ev);
    const d = i - drag.i;
    if (Math.abs(d) > 1) drag.moved = true;
    if (drag.role === "win") {
      const s = Math.max(-drag.i0, Math.min(n - 1 - drag.i1, d));
      setOvRange(drag.i0 + s, drag.i1 + s);
    } else if (drag.role === "l") setOvRange(Math.min(i, ov.i1 - OV_MIN), ov.i1);
    else if (drag.role === "r") setOvRange(ov.i0, Math.max(i, ov.i0 + OV_MIN));
    else if (drag.moved) setOvRange(Math.min(drag.i, i), Math.max(drag.i, i));
  });
  const end = () => {
    // A click outside the window centres the window there, without changing its size.
    if (drag?.role === "new" && !drag.moved) {
      const half = (ov.i1 - ov.i0) / 2;
      setOvRange(drag.i - half, drag.i + half);
    }
    drag = null;
  };
  svg.addEventListener("pointerup", end);
  svg.addEventListener("pointercancel", () => (drag = null));
  svg.addEventListener("keydown", (ev) => {
    const step = Math.max(1, Math.round((ov.i1 - ov.i0) / 5));
    if (ev.key === "ArrowLeft") setOvRange(ov.i0 - step, ov.i1 - step);
    else if (ev.key === "ArrowRight") setOvRange(ov.i0 + step, ov.i1 + step);
    else return;
    ev.preventDefault();
  });
  box.replaceChildren(svg);
}

function updateOvNav() {
  const N = ov.nav;
  if (!N) return;
  const a = N.x(ov.i0), b = N.x(ov.i1);
  N.shadeL.setAttribute("x", N.m.l);
  N.shadeL.setAttribute("width", Math.max(0, a - N.m.l));
  N.shadeR.setAttribute("x", b);
  N.shadeR.setAttribute("width", Math.max(0, N.m.l + N.pw - b));
  N.win.setAttribute("x", a);
  N.win.setAttribute("width", Math.max(2, b - a));
  N.hl.setAttribute("x", a - 4);
  N.hr.setAttribute("x", b - 4);
  const data = B.outlook.backtest.days;
  N.svg.setAttribute("aria-valuetext", `${dmy(data[ov.i0].day)} – ${dmy(data[ov.i1].day)}`);
}

async function verifyOutlook(o) {
  const R = B.outlook.rule;
  panel("", `<h3>${L("Verificando a perspectiva de", "Verifying the outlook of")} ${dmy(o.day)}…</h3>`, "#outlook-verify");
  try {
    const text = await (await fetch(`data/outlooks/${o.content_sha256}.txt`)).text();
    const local = await sha256Hex(text);
    const r = await onchain(o, o.record, decodeOutlook);
    const acc = { slot: r.contextSlot };
    const c = JSON.parse(text);
    // Rebuilds the summary from the per-model, per-point totals (same integer arithmetic as the
    // node).
    const perModel = Object.values(c.models).map((pts) => Math.floor((Object.values(pts).flat().reduce((a, b) => a + b, 0) * 100) / R.points.length));
    const mean = Math.floor(perModel.reduce((a, b) => a + b, 0) / perModel.length);
    const checks = [
      [OWNED_BY(r), r.owned],
      [L("SHA-256 do conteúdo guardado, calculado no navegador = hash on-chain", "SHA-256 of the stored content, computed in the browser = on-chain hash"), local === r.content_sha256],
      [L("O conteúdo usa a regra fixada on-chain (rule_sha256)", "The content uses the rule fixed on-chain (rule_sha256)"), c.rule_sha256 === R.sha256],
      [
        L(
          `Refazendo a conta: média ${mm(mean)} mm (modelos ${mm(Math.min(...perModel))}–${mm(Math.max(...perModel))}) = valores on-chain`,
          `Redoing the math: average ${mm(mean)} mm (models ${mm(Math.min(...perModel))}–${mm(Math.max(...perModel))}) = on-chain values`,
        ),
        mean === r.rain_mean_milli && Math.min(...perModel) === r.rain_min_milli && Math.max(...perModel) === r.rain_max_milli && perModel.length === r.models,
      ],
      [
        `${L("Regra aplicada pelo programa", "Rule applied by the program")}: ${mm(r.rain_mean_milli)} ${r.triggered ? "≥" : "<"} ${mm(r.threshold_milli)} mm → ${
          r.triggered ? L("alerta", "alert") : L("sem alerta", "no alert")
        }`,
        r.triggered === r.rain_mean_milli >= r.threshold_milli && r.threshold_milli === R.threshold_mm * 1000,
      ],
      [
        L(`Emitida no fim de ${dmy(o.day)} (lote ${r.based_on_batch_id}), janela até ${utc(r.window_end)}`, `Issued at the end of ${dmy(o.day)} (batch ${r.based_on_batch_id}), window until ${utc(r.window_end)}`),
        r.issued_at === c.issued_at && r.issued_at === o.issued_at,
      ],
      [ATTESTED(r.signer_count), r.signer_count >= B.meta.threshold],
    ];
    const next = [...B.live.days].find((d) => d.batch_id === r.based_on_batch_id + 1);
    if (next) {
      const nb = await onchain(next, next.batch_account, decodeBatch);
      checks.push([
        L(
          `Gravada (slot ${nf0.format(r.slot)}) antes do lote de ${dmy(next.day)} (slot ${nf0.format(nb.slot)}), o 1º dia da janela`,
          `Recorded (slot ${nf0.format(r.slot)}) before the batch of ${dmy(next.day)} (slot ${nf0.format(nb.slot)}), the window's first day`,
        ),
        r.slot < nb.slot,
      ]);
    }
    const ok = checks.every(([, v]) => v);
    panel(
      ok ? "ok" : "fail",
      `<h3>${L("Perspectiva de", "Outlook of")} ${dmy(o.day)}: ${CHECKED(acc.slot)}</h3><ol>${checks
        .map(([t, v]) => `<li>${v ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol>
      <div class="verdict ${ok ? "good-text" : "crit-text"}">${
        ok
          ? L(
              `✓ No slot ${nf0.format(r.slot)} (${br(r.committed_at)}) o comitê já registrava ${mm(r.rain_mean_milli)} mm previstos até ${dmy(addDays(o.day, R.window_days))}.`,
              `✓ At slot ${nf0.format(r.slot)} (${br(r.committed_at)}) the committee had already recorded ${mm(r.rain_mean_milli)} mm forecast through ${dmy(addDays(o.day, R.window_days))}.`,
            )
          : MISMATCH()
      }</div>
      <details><summary>${L("Conteúdo guardado (décimos de mm por dia, por modelo e ponto)", "Stored content (tenths of mm per day, per model and point)")}</summary><pre class="hash" style="white-space:pre-wrap">${esc(text)}</pre></details>`,
      "#outlook-verify",
    );
  } catch (e) {
    panel("fail", `<h3>${CANT()}</h3><p>${esc(e.message)}</p>`, "#outlook-verify");
  }
}

// ---- parametric vaults ------------------------------------------------------

const sol = (lamports) => `${nf3.format(lamports / 1e9)} SOL`;
const TRIGGERS = { 0: ["alerta antecipado (chuva prevista)", "early warning (forecast rain)"], 1: ["alerta do modelo de vazão", "flow-model alert"] };
const trigger = (k) => L(...TRIGGERS[k]);
const VAULT_TITLES = { "ao-vivo": ["Cofre do Guaíba", "Guaíba vault"], "replay-2024": ["Cofre da cheia de 2024", "2024 flood vault"] };
const endOf = (ts) => dmy(new Date((ts - 1) * 1000).toISOString().slice(0, 10));

function renderVaults() {
  const list = $("#vault-list");
  if (!B.vaults?.length) {
    list.innerHTML = `<div class="empty-note">${L("Nenhum cofre criado.", "No vault created.")}</div>`;
    return;
  }
  const sum = (f) => B.vaults.reduce((a, v) => a + f(v), 0);
  const stat = (value, label) => `<div><div class="value">${value}</div><div class="label">${label}</div></div>`;
  $("#vault-stats").innerHTML =
    stat(sum((v) => v.payouts.length), L("pagamentos automáticos feitos", "automatic payouts made")) +
    stat(sol(sum((v) => v.state.total_paid)), L("pagos aos beneficiários", "paid to beneficiaries")) +
    stat(sol(sum((v) => v.state.balance)), L("travados à espera do próximo alerta", "locked, waiting for the next warning")) +
    stat("0", L("aprovações humanas necessárias", "human approvals needed"));
  list.innerHTML = B.vaults
    .map((v, vi) => {
      const st = v.state;
      const tiles = [
        [L("Saldo", "Balance"), sol(st.balance), `${L("vence em", "expires")} ${dmy(new Date(st.expires_at * 1000).toISOString().slice(0, 10))}`],
        [L("Paga por evento", "Pays per event"), sol(st.payout_lamports), L(`carência de ${nf0.format(st.cooldown_secs / 86400)} dias entre eventos`, `${nf0.format(st.cooldown_secs / 86400)}-day cooldown between events`)],
        [L("Pagamentos", "Payouts"), `${st.payouts}`, `${sol(st.total_paid)} ${L("ao beneficiário", "to the beneficiary")}`],
        [
          L("Gatilho", "Trigger"),
          st.kind === 0 ? L("Alerta antecipado", "Early warning") : L("Alerta de vazão", "Flow alert"),
          `${L("local", "location")}: ${st.subject}${st.accept_from > 0 ? L(` · só gravados desde ${br(st.accept_from)}`, ` · only recorded since ${br(st.accept_from)}`) : L(" · aceita os já registrados", " · accepts past records")}`,
        ],
      ];
      const rows = v.payouts
        .map(
          (p, pi) => `<tr><td>${L("fim de", "end of")} ${endOf(p.event_at)}</td>
          <td>${
            v.protocol === "v2"
              ? chainLink({ protocol: "v2", record_index: p.trigger_index })
              : `<a href="${explorerAddr(p.trigger)}" target="_blank" rel="noopener" class="hash">${short(p.trigger)}</a>`
          }</td>
          <td class="num">${sol(p.amount)}</td>
          <td>${br(p.paid_at)}<br><span class="hash">slot ${nf0.format(p.slot)}</span></td>
          <td><button type="button" class="link-btn" data-v="${vi}" data-p="${pi}">${L("verificar", "verify")}</button>${p.explorer ? ` · <a href="${p.explorer}" target="_blank" rel="noopener">tx</a>` : ""}</td></tr>`,
        )
        .join("");
      return `<div class="vault">
        <div class="vault-head"><h3>${esc(VAULT_TITLES[v.name] ? L(...VAULT_TITLES[v.name]) : v.name)}</h3>${
          st.accept_from > 0 ? `<span class="vault-tag live">${L("ao vivo", "live")}</span>` : `<span class="vault-tag">${L("demonstração com o teste de 2024", "demo with the 2024 test run")}</span>`
        }</div>
        <p class="sub">${esc(L(v.description, v.description_en || v.description).replace(/\.?$/, "."))} ${L("Cofre", "Vault")} <a href="${explorerAddr(v.vault)}" target="_blank" rel="noopener">${short(v.vault)}</a> ·
          ${L("beneficiário", "beneficiary")} <a href="${explorerAddr(st.beneficiary)}" target="_blank" rel="noopener">${short(st.beneficiary)}</a> ·
          <a href="${v.created_explorer}" target="_blank" rel="noopener">${L("criação", "creation")}</a></p>
        <div class="kpis">${tiles
          .map(([l, val, n]) => `<div class="kpi"><div class="label">${l}</div><div class="value">${esc(val)}</div><div class="note">${n}</div></div>`)
          .join("")}</div>
        <div class="table-wrap">${
          rows
            ? `<table class="data"><thead><tr><th>${L("Evento do gatilho", "Trigger event")}</th><th>${L("Gatilho", "Trigger")}</th><th class="num">${L("Valor", "Amount")}</th><th>${L("Pago on-chain", "Paid on-chain")}</th><th></th></tr></thead><tbody>${rows}</tbody></table>`
            : `<div class="empty-note">${L("Nenhum pagamento ainda: o cofre espera o próximo gatilho.", "No payout yet: the vault is waiting for the next trigger.")}</div>`
        }</div></div>`;
    })
    .join("");
  list.querySelectorAll("[data-v]").forEach((b) => b.addEventListener("click", () => verifyPayout(B.vaults[+b.dataset.v], +b.dataset.p)));
}

async function verifyPayout(v, pi) {
  const p = v.payouts[pi];
  panel("", `<h3>${L("Verificando o pagamento do cofre", "Verifying the payout of vault")} ${esc(v.name)}…</h3>`, "#vault-verify");
  try {
    const v2 = v.protocol === "v2";
    const prog = programOf(v);
    const [pa, va] = await Promise.all([p.payout, v.vault].map((a) => fetchAccount(B.meta.rpc, a)));
    const pay = v2 ? decodePayoutV2(pa.data) : decodePayout(pa.data);
    const vault = decodeVault(va.data);
    let trig, kindOk, trigOwned, pointsHere;
    if (v2) {
      // The trigger is a record: proven to be in the program's tree, and the receipt names its leaf.
      trig = await onchain({ protocol: "v2", record: p.trigger_record, record_index: p.trigger_index }, null, null);
      trigOwned = trig.owned;
      kindOk =
        vault.kind === 0
          ? trig.kind === "outlook" && trig.triggered && trig.region === vault.subject
          : trig.kind === "forecast" && trig.alert != null && trig.station_id === vault.subject;
      pointsHere = pay.vault === v.vault && pay.trigger_index === p.trigger_index && pay.trigger_leaf === toHex(await hashLeaf(fromHex(p.trigger_record)));
    } else {
      const ta = await fetchAccount(B.meta.rpc, p.trigger);
      trigOwned = ta.owner === prog;
      if (vault.kind === 0) {
        trig = decodeOutlook(ta.data);
        kindOk = trig.triggered && trig.region === vault.subject;
      } else {
        trig = decodeAlertReceipt(ta.data);
        kindOk = trig.station_id === vault.subject;
      }
      pointsHere = pay.vault === v.vault && pay.trigger === p.trigger;
    }
    const checks = [
      [
        v2
          ? L("Pagamento e cofre são contas do programa do Cota; o gatilho é um registro da árvore dele", "Payout and vault are Cota program accounts; the trigger is a record in its tree")
          : L("Pagamento, cofre e gatilho são contas do programa do Cota", "Payout, vault and trigger are all Cota program accounts"),
        [pa, va].every((a) => a.owner === prog) && trigOwned,
      ],
      [L("O recibo aponta para este cofre e este gatilho", "The receipt points to this vault and this trigger"), pointsHere],
      [
        L(
          `O gatilho é um ${trigger(vault.kind)} de ${esc(vault.subject)}${vault.kind === 0 ? " que disparou a regra" : ""}`,
          `The trigger is a ${trigger(vault.kind)} for ${esc(vault.subject)}${vault.kind === 0 ? " that fired the rule" : ""}`,
        ),
        kindOk,
      ],
      [L(`Evento do gatilho = fim de ${endOf(pay.event_at)}`, `Trigger event = end of ${endOf(pay.event_at)}`), trig.issued_at === pay.event_at],
      [
        vault.accept_from > 0
          ? L(`Gatilho gravado (${br(trig.committed_at)}) depois de o cofre passar a aceitar`, `Trigger recorded (${br(trig.committed_at)}) after the vault started accepting`)
          : L("O cofre aceita gatilhos já registrados", "The vault accepts triggers recorded earlier"),
        trig.committed_at >= vault.accept_from,
      ],
      [
        L(`Pago ${sol(pay.amount)} (até ${sol(vault.payout_lamports)}) ao beneficiário gravado no cofre`, `Paid ${sol(pay.amount)} (up to ${sol(vault.payout_lamports)}) to the beneficiary stored in the vault`),
        pay.amount <= vault.payout_lamports && pay.beneficiary === vault.beneficiary,
      ],
      [
        L(`Pago no slot ${nf0.format(pay.slot)}, depois do gatilho (slot ${nf0.format(trig.slot)})`, `Paid at slot ${nf0.format(pay.slot)}, after the trigger (slot ${nf0.format(trig.slot)})`),
        pay.slot >= trig.slot,
      ],
      [
        L(`Totais do cofre: ${vault.payouts} pagamento(s), ${sol(vault.total_paid)}; saldo atual ${sol(va.lamports)}`, `Vault totals: ${vault.payouts} payout(s), ${sol(vault.total_paid)}; current balance ${sol(va.lamports)}`),
        vault.payouts >= v.payouts.length,
      ],
    ];
    const ok = checks.every(([, x]) => x);
    panel(
      ok ? "ok" : "fail",
      `<h3>${L("Pagamento", "Payout")} ${CHECKED(pa.slot)}</h3><ol>${checks
        .map(([t, x]) => `<li>${x ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol>
      <div class="verdict ${ok ? "good-text" : "crit-text"}">${
        ok
          ? L(`✓ O programa liberou ${sol(pay.amount)} em ${br(pay.paid_at)} só porque o gatilho existia on-chain.`, `✓ The program released ${sol(pay.amount)} on ${br(pay.paid_at)} only because the trigger existed on-chain.`)
          : MISMATCH()
      }</div>`,
      "#vault-verify",
    );
  } catch (e) {
    panel("fail", `<h3>${CANT()}</h3><p>${esc(e.message)}</p>`, "#vault-verify");
  }
}

// ---- theme and startup ----------------------------------------------------

function initTheme() {
  let saved = null;
  try {
    saved = localStorage.getItem("cota-theme");
  } catch {}
  if (saved) document.documentElement.dataset.theme = saved;
  $("#theme").addEventListener("click", () => {
    const dark = document.documentElement.dataset.theme
      ? document.documentElement.dataset.theme === "dark"
      : matchMedia("(prefers-color-scheme: dark)").matches;
    const next = dark ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    try {
      localStorage.setItem("cota-theme", next);
    } catch {}
  });
}

// Language: ?lang=en in the URL, then the saved choice, then the browser language.
function initialLang() {
  const q = new URLSearchParams(location.search).get("lang");
  if (q === "pt" || q === "en") return q;
  try {
    const saved = localStorage.getItem("cota-lang");
    if (saved === "pt" || saved === "en") return saved;
  } catch {}
  return (navigator.languages?.[0] ?? navigator.language ?? "pt").toLowerCase().startsWith("pt") ? "pt" : "en";
}

// Fixed page text: the CSS hides the [data-lang] blocks; this handles the title, the description and the
// aria-labels.
function applyLang(next) {
  lang = next;
  setFormats();
  document.documentElement.lang = L("pt-BR", "en");
  document.title = L("Cota — alertas verificáveis", "Cota — verifiable flood alerts");
  document
    .querySelector('meta[name="description"]')
    .setAttribute(
      "content",
      L(
        "O diário do rio que ninguém consegue apagar: dados, previsões e alertas de cheia gravados na Solana para qualquer pessoa conferir.",
        "A river diary nobody can erase: flood data, forecasts and warnings recorded on Solana for anyone to check.",
      ),
    );
  document.querySelectorAll("[data-label-en]").forEach((n) => {
    n.dataset.labelPt ??= n.getAttribute("aria-label");
    n.setAttribute("aria-label", L(n.dataset.labelPt, n.dataset.labelEn));
  });
  document.querySelectorAll("[data-set-lang]").forEach((b) => b.setAttribute("aria-checked", String(b.dataset.setLang === lang)));
}

function renderAll() {
  renderPicker();
  $("#generated").textContent = L("Dados gerados em", "Data generated at") + ` ${utc(Date.parse(B.meta.generated_at) / 1000)}.`;
  // With no location chosen the sections are hidden and the charts would have no width: wait for the
  // choice.
  if (!state.place) return;
  // Open verification panels were left in the other language: close them.
  document.querySelectorAll(".verify").forEach((p) => (p.hidden = true));
  renderLive();
  renderStamp();
  renderUpgrade();
  renderOutlook();
  renderVaults();
  renderKpis();
  renderRain();
  renderQuality();
  renderMetrics();
  selectDay(state.sel);
}

// ---- location -------------------------------------------------------------
// For now only the Guaíba is monitored; the choice comes before the data because each location has
// its own.

const PLACES = [
  { id: "guaiba", short: "Guaíba", name: ["Rio Guaíba", "Guaíba river"], where: ["Porto Alegre, Rio Grande do Sul, Brasil", "Porto Alegre, Rio Grande do Sul, Brazil"] },
];

function renderPicker() {
  const since = B?.live.days[0]?.day;
  $("#place-list").innerHTML = PLACES.map(
    (p) => `<button type="button" class="place-card" data-place="${p.id}" aria-pressed="${p.id === state.place}">
      <span class="place-pin" aria-hidden="true"><svg viewBox="0 0 16 16" width="20" height="20"><path d="M8 15s5-4.6 5-8.5A5 5 0 0 0 3 6.5C3 10.4 8 15 8 15Z" fill="none" stroke="currentColor" stroke-width="1.4"/><circle cx="8" cy="6.5" r="1.8" fill="currentColor"/></svg></span>
      <span><strong>${L(...p.name)}</strong><small>${L(...p.where)}${since ? L(` · ao vivo desde ${dmy(since)}`, ` · live since ${dmy(since)}`) : ""}</small></span>
      <span class="place-go">${p.id === state.place ? L("Selecionado", "Selected") : L("Ver dados →", "See data →")}</span>
    </button>`,
  ).join("");
  $("#place-list")
    .querySelectorAll("[data-place]")
    .forEach((b) => b.addEventListener("click", () => choosePlace(b.dataset.place, true)));
}

function choosePlace(id, scroll) {
  const p = PLACES.find((x) => x.id === id);
  if (!p) return;
  const first = !state.place;
  state.place = id;
  document.body.dataset.place = id;
  $("#place-name").textContent = p.short;
  try {
    localStorage.setItem("cota-place", id);
  } catch {}
  if (B && (first || scroll)) renderAll();
  if (scroll) $("#how").scrollIntoView();
}

function initPlace() {
  let id = new URLSearchParams(location.search).get("local");
  if (!id) {
    try {
      id = localStorage.getItem("cota-place");
    } catch {}
  }
  if (PLACES.some((p) => p.id === id)) choosePlace(id, false);
  $("#place-chip").addEventListener("click", () => {
    scrollTo({ top: 0 });
    $("#place-list .place-card")?.focus();
  });
}

function initLang() {
  applyLang(initialLang());
  document.querySelectorAll("[data-set-lang]").forEach((b) =>
    b.addEventListener("click", () => {
      if (b.dataset.setLang === lang) return;
      applyLang(b.dataset.setLang);
      try {
        localStorage.setItem("cota-lang", lang);
      } catch {}
      const url = new URL(location.href);
      if (url.searchParams.has("lang")) {
        url.searchParams.set("lang", lang);
        history.replaceState(null, "", url);
      }
      if (B) renderAll();
    }),
  );
}

// Collapsed blocks (guide, 2024 test): a link to them, or the #id in the URL, opens them.
function initFolds() {
  for (const id of ["guide", "replay"]) {
    const d = $(`#${id}`);
    document.querySelectorAll(`a[href="#${id}"]`).forEach((a) => a.addEventListener("click", () => (d.open = true)));
    if (location.hash === `#${id}`) d.open = true;
  }
  // The test charts were drawn with the block closed, without width: redraw them on open.
  $("#replay").addEventListener("toggle", (e) => {
    if (e.target.open && B && state.place) {
      renderFlow();
      renderRain();
    }
  });
}

async function main() {
  initTheme();
  initLang();
  initFolds();
  initPlace();
  B = await (await fetch("data/bundle.json")).json();
  derive();
  const prog = $("#program-link");
  prog.textContent = short(B.meta.program, 8);
  prog.href = explorerAddr(B.meta.program);
  $("#cluster").textContent = B.meta.cluster;
  // Starts at the first threshold crossing: the most important moment of the replay.
  state.sel = B.dayIndex.get(B.crossings[0]?.day) ?? 0;
  if (state.sel > 0) state.sel -= 2; // two days earlier: when the first alert went out
  renderAll();

  document.querySelectorAll("#horizon button").forEach((b) =>
    b.addEventListener("click", () => {
      state.h = +b.dataset.h;
      document.querySelectorAll("#horizon button").forEach((x) => x.setAttribute("aria-checked", String(x === b)));
      renderFlow();
    }),
  );
  $("#flow-table-toggle").addEventListener("click", (e) => {
    const t = $("#flow-table");
    t.hidden = !t.hidden;
    e.currentTarget.setAttribute("aria-expanded", String(!t.hidden));
  });
  $("#prev-day").addEventListener("click", () => selectDay(state.sel - 1));
  $("#next-day").addEventListener("click", () => selectDay(state.sel + 1));
  $("#station-filter").addEventListener("change", (e) => {
    state.station = e.target.value;
    renderReadings(B.days[state.sel]);
  });
  let t;
  addEventListener("resize", () => {
    clearTimeout(t);
    t = setTimeout(() => {
      if (!state.place) return;
      renderFlow();
      renderRain();
      renderLiveChart();
      if (B.live.stage_series?.length) renderStageChart(B.live.stage_series);
      if (basin.map) drawBasin();
      renderOutlookChart();
    }, 150);
  });
}

main().catch((e) => {
  document
    .querySelector("main")
    .insertAdjacentHTML("afterbegin", `<p class="crit-text wrap">${L("Erro ao carregar os dados", "Failed to load the data")}: ${esc(e.message)}</p>`);
});
