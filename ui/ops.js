window.Views = window.Views || {};
// Operator view (port 8081 only): worker table with full names, blocks, ledger and the admin console. Loaded by the shell when the admin API
// answers; the public port has no /api/v1, so the view never appears there.
Views.ops = (() => {
'use strict';
const timers = [], offs = [];
const on = (t, ev, fn) => { t.addEventListener(ev, fn); offs.push(() => t.removeEventListener(ev, fn)); };
const TEMPLATE = `<div class="statusline" id="statusline" role="status" aria-live="off"></div>
  <main class="grid ops">
    <section class="pane p-miners" aria-label="Miners"><h2><span>MINERS</span><em id="miners-hint"></em></h2><pre id="pane-miners"></pre></section>
    <section class="pane p-oblocks" aria-label="Blocks"><h2><span>BLOCKS</span></h2><pre id="pane-blocks"></pre></section>
    <section class="pane p-ledger" aria-label="Ledger"><h2><span>LEDGER</span></h2><pre id="pane-ledger"></pre></section>
    <section class="pane p-tty" aria-label="Console"><h2><span>TTY</span><em>type <kbd>help</kbd></em></h2>
      <div class="tty" id="tty" tabindex="-1"></div>
      <form class="prompt" id="prompt" autocomplete="off">
        <label for="cmd"><span class="u">photon</span><span class="at">@</span><span class="h">elektron</span><span class="sep">:~$</span></label>
        <input id="cmd" type="text" spellcheck="false" autocapitalize="off" autocomplete="off" autocorrect="off" aria-label="command line">
      </form>
    </section>
  </main>`;

/* ---------- data ---------- */

function liveSource(base) {
  const get = async (p) => { const r = await fetch(base + p, { cache: 'no-store' }); if (!r.ok) throw new Error(r.status); return r.json(); };
  let cache = { summary: null, miners: [], blocks: [], ledger: null }, queue = [], es = null, n = 0;
  const poll = async () => {
    try {
      const [s, m, b] = await Promise.all([get('/summary'), get('/miners?limit=1000'), get('/blocks?limit=50')]);
      cache.summary = s; cache.miners = m; cache.blocks = b;
      if (n++ % 5 === 0) cache.ledger = await get('/ledger').catch(() => cache.ledger);
    } catch (e) { queue.push({ t: Date.now() / 1000, lvl: 'err', msg: 'api poll failed: ' + e.message }); }
  };
  return {
    async init() { await poll(); if (cache.summary) { try { es = new EventSource(base + '/events'); es.onmessage = (e) => { try { queue.push(JSON.parse(e.data)); } catch (_) { /* ignore malformed event */ } }; } catch (_) { /* no live log */ } } },
    tick() { poll(); },
    close() { if (es) es.close(); },
    summary() { return cache.summary; },
    miners() { return cache.miners; },
    blocks(n) { return n ? cache.blocks.slice(0, n) : cache.blocks.slice(0, 12); },
    ledger() { return cache.ledger; },
    events() { const o = queue; queue = []; return o; },
  };
}

/* ---------- ui ---------- */

const ui = { src: null, follow: true, minerFilter: '', minerLimit: 40, tty: null, log: [] };
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
  line(`<span class="t">${t}</span> <span class="lv lv-${lv}">${esc(lv.toUpperCase().padEnd(5))}</span>${hashSpans(e.msg)}`);
  ui.log.push(e); if (ui.log.length > 600) ui.log.shift();
}
const out = (html) => line(html);

const onDepth = () => { try { render(); } catch (_) { /* no data yet */ } };
function render() {
  const s = ui.src.summary();
  if (!s) return;
  const age = Date.now() / 1000 - s.node.tipTime;
  const efficiency = s.shares.total ? s.shares.ok : null;
  $('#statusline').innerHTML = [
    `<span>height <b>${num(s.node.height)}</b></span>`,
    `<span>pool <b>${fmtHash(s.hashrate.now)}</b></span>`,
    `<span>miners <b>${num(s.miners.online)}</b>/${num(s.miners.total)}</span>`,
    `<span class="${efficiency === null || efficiency > 0.985 ? 'ok' : 'warn'}">valid <b>${efficiency === null ? 'n/a' : pct(efficiency)}</b> of ${num(s.shares.total)}</span>`,
    `<span class="${s.node.connected ? 'ok' : 'warn'}">node <b>${s.node.connected ? 'UP' : 'DOWN'}</b> ${esc(s.node.transport)}</span>`,
    `<span>tip age <b>${hms(age)}</b></span>`,
  ].join('');
  renderMiners();
  const bw = wide('oblocks');
  const blocks = sortRows('oblocks', bw ? ui.src.blocks(50) : ui.src.blocks(), { height: (b) => b.height, time: (b) => b.time, worker: (b) => b.worker, status: (b) => b.status }, 'height');
  const bwk = colw(blocks.map((b) => b.worker), 6), bhk = colw(blocks.map((b) => b.hash || ''), 4);
  $('#pane-blocks').innerHTML = [`<span class="row-h">${hdr('oblocks', [['height', 'HEIGHT', 9], ['time', 'AGE', 9], ['worker', 'WORKER', bw ? bwk : 19, false, 1]], 'height')}${bw ? pad('HASH', bhk) : ''}${hdr('oblocks', [['status', 'STATE', 8, false, 1]], 'height')}</span>`]
    .concat(blocks.map((b) => {
      const a = Date.now() / 1000 - b.time;
      const cls = b.status === 'found' ? 'ok' : b.status === 'maturing' ? 'c3' : 'dim';
      return `${linkPad(b.url || blockUrl(s.pool.explorerUrl, b.hash), num(b.height), 9)}${pad(a < 3600 ? Math.floor(a / 60) + 'm' : (a / 3600).toFixed(1) + 'h', 9)}${bw ? pad(b.worker, bwk) : pad(trunc(workerLabel(b.worker), 18), 19)}${bw ? pad(b.hash || '', bhk) : ''}${span(cls, b.status)}`;
    })).join('\n');
  renderLedger();
  Extras.tick();
}

function renderLedger() {
  const l = ui.src.ledger();
  if (!l) { $('#pane-ledger').textContent = 'no ledger data yet'; return; }
  const liab = Object.values(l.liabilities || {}).reduce((a, b) => a + (+b || 0), 0);
  const blocks = Object.entries(l.blocks || {}).map(([k, v]) => `${k} ${v}`).join('  ') || 'none';
  $('#pane-ledger').innerHTML = [
    `${span('dim', 'payouts    ')}${l.dryRun ? span('warn', 'DRY RUN (nothing is sent)') : span('ok', 'live')}  ${span('dim', 'wallet')} ${l.wallet ? span('ok', 'yes') : span('dim', 'no')}`,
    `${span('dim', 'pps ready  ')}${l.ppsReady ? span('ok', 'yes') : span('dim', 'no')}   ${span('dim', 'treasury free')} ${coins(l.treasuryFree || 0)}`,
    `${span('dim', 'liabilities')} ${coins(liab)}   ${span('dim', 'payable accounts')} ${num(l.payableAccounts || 0)}`,
    `${span('dim', 'blocks     ')}${esc(blocks)}`,
    span('dim', 'recent payouts'),
  ].concat((l.payouts || []).slice(0, wide('ledger') ? 10 : 4).map((x) => `  ${pad(ago(x.time), 8)}${pad(coins(x.amount), 14)}${pad(x.status, 11)}${x.txid ? short(x.txid, 10, 8) : '-'}`)).join('\n');
}

const MINER_KEYS = { name: (m) => m.name, device: (m) => m.device || '', proto: (m) => m.proto, hashrate: (m) => m.hashrate, diff: (m) => m.diff, ok: (m) => m.ok, stale: (m) => m.stale, dup: (m) => m.dup, seen: (m) => m.seen, unit: (m) => m.unit, jobMs: (m) => m.jobMs || 0 };
function minerRows() {
  let rows = ui.src.miners();
  if (ui.minerFilter) rows = rows.filter((m) => (m.name + ' ' + m.proto).toLowerCase().includes(ui.minerFilter));
  return sortRows('miners', rows, MINER_KEYS, 'hashrate');
}
function renderMiners() {
  const rows = minerRows();
  const mw = wide('miners');
  const shown = rows.slice(0, mw ? Math.max(ui.minerLimit, 200) : ui.minerLimit);
  const mwk = colw(shown.map((m) => m.name), 6);
  $('#miners-hint').textContent = `${shown.length}/${rows.length} sort:${sortOf('miners', 'hashrate').k}${sortOf('miners', 'hashrate').d > 0 ? '+' : '-'}${ui.minerFilter ? ' grep:' + ui.minerFilter : ''}`;
  $('#pane-miners').innerHTML = [`<span class="row-h">${hdr('miners', [['name', 'WORKER', mw ? mwk : 19, false, 1], ['device', 'DEVICE', 13, false, 1], ['proto', 'PROTO', 6, false, 1], ['hashrate', 'HASHRATE', 12, true], ['diff', 'DIFF', 10, true], ['ok', 'OK', 8, true], ['stale', 'STALE', 7, true], ['dup', 'DUP', 6, true]], 'hashrate')}  ${hdr('miners', [['seen', 'LAST', 6, false, 1], ['unit', 'UNIT', 14, false, 1], ['jobMs', 'JOB ms', 6, true]], 'hashrate')}</span>`]
    .concat(shown.map((m) => {
      const stale = m.stale / Math.max(1, m.ok + m.stale);
      const seen = m.seen === 0 ? span('ok', 'now   ') : span(m.seen > 5 ? 'warn' : 'dim', pad(m.seen + 's', 6));
      return `${mw ? pad(m.name, mwk) : pad(trunc(workerLabel(m.name), 18), 19)}${span('c2', pad(m.device || '-', 13))}${span(m.proto === 'V2' ? 'c2' : m.proto === 'EDGE' ? 'c3' : 'dim', pad(m.proto, 6))}${lpad(fmtHash(m.hashrate), 12)}${lpad(m.diff.toExponential(1), 10)}${lpad(num(m.ok), 8)}${span(stale > 0.01 ? 'warn' : 'dim', lpad(num(m.stale), 7))}${lpad(num(m.dup), 6)}  ${seen}${span('dim', pad(m.unit, 14))}${lpad(m.jobMs ? Math.round(m.jobMs) : '-', 6)}`;
    })).join('\n');
}


const card = Extras.card;
const timeLine = (host, series, fmt, extra = {}) => Neon.line(host, { series, fmt, xfmt: agoLabel, ...extra });

Extras.add('miners', (el) => {
  const k = card(el, 'fleet', 'wide'), hist = card(el, 'workers per hashrate class'), pr = card(el, 'protocols'), top = card(el, 'top 10 workers by hashrate'), bad = card(el, 'highest stale ratio');
  return () => {
    const all = ui.src.miners(); if (!all.length) return;
    const hrs = all.map((m) => m.hashrate).sort((a, b) => a - b), total = hrs.reduce((a, b) => a + b, 0);
    const top10 = hrs.slice(-10).reduce((a, b) => a + b, 0);
    Neon.kpis(k, [{ label: 'workers', value: all.length, fmt: num }, { label: 'total', value: total, fmt: fmtHash, color: 'c2' }, { label: 'median', value: hrs[Math.floor(hrs.length / 2)], fmt: fmtHash, color: 'c2' },
      { label: 'top 10 share', value: total ? top10 / total * 100 : 0, fmt: (v) => v.toFixed(1) + '%', color: 'c3' }, { label: 'largest', value: hrs[hrs.length - 1], fmt: fmtHash, color: 'c3' },
      { label: 'idle (>120 s)', value: all.filter((m) => m.seen > 120).length, fmt: num, color: 'warn' }]);
    const pos = hrs.filter((h) => h > 0), lo = Math.log10(pos[0] || 1), hi = Math.log10(pos[pos.length - 1] || 10), n = 12, step = (hi - lo || 1) / n;
    const cnt = Array.from({ length: n }, () => 0); pos.forEach((h) => { cnt[Math.min(n - 1, Math.floor((Math.log10(h) - lo) / step))]++; });
    Neon.columns(hist, cnt.map((c, i) => ({ key: i, label: fmtHash(10 ** (lo + i * step)).replace(/\.\d+/, ''), value: c, text: `${c} workers`, tip: `${fmtHash(10 ** (lo + i * step))} .. ${fmtHash(10 ** (lo + (i + 1) * step))}` })), {});
    const cp = {}; all.forEach((m) => { cp[m.proto] = (cp[m.proto] || 0) + 1; });
    Neon.donut(pr, Object.entries(cp).map(([p, c], i) => ({ label: p, value: c, color: p === 'V2' ? 'c2' : p === 'EDGE' ? 'c3' : 'c1', text: num(c) })), { big: num(all.length), sub: 'workers' });
    Neon.bars(top, [...all].sort((a, b) => b.hashrate - a.hashrate).slice(0, 10).map((m) => ({ key: m.name, label: trunc(workerLabel(m.name), 22), value: m.hashrate, text: fmtHash(m.hashrate) })));
    Neon.bars(bad, all.filter((m) => m.ok + m.stale > 20).map((m) => ({ key: m.name, label: trunc(workerLabel(m.name), 22), value: m.stale / (m.ok + m.stale), text: pct(m.stale / (m.ok + m.stale)) + `  ${num(m.stale)} of ${num(m.ok + m.stale)}`, color: 'warn' })).sort((a, b) => b.value - a.value).slice(0, 10), { max: 0.05 });
  };
});


Extras.add('oblocks', (el) => {
  const k = card(el, 'found blocks', 'wide'), gap = card(el, 'minutes between blocks', 'wide'), st = card(el, 'status'), wk = card(el, 'blocks per worker');
  return () => {
    const b = ui.src.blocks(50); if (!b.length) { k.innerHTML = '<span class="dim">no block found yet</span>'; return; }
    const t = b.map((x) => x.time).sort((a, c) => a - c), gaps = t.slice(1).map((x, i) => (x - t[i]) / 60);
    Neon.kpis(k, [{ label: 'listed', value: b.length, fmt: num }, { label: 'last found', value: Date.now() / 1000 - t[t.length - 1], fmt: hms, color: 'c2' }, { label: 'avg gap', value: gaps.length ? gaps.reduce((a, c) => a + c, 0) / gaps.length : 0, fmt: (v) => v.toFixed(1) + ' min', color: 'c3' },
      { label: 'maturing', value: b.filter((x) => x.status === 'maturing').length, fmt: num, color: 'c3' }]);
    if (gaps.length) Neon.columns(gap, gaps.map((g, i) => ({ key: t[i + 1], label: '', value: g, text: g.toFixed(1) + ' min', tip: agoLabel(t[i + 1]) })), { color: 'c3' });
    else gap.innerHTML = '<span class="dim">needs at least two blocks</span>';
    const sc = {}; b.forEach((x) => { sc[x.status] = (sc[x.status] || 0) + 1; });
    Neon.donut(st, Object.entries(sc).map(([n, c]) => ({ label: n, value: c, color: n === 'found' ? 'ok' : n === 'maturing' ? 'c3' : 'c1', text: num(c) })), { big: num(b.length), sub: 'blocks' });
    const wc = {}; b.forEach((x) => { wc[x.worker] = (wc[x.worker] || 0) + 1; });
    Neon.bars(wk, Object.entries(wc).sort((a, c) => c[1] - a[1]).slice(0, 10).map(([n, c]) => ({ key: n, label: trunc(workerLabel(n), 22), value: c, text: num(c) })));
  };
});


/* ---------- console ---------- */

const cmds = {
  help() {
    out(span('c1', 'commands'));
    [['status', 'one-screen summary'], ['tip', 'chain tip and latency'], ['miners [n] [sort k] [grep s] [reset]', 'worker table, k = hashrate|name|proto|diff|ok|stale|dup|seen|unit (or click a column header)'],
      ['blocks', 'recent blocks'], ['alloc', 'candidate and unit allocator'], ['log [n]', 'replay last n events'], ['watch', 'toggle log follow'],
      ['theme [name]', 'list or set the colour theme (default cyan)'], ['clear', 'clear console (Ctrl+L)'], ['about', 'what this is']]
      .forEach(([c, d]) => out(`  ${span('c3', pad(c, 38))}${span('dim', d)}`));
  },
  status() {
    const s = ui.src.summary();
    out(`${span('c1', 'pool')}    ${fmtHash(s.hashrate.now)}  ${num(s.miners.online)}/${num(s.miners.total)} miners  valid ${pct(s.shares.ok)} of ${num(s.shares.total)} shares`);
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
      else if (a[i] === 'sort' && a[i + 1]) { const k = a[i + 1] === 'shares' ? 'ok' : a[i + 1]; if (MINER_KEYS[k]) { const s = sortOf('miners', 'hashrate'); s.k = k; s.d = ['name', 'proto', 'unit', 'seen'].includes(k) ? 1 : -1; } i++; }
      else if (a[i] === 'grep') { ui.minerFilter = (a[i + 1] || '').toLowerCase(); i++; }
      else if (a[i] === 'reset') { ui.minerFilter = ''; delete SortS.miners; ui.minerLimit = 40; }
    }
    renderMiners();
    out(span('dim', `miners pane: ${ui.minerLimit} rows, sort ${sortOf('miners', 'hashrate').k}${ui.minerFilter ? ', grep ' + ui.minerFilter : ''}`));
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
    if (!t) return Themes.listHtml().split('\n').forEach((l) => out(l));
    if (!Themes.set(t)) return out(span('warn', 'usage: theme ' + Themes.list().join('|')));
  },
  clear() { ui.tty.innerHTML = ''; },
  about() {
    out(`${span('c1', 'PHOTON')} pool operator terminal for Elektron Net`);
    out(span('dim', 'connected to the pool admin API at /api/v1'));
  },
};
cmds.ls = cmds.help; cmds.top = cmds.status;


function bindConsole() {
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
  on(document, 'keydown', (e) => { if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key.length === 1 && document.activeElement !== input) input.focus(); });
}


/* ---------- mount ---------- */

async function mount(root) {
  root.innerHTML = TEMPLATE;
  ui.tty = $('#tty'); ui.follow = true;
  $('#who').textContent = 'operator';
  const tag = $('#mode-tag'); tag.textContent = 'ADMIN'; tag.className = 'tag live';
  bindConsole();
  on(document, 'depthchange', onDepth);
  ui.src = liveSource('/api/v1');
  await ui.src.init();
  [['info', 'PHOTON operator terminal, attached to /api/v1'], ['info', 'type help for commands, Tab completes, Up/Down recalls history']].forEach(([lvl, msg]) => logEvent({ t: Date.now() / 1000, lvl, msg }));
  ui.src.events().forEach(logEvent);
  render();
  timers.push(setInterval(() => { ui.src.tick(); ui.src.events().forEach(logEvent); try { render(); } catch (_) { /* no data yet */ } }, 1000));
  if (matchMedia('(pointer: fine)').matches) $('#cmd').focus();
}
function unmount() {
  timers.splice(0).forEach(clearInterval);
  offs.splice(0).forEach((f) => f());
  if (ui.src) ui.src.close();
}
return { mount, unmount };
})();
