import {
  NETWORK,
  accountUrl,
  call,
  createGuest,
  createSubGuest,
  forgetGuest,
  loadGuest,
  nearBalance,
  txUrl,
  view,
} from "./near.js";

const params = new URLSearchParams(location.search);
const CONTRACT = params.get("contract") || "staghunt.testnet";
const STAG = 10n ** 18n;
const STORAGE_DEPOSIT = "10000000000000000000000"; // 0.01 NEAR, the unused part is refunded

const $ = (id) => document.getElementById(id);
const HELPERS_KEY = "staghunt.helpers.v1";
let guest = loadGuest();
let status = null;
let config = null;
let me = null;
let clockSkewMs = 0;
let busy = false;

// ---------- formatting ----------
function fmt(base, digits = 2) {
  const v = BigInt(base);
  const neg = v < 0n;
  const abs = neg ? -v : v;
  const whole = abs / STAG;
  const frac = abs % STAG;
  let s = whole.toLocaleString("en-US");
  if (digits > 0) {
    const f = (frac * 10n ** BigInt(digits)) / STAG;
    s += "." + f.toString().padStart(digits, "0");
  }
  return (neg ? "-" : "") + s;
}
const pct = (bps) => `${(bps / 100).toFixed(bps % 100 ? 1 : 0)}%`;
function parseStag(text) {
  const m = String(text).trim().match(/^(\d+)(?:\.(\d{0,18}))?$/);
  if (!m) return null;
  return BigInt(m[1]) * STAG + BigInt((m[2] || "").padEnd(18, "0") || "0");
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

// ---------- helper hunters ----------
// A solo visitor cannot win: a hunt needs several accounts. "Helpers" are two
// sub-accounts of your guest account, created and signed for in this browser, that
// join with you, standing in for other community members. Everything they do is a
// normal signed transaction.
function loadHelpers() {
  try {
    return JSON.parse(localStorage.getItem(HELPERS_KEY) || "[]");
  } catch {
    return [];
  }
}
function saveHelpers(list) {
  try {
    localStorage.setItem(HELPERS_KEY, JSON.stringify(list));
  } catch {
    /* ignore */
  }
}
async function huntWithHelpers() {
  // helpers are sub-accounts of your guest account, funded with 0.3 testnet NEAR each
  let helpers = loadHelpers().filter((h) => h.accountId.endsWith(`.${guest.accountId}`));
  const round = status.round;
  while (helpers.length < 2) {
    toast(`Creating helper account ${helpers.length + 1} of 2…`);
    helpers.push(await createSubGuest(guest, `helper${helpers.length + 1}`, "300000000000000000000000"));
    saveHelpers(helpers);
  }
  const links = [];
  for (const [i, h] of helpers.entries()) {
    let info = await view(CONTRACT, "get_hunter", { account_id: h.accountId });
    if (!info.faucet_claimed) {
      toast(`Helper ${i + 1}: claiming starter STAG…`);
      await call(h, CONTRACT, "claim_starter", {}, { deposit: STORAGE_DEPOSIT, gasTgas: 50 });
      info = await view(CONTRACT, "get_hunter", { account_id: h.accountId });
    }
    const bal = BigInt(info.balance);
    if (bal === 0n) continue;
    toast(`Helper ${i + 1}: joining hunt #${round} with ${fmt(bal)} STAG…`);
    const r = await call(h, CONTRACT, "join_hunt", { amount: bal.toString() }, { deposit: "1", gasTgas: 50 });
    links.push(`<a href="${txUrl(r.txHash)}" target="_blank" rel="noopener">helper ${i + 1}</a>`);
  }
  return links;
}

async function onHelpers() {
  try {
    const links = await huntWithHelpers();
    toast(`Helpers joined: ${links.join(", ")}. Watch the countdown.`, "ok");
  } catch (e) {
    toast(`Helpers failed: ${esc(e.message)}`, "err");
  }
  await refresh();
}

// ---------- rendering ----------
function renderStatus() {
  if (!status) return;
  const pot = BigInt(status.pot.toString());
  const target = BigInt(status.target_now.toString());
  const ratio = target === 0n ? 0 : Number((pot * 1000n) / target) / 10;
  $("round").textContent = `Hunt #${status.round}`;
  $("pot").textContent = fmt(pot);
  $("target").textContent = fmt(target);
  $("target-pct").textContent = pct(status.target_bps);
  $("hunters").textContent = `${status.hunters} / ${status.min_hunters}`;
  $("bar").style.width = `${Math.min(100, ratio)}%`;
  $("bar").classList.toggle("ok", status.outlook === "on_track");
  $("outlook").textContent =
    status.outlook === "on_track"
      ? "If the hunt closed now it would succeed: every hunter +" + pct(config.reward_bps)
      : status.hunters < status.min_hunters && pot >= target && pot > 0n
        ? `Pot is big enough, but at least ${status.min_hunters} hunters are needed`
        : `Short by ${fmt(target > pot ? target - pot : 0n)} STAG. If it closes like this, hunters lose ${pct(config.burn_bps)}`;
  $("supply").textContent = fmt(status.supply);
  $("minted").textContent = fmt(status.total_minted);
  $("burned").textContent = fmt(status.total_burned);
  $("streak").textContent = `${status.streak} (best ${status.best_streak})`;
  $("record").textContent = `${status.rounds_won} won · ${status.rounds_lost} lost · ${status.rounds_empty} empty`;
  $("contract-link").href = accountUrl(CONTRACT);
  $("contract-link").textContent = CONTRACT;
}

function renderClock() {
  if (!status) return;
  const left = Math.max(0, status.round_ends_at_ms - (Date.now() + clockSkewMs));
  const s = Math.ceil(left / 1000);
  $("clock").textContent = `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  if (left === 0 && !busy) refresh();
}

function renderMe() {
  const box = $("me");
  if (!guest) {
    box.innerHTML = `
      <p>Play with a throwaway <b>testnet</b> account. The page asks the public testnet faucet for one (free, about 5 seconds); its key stays in this browser.</p>
      <button id="btn-guest" class="primary">Create a testnet account</button>`;
    $("btn-guest").onclick = onCreateGuest;
    return;
  }
  const h = me;
  const bal = h ? fmt(h.balance) : "…";
  let state = "Not hunting this round.";
  if (h && h.status === "hunting") state = `Hunting with <b>${fmt(h.stake)}</b> STAG in hunt #${h.stake_round}. If it closed now you would get back <b>${fmt(h.returns)}</b>.`;
  if (h && h.status === "won") state = `Hunt #${h.stake_round} succeeded: <b>${fmt(h.stake)}</b> came back as <b>${fmt(h.returns)}</b> (already in your balance).`;
  if (h && h.status === "lost") state = `Hunt #${h.stake_round} failed: <b>${fmt(h.stake)}</b> came back as <b>${fmt(h.returns)}</b> (already in your balance).`;
  box.innerHTML = `
    <div class="row"><span class="muted">Account</span><a href="${accountUrl(guest.accountId)}" target="_blank" rel="noopener">${esc(guest.accountId)}</a></div>
    <div class="row"><span class="muted">Balance</span><b class="big">${bal} STAG</b></div>
    <p class="state">${state}</p>
    <div class="actions">
      ${h && !h.faucet_claimed ? `<button id="btn-faucet" class="primary">Get ${fmt(config.faucet_amount, 0)} starter STAG</button>` : ""}
      <div class="join">
        <input id="amount" inputmode="decimal" placeholder="amount" value="${h && BigInt(h.balance) > 0n ? fmt(BigInt(h.balance) / 2n, 0) : ""}" />
        <button id="btn-join" class="primary" ${h && BigInt(h.balance) > 0n ? "" : "disabled"}>Join hunt #${status ? status.round : ""}</button>
      </div>
    </div>
    ${h && h.status === "hunting" && status && status.hunters < status.min_hunters ? `<div class="helpers"><p class="tiny">Alone you cannot win: a hunt needs ${status.min_hunters} hunters. Add two helper accounts (sub-accounts of yours, created in this browser) standing in for other holders.</p><button id="btn-helpers">Hunt with 2 helper accounts</button></div>` : ""}
    <p class="tiny"><a href="#" id="btn-forget">Forget this guest account</a></p>`;
  if ($("btn-helpers")) $("btn-helpers").onclick = onHelpers;
  if ($("btn-faucet")) $("btn-faucet").onclick = onFaucet;
  $("btn-join").onclick = onJoin;
  $("btn-forget").onclick = (e) => {
    e.preventDefault();
    if (confirm("Forget this guest account? Its key is only stored in this browser.")) {
      forgetGuest();
      guest = null;
      me = null;
      renderMe();
    }
  };
}

function renderRounds(rounds) {
  const body = $("rounds");
  if (!rounds.length) {
    body.innerHTML = `<tr><td colspan="6" class="muted">No hunt has closed with hunters yet.</td></tr>`;
    return;
  }
  body.innerHTML = rounds
    .map(
      (r) => `<tr>
        <td>#${r.round}</td>
        <td>${new Date(r.ended_at_ms).toLocaleString()}</td>
        <td>${fmt(r.pot)} / ${fmt(r.target)} <span class="muted">(${pct(r.target_bps)})</span></td>
        <td>${r.hunters}</td>
        <td class="${r.won ? "win" : "loss"}">${r.won ? "succeeded" : "failed"}</td>
        <td>${r.won ? "+" + fmt(r.minted) + " minted" : "−" + fmt(r.burned) + " burned"}</td>
      </tr>`,
    )
    .join("");
  renderChart(rounds);
}

function renderChart(rounds) {
  const pts = [...rounds].reverse().map((r) => {
    const after = BigInt(r.supply_at_close) + (r.won ? BigInt(r.minted) : -BigInt(r.burned));
    return Number(after / 10n ** 15n) / 1000;
  });
  const svg = $("chart");
  if (pts.length < 2) {
    svg.innerHTML = "";
    return;
  }
  const w = 600, h = 120, pad = 6;
  const min = Math.min(...pts), max = Math.max(...pts);
  const span = max - min || 1;
  const xy = pts.map((v, i) => [pad + (i * (w - 2 * pad)) / (pts.length - 1), h - pad - ((v - min) / span) * (h - 2 * pad)]);
  svg.innerHTML = `<polyline fill="none" stroke="currentColor" stroke-width="2" points="${xy.map((p) => p.join(",")).join(" ")}"/>
    <text x="${pad}" y="12" class="axis">${max.toLocaleString()} STAG</text>
    <text x="${pad}" y="${h - 2}" class="axis">${min.toLocaleString()} STAG</text>`;
}

function toast(html, kind = "info") {
  const t = $("toast");
  t.innerHTML = html;
  t.className = `toast ${kind}`;
  t.hidden = false;
}

// ---------- data ----------
async function refresh() {
  if (busy) return;
  busy = true;
  try {
    const [s, rounds] = await Promise.all([
      view(CONTRACT, "get_status"),
      view(CONTRACT, "get_rounds", { limit: 30 }),
    ]);
    status = s;
    clockSkewMs = s.now_ms - Date.now();
    if (guest) me = await view(CONTRACT, "get_hunter", { account_id: guest.accountId });
    renderStatus();
    renderMe();
    renderRounds(rounds);
    $("error").hidden = true;
  } catch (e) {
    $("error").hidden = false;
    $("error").textContent = `Could not read ${CONTRACT} on testnet: ${e.message}`;
  } finally {
    busy = false;
  }
}

async function act(label, fn) {
  toast(`${label}…`);
  try {
    const { txHash } = await fn();
    toast(`${label}: done. <a href="${txUrl(txHash)}" target="_blank" rel="noopener">View transaction</a>`, "ok");
  } catch (e) {
    const link = e.txHash ? ` <a href="${txUrl(e.txHash)}" target="_blank" rel="noopener">transaction</a>` : "";
    toast(`${label} failed: ${esc(e.message)}${link}`, "err");
  }
  await refresh();
}

async function onCreateGuest() {
  toast("Asking the testnet faucet for an account…");
  try {
    guest = await createGuest("hunter");
    const near = await nearBalance(guest.accountId);
    toast(`Created ${esc(guest.accountId)} with ${(Number(near / 10n ** 21n) / 1000).toFixed(2)} testnet NEAR.`, "ok");
  } catch (e) {
    toast(`Faucet failed: ${esc(e.message)}. Try again in a minute.`, "err");
  }
  await refresh();
}

function onFaucet() {
  return act("Claim starter STAG", () =>
    call(guest, CONTRACT, "claim_starter", {}, { deposit: STORAGE_DEPOSIT, gasTgas: 50 }),
  );
}

function onJoin() {
  const amount = parseStag($("amount").value);
  if (amount === null || amount <= 0n) {
    toast("Enter an amount like 25 or 12.5", "err");
    return;
  }
  return act(`Join hunt #${status.round} with ${fmt(amount)} STAG`, () =>
    call(guest, CONTRACT, "join_hunt", { amount: amount.toString() }, { deposit: "1", gasTgas: 50 }),
  );
}

// ---------- boot ----------
async function boot() {
  $("network").textContent = "NEAR testnet";
  try {
    config = await view(CONTRACT, "get_config");
  } catch (e) {
    $("error").hidden = false;
    $("error").textContent = `Could not read ${CONTRACT} on testnet: ${e.message}`;
    return;
  }
  $("rule-target").textContent = pct(config.start_target_bps);
  $("rule-reward").textContent = pct(config.reward_bps);
  $("rule-burn").textContent = pct(config.burn_bps);
  $("rule-up").textContent = pct(config.target_step_up_bps);
  $("rule-down").textContent = pct(config.target_step_down_bps);
  $("rule-min").textContent = pct(config.min_target_bps);
  $("rule-max").textContent = pct(config.max_target_bps);
  $("rule-hunters").textContent = config.min_hunters;
  $("rule-round").textContent = config.round_secs >= 120 ? `${config.round_secs / 60} minutes` : `${config.round_secs} seconds`;
  $("config-json").textContent = JSON.stringify(config, null, 2);
  await refresh();
  setInterval(renderClock, 500);
  setInterval(refresh, 10000);
}

window.__staghunt = { CONTRACT, NETWORK };
boot();
