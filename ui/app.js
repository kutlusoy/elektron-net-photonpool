'use strict';

/* ---------- data sources ---------- */

function demoSource() {
  const r = rng(20261103);
  const now = () => Date.now() / 1000;
  const miners = [];
  for (let i = 0; i < 1000; i++) {
    const k = r();
    const th = k < 0.6 ? 20 + r() * 60 : k < 0.9 ? 80 + r() * 120 : 200 + r() * 100;
    miners.push({ name: 'rig-' + String(i + 1).padStart(4, '0'), proto: r() < 0.81 ? 'V1' : r() < 0.8 ? 'V2' : 'EDGE', th, ok: Math.floor(r() * 4000), stale: 0, dup: 0, inv: 0, seen: 0, unit: '' });
  }
  const st = {
    start: now() - 3 * 86400 - 4 * 3600,
    height: 132457, tip: hex(r, 64), tipTime: now() - 23,
    hist: Array.from({ length: 60 }, () => 0),
    lat: Array.from({ length: 60 }, () => 28 + r() * 30), ev: [], blocks: [], batchMs: 118, candidates: 430, found: 0, overlap: 0,
  };
  for (let i = 0; i < 8; i++) st.blocks.push({ height: st.height - 41 - i * 37, hash: hex(r, 64), time: now() - 3600 * (i * 2 + 1), worker: miners[Math.floor(r() * 1000)].name, status: i < 2 ? 'maturing' : 'mature' });
  const total = () => miners.reduce((a, m) => a + m.th, 0) * 1e12;
  const push = (lvl, msg) => st.ev.push({ t: now(), lvl, msg });
  push('info', 'photond 0.1.0 demo engine started (no pool API detected)');
  push('info', 'node link: ipc ok, waitTipChanged armed');
  let tick = 0;
  function step() {
    tick++;
    const hr = total() * (0.985 + r() * 0.03);
    st.hist.push(hr); st.hist.shift();
    const tipAge = now() - st.tipTime;
    if (tipAge > 55 + r() * 12) {
      st.height++; st.tip = hex(r, 64); st.tipTime = now();
      const l = 30 + r() * 25; st.lat.push(l); if (st.lat.length > 400) st.lat.shift();
      st.batchMs = 95 + r() * 50;
      push('info', `tip ${st.height} ${short(st.tip)} | batch ${st.candidates} cand ${st.batchMs.toFixed(0)}ms | fan-out 1000 conns ${l.toFixed(0)}ms`);
      push('job', `clean_jobs sent to ${num(miners.length)} miners`);
      if (r() < 0.14) {
        const w = miners[Math.floor(r() * 1000)];
        st.found++;
        st.blocks.unshift({ height: st.height, hash: st.tip, time: now(), worker: w.name, status: 'found' });
        push('blk', `BLOCK FOUND ${st.height} by ${w.name} -> submitblock: accepted`);
      }
    }
    for (const m of miners) {
      const rate = m.th / 100 * 0.1;
      const n = r() < rate ? 1 : 0;
      if (n) { const x = r(); if (x < 0.992) m.ok++; else if (x < 0.997) m.stale++; else if (x < 0.999) m.dup++; else m.inv++; }
      m.seen = r() < 0.995 ? 0 : Math.floor(r() * 9);
      m.unit = `c${String(Math.floor(r() * st.candidates)).padStart(3, '0')}/t${String(Math.floor(r() * 60)).padStart(2, '0')}/s${String(Math.floor(r() * 16)).padStart(2, '0')}`;
    }
    if (tick % 2 === 0) {
      const m = miners[Math.floor(r() * 1000)];
      push('share', `share ok ${m.name} ${m.proto} d=${(m.th * 1e12 * 10 / 4294967296).toExponential(2)} unit ${m.unit}`);
    }
    if (tick % 17 === 0) { const m = miners[Math.floor(r() * 1000)]; push('warn', `stale share ${m.name} (job from previous tip, ${(r() * 300).toFixed(0)}ms late)`); }
    if (tick % 41 === 0) push('job', `unit refresh ${Math.floor(total() / 17592186044416).toLocaleString()} units/s allocated, overlap 0`);
  }
  for (let i = 0; i < 60; i++) { st.tipTime = now() - 5; step(); }
  let evCursor = 0;
  return {
    demo: true,
    tick() { step(); },
    summary() {
      const hr = st.hist[st.hist.length - 1];
      const sum = miners.reduce((a, m) => { a.ok += m.ok; a.stale += m.stale; a.dup += m.dup; a.inv += m.inv; return a; }, { ok: 0, stale: 0, dup: 0, inv: 0 });
      const tot = sum.ok + sum.stale + sum.dup + sum.inv;
      const sorted = [...st.lat].sort((a, b) => a - b);
      const q = (p) => (sorted.length ? sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))] : 0);
      const units = hr / 17592186044416;
      return {
        pool: { name: 'photon', version: '0.1.0-demo', uptime: now() - st.start, mode: 'pplns', explorerUrl: '' },
        node: { height: st.height, tip: st.tip, tipTime: st.tipTime, connected: true, transport: 'ipc', rttMs: 0.4, p50: q(0.5), p95: q(0.95), samples: st.lat.length, batchMs: st.batchMs },
        hashrate: { now: hr, m1: st.hist.slice(-12).reduce((a, b) => a + b, 0) / 12, h1: st.hist.reduce((a, b) => a + b, 0) / st.hist.length, hist: st.hist },
        miners: { online: miners.filter((m) => m.seen === 0).length, total: miners.length, v1: miners.filter((m) => m.proto === 'V1').length, v2: miners.filter((m) => m.proto === 'V2').length, edge: miners.filter((m) => m.proto === 'EDGE').length },
        shares: { perSec: miners.reduce((a, m) => a + m.th / 100 * 0.1, 0), ok: sum.ok / tot, stale: sum.stale / tot, dup: sum.dup / tot, inv: sum.inv / tot, total: tot },
        alloc: { candidates: st.candidates, unitsPerSec: units, epochUse: Math.min(0.97, units * 60 / (st.candidates * 60 * 16)), overlap: st.overlap },
        found: st.found,
      };
    },
    miners() {
      return miners.map((m) => ({ name: m.name, proto: m.proto, hashrate: m.th * 1e12, diff: m.th * 1e12 * 10 / 4294967296, ok: m.ok, stale: m.stale, dup: m.dup + m.inv, seen: m.seen, unit: m.unit }));
    },
    blocks(n) { return st.blocks.slice(0, n || 12); },
    events() { const out = st.ev.slice(evCursor); evCursor = st.ev.length; return out; },
  };
}

function liveSource(base) {
  const get = async (p) => { const r = await fetch(base + p, { cache: 'no-store' }); if (!r.ok) throw new Error(r.status); return r.json(); };
  let cache = { summary: null, miners: [], blocks: [] }, queue = [];
  const poll = async () => {
    try {
      const [s, m, b] = await Promise.all([get('/summary'), get('/miners?limit=1000'), get('/blocks?limit=12')]);
      cache = { summary: s, miners: m, blocks: b };
    } catch (e) { queue.push({ t: Date.now() / 1000, lvl: 'err', msg: 'api poll failed: ' + e.message }); }
  };
  const stream = () => { try { const es = new EventSource(base + '/events'); es.onmessage = (e) => { try { queue.push(JSON.parse(e.data)); } catch (_) { /* ignore malformed event */ } }; } catch (_) { /* no SSE */ } };
  return {
    demo: false,
    async init() { await poll(); if (cache.summary) stream(); },
    tick() { poll(); },
    summary() { return cache.summary; },
    miners() { return cache.miners; },
    blocks() { return cache.blocks; },
    events() { const o = queue; queue = []; return o; },
  };
}

/* ---------- ui ---------- */

const ui = { src: null, follow: true, minerSort: 'hashrate', minerFilter: '', minerLimit: 40, tty: $('#tty'), log: [] };

function line(html) {
  const d = document.createElement('div');
  d.innerHTML = html;
  ui.tty.appendChild(d);
  while (ui.tty.childElementCount > 600) ui.tty.firstChild.remove();
  if (ui.follow) ui.tty.scrollTop = ui.tty.scrollHeight;
}
function logEvent(e) {
  const t = new Date(e.t * 1000).toTimeString().slice(0, 8);
  const lv = (e.lvl || 'info').toLowerCase();
  line(`<span class="t">${t}</span> <span class="lv lv-${lv}">${esc(lv.toUpperCase().padEnd(5))}</span>${esc(e.msg)}`);
  ui.log.push(e); if (ui.log.length > 600) ui.log.shift();
}
const out = (html) => line(html);

document.addEventListener('depthchange', () => { try { render(); } catch (_) { /* no data yet */ } });
function render() {
  const s = ui.src.summary();
  if (!s) return;
  const age = Date.now() / 1000 - s.node.tipTime;
  const efficiency = s.shares.total ? s.shares.ok : null;
  $('#statusline').innerHTML = [
    `<span>net <b>ELEKTRON</b></span>`,
    `<span>height <b>${num(s.node.height)}</b></span>`,
    `<span>pool <b>${fmtHash(s.hashrate.now)}</b></span>`,
    `<span>miners <b>${num(s.miners.online)}</b>/${num(s.miners.total)}</span>`,
    `<span class="${efficiency === null || efficiency > 0.985 ? 'ok' : 'warn'}">valid <b>${efficiency === null ? 'n/a' : pct(efficiency)}</b></span>`,
    `<span class="${s.node.connected ? 'ok' : 'warn'}">node <b>${s.node.connected ? 'UP' : 'DOWN'}</b> ${esc(s.node.transport)}</span>`,
    `<span>tip age <b>${hms(age)}</b></span>`,
  ].join('');
  $('#pane-pool').innerHTML = [
    `${span('dim', 'hashrate  ')}${span('hl', fmtHash(s.hashrate.now))}  ${span('c2', spark(s.hashrate.hist, 28))}`,
    `${span('dim', 'avg 1m    ')}${fmtHash(s.hashrate.m1)}`,
    `${span('dim', 'avg 1h    ')}${fmtHash(s.hashrate.h1)}`,
    `${span('dim', 'miners    ')}${num(s.miners.online)} online  ${span('dim', 'v1')} ${num(s.miners.v1)} ${span('dim', '| v2')} ${num(s.miners.v2)} ${span('dim', '| edge')} ${num(s.miners.edge)}`,
    `${span('dim', 'shares/s  ')}${s.shares.perSec.toFixed(1)}  ${span('ok', 'ok ' + pct(s.shares.ok))} ${span('warn', 'stale ' + pct(s.shares.stale))} ${span('dim', 'dup ' + pct(s.shares.dup))} ${span('err', 'inv ' + pct(s.shares.inv))}`,
    `${span('dim', 'scheme    ')}${esc(s.pool.mode)}   ${span('dim', 'uptime')} ${dhms(s.pool.uptime)}`,
  ].join('\n');
  $('#pane-tip').innerHTML = [
    `${span('dim', 'height    ')}${span('hl', num(s.node.height))}`,
    `${span('dim', 'hash      ')}${link(blockUrl(s.pool.explorerUrl, s.node.tip), wide('tip') ? s.node.tip : short(s.node.tip, 10, 8), 'c1')}`,
    `${span('dim', 'age       ')}${hms(age)} ${bar(age / 60, 14, age > 75 ? 'warn' : 'c1')}`,
    `${span('dim', 'node link ')}${s.node.connected ? span('ok', 'connected') : span('err', 'DOWN')} ${esc(s.node.transport)} rtt ${s.node.rttMs.toFixed(1)}ms`,
    `${span('dim', 'tip>job   ')}p50 ${s.node.p50.toFixed(0)}ms  p95 ${s.node.p95.toFixed(0)}ms  ${span('dim', 'n=' + s.node.samples)}`,
    `${span('dim', 'backend   ')}${esc(s.node.backend || 'gbt')}  ${span('dim', 'found')} ${s.found}`,
  ].join('\n');
  $('#pane-alloc').innerHTML = [
    `${span('dim', 'candidates ')}${num(s.alloc.candidates)} ${span('dim', 'be1q payout scripts')}`,
    `${span('dim', 'units/s    ')}${num(s.alloc.unitsPerSec)}`,
    `${span('dim', 'epoch use  ')}${bar(s.alloc.epochUse, 14, s.alloc.epochUse > 0.9 ? 'warn' : 'c2')} ${pct(s.alloc.epochUse)}`,
    `${span('dim', 'overlap    ')}${s.alloc.overlap ? span('err', s.alloc.overlap + ' flagged') : span('ok', '0 flagged')}`,
    `${span('dim', 'batch rpc  ')}${s.node.batchMs.toFixed(0)} ms ${span('dim', 'last call')}`,
    `${span('dim', 'unit       ')}${span('dim', 'candidate / ntime / slice')}`,
  ].join('\n');
  renderMiners();
  const bw = wide('blocks');
  const blocks = bw ? ui.src.blocks(50) : ui.src.blocks();
  const bwk = colw(blocks.map((b) => b.worker), 6), bhk = colw(blocks.map((b) => b.hash || ''), 4);
  $('#pane-blocks').innerHTML = [span('row-h', pad('HEIGHT', 9) + pad('AGE', 9) + fit('WORKER', bw ? bwk : 19, bw) + (bw ? pad('HASH', bhk) : '') + 'STATE')]
    .concat(blocks.map((b) => {
      const a = Date.now() / 1000 - b.time;
      const cls = b.status === 'found' ? 'ok' : b.status === 'maturing' ? 'c3' : 'dim';
      return `${linkPad(b.url || blockUrl(s.pool.explorerUrl, b.hash), num(b.height), 9)}${pad(a < 3600 ? Math.floor(a / 60) + 'm' : (a / 3600).toFixed(1) + 'h', 9)}${bw ? pad(b.worker, bwk) : pad(trunc(workerLabel(b.worker), 18), 19)}${bw ? pad(b.hash || '', bhk) : ''}${span(cls, b.status)}`;
    })).join('\n');
}

function minerRows() {
  let rows = ui.src.miners();
  if (ui.minerFilter) rows = rows.filter((m) => (m.name + ' ' + m.proto).toLowerCase().includes(ui.minerFilter));
  const key = { hashrate: (m) => -m.hashrate, name: (m) => m.name, shares: (m) => -m.ok, stale: (m) => -m.stale }[ui.minerSort];
  return [...rows].sort((a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0));
}
function renderMiners() {
  const rows = minerRows();
  const mw = wide('miners');
  const shown = rows.slice(0, mw ? Math.max(ui.minerLimit, 200) : ui.minerLimit);
  const mwk = colw(shown.map((m) => m.name), 6);
  $('#miners-hint').textContent = `${shown.length}/${rows.length} sort:${ui.minerSort}${ui.minerFilter ? ' grep:' + ui.minerFilter : ''}`;
  $('#pane-miners').innerHTML = [span('row-h', fit('WORKER', mw ? mwk : 19, mw) + pad('PROTO', 6) + lpad('HASHRATE', 12) + lpad('DIFF', 10) + lpad('OK', 8) + lpad('STALE', 7) + lpad('DUP', 6) + '  ' + pad('LAST', 6) + 'UNIT')]
    .concat(shown.map((m) => {
      const stale = m.stale / Math.max(1, m.ok + m.stale);
      const seen = m.seen === 0 ? span('ok', 'now   ') : span(m.seen > 5 ? 'warn' : 'dim', pad(m.seen + 's', 6));
      return `${mw ? pad(m.name, mwk) : pad(trunc(workerLabel(m.name), 18), 19)}${span(m.proto === 'V2' ? 'c2' : m.proto === 'EDGE' ? 'c3' : 'dim', pad(m.proto, 6))}${lpad(fmtHash(m.hashrate), 12)}${lpad(m.diff.toExponential(1), 10)}${lpad(num(m.ok), 8)}${span(stale > 0.01 ? 'warn' : 'dim', lpad(num(m.stale), 7))}${lpad(num(m.dup), 6)}  ${seen}${span('dim', m.unit)}`;
    })).join('\n');
}

/* ---------- console ---------- */

const cmds = {
  help() {
    out(span('c1', 'commands'));
    [['status', 'one-screen summary'], ['tip', 'chain tip and latency'], ['miners [n] [sort k] [grep s] [reset]', 'worker table, k = hashrate|name|shares|stale'],
      ['blocks', 'recent blocks'], ['alloc', 'candidate and unit allocator'], ['log [n]', 'replay last n events'], ['watch', 'toggle log follow'],
      ['theme cyan|amber|green|magenta', 'colour scheme'], ['clear', 'clear console (Ctrl+L)'], ['about', 'what this is']]
      .forEach(([c, d]) => out(`  ${span('c3', pad(c, 38))}${span('dim', d)}`));
  },
  status() {
    const s = ui.src.summary();
    out(`${span('c1', 'pool')}    ${fmtHash(s.hashrate.now)}  ${num(s.miners.online)}/${num(s.miners.total)} miners  valid ${pct(s.shares.ok)}`);
    out(`${span('c1', 'chain')}   height ${num(s.node.height)}  tip ${short(s.node.tip)}  ${s.node.connected ? span('ok', 'node up') : span('err', 'node down')}`);
    out(`${span('c1', 'alloc')}   ${num(s.alloc.candidates)} candidates  ${num(s.alloc.unitsPerSec)} units/s  overlap ${s.alloc.overlap}`);
  },
  tip() {
    const s = ui.src.summary();
    out(`height ${span('hl', num(s.node.height))}   hash ${link(blockUrl(s.pool.explorerUrl, s.node.tip), s.node.tip, 'c1')}`);
    out(`tip>job p50 ${s.node.p50.toFixed(0)}ms p95 ${s.node.p95.toFixed(0)}ms (n=${s.node.samples})   batch rpc ${s.node.batchMs.toFixed(0)}ms`);
  },
  miners(a) {
    for (let i = 0; i < a.length; i++) {
      if (/^\d+$/.test(a[i])) ui.minerLimit = Math.min(1000, +a[i]);
      else if (a[i] === 'sort' && a[i + 1]) { ui.minerSort = ['hashrate', 'name', 'shares', 'stale'].includes(a[i + 1]) ? a[i + 1] : ui.minerSort; i++; }
      else if (a[i] === 'grep') { ui.minerFilter = (a[i + 1] || '').toLowerCase(); i++; }
      else if (a[i] === 'reset') { ui.minerFilter = ''; ui.minerSort = 'hashrate'; ui.minerLimit = 40; }
    }
    renderMiners();
    out(span('dim', `miners pane: ${ui.minerLimit} rows, sort ${ui.minerSort}${ui.minerFilter ? ', grep ' + ui.minerFilter : ''}`));
  },
  blocks() {
    const base = ui.src.summary().pool.explorerUrl;
    ui.src.blocks().slice(0, 8).forEach((b) => out(`${pad(num(b.height), 9)}${link(b.url || blockUrl(base, b.hash), short(b.hash, 12, 8), 'c1')}  ${trunc(workerLabel(b.worker), 24)} ${b.status}`));
  },
  alloc() {
    const s = ui.src.summary();
    out(`candidates ${num(s.alloc.candidates)}   units/s ${num(s.alloc.unitsPerSec)}   epoch use ${pct(s.alloc.epochUse)}   overlap ${s.alloc.overlap}`);
    out(span('dim', 'unit = (candidate, nTime, pool slice); a unit is never handed to two miners within one tip epoch.'));
  },
  log(a) { ui.log.slice(-(+a[0] || 20)).forEach(logEvent); },
  watch() { ui.follow = !ui.follow; out(span('dim', 'follow ' + (ui.follow ? 'on' : 'off'))); },
  theme(a) {
    const t = a[0];
    if (!['cyan', 'amber', 'green', 'magenta'].includes(t)) return out(span('warn', 'usage: theme cyan|amber|green|magenta'));
    document.body.dataset.theme = t;
    try { localStorage.setItem('photon-theme', t); } catch (_) { /* storage unavailable */ }
  },
  clear() { ui.tty.innerHTML = ''; },
  about() {
    out(`${span('c1', 'PHOTON')} pool operator terminal for Elektron Net`);
    out(span('dim', ui.src.demo ? 'running on simulated demo data (no pool API found at /api/v1)' : 'connected to the pool admin API at /api/v1'));
  },
};
cmds.ls = cmds.help; cmds.top = cmds.status;

const hist = []; let hi = 0;
const input = $('#cmd');
$('#prompt').addEventListener('submit', (e) => {
  e.preventDefault();
  const raw = input.value.trim();
  input.value = '';
  if (!raw) return;
  hist.push(raw); hi = hist.length;
  out(`${span('u', 'photon')}${span('at', '@')}${span('h', 'elektron')}${span('sep', ':~$')} ${esc(raw)}`);
  const [c, ...a] = raw.split(/\s+/);
  if (cmds[c]) { try { cmds[c](a); } catch (err) { out(span('err', 'error: ' + err.message)); } }
  else if (c === 'sudo') out(span('err', 'photon is not in the sudoers file. this incident will be reported.'));
  else out(span('warn', `unknown command: ${c}. try help`));
});
input.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowUp') { if (hi > 0) input.value = hist[--hi]; e.preventDefault(); }
  else if (e.key === 'ArrowDown') { input.value = hi < hist.length - 1 ? hist[++hi] : ((hi = hist.length), ''); e.preventDefault(); }
  else if (e.key === 'Tab') {
    e.preventDefault();
    const m = Object.keys(cmds).filter((c) => c.startsWith(input.value));
    if (m.length === 1) input.value = m[0] + ' ';
  } else if (e.key === 'l' && e.ctrlKey) { e.preventDefault(); cmds.clear(); }
  else if (e.key === 'c' && e.ctrlKey) { input.value = ''; }
});
document.addEventListener('keydown', (e) => { if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key.length === 1 && document.activeElement !== input) input.focus(); });

/* ---------- boot ---------- */

async function boot() {
  try { const t = localStorage.getItem('photon-theme'); if (t) document.body.dataset.theme = t; } catch (_) { /* storage unavailable */ }
  const forceDemo = new URLSearchParams(location.search).has('demo');
  let src = null;
  if (!forceDemo && location.protocol !== 'file:') {
    try { const l = liveSource('/api/v1'); await Promise.race([l.init(), new Promise((_, rej) => setTimeout(() => rej(new Error('timeout')), 1500))]); if (l.summary()) src = l; } catch (_) { /* fall back to demo */ }
  }
  ui.src = src || demoSource();
  const tag = $('#mode-tag');
  tag.textContent = ui.src.demo ? 'DEMO DATA' : 'LIVE';
  tag.className = 'tag ' + (ui.src.demo ? 'demo' : 'live');
  [
    ['info', 'PHOTON pool terminal'],
    ['info', ui.src.demo ? 'no pool API at /api/v1, showing simulated data (clearly marked DEMO DATA)' : 'attached to /api/v1'],
    ['info', 'type help for commands, Tab completes, Up/Down recalls history'],
  ].forEach(([lvl, msg]) => logEvent({ t: Date.now() / 1000, lvl, msg }));
  ui.src.events().forEach(logEvent);
  render();
  setInterval(() => { ui.src.tick(); ui.src.events().forEach(logEvent); render(); }, 1000);
  setInterval(() => { $('#clock').textContent = new Date().toISOString().slice(11, 19) + ' UTC'; }, 500);
  if (matchMedia('(pointer: fine)').matches) input.focus();
}
boot();
