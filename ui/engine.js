window.Views = window.Views || {};
// Engine view: what the pool is doing right now (chain tip, allocator, share quality, a sanitized event feed). It reads only the public API,
// so it is the same on the public and the operator port and shows no worker, address or node detail.
Views.engine = (() => {
'use strict';
const timers = [], offs = [];
const on = (t, ev, fn) => { t.addEventListener(ev, fn); offs.push(() => t.removeEventListener(ev, fn)); };
const TEMPLATE = `<div class="statusline" id="statusline" role="status" aria-live="off"></div>
  <main class="grid engine">
    <section class="pane p-tip" aria-label="Chain tip"><h2><span>TIP</span></h2><pre id="pane-tip"></pre></section>
    <section class="pane p-alloc" aria-label="Allocator"><h2><span>ALLOCATOR</span></h2><pre id="pane-alloc"></pre></section>
    <section class="pane p-shares" aria-label="Shares"><h2><span>SHARES</span></h2><pre id="pane-shares"></pre></section>
    <section class="pane p-events" aria-label="Events"><h2><span>EVENTS</span><em>public feed, no miner data</em></h2><div class="tty" id="events" tabindex="-1"></div></section>
  </main>`;
const view = { s: null, seq: 0, lastTip: { h: null, t: 0 }, follow: true };

const get = async (p) => { const r = await fetch(p, { cache: 'no-store' }); if (!r.ok) throw new Error(r.status); return r.json(); };

function addEvent(e) {
  const box = $('#events');
  if (!box) return;
  const d = document.createElement('div');
  const t = new Date(e.t * 1000).toTimeString().slice(0, 8), lv = String(e.lvl || 'info').toLowerCase().replace(/[^a-z]/g, '');
  d.innerHTML = `<span class="t">${t}</span> <span class="lv lv-${lv}">${esc(lv.toUpperCase().padEnd(5))}</span>${esc(e.msg)}`;
  box.appendChild(d);
  while (box.childElementCount > 300) box.firstChild.remove();
  if (view.follow) box.scrollTop = box.scrollHeight;
}

async function refresh() {
  const [s, ev] = await Promise.all([get('/public/v1/engine'), get('/public/v1/events?since=' + view.seq)]);
  view.s = s;
  const n = s.node;
  Series.seed('hr', s.hashrate.hist);
  Series.push('p50', n.p50); Series.push('p95', n.p95);
  Series.push('units', s.alloc.unitsPerSec); Series.push('epoch', s.alloc.epochUse * 100);
  Series.push('sps', s.shares.perSec);
  if (view.lastTip.h !== null && n.height !== view.lastTip.h && view.lastTip.t) Series.push('tipint', n.tipTime - view.lastTip.t, n.tipTime);
  if (n.height !== view.lastTip.h) view.lastTip = { h: n.height, t: n.tipTime };
  (ev.events || []).forEach(addEvent);
  if (typeof ev.last === 'number') view.seq = ev.last;
}

function render() {
  const s = view.s;
  if (!s) return;
  const n = s.node, age = Date.now() / 1000 - n.tipTime;
  $('#statusline').innerHTML = [
    `<span>height <b>${num(n.height)}</b></span>`,
    `<span class="${n.connected ? 'ok' : 'warn'}">node <b>${n.connected ? 'UP' : 'DOWN'}</b></span>`,
    `<span>tip age <b>${hms(age)}</b></span>`,
    `<span>job latency <b>${n.p50.toFixed(0)}</b> ms p50 / <b>${n.p95.toFixed(0)}</b> ms p95</span>`,
    `<span>candidates <b>${num(s.alloc.candidates)}</b></span>`,
    `<span>pool <b>${fmtHash(s.hashrate.now)}</b></span>`,
  ].join('');
  $('#pane-tip').innerHTML = [
    `${span('dim', 'height    ')}${span('hl', num(n.height))}`,
    `${span('dim', 'hash      ')}${link(blockUrl(s.pool.explorerUrl, n.tip), wide('tip') ? n.tip : short(n.tip, 10, 8), 'c1')}`,
    `${span('dim', 'age       ')}${hms(age)} ${bar(age / 60, 14, age > 75 ? 'warn' : 'c1')}`,
    `${span('dim', 'node link ')}${n.connected ? span('ok', 'connected') : span('err', 'DOWN')} rtt ${n.rttMs.toFixed(1)}ms`,
    `${span('dim', 'tip>job   ')}p50 ${n.p50.toFixed(0)}ms  p95 ${n.p95.toFixed(0)}ms  ${span('dim', 'n=' + n.samples)}`,
    `${span('dim', 'backend   ')}${esc(n.backend || 'gbt')}  ${span('dim', 'batch rpc')} ${n.batchMs.toFixed(0)} ms`,
  ].join('\n');
  $('#pane-alloc').innerHTML = [
    `${span('dim', 'candidates ')}${num(s.alloc.candidates)}${n.candidatesAvailable ? span('dim', ' of ' + num(n.candidatesAvailable) + ' available (grows with the work demand)') : ''}`,
    `${span('dim', 'units/s    ')}${num(s.alloc.unitsPerSec)}`,
    `${span('dim', 'epoch use  ')}${bar(s.alloc.epochUse, 14, s.alloc.epochUse > 0.9 ? 'warn' : 'c2')} ${pct(s.alloc.epochUse)}`,
    `${span('dim', 'overlap    ')}${s.alloc.overlap ? span('err', s.alloc.overlap + ' flagged') : span('ok', '0 flagged')}`,
    `${span('dim', 'unit       ')}${span('dim', 'candidate / ntime / slice, never given twice within a tip epoch')}`,
  ].join('\n');
  $('#pane-shares').innerHTML = [
    `${span('dim', 'shares/s  ')}${s.shares.perSec.toFixed(1)}`,
    `${span('dim', 'accepted  ')}${span('ok', pct(s.shares.ok))}`,
    `${span('dim', 'stale     ')}${span('warn', pct(s.shares.stale))}`,
    `${span('dim', 'duplicate ')}${pct(s.shares.dup)}`,
    `${span('dim', 'invalid   ')}${span('err', pct(s.shares.inv))}`,
    `${span('dim', 'total     ')}${num(s.shares.total)}`,
  ].join('\n');
  Extras.tick();
}

const card = Extras.card;
const timeLine = (host, series, fmt, extra = {}) => Neon.line(host, { series, fmt, xfmt: agoLabel, ...extra });

Extras.add('tip', (el) => {
  const k = card(el, 'chain and node link', 'wide'), lat = card(el, 'tip to job latency (ms)', 'wide'), iv = card(el, 'seconds between new tips', 'wide');
  return () => {
    const s = view.s; if (!s) return;
    Neon.kpis(k, [{ label: 'height', value: s.node.height, fmt: num }, { label: 'tip age', value: Date.now() / 1000 - s.node.tipTime, fmt: (v) => hms(v), color: 'c2' }, { label: 'latency p50', value: s.node.p50, fmt: (v) => v.toFixed(0) + ' ms', color: 'c3' },
      { label: 'latency p95', value: s.node.p95, fmt: (v) => v.toFixed(0) + ' ms', color: 'c3' }, { label: 'rpc rtt', value: s.node.rttMs, fmt: (v) => v.toFixed(1) + ' ms', color: 'c2' }, { label: 'batch rpc', value: s.node.batchMs, fmt: (v) => v.toFixed(0) + ' ms', color: 'c2' }]);
    timeLine(lat, [{ name: 'p50', data: Series.get('p50'), color: 'c1' }, { name: 'p95', data: Series.get('p95'), color: 'c3' }], (v) => v.toFixed(0));
    const ti = Series.get('tipint');
    if (ti.length) Neon.columns(iv, ti.slice(-40).map((p) => ({ key: p.x, label: '', value: p.y, text: Math.round(p.y) + ' s', tip: agoLabel(p.x) })), { color: 'c2' });
    else iv.innerHTML = '<span class="dim">no new tip seen yet, waiting for the next block</span>';
  };
});

Extras.add('alloc', (el) => {
  const k = card(el, 'allocator', 'wide'), u = card(el, 'units per second'), e = card(el, 'epoch use (%)'), g = card(el, 'epoch use');
  return () => {
    const s = view.s; if (!s) return;
    Neon.kpis(k, [{ label: 'candidates', value: s.alloc.candidates, fmt: num }, { label: 'units / s', value: s.alloc.unitsPerSec, fmt: num, color: 'c2' }, { label: 'epoch use', value: s.alloc.epochUse * 100, fmt: (v) => v.toFixed(1) + '%', color: 'c3' },
      { label: 'overlap flagged', value: s.alloc.overlap, fmt: num, color: s.alloc.overlap ? 'err' : 'ok' }]);
    timeLine(u, [{ name: 'units/s', data: Series.get('units'), color: 'c2' }], (v) => (Math.abs(v) < 10 && v % 1 ? v.toFixed(1) : num(v)));
    timeLine(e, [{ name: 'epoch use', data: Series.get('epoch'), color: 'c3' }], (v) => v.toFixed(1) + '%');
    Neon.donut(g, [{ label: 'used', value: s.alloc.epochUse, color: s.alloc.epochUse > 0.9 ? 'warn' : 'c3', text: pct(s.alloc.epochUse) }, { label: 'free', value: Math.max(0, 1 - s.alloc.epochUse), color: 'c1', text: pct(1 - s.alloc.epochUse) }], { big: pct(s.alloc.epochUse), sub: 'of the epoch' });
  };
});

Extras.add('shares', (el) => {
  const k = card(el, 'share quality', 'wide'), q = card(el, 'share outcome'), r = card(el, 'shares per second');
  return () => {
    const s = view.s; if (!s) return;
    Neon.kpis(k, [{ label: 'shares / s', value: s.shares.perSec, fmt: (v) => v.toFixed(1), color: 'c3' }, { label: 'accepted', value: s.shares.ok * 100, fmt: (v) => v.toFixed(2) + '%', color: 'ok' },
      { label: 'stale', value: s.shares.stale * 100, fmt: (v) => v.toFixed(2) + '%', color: 'warn' }, { label: 'total', value: s.shares.total, fmt: num, color: 'c2' }]);
    Neon.donut(q, [{ label: 'accepted', value: s.shares.ok, color: 'ok', text: pct(s.shares.ok) }, { label: 'stale', value: s.shares.stale, color: 'warn', text: pct(s.shares.stale) }, { label: 'duplicate', value: s.shares.dup, color: 'c3', text: pct(s.shares.dup) }, { label: 'invalid', value: s.shares.inv, color: 'err', text: pct(s.shares.inv) }], { big: pct(s.shares.ok), sub: 'accepted' });
    timeLine(r, [{ name: 'shares/s', data: Series.get('sps'), color: 'c2' }], (v) => v.toFixed(1));
  };
});

async function mount(root) {
  root.innerHTML = TEMPLATE;
  view.seq = 0; view.s = null; view.follow = true;
  $('#who').textContent = 'engine';
  const tag = $('#mode-tag'); tag.textContent = 'LIVE'; tag.className = 'tag live';
  const box = $('#events');
  on(box, 'scroll', () => { view.follow = box.scrollTop + box.clientHeight >= box.scrollHeight - 6; });
  on(document, 'depthchange', () => { try { render(); } catch (_) { /* no data yet */ } });
  const tick = async () => { try { await refresh(); render(); } catch (e) { if (!view.s) $('#pane-tip').textContent = 'pool API not reachable: ' + e.message; } };
  await tick();
  timers.push(setInterval(tick, 2000));
}
function unmount() { timers.splice(0).forEach(clearInterval); offs.splice(0).forEach((f) => f()); }
return { mount, unmount };
})();
