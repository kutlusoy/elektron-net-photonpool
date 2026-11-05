'use strict';
// Shared statistics panes of the public page and the operator page: the device table (what the miners call themselves, with links to the
// open source projects) and the stored pool history with a range switch. Both read /public/v1 or /api/v1 (`base`), with demo data as a fallback.
const Stats = (() => {
  const RANGES = ['1h', '24h', '7d', '30d', 'all'];
  const S = { devices: null, hist: null, histRange: '24h', keepDays: 0 };

  /* ---- devices ---- */
  const DEV_KEYS = { name: (d) => d.name.toLowerCase(), working: (d) => d.working, hashrate: (d) => d.hashrate, best: (d) => d.best };
  function devicesHtml(list) {
    const rows = sortRows('devices', list, DEV_KEYS, 'hashrate');
    const nw = Math.max(8, ...rows.map((d) => d.name.length + 2)) + 2;
    const head = `<span class="row-h">${hdr('devices', [['name', 'DEVICE', nw, false, 1], ['working', 'WORKING', 9, true], ['hashrate', 'HASHRATE', 13, true], ['best', 'BEST DIFF', 12, true]], 'hashrate')}</span>`;
    const body = rows.map((d) => {
      const label = d.name + (d.openSource ? ' ★' : '');
      const cell = d.url ? link(d.url, d.name, 'c1') + (d.openSource ? span('c3', ' ★') : '') : esc(label);
      return `${cell}${' '.repeat(Math.max(1, nw - label.length))}${lpad(num(d.working), 9)}${lpad(fmtHash(d.hashrate), 13)}${lpad(fmtDiff(d.best), 12)}`;
    });
    return [head, ...body, ...(rows.length ? ['', span('dim', '★ = open source software and hardware')] : [span('dim', 'no miner is connected right now')])].join('\n');
  }
  async function loadDevices(base) {
    const r = await fetch(base + '/devices', { cache: 'no-store' });
    if (!r.ok) throw new Error(r.status);
    S.devices = (await r.json()).devices;
    return S.devices;
  }
  function demoDevices() {
    return [
      { key: 'bitaxe', name: 'Bitaxe', url: 'https://github.com/skot/bitaxe/', openSource: true, working: 412, hashrate: 4.1e14, best: 330.46e6 },
      { key: 'nerdminer', name: 'NerdMiner', url: 'https://github.com/BitMaker-hub/NerdMiner_v2', openSource: true, working: 97, hashrate: 1.1e8, best: 9.38e3 },
      { key: 'nerdqaxe+', name: 'NerdQAxe+', url: 'https://github.com/shufps/qaxe', openSource: true, working: 31, hashrate: 1.1e14, best: 52.1e6 },
      { key: 'cgminer', name: 'cgminer', url: 'https://cgminer.info/', openSource: false, working: 12, hashrate: 8.4e15, best: 2.7e9 },
      { key: 'cpuminer', name: 'cpuminer', url: 'https://github.com/pooler/cpuminer', openSource: false, working: 5, hashrate: 2.0e6, best: 1.8e3 },
    ];
  }

  /* ---- history ---- */
  const hist = (p) => ({ t: p[0], hr: p[1], miners: p[2], ok: p[3], stale: p[4], dup: p[5], nd: p[6] });
  function demoHistory(range) {
    const cfg = { '1h': [60, 1], '24h': [288, 5], '7d': [336, 30], '30d': [360, 120], all: [400, 216] }[range] || [288, 5];
    const now = Math.floor(Date.now() / 1000 / 60) * 60, step = cfg[1] * 60;
    return { range, stepMinutes: cfg[1], keepDays: 60, points: Array.from({ length: cfg[0] }, (_, i) => {
      const t = now - (cfg[0] - 1 - i) * step, w = Math.sin(i / 9) * 0.12 + Math.sin(i / 3.1) * 0.03;
      return [t, 9.4e16 * (1 + w), Math.round(980 + w * 300), 400, 4, 2, 1.27e5];
    }) };
  }
  async function loadHistory(base, range) {
    const r = await fetch(`${base}/history?range=${range}`, { cache: 'no-store' });
    if (!r.ok) throw new Error(r.status);
    return r.json();
  }
  const xlabel = (range) => (x) => { const d = new Date(x * 1000).toISOString(); return range === '1h' || range === '24h' ? d.slice(11, 16) : d.slice(5, 10) + (range === '7d' ? ' ' + d.slice(11, 13) + 'h' : ''); };

  /* A pane with range buttons and a hash rate chart. `pane` is the .pane element with .rng and .chartpane inside. */
  function historyPane(pane, base, demo) {
    const chart = pane.querySelector('.chartpane'), rng = pane.querySelector('.rng');
    try { const r = localStorage.getItem('photon-hist-range'); if (RANGES.includes(r)) S.histRange = r; } catch (_) { /* storage unavailable */ }
    rng.innerHTML = RANGES.map((r) => `<button type="button" class="rbtn" data-r="${r}">${r}</button>`).join('');
    const mark = () => rng.querySelectorAll('.rbtn').forEach((b) => b.classList.toggle('on', b.dataset.r === S.histRange));
    let dead = false;
    const draw = () => {
      if (dead || !S.hist) return;
      const pts = S.hist.points.map(hist);
      S.keepDays = S.hist.keepDays;
      if (!pts.length) { chart.innerHTML = '<span class="dim">no statistics stored yet; one sample is added every minute</span>'; delete chart._neon; return; }
      Neon.line(chart, { series: [{ name: 'hashrate', data: pts.map((p) => ({ x: p.t, y: p.hr })), color: 'c1' }], fmt: fmtHash, xfmt: xlabel(S.histRange) });
    };
    async function load() {
      try { S.hist = demo ? demoHistory(S.histRange) : await loadHistory(base, S.histRange); } catch (_) { S.hist = demoHistory(S.histRange); }
      draw();
    }
    rng.addEventListener('click', (e) => { const b = e.target.closest('.rbtn'); if (!b) return; S.histRange = b.dataset.r; try { localStorage.setItem('photon-hist-range', S.histRange); } catch (_) { /* ignore */ } mark(); chart.textContent = ''; delete chart._neon; load(); });
    mark(); load();
    const timer = setInterval(load, 60000);
    return { destroy() { dead = true; clearInterval(timer); }, draw };
  }

  /* ---- extras of the maximized panes ---- */
  function registerExtras() {
    Extras.add('history', (el) => {
      const k = Extras.card(el, 'pool history', 'wide'), hr = Extras.card(el, 'hash rate', 'wide'), mi = Extras.card(el, 'miners online'), va = Extras.card(el, 'valid shares (%)'), nd = Extras.card(el, 'network difficulty');
      return () => {
        if (!S.hist || !S.hist.points.length) return;
        const p = S.hist.points.map(hist), x = xlabel(S.histRange), avg = (f) => p.reduce((a, q) => a + f(q), 0) / p.length;
        const ok = p.reduce((a, q) => a + q.ok, 0), bad = p.reduce((a, q) => a + q.stale + q.dup, 0);
        Neon.kpis(k, [{ label: 'now', value: p[p.length - 1].hr, fmt: fmtHash }, { label: 'average', value: avg((q) => q.hr), fmt: fmtHash, color: 'c2' }, { label: 'peak', value: Math.max(...p.map((q) => q.hr)), fmt: fmtHash, color: 'c3' },
          { label: 'miners peak', value: Math.max(...p.map((q) => q.miners)), fmt: num, color: 'ok' }, { label: 'valid shares', value: ok + bad ? ok / (ok + bad) * 100 : 100, fmt: (v) => v.toFixed(2) + '%', color: 'ok' },
          { label: 'kept for', value: S.keepDays || 0, fmt: (v) => v + ' days', color: 'c2', sub: S.hist.points.length + ' points' }]);
        Neon.line(hr, { series: [{ name: 'hash rate', data: p.map((q) => ({ x: q.t, y: q.hr })), color: 'c1' }], fmt: fmtHash, xfmt: x });
        Neon.line(mi, { series: [{ name: 'miners', data: p.map((q) => ({ x: q.t, y: q.miners })), color: 'ok' }], fmt: num, xfmt: x });
        Neon.line(va, { series: [{ name: 'valid %', data: p.map((q) => ({ x: q.t, y: q.ok + q.stale + q.dup ? q.ok / (q.ok + q.stale + q.dup) * 100 : 100 })), color: 'c3' }], fmt: (v) => v.toFixed(1), xfmt: x });
        Neon.line(nd, { series: [{ name: 'difficulty', data: p.map((q) => ({ x: q.t, y: q.nd })), color: 'c2' }], fmt: fmtDiff, xfmt: x });
      };
    });
    Extras.add('devices', (el) => {
      const k = Extras.card(el, 'devices', 'wide'), hr = Extras.card(el, 'hash rate by device'), wk = Extras.card(el, 'working connections'), bs = Extras.card(el, 'best share by device');
      return () => {
        const d = S.devices; if (!d || !d.length) return;
        const tot = d.reduce((a, x) => a + x.hashrate, 0), conns = d.reduce((a, x) => a + x.working, 0);
        Neon.kpis(k, [{ label: 'device types', value: d.length, fmt: num }, { label: 'working', value: conns, fmt: num, color: 'c2' }, { label: 'hash rate', value: tot, fmt: fmtHash, color: 'c3' },
          { label: 'open source', value: d.filter((x) => x.openSource).reduce((a, x) => a + x.working, 0), fmt: num, color: 'ok', sub: 'connections with ★' }]);
        Neon.bars(hr, [...d].sort((a, b) => b.hashrate - a.hashrate).map((x) => ({ key: x.key, label: x.name, value: x.hashrate, text: fmtHash(x.hashrate) })));
        Neon.donut(wk, d.map((x) => ({ key: x.key, label: x.name, value: x.working, text: num(x.working) })), { big: num(conns), sub: 'connections' });
        Neon.bars(bs, [...d].sort((a, b) => b.best - a.best).map((x) => ({ key: x.key, label: x.name, value: x.best, text: fmtDiff(x.best), color: 'c3' })));
      };
    });
  }
  registerExtras();
  return { devicesHtml, loadDevices, demoDevices, historyPane, S };
})();
