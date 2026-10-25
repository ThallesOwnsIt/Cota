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
  toHex,
  verifyInclusion,
} from "./verify.js";

const $ = (s) => document.querySelector(s);
const SVG = "http://www.w3.org/2000/svg";
const nf0 = new Intl.NumberFormat("pt-BR", { maximumFractionDigits: 0 });
const nf1 = new Intl.NumberFormat("pt-BR", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const nf2 = new Intl.NumberFormat("pt-BR", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const pct = (x) => `${nf0.format(x * 100)}%`;
const m3 = (milli) => (milli == null ? "—" : nf0.format(milli / 1000));
const short = (h, n = 8) => `${h.slice(0, n)}…${h.slice(-4)}`;
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const STATUS = {
  signed: { cls: "st-good", icon: "✓", label: "assinou" },
  absent: { cls: "st-warning", icon: "–", label: "ausente" },
  disagreed: { cls: "st-critical", icon: "✕", label: "discordou" },
  invalid_signature: { cls: "st-critical", icon: "!", label: "assinatura inválida" },
};
const REASON = {
  unavailable: "indisponível",
  stuck: "sensor travado",
  jump: "salto impossível",
  disagreement: "divergência entre nós",
  stale: "sem dados recentes",
};
const UNIT = { precipitation: "mm", river_discharge: "m³/s", river_stage: "m" };

const state = { h: 24, sel: 0, station: "", batches: new Map(), activeReading: null, tamper: false };
let B; // bundle

// ---- utilidades de data ---------------------------------------------------

const dm = (iso) => `${iso.slice(8, 10)}/${iso.slice(5, 7)}`;
const dmy = (iso) => `${dm(iso)}/${iso.slice(0, 4)}`;
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
  // Cruzamentos observados e o primeiro alerta que apontava para aquele dia.
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
  const leads = B.crossings.map((c) => (c.lead == null ? "sem alerta" : `${c.lead} h`));
  const tiles = [
    ["Lotes ancorados", a.batches, `${partial} com algum nó ausente ou discordante`],
    ["Previsões antes da observação", `${a.forecasts_recorded_before_observation}/${a.forecasts_observed}`, "auditado pelos slots da Solana"],
    ["Alertas on-chain", a.alerts, `${a.alerts_recorded_before_observation} gravados antes da observação`],
    ["Cruzamentos do limiar", B.crossings.length, `antecedência do 1º alerta: ${leads.join(" e ")}`],
    ["Cobertura P10–P90", `${pct(B.stats[24].coverage)} · ${pct(B.stats[48].coverage)}`, "1 dia · 2 dias antes, todo o replay on-chain (meta ≈ 80%)"],
    ["Fontes degradadas", degr, "eventos SourceDegraded"],
  ];
  $("#kpis").innerHTML = tiles
    .map(([l, v, n]) => `<div class="kpi"><div class="label">${l}</div><div class="value">${v}</div><div class="note">${n}</div></div>`)
    .join("");
}

// ---- gráfico de vazão -----------------------------------------------------

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

// Gráfico de vazão observada × faixa prevista. Reutilizado pelo replay e pelo modo ao vivo.
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
  // grade e eixo y
  const step = niceMax(ymax / 5);
  for (let v = 0; v <= ymax + 1; v += step) {
    el("line", { x1: m.l, x2: W - m.r, y1: y(v), y2: y(v), class: v === 0 ? "baseline" : "gridline" }, svg);
    el("text", { x: m.l - 6, y: y(v) + 4, "text-anchor": "end", class: "tick-label" }, svg).textContent = nf0.format(v / 1000);
  }
  // eixo x: um rótulo a cada ~5 dias
  const every = Math.max(1, Math.ceil(data.length / (W < 560 ? 5 : 8)));
  data.forEach((d, i) => {
    if (i % every === 0) el("text", { x: x(i), y: H - 6, "text-anchor": "middle", class: "tick-label" }, svg).textContent = dm(d.day);
  });

  // dia selecionado
  const si = data.findIndex((d) => d.day === selDay);
  const colW = (W - m.l - m.r) / Math.max(1, data.length - 1);
  if (si >= 0) el("rect", { x: x(si) - colW / 2, y: m.t, width: colW, height: H - m.t - m.b, fill: "var(--surface-2)" }, svg);

  // faixa P10–P90 e P50
  const fcPts = data.map((d, i) => (d.fc ? { i, ...d.fc } : null)).filter(Boolean);
  const band = fcPts.map((p) => `${x(p.i)},${y(p.p90)}`).concat([...fcPts].reverse().map((p) => `${x(p.i)},${y(p.p10)}`));
  el("polygon", { points: band.join(" "), fill: "var(--band)" }, svg);
  el("polyline", { points: fcPts.map((p) => `${x(p.i)},${y(p.p50)}`).join(" "), fill: "none", stroke: "var(--forecast)", "stroke-width": 2, "stroke-dasharray": "5 4" }, svg);

  // limiar
  el("line", { x1: m.l, x2: W - m.r, y1: y(thr), y2: y(thr), stroke: "var(--critical)", "stroke-width": 1, "stroke-dasharray": "2 3" }, svg);
  el("text", { x: m.l + 6, y: y(thr) - 5, class: "tick-label" }, svg).textContent = `limiar de alerta · ${m3(thr)} m³/s`;

  // observada
  const obsPts = data.map((d, i) => (d.obs != null ? `${x(i)},${y(d.obs)}` : null)).filter(Boolean);
  el("polyline", { points: obsPts.join(" "), fill: "none", stroke: "var(--observed)", "stroke-width": 2, "stroke-linejoin": "round" }, svg);

  // alertas: triângulo acima da P90 do dia-alvo
  for (const p of fcPts) {
    if (!p.alert) continue;
    const cx = x(p.i), cy = y(p.p90) - 9;
    el("path", { d: `M${cx - 5},${cy - 4} L${cx + 5},${cy - 4} L${cx},${cy + 4} Z`, fill: "var(--critical)", stroke: "var(--surface)", "stroke-width": 1.5 }, svg);
  }

  // rótulos diretos nas pontas
  const lastObs = data.findLastIndex((d) => d.obs != null);
  if (lastObs >= 0)
    el("text", { x: x(lastObs) - 4, y: y(data[lastObs].obs) - 8, "text-anchor": "end", class: "tick-label", fill: "var(--text-2)" }, svg).textContent = "observada";
  const lastFc = fcPts.at(-1);
  if (lastFc) el("text", { x: x(lastFc.i), y: y(lastFc.p90) - 8, "text-anchor": "end", class: "tick-label" }, svg).textContent = "P10–P90";

  // camada de interação
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
        row("Observada", d.obs == null ? "ainda não ancorada" : `${m3(d.obs)} m³/s`) +
        (d.fc
          ? row(`Prevista em ${dm(d.issue.issue_day)} (${hd} d)`, "") +
            row("P10 · P50 · P90", `${m3(d.fc.p10)} · ${m3(d.fc.p50)} · ${m3(d.fc.p90)}`) +
            row("Alerta", d.fc.alert ? "▼ sim (P90 ≥ limiar)" : "não")
          : row("Previsão", "sem previsão neste horizonte")),
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
    `<span class="key" style="color:var(--observed)"><span class="swatch-line"></span><span style="color:var(--text-2)">Observada (GloFAS)</span></span>` +
    `<span class="key" style="color:var(--forecast)"><span class="swatch-dash"></span><span style="color:var(--text-2)">Previsão P50</span></span>` +
    `<span class="key"><span class="swatch-band"></span>Faixa P10–P90</span>` +
    `<span class="key"><span style="color:var(--critical)">▼</span>Alerta emitido para o dia (P90 ≥ limiar)</span>`;

  if (table) renderFlowTable(data);
}

function renderFlowTable(data) {
  const rows = data
    .map(
      (d) =>
        `<tr><td>${dmy(d.day)}</td><td class="num">${m3(d.obs)}</td><td class="num">${m3(d.fc?.p10)}</td><td class="num">${m3(d.fc?.p50)}</td><td class="num">${m3(d.fc?.p90)}</td><td>${d.fc?.alert ? "▼ alerta" : ""}</td><td>${d.fc?.inside_p10_p90 == null ? "" : d.fc.inside_p10_p90 ? "dentro" : "fora"}</td></tr>`,
    )
    .join("");
  $("#flow-table").innerHTML =
    `<table class="data"><thead><tr><th>Dia</th><th class="num">Observada</th><th class="num">P10</th><th class="num">P50</th><th class="num">P90</th><th>Alerta</th><th>Faixa</th></tr></thead><tbody>${rows}</tbody></table>`;
}

function renderRain() {
  const box = $("#rain-chart");
  const W = Math.max(320, box.clientWidth);
  const H = 120;
  const m = { l: 52, r: 18, t: 22, b: 22 };
  const n = B.days.length + 2; // mesmo eixo x do gráfico de vazão
  const vals = B.days.map((d) => {
    const v = Object.entries(d.rain_milli).filter(([s]) => (d.rain_hours[s] ?? 0) >= 20).map(([, x]) => x / 1000);
    return v.length ? v.reduce((a, b) => a + b, 0) / v.length : null;
  });
  const ymax = niceMax(Math.max(...vals.filter((v) => v != null)));
  const x = (i) => m.l + (i / (n - 1)) * (W - m.l - m.r);
  const y = (v) => H - m.b - (v / ymax) * (H - m.t - m.b);
  const bw = Math.max(3, ((W - m.l - m.r) / (n - 1)) * 0.6);
  const svg = el("svg", { viewBox: `0 0 ${W} ${H}`, width: W, height: H });
  el("text", { x: m.l, y: 12, class: "tick-label", fill: "var(--text-2)" }, svg).textContent = "Chuva média diária nas estações do INMET (mm)";
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
        `<div class="t-title">${dmy(d.day)} · média ${nf1.format(v)} mm</div>` +
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

// ---- linha do tempo -------------------------------------------------------

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
        return cell(i, st.cls, st.icon, `${dmy(d.day)} · node-${k + 1} ${st.label}`);
      })
      .join("");
  });
  html += `<div class="tl-label">Alerta emitido</div>`;
  html += B.days
    .map((d, i) => {
      const is = B.issueByDay.get(d.day);
      const al = is?.horizons.filter((h) => h.alert) ?? [];
      return al.length
        ? cell(i, "st-critical", "▼", `${dmy(d.day)} · alerta para ${al.map((h) => dm(h.target_day)).join(" e ")}`)
        : cell(i, "st-forecast", "", `${dmy(d.day)} · previsão sem alerta`);
    })
    .join("");
  html += `<div class="tl-label">Fonte degradada</div>`;
  html += B.days
    .map((d, i) =>
      d.degradations.length
        ? cell(i, "st-warning", "!", `${dmy(d.day)} · ${d.degradations.map((g) => `${g.station_id} (${REASON[g.reason]})`).join(", ")}`)
        : cell(i, "empty", "", dmy(d.day)),
    )
    .join("");
  html += "</div>";
  const box = $("#timeline");
  box.innerHTML = html;
  box.querySelectorAll("[data-i]").forEach((b) => b.addEventListener("click", () => selectDay(+b.dataset.i)));

  $("#timeline-legend").innerHTML = [
    ["st-good", "✓", "assinou"],
    ["st-warning", "–", "ausente"],
    ["st-critical", "✕", "discordou (assinou outra raiz)"],
    ["st-critical", "▼", "alerta emitido no fim do dia"],
    ["st-warning", "!", "SourceDegraded"],
  ]
    .map(([c, i, l]) => `<span class="key"><span class="dot ${c}">${i}</span>${l}</span>`)
    .join("");
}

// ---- dia selecionado ------------------------------------------------------

async function loadBatch(id) {
  if (!state.batches.has(id)) {
    const res = await fetch(`data/batches/${String(id).padStart(5, "0")}.json`);
    state.batches.set(id, await res.json());
  }
  return state.batches.get(id);
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
  $("#day-title").textContent = `Lote ${d.batch_id} · ${dmy(d.day)}`;
  $("#day-sub").textContent = `Janela ${utc(d.t_start)} → ${utc(d.t_end)} (tempo do replay). Ancorado na devnet em ${utc(d.committed_at)}, slot ${nf0.format(d.slot)}.`;
  $("#prev-day").disabled = state.sel === 0;
  $("#next-day").disabled = state.sel === B.days.length - 1;

  const signers = d.members
    .map((m) => {
      const st = STATUS[m.status];
      return `<div class="status"><span class="dot ${st.cls}">${st.icon}</span>${nodeName(m.member)} ${st.label} <a class="mono" href="${explorerAddr(m.member)}" target="_blank" rel="noopener">${short(m.member, 6)}</a></div>`;
    })
    .join("");
  const events = [
    ...d.degradations.map(
      (g) => `<li><span class="dot st-warning">!</span> <strong>SourceDegraded</strong> ${g.station_id}: ${REASON[g.reason]}, C = ${nf2.format(g.score_milli / 1000)} · <a href="${g.explorer}" target="_blank" rel="noopener">transação</a></li>`,
    ),
    ...d.dissents.map(
      (x) =>
        `<li><span class="dot st-critical">✕</span> ${nodeName(x.member)} informou ${x.station_id} às ${utc(x.ts_utc).slice(11, 16)} = ${x.value_milli == null ? "nada" : nf1.format(x.value_milli / 1000)}; mediana dos nós = ${x.median_milli == null ? "nada" : nf1.format(x.median_milli / 1000)} (MAD)</li>`,
    ),
  ];
  $("#day-batch").innerHTML = `<div class="panel">
    <h3>Lote ancorado</h3>
    <dl class="kv">
      <dt>Raiz Merkle</dt><dd class="hash" title="${d.merkle_root}">${d.merkle_root}</dd>
      <dt>Raiz anterior</dt><dd class="hash" title="${d.prev_batch_root}">${short(d.prev_batch_root, 16)}</dd>
      <dt>Leituras</dt><dd>${d.leaf_count}</dd>
      <dt>Assinaturas</dt><dd>${signers}</dd>
      <dt>Na cadeia</dt><dd><a href="${d.explorer}" target="_blank" rel="noopener">transação</a> · <a href="${explorerAddr(d.batch_account)}" target="_blank" rel="noopener">conta do lote</a></dd>
    </dl>
    ${events.length ? `<ul class="events">${events.join("")}</ul>` : ""}
    <p><button type="button" class="ghost" id="verify-batch">Verificar o lote inteiro na devnet</button></p>
  </div>`;
  $("#verify-batch").addEventListener("click", () => verifyBatch(d));

  // Quem sabia o quê: previsões que apontavam para este dia e a emitida no fim dele.
  const before = [48, 24]
    .map((h) => B.byTarget[h].get(d.day))
    .filter(Boolean)
    .map(
      ({ issue, f }) =>
        `<tr><td>${dm(issue.issue_day)} (${f.horizon_hours / 24} d antes)</td><td>${m3(f.p10)}</td><td>${m3(f.p50)}</td><td>${m3(f.p90)}</td><td>${f.alert ? '<span class="crit-text">▼ alerta</span>' : "—"}</td><td>${f.inside_p10_p90 == null ? "—" : f.inside_p10_p90 ? '<span class="good-text">dentro</span>' : "fora"}</td></tr>`,
    )
    .join("");
  const is = B.issueByDay.get(d.day);
  const issued = is
    ? is.horizons
        .map(
          (f) =>
            `<tr><td>${dm(f.target_day)} (+${f.horizon_hours} h)</td><td>${m3(f.p10)}</td><td>${m3(f.p50)}</td><td>${m3(f.p90)}</td><td>${f.alert ? `<a class="crit-text" href="${f.alert.explorer}" target="_blank" rel="noopener">▼ alerta</a>` : "—"}</td><td><a href="${f.explorer}" target="_blank" rel="noopener">slot ${nf0.format(f.slot)}</a></td></tr>`,
        )
        .join("")
    : "";
  const obsTxt = d.guaiba_milli == null ? "sem leitura" : `${m3(d.guaiba_milli)} m³/s`;
  $("#day-forecast").innerHTML = `<div class="panel">
    <h3>O que já se previa para ${dm(d.day)}</h3>
    <p class="sub">Observada (Guaíba, GloFAS): <strong>${obsTxt}</strong></p>
    ${before ? `<table class="fc-table"><thead><tr><th>Emitida em</th><th>P10</th><th>P50</th><th>P90</th><th>Alerta</th><th>Observada</th></tr></thead><tbody>${before}</tbody></table>` : '<p class="sub">Nenhuma previsão apontava para este dia.</p>'}
    <h3 style="margin-top:14px">Previsão emitida no fim do dia</h3>
    ${
      is
        ? `<table class="fc-table"><thead><tr><th>Alvo</th><th>P10</th><th>P50</th><th>P90</th><th>Alerta</th><th>Gravada</th></tr></thead><tbody>${issued}</tbody></table>
           <p class="sub">Baseada no lote ${is.based_on_batch_id} (raiz ${short(is.based_on_root)}), features_root ${short(is.features_root)}.</p>
           <p><button type="button" class="ghost" id="verify-forecast">Verificar previsão na devnet</button></p>`
        : '<p class="sub">Sem previsão (dados insuficientes).</p>'
    }
  </div>`;
  $("#verify-forecast")?.addEventListener("click", () => verifyForecast(is));

  renderReadings(d);
}

async function renderReadings(d) {
  const b = await loadBatch(d.batch_id);
  if (B.days[state.sel] !== d) return;
  const sel = $("#station-filter");
  const ids = [...new Set(b.readings.map((r) => r.station_id))].sort();
  if (!ids.includes(state.station)) state.station = "";
  sel.innerHTML = `<option value="">todas (${b.readings.length})</option>` + ids.map((s) => `<option${s === state.station ? " selected" : ""}>${s}</option>`).join("");
  const rows = b.readings
    .map((r, i) => ({ r, i }))
    .filter(({ r }) => !state.station || r.station_id === state.station)
    .map(
      ({ r, i }) =>
        `<tr data-i="${i}"${state.activeReading === i ? ' class="active"' : ""}><td>${r.station_id}</td><td>${utc(r.ts_utc).slice(0, 16)}</td><td class="num">${(r.variable === "precipitation" ? nf1 : nf0).format(r.value_milli / 1000)}</td><td>${UNIT[r.variable]}</td><td class="hash">${short(r.raw_payload_sha256, 10)}</td><td><button type="button" class="link-btn" data-v="${i}">verificar</button></td></tr>`,
    )
    .join("");
  $("#readings").innerHTML =
    `<table class="data"><thead><tr><th>Estação</th><th>Instante</th><th class="num">Valor</th><th>Unid.</th><th>SHA-256 do dado bruto</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
  $("#readings").querySelectorAll("[data-v]").forEach((btn) => btn.addEventListener("click", () => verifyReading(d, +btn.dataset.v)));
}

// ---- verificação ----------------------------------------------------------

function panel(cls, html, target = "#verify-panel") {
  const p = $(target);
  p.className = `verify ${cls}`;
  p.innerHTML = html;
  p.hidden = false;
  return p;
}

async function onchainBatch(d) {
  const acc = await fetchAccount(B.meta.rpc, d.batch_account);
  if (acc.owner !== B.meta.program) throw new Error(`conta pertence a ${acc.owner}, não ao programa do Cota`);
  return { ...decodeBatch(acc.data), contextSlot: acc.slot };
}

async function verifyReading(d, index) {
  state.activeReading = index;
  $("#readings").querySelectorAll("tr[data-i]").forEach((tr) => tr.classList.toggle("active", +tr.dataset.i === index));
  const b = await loadBatch(d.batch_id);
  const original = b.readings[index];
  const r = state.tamper ? { ...original, value_milli: original.value_milli + 1 } : original;
  panel("", `<h3>Verificando a leitura ${original.station_id} · ${utc(original.ts_utc)}…</h3>`);
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
      .map((s, k) => `<li>nível ${k + 1}: irmão à ${s.side} <span class="hash">${short(toHex(s.sibling), 12)}</span> → <span class="hash">${short(toHex(s.result), 12)}</span></li>`)
      .join("");
    const p = panel(
      ok ? "ok" : "fail",
      `<h3>Prova de inclusão — ${original.station_id}, ${utc(original.ts_utc)}${state.tamper ? " (adulterada: +0,001)" : ""}</h3>
      <ol>
        <li>Bytes canônicos (Borsh, ${bytes.length} B): <span class="hash">${short(toHex(bytes), 24)}</span></li>
        <li>Folha = SHA-256(0x00 ‖ bytes) = <span class="hash">${toHex(leaf)}</span></li>
        <li>Caminho de ${path.length} irmãos (posição ${index} de ${leaves.length}):<ol>${steps}</ol></li>
        <li>Raiz recalculada no navegador: <span class="hash">${computed}</span></li>
        <li>Raiz on-chain, lida agora da devnet${chain ? ` (conta <a href="${explorerAddr(d.batch_account)}" target="_blank" rel="noopener">${short(d.batch_account, 6)}</a>, slot ${nf0.format(chain.contextSlot)})` : ""}: <span class="hash">${chain ? chain.merkle_root : `não foi possível consultar: ${esc(chainErr)}`}</span></li>
      </ol>
      <div class="verdict ${ok ? "good-text" : "crit-text"}">${
        ok
          ? "✓ A leitura está no lote que o comitê assinou e a Solana registrou."
          : chain
            ? "✕ Não confere: esta leitura não é a que foi ancorada."
            : "Não foi possível comparar com a cadeia agora (a raiz do ledger local é " + d.merkle_root.slice(0, 12) + "…)."
      }</div>
      <label class="tamper"><input type="checkbox" id="tamper"${state.tamper ? " checked" : ""}/> Simular adulteração (+0,001 no valor)</label>`,
    );
    p.querySelector("#tamper").addEventListener("change", (e) => {
      state.tamper = e.target.checked;
      verifyReading(d, index);
    });
  } catch (e) {
    panel("fail", `<h3>Erro na verificação</h3><p>${esc(e.message)}</p>`);
  }
}

async function verifyBatch(d) {
  panel("", `<h3>Verificando o lote ${d.batch_id}…</h3>`);
  try {
    const b = await loadBatch(d.batch_id);
    const root = toHex(await rootFromLeaves(await Promise.all(b.readings.map((x) => hashLeaf(readingBytes(x))))));
    const c = await onchainBatch(d);
    const prev = B.days[state.sel - 1];
    const checks = [
      ["Raiz recalculada das " + b.readings.length + " leituras = raiz on-chain", root === c.merkle_root],
      ["Encadeia na raiz do lote anterior", c.prev_batch_root === (prev ? prev.merkle_root : "0".repeat(64))],
      ["Quantidade de folhas", c.leaf_count === b.readings.length],
      [`Assinantes on-chain (bitmap ${c.signers.toString(2).padStart(3, "0")}): ${c.signer_count} de ${B.meta.members.length}, limiar ${B.meta.threshold}`, c.signer_count >= B.meta.threshold],
      ["Conta pertence ao programa do Cota", true],
    ];
    const ok = checks.every(([, v]) => v);
    panel(
      ok ? "ok" : "fail",
      `<h3>Lote ${d.batch_id} conferido contra a devnet (slot ${nf0.format(c.contextSlot)})</h3><ol>${checks
        .map(([t, v]) => `<li>${v ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol><div class="verdict ${ok ? "good-text" : "crit-text"}">${ok ? "✓ Lote íntegro." : "✕ Divergência encontrada."}</div>`,
    );
  } catch (e) {
    panel("fail", `<h3>Não foi possível consultar a devnet</h3><p>${esc(e.message)}</p>`);
  }
}

async function verifyForecast(is) {
  panel("", `<h3>Verificando a previsão emitida em ${dmy(is.issue_day)}…</h3>`);
  try {
    const froot = toHex(await rootFromLeaves(await Promise.all(is.features.map((f) => hashLeaf(featureLeaf(f))))));
    const items = [];
    let ok = froot === is.features_root;
    items.push([`features_root recalculada de ${is.features.length} features = ${short(froot)}`, ok]);
    for (const f of is.horizons) {
      const acc = await fetchAccount(B.meta.rpc, f.account);
      const c = decodeForecast(acc.data);
      const checks = [
        acc.owner === B.meta.program,
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
        `+${f.horizon_hours} h → ${dm(f.target_day)}: quantis, modelo e features conferem; gravada no slot ${nf0.format(c.slot)}` +
          (target ? `, ${before ? "antes" : "DEPOIS"} do lote da observação (slot ${nf0.format(target.slot)})` : ", observação ainda não ancorada"),
        pass,
      ]);
    }
    panel(
      ok ? "ok" : "fail",
      `<h3>Previsão de ${dmy(is.issue_day)} conferida contra a devnet</h3><ol>${items
        .map(([t, v]) => `<li>${v ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol><p class="sub">Modelo <span class="hash">${B.meta.model_sha256}</span></p>`,
    );
  } catch (e) {
    panel("fail", `<h3>Não foi possível consultar a devnet</h3><p>${esc(e.message)}</p>`);
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
          row("C<sub>s</sub> (média móvel)", nf2.format(q.score)) +
          row("C do dia", nf2.format(q.raw)) +
          row("Disponibilidade", `${q.present}/${q.expected}`) +
          row("Flags", q.flags.length ? q.flags.map((f) => REASON[f]).join(", ") : "nenhuma") +
          row("Degradada", q.degraded ? REASON[q.degraded] : "não"),
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
  const labels = ["≥ 0,995", "0,95", "0,85", "0,70", "0,60", "< 0,60"];
  $("#quality-legend").innerHTML =
    `<span class="key">Índice C <span class="ramp">${labels.map((_, k) => `<span style="background:var(--seq-${k})"></span>`).join("")}</span> ${labels[0]} → ${labels[5]}</span>` +
    `<span class="key"><span class="dot" style="background:var(--seq-5);box-shadow:inset 0 0 0 2px var(--critical)">!</span>SourceDegraded on-chain</span>` +
    `<span class="key"><span class="dot" style="background:var(--seq-0);color:var(--text)">•</span>flag de sensor (travado/salto)</span>`;
}

function renderMetrics() {
  const t = B.metrics.test;
  const rows = ["h1", "h2"]
    .map((k, j) => {
      const r = t[k];
      return `<tr><td>${j + 1} dia${j ? "s" : ""}</td><td class="num">${nf2.format(r.nse_p50)}</td><td class="num">${nf2.format(r.nse_persistence)}</td><td class="num">${nf2.format(r.kge_p50)}</td><td class="num">${nf0.format(r.crps)}</td><td class="num">${pct(r.coverage_p10_p90)}</td><td class="num">${nf2.format(r.alerts.pod)}</td><td class="num">${nf2.format(r.alerts.far)}</td><td class="num">${nf2.format(r.alerts.csi)}</td></tr>`;
    })
    .join("");
  $("#metrics").innerHTML =
    `<table class="data"><thead><tr><th>Avaliação do modelo (alvos 26/04–25/05)</th><th class="num">NSE P50</th><th class="num">NSE persistência</th><th class="num">KGE</th><th class="num">CRPS (m³/s)</th><th class="num">Cobertura P10–P90</th><th class="num">POD</th><th class="num">FAR</th><th class="num">CSI</th></tr></thead><tbody>${rows}</tbody></table>`;
  $("#model-sha").textContent = `model_sha256 ${B.meta.model_sha256.slice(0, 16)}…`;
}

// ---- ao vivo --------------------------------------------------------------

const br = (ts) => new Date(ts * 1000).toLocaleString("pt-BR", { timeZone: "America/Sao_Paulo", dateStyle: "short", timeStyle: "short" });
const SEVERITY = {
  3: { cls: "st-critical", label: "Grande perigo (vermelho)" },
  2: { style: "background:var(--serious)", label: "Perigo (laranja)" },
  1: { cls: "st-warning", label: "Perigo potencial (amarelo)" },
  0: { style: "background:var(--muted)", label: "Outro" },
};

function liveSeries() {
  const L = B.live;
  const byTarget = new Map();
  for (const is of L.issues) for (const f of is.horizons) if (f.horizon_hours === 24) byTarget.set(f.target_day, { issue: is, f });
  const obs = new Map(L.days.map((d) => [d.day, d.guaiba_milli]));
  const rows = [];
  for (let d = L.days[0].day; d <= addDays(L.days.at(-1).day, 2); d = addDays(d, 1)) {
    const fc = byTarget.get(d);
    rows.push({ day: d, obs: obs.get(d) ?? null, fc: fc?.f ?? null, issue: fc?.issue ?? null });
  }
  // O alvo de +48 h da última emissão também entra na faixa.
  const last = L.issues.at(-1);
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
  const L = B.live;
  const thr = B.meta.alert_threshold_milli;
  if (!L.days.length) {
    $("#live-sub").textContent = "Modo ao vivo ainda não iniciado: nenhum lote depois do replay.";
  } else {
    const d = L.days.at(-1);
    $("#live-sub").textContent =
      `Último lote: ${dmy(d.day)} (lote ${d.batch_id}), ancorado em ${br(d.committed_at)} (horário de Brasília). ` +
      `${L.days.length} dia(s) desde ${dmy(L.days[0].day)}. Vazão modelada pelo GloFAS; a chuva do INMET entra com ~1 mês de atraso.`;
    const is = L.issues.at(-1);
    const fc = (h) => is?.horizons.find((f) => f.horizon_hours === h);
    const alerting = is?.horizons.filter((f) => f.alert) ?? [];
    const red = L.public_alerts.filter((a) => a.severity === 3).length;
    const tiles = [
      [`Vazão em ${dm(d.day)}`, `${m3(d.guaiba_milli)}`, "m³/s, Guaíba (GloFAS)"],
      ...[24, 48]
        .map((h) => fc(h))
        .filter(Boolean)
        .map((f) => [`Previsão para ${dm(f.target_day)}`, m3(f.p50), `P10–P90: ${m3(f.p10)}–${m3(f.p90)} m³/s`]),
      [
        "Situação do modelo",
        alerting.length ? "▼ alerta" : "sem alerta",
        alerting.length ? `P90 ≥ ${m3(thr)} para ${alerting.map((f) => dm(f.target_day)).join(" e ")}` : `P90 abaixo de ${m3(thr)} m³/s`,
      ],
      ["Avisos oficiais registrados", L.public_alerts.length, `${red} vermelho(s) · desde o início do monitoramento`],
    ];
    $("#live-kpis").innerHTML = tiles
      .map(([l, v, n]) => `<div class="kpi"><div class="label">${l}</div><div class="value">${v}</div><div class="note">${n}</div></div>`)
      .join("");
    renderLiveChart();
  }

  if (!L.public_alerts.length) {
    $("#live-alerts").innerHTML = `<div class="empty-note">Nenhum aviso do INMET para o RS registrado até agora.</div>`;
    return;
  }
  const rows = L.public_alerts
    .map((a, i) => {
      const s = SEVERITY[a.severity] ?? SEVERITY[0];
      const n = a.signers.toString(2).split("").filter((b) => b === "1").length;
      return `<tr><td>${br(a.committed_at)}<br><span class="hash">slot ${nf0.format(a.slot)}</span></td>
        <td>${esc(a.description)}<br><span class="hash" title="${esc(a.alert_id)}">${esc(a.alert_id.split(".").slice(-2).join("."))}</span></td>
        <td><span class="sev"><span class="dot ${s.cls ?? ""}" style="${s.style ?? ""}">!</span>${s.label}</span></td>
        <td>${br(a.onset)} → ${br(a.expires)}</td>
        <td class="num">${n}/${B.meta.members.length}</td>
        <td><button type="button" class="link-btn" data-a="${i}">verificar</button>${a.explorer ? ` · <a href="${a.explorer}" target="_blank" rel="noopener">tx</a>` : ""}</td></tr>`;
    })
    .join("");
  $("#live-alerts").innerHTML =
    `<table class="data"><thead><tr><th>Gravado on-chain</th><th>Aviso</th><th>Severidade</th><th>Vigência</th><th class="num">Nós</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
  $("#live-alerts").querySelectorAll("[data-a]").forEach((b) => b.addEventListener("click", () => verifyPublicAlert(L.public_alerts[+b.dataset.a])));
}

async function verifyPublicAlert(a) {
  panel("", `<h3>Verificando o aviso ${esc(a.alert_id)}…</h3>`, "#live-verify");
  try {
    const text = await (await fetch(`data/public_alerts/${a.content_sha256}.txt`)).text();
    const local = await sha256Hex(text);
    const acc = await fetchAccount(B.meta.rpc, a.record);
    const r = decodePublicAlert(acc.data);
    const checks = [
      ["A conta pertence ao programa do Cota", acc.owner === B.meta.program],
      [`SHA-256 do conteúdo guardado (${nf0.format(text.length)} caracteres), calculado no navegador = hash on-chain`, local === r.content_sha256],
      ["Código, severidade e vigência conferem", r.alert_id === a.alert_id && r.severity === a.severity && r.onset === a.onset && r.expires === a.expires],
      [`Atestado por ${r.signer_count} de ${B.meta.members.length} nós (limiar ${B.meta.threshold})`, r.signer_count >= B.meta.threshold],
    ];
    const ok = checks.every(([, v]) => v);
    panel(
      ok ? "ok" : "fail",
      `<h3>${esc(a.description)}: conferido contra a devnet (slot atual ${nf0.format(acc.slot)})</h3><ol>${checks
        .map(([t, v]) => `<li>${v ? '<span class="good-text">✓</span>' : '<span class="crit-text">✕</span>'} ${t}</li>`)
        .join("")}</ol>
      <p class="sub">Hash <span class="hash">${r.content_sha256}</span></p>
      <div class="verdict ${ok ? "good-text" : "crit-text"}">${
        ok ? `✓ Esta versão do aviso existia, no máximo, no slot ${nf0.format(r.slot)} (${br(r.committed_at)}).` : "✕ Divergência encontrada."
      }</div>
      <details><summary>Conteúdo guardado</summary><pre class="hash" style="white-space:pre-wrap">${esc(text.slice(0, 4000))}${text.length > 4000 ? "…" : ""}</pre></details>`,
      "#live-verify",
    );
  } catch (e) {
    panel("fail", `<h3>Não foi possível verificar</h3><p>${esc(e.message)}</p>`, "#live-verify");
  }
}

// ---- tema e inicialização -------------------------------------------------

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

async function main() {
  initTheme();
  B = await (await fetch("data/bundle.json")).json();
  derive();
  const prog = $("#program-link");
  prog.textContent = short(B.meta.program, 8);
  prog.href = explorerAddr(B.meta.program);
  $("#cluster").textContent = B.meta.cluster;
  $("#generated").textContent = `Dados gerados em ${utc(Date.parse(B.meta.generated_at) / 1000)}.`;
  // Começa no primeiro cruzamento do limiar: o momento mais importante do replay.
  state.sel = B.dayIndex.get(B.crossings[0]?.day) ?? 0;
  if (state.sel > 0) state.sel -= 2; // dois dias antes: quando o primeiro alerta saiu

  renderLive();
  renderKpis();
  renderRain();
  renderQuality();
  renderMetrics();
  selectDay(state.sel);

  document.querySelectorAll(".seg button").forEach((b) =>
    b.addEventListener("click", () => {
      state.h = +b.dataset.h;
      document.querySelectorAll(".seg button").forEach((x) => x.setAttribute("aria-checked", String(x === b)));
      renderFlow();
    }),
  );
  $("#flow-table-toggle").addEventListener("click", (e) => {
    const t = $("#flow-table");
    t.hidden = !t.hidden;
    e.target.setAttribute("aria-expanded", String(!t.hidden));
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
      renderFlow();
      renderRain();
      renderLiveChart();
    }, 150);
  });
}

main().catch((e) => {
  document.querySelector("main").insertAdjacentHTML("afterbegin", `<p class="crit-text">Erro ao carregar os dados: ${esc(e.message)}</p>`);
});
