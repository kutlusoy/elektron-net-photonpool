'use strict';

const modeLabel = { solo: 'SOLO', pplns: 'PPLNS', pps: 'PPS' };
const modeShort = { solo: 'reward straight to you', pplns: 'share of each pool block', pps: 'fixed pay per share' };
const modeText = {
  solo: 'direct payout in the block you find, no pool fee',
  pplns: 'share of the reward of every pool block by recent work',
  pps: 'fixed payment per share, paid by the pool',
};

const view = { src: null, pool: null, blocks: [], payouts: [] };

/* ---------- data ---------- */

function liveSource() {
  const get = async (p) => { const r = await fetch('/public/v1' + p, { cache: 'no-store' }); if (!r.ok) throw new Error(r.status); return r.json(); };
  return {
    demo: false,
    async refresh() { [view.pool, view.blocks, view.payouts] = await Promise.all([get('/pool'), get('/blocks?limit=12'), get('/payouts?limit=8')]); },
    async account(addr) { const r = await fetch('/public/v1/account/' + encodeURIComponent(addr), { cache: 'no-store' }); if (!r.ok) throw new Error(r.status === 400 ? 'not a valid address for this network' : 'HTTP ' + r.status); return r.json(); },
  };
}

function demoSource() {
  const r = rng(77);
  const now = () => Date.now() / 1000;
  const hist = Array.from({ length: 60 }, () => 9.4e16 + r() * 4e15);
  const blocks = Array.from({ length: 10 }, (_, i) => ({ height: 132400 - i * 23, hash: hex(r, 64), time: now() - 1800 * (i + 1), mode: ['pplns', 'pplns', 'solo', 'pplns', 'pps'][i % 5], status: i < 2 ? 'maturing' : 'mature', finder: 'be1q' + hex(r, 6) + '..' + hex(r, 4) }));
  const payouts = Array.from({ length: 5 }, (_, i) => ({ time: now() - 5400 * (i + 1), txid: hex(r, 64), amount: Math.round((3 + r() * 40) * LEPTON), status: 'broadcast' }));
  return {
    demo: true,
    async refresh() {
      hist.push(9.4e16 + r() * 4e15); hist.shift();
      view.pool = {
        name: 'Photon Demo', version: '0.1.0-demo', network: 'mainnet', explorerUrl: '', uptime: 3 * 86400 + 4000,
        stratum: { host: 'pool.example.org', ports: [{ port: 3333, mode: 'pplns', override: true }, { port: 3334, mode: 'pps', override: false }, { port: 3335, mode: 'solo', override: false }] },
        modes: [{ mode: 'solo', fee: 0, available: true }, { mode: 'pplns', fee: 1, available: true }, { mode: 'pps', fee: 2, available: false }],
        minPayoutLeptons: 100000, pplnsWindowFactor: 2, hashrate: { now: hist[hist.length - 1], m1: 9.5e16, h1: 9.45e16, hist },
        miners: { online: 998, workers: 1000 }, found24h: 4, blocks: { found: 0, maturing: 2, mature: 40, orphaned: 1 },
        node: { height: 132457, tip: hex(r, 64), tipTime: now() - 23 }, networkDifficulty: 1.2e9,
      };
      view.blocks = blocks; view.payouts = payouts;
    },
    async account(addr) {
      return { address: addr, hashrate: 1.2e14, workers: 3, shares1h: { pplns: { difficulty: 5200, count: 310 } }, shares24h: { pplns: { difficulty: 120000, count: 7400 } },
        balance: { pending: 12 * LEPTON, confirmed: 3.2 * LEPTON, paid: 40 * LEPTON }, payouts: [{ time: now() - 7000, txid: hex(r, 64), amount: 5 * LEPTON, status: 'broadcast' }] };
    },
  };
}

/* ---------- rendering ---------- */

function txUrl(base, txid) {
  return safeUrl(base) && /^[0-9a-f]{64}$/.test(txid || '') ? `${base.replace(/\/+$/, '')}/tx/${txid}` : '';
}

document.addEventListener('depthchange', () => { try { render(); } catch (_) { /* no data yet */ } });
function render() {
  const p = view.pool;
  if (!p) return;
  $('#pool-name').textContent = p.name;
  document.title = p.name.toUpperCase();
  const age = Date.now() / 1000 - p.node.tipTime;
  $('#statusline').innerHTML = [
    `<span>net <b>${esc(p.network)}</b></span>`,
    `<span>height <b>${num(p.node.height)}</b></span>`,
    `<span>pool <b>${fmtHash(p.hashrate.now)}</b></span>`,
    `<span>miners <b>${num(p.miners.online)}</b></span>`,
    `<span>blocks 24h <b>${num(p.found24h)}</b></span>`,
    `<span>tip age <b>${hms(age)}</b></span>`,
  ].join('');
  $('#pane-pool').innerHTML = [
    `${span('dim', 'hashrate  ')}${span('hl', fmtHash(p.hashrate.now))}  ${span('c2', spark(p.hashrate.hist, 24))}`,
    `${span('dim', 'avg 1h    ')}${fmtHash(p.hashrate.h1)}`,
    `${span('dim', 'miners    ')}${num(p.miners.online)} online, ${num(p.miners.workers)} workers`,
    `${span('dim', 'blocks    ')}${num(p.found24h)} in 24h, ${num(p.blocks.mature || 0)} mature`,
    `${span('dim', 'network   ')}difficulty ${p.networkDifficulty.toExponential(3)}`,
    `${span('dim', 'uptime    ')}${dhms(p.uptime)}`,
  ].join('\n');
  const host = p.stratum.host || location.hostname;
  $('#pane-connect').innerHTML = [
    span('dim', 'urls (the port decides the payout mode):'),
    `${span('c3', '+m')}${span('dim', ' = password may add ')}${span('c3', 'm=solo|pplns|pps')}`,
    ...p.stratum.ports.map((x) => `${span('c1', `stratum+tcp://${host}:${x.port}`)}  ${span('c2', modeLabel[x.mode] || x.mode)}${x.override ? span('c3', ' +m') : ''}`),
    '',
    `${span('dim', 'user      ')}${span('c3', '<your address>.<worker>')}`,
    `${span('dim', 'password  ')}x`,
    `${span('dim', 'miner     ')}enable version rolling`,
  ].join('\n');
  const portsOf = (mode) => p.stratum.ports.filter((x) => x.mode === mode).map((x) => x.port + (x.override ? '*' : '')).join(',') || '-';
  $('#pane-modes').innerHTML = [span('row-h', pad('MODE', 7) + pad('FEE', 6) + pad('STATE', 8) + 'PORT')]
    .concat(p.modes.map((m) => `${span('c2', pad(modeLabel[m.mode] || m.mode, 7))}${pad(m.fee + '%', 6)}${m.available ? span('ok', pad('open', 8)) : span('warn', pad('paused', 8))}${portsOf(m.mode)}`))
    .concat(['', span('dim', '* port also accepts m=<mode>'),
      ...p.modes.map((m) => `${span('c2', pad((modeLabel[m.mode] || m.mode).toLowerCase(), 6))}${span('dim', modeShort[m.mode] || '')}`),
      '', span('dim', `min payout ${leptons(p.minPayoutLeptons)} leptons`)]).join('\n');
  $('#pane-blocks').innerHTML = [span('row-h', pad('HEIGHT', 9) + pad('AGE', 8) + pad('MODE', 7) + fit('FINDER', wide('blocks') ? 40 : 17, wide('blocks')) + 'STATE')]
    .concat(view.blocks.map((b) => {
      const a = Date.now() / 1000 - b.time;
      const cls = b.status === 'mature' ? 'dim' : b.status === 'orphaned' ? 'err' : 'c3';
      return `${linkPad(b.url, num(b.height), 9)}${pad(a < 3600 ? Math.floor(a / 60) + 'm' : (a / 3600).toFixed(1) + 'h', 8)}${pad(modeLabel[b.mode] || b.mode, 7)}${fit(b.finder, wide('blocks') ? 40 : 17, wide('blocks'))}${span(cls, b.status)}`;
    })).join('\n');
  $('#pane-payouts').innerHTML = [span('row-h', pad('AGE', 8) + pad('AMOUNT', 15) + 'TX')]
    .concat(view.payouts.map((x) => {
      const a = Date.now() / 1000 - x.time;
      return `${pad(a < 3600 ? Math.floor(a / 60) + 'm' : (a / 3600).toFixed(1) + 'h', 8)}${pad(coins(x.amount), 15)}${link(txUrl(p.explorerUrl, x.txid), wide('payouts') ? (x.txid || '') : short(x.txid || '', 6, 4), 'c1')}`;
    })).join('\n') || span('dim', 'no payouts yet');
}

/* ---------- console ---------- */

const tty = $('#tty');
function out(html) {
  const d = document.createElement('div');
  d.innerHTML = html;
  tty.appendChild(d);
  while (tty.childElementCount > 300) tty.firstChild.remove();
  tty.scrollTop = tty.scrollHeight;
}

async function accountCmd(addr) {
  if (!addr) return out(span('warn', 'usage: account <your payout address>'));
  try {
    const a = await view.src.account(addr);
    const sh = (m) => Object.entries(m).map(([k, v]) => `${modeLabel[k] || k} ${num(v.count)} shares`).join(', ') || 'none';
    out(`${span('c1', maskAddr(a.address))}  ${span('hl', fmtHash(a.hashrate))}  ${num(a.workers)} workers online`);
    out(`${span('dim', 'last hour  ')}${esc(sh(a.shares1h))}`);
    out(`${span('dim', 'last 24h   ')}${esc(sh(a.shares24h))}`);
    out(`${span('dim', 'pending    ')}${coins(a.balance.pending)}   ${span('dim', 'payable')} ${coins(a.balance.confirmed)}   ${span('dim', 'paid')} ${coins(a.balance.paid)}`);
    for (const x of a.payouts.slice(0, 5)) {
      out(`${span('dim', new Date(x.time * 1000).toISOString().slice(0, 16).replace('T', ' '))}  ${coins(x.amount)}  ${link(txUrl(view.pool.explorerUrl, x.txid), short(x.txid || 'pending', 8, 6), 'c1')}`);
    }
  } catch (e) {
    out(span('err', 'account lookup failed: ' + e.message));
  }
}

const cmds = {
  help() {
    out(span('c1', 'commands'));
    [['pool', 'pool summary'], ['connect', 'how to point your miner at this pool'], ['modes', 'payout modes, fees, availability'], ['blocks', 'recent blocks'], ['payouts', 'recent payouts'],
      ['account <address>', 'your hashrate, shares, balance and payouts'], ['miner <address>', 'open your personal miner page'], ['theme cyan|amber|green|magenta', 'colour scheme'], ['clear', 'clear console']]
      .forEach(([c, d]) => out(`  ${span('c3', pad(c, 34))}${span('dim', d)}`));
  },
  pool() { const p = view.pool; out(`${fmtHash(p.hashrate.now)}, ${num(p.miners.online)} miners, ${num(p.found24h)} blocks in 24h, height ${num(p.node.height)}`); },
  connect() {
    const p = view.pool; const host = p.stratum.host || location.hostname;
    p.stratum.ports.forEach((x) => out(`${span('c1', `stratum+tcp://${host}:${x.port}`)}  ${span('c2', modeLabel[x.mode] || x.mode)}`));
    out(span('dim', 'user: <your address>.<worker>   password: x   (version rolling on)'));
  },
  modes() { view.pool.modes.forEach((m) => out(`${span('c2', pad(modeLabel[m.mode] || m.mode, 6))} fee ${m.fee}%  ${m.available ? span('ok', 'open') : span('warn', 'paused')}  ${span('dim', modeText[m.mode] || '')}`)); },
  miner(a) { if (!a[0]) return out(span('warn', 'usage: miner <your address>')); location.href = 'miner.html#' + encodeURIComponent(a[0]); },
  blocks() { view.blocks.slice(0, 8).forEach((b) => out(`${linkPad(b.url, num(b.height), 9)}${pad(modeLabel[b.mode] || b.mode, 7)}${b.finder}  ${b.status}`)); },
  payouts() { view.payouts.forEach((x) => out(`${new Date(x.time * 1000).toISOString().slice(0, 16).replace('T', ' ')}  ${coins(x.amount)}  ${link(txUrl(view.pool.explorerUrl, x.txid), short(x.txid || '', 8, 6), 'c1')}`)); if (!view.payouts.length) out(span('dim', 'no payouts yet')); },
  account(a) { return accountCmd(a[0]); },
  theme(a) {
    const t = a[0];
    if (!['cyan', 'amber', 'green', 'magenta'].includes(t)) return out(span('warn', 'usage: theme cyan|amber|green|magenta'));
    document.body.dataset.theme = t;
    try { localStorage.setItem('photon-theme', t); } catch (_) { /* storage unavailable */ }
  },
  clear() { tty.innerHTML = ''; },
};

const input = $('#cmd');
const hist = []; let hi = 0;
$('#prompt').addEventListener('submit', async (e) => {
  e.preventDefault();
  const raw = input.value.trim();
  input.value = '';
  if (!raw) return;
  hist.push(raw); hi = hist.length;
  out(`${span('u', 'guest')}${span('at', '@')}${span('h', 'photon')}${span('sep', ':~$')} ${esc(raw)}`);
  const [c, ...a] = raw.split(/\s+/);
  if (cmds[c]) { try { await cmds[c](a); } catch (err) { out(span('err', 'error: ' + err.message)); } } else out(span('warn', `unknown command: ${c}. try help`));
});
input.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowUp') { if (hi > 0) input.value = hist[--hi]; e.preventDefault(); }
  else if (e.key === 'ArrowDown') { input.value = hi < hist.length - 1 ? hist[++hi] : ((hi = hist.length), ''); e.preventDefault(); }
  else if (e.key === 'Tab') { e.preventDefault(); const m = Object.keys(cmds).filter((c) => c.startsWith(input.value)); if (m.length === 1) input.value = m[0] + ' '; }
  else if (e.key === 'l' && e.ctrlKey) { e.preventDefault(); cmds.clear(); }
});
document.addEventListener('keydown', (e) => { if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key.length === 1 && document.activeElement !== input) input.focus(); });

/* ---------- boot ---------- */

async function boot() {
  try { const t = localStorage.getItem('photon-theme'); if (t) document.body.dataset.theme = t; } catch (_) { /* storage unavailable */ }
  let src = null;
  if (!new URLSearchParams(location.search).has('demo') && location.protocol !== 'file:') {
    try { const l = liveSource(); await l.refresh(); src = l; } catch (_) { /* fall back to demo */ }
  }
  view.src = src || demoSource();
  if (!src) await view.src.refresh();
  const tag = $('#mode-tag');
  tag.textContent = view.src.demo ? 'DEMO DATA' : 'LIVE';
  tag.className = 'tag ' + (view.src.demo ? 'demo' : 'live');
  out(span('c1', 'welcome to the pool terminal. type help, or look up your address with: account <address>'));
  render();
  setInterval(async () => { try { await view.src.refresh(); render(); } catch (_) { /* keep the last data */ } }, 5000);
  setInterval(() => { $('#clock').textContent = new Date().toISOString().slice(11, 19) + ' UTC'; }, 500);
  const hash = decodeURIComponent(location.hash.replace(/^#/, ''));
  if (hash) { out(`${span('u', 'guest')}${span('at', '@')}${span('h', 'photon')}${span('sep', ':~$')} account ${esc(hash)}`); accountCmd(hash); }
  if (matchMedia('(pointer: fine)').matches) input.focus();
}
boot();
