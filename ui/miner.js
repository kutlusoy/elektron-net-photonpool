'use strict';

const modeLabel = { solo: 'SOLO', pplns: 'PPLNS', pps: 'PPS' };
const M = { addr: '', acct: null, pool: null, demo: false, timer: null };

/* ---------- data ---------- */

async function getJson(path) {
  const r = await fetch(path, { cache: 'no-store' });
  if (!r.ok) throw new Error(r.status === 400 ? 'not a valid address for this network' : r.status === 429 ? 'too many requests, try again in a moment' : 'HTTP ' + r.status);
  return r.json();
}

function demoData(addr) {
  const r = rng(5);
  const now = Date.now() / 1000;
  const history = Array.from({ length: 144 }, (_, i) => ({ time: now - (143 - i) * 600, hashrate: 1.1e14 + Math.sin(i / 9) * 2e13 + r() * 1e13 }));
  return {
    pool: { name: 'Photon Demo', explorerUrl: '', minPayoutLeptons: 100000, node: { height: 132457 }, modes: [{ mode: 'pplns', fee: 1, available: true }] },
    acct: {
      address: addr, hashrate: 1.2e14, workers: 3, shares1h: { pplns: { difficulty: 5200, count: 310 } }, shares24h: { pplns: { difficulty: 120000, count: 7400 } },
      balance: { pending: 12.5 * LEPTON, confirmed: 0.0008 * LEPTON, paid: 40 * LEPTON }, minPayoutLeptons: 100000,
      workerList: [{ name: 'rig1', mode: 'pplns', hashrate: 5.1e13, difficulty: 8192, accepted: 2210, stale: 12, invalid: 1, seen: 3 }, { name: 'rig2', mode: 'pplns', hashrate: 4.4e13, difficulty: 8192, accepted: 1930, stale: 9, invalid: 0, seen: 8 }, { name: 'rig3', mode: 'solo', hashrate: 2.5e13, difficulty: 4096, accepted: 880, stale: 2, invalid: 0, seen: 1 }],
      history,
      credits: [{ height: 132440, hash: hex(r, 64), amount: 3.1 * LEPTON, status: 'pending', blockStatus: 'maturing', maturesIn: 84 }, { height: 132390, hash: hex(r, 64), amount: 2.2 * LEPTON, status: 'pending', blockStatus: 'maturing', maturesIn: 34 }, { height: 132100, hash: hex(r, 64), amount: 1.9 * LEPTON, status: 'confirmed', blockStatus: 'mature', maturesIn: 0 }],
      payouts: [{ time: now - 7000, txid: hex(r, 64), amount: 5 * LEPTON, status: 'broadcast' }, { time: now - 90000, txid: hex(r, 64), amount: 8.2 * LEPTON, status: 'broadcast' }],
      blocksFound: [{ height: 132200, hash: hex(r, 64), time: now - 40000, mode: 'solo', status: 'mature' }],
    },
  };
}

async function load() {
  if (!M.addr) return;
  if (M.demo) { const d = demoData(M.addr); M.acct = d.acct; M.pool = d.pool; return; }
  const [acct, pool] = await Promise.all([getJson('/public/v1/account/' + encodeURIComponent(M.addr)), M.pool ? Promise.resolve(M.pool) : getJson('/public/v1/pool')]);
  M.acct = acct;
  M.pool = pool;
}

/* ---------- rendering ---------- */

const txUrl = (base, txid) => (safeUrl(base) && /^[0-9a-f]{64}$/.test(txid || '') ? `${base.replace(/\/+$/, '')}/tx/${txid}` : '');

function compress(values, n) {
  if (values.length <= n) return values;
  const size = values.length / n;
  return Array.from({ length: n }, (_, i) => { const part = values.slice(Math.floor(i * size), Math.floor((i + 1) * size)); return part.reduce((a, b) => a + b, 0) / Math.max(1, part.length); });
}

document.addEventListener('depthchange', () => { try { render(); } catch (_) { /* no data yet */ } });
function render() {
  const p = M.pool, a = M.acct;
  if (p) { $('#pool-name').textContent = p.name; document.title = 'MINER | ' + p.name; }
  if (!a) {
    $('#statusline').innerHTML = `<span>no address selected</span>`;
    for (const id of ['acct', 'bal', 'hr', 'workers', 'pend', 'pay', 'found']) $('#pane-' + id).innerHTML = '';
    $('#pane-acct').innerHTML = span('dim', 'paste your payout address into the console below\nor open this page as miner.html#<address>');
    return;
  }
  const explorer = p ? p.explorerUrl : '';
  const min = a.minPayoutLeptons || 100000;
  const avg24 = a.history.length ? a.history.reduce((s, x) => s + x.hashrate, 0) / a.history.length : 0;
  const online = a.workerList.filter((w) => w.seen < 120).length;
  const lastSeen = a.workerList.length ? Math.min(...a.workerList.map((w) => w.seen)) : null;
  $('#statusline').innerHTML = [
    `<span>miner <b>${esc(maskAddr(a.address))}</b></span>`,
    `<span>hashrate <b>${fmtHash(a.hashrate)}</b></span>`,
    `<span>workers <b>${online}</b></span>`,
    `<span class="${a.balance.confirmed >= min ? 'ok' : ''}">payable <b>${leptons(a.balance.confirmed)}</b> leptons</span>`,
  ].join('');
  $('#pane-acct').innerHTML = [
    `${span('dim', 'address   ')}${span('c1', maskAddr(a.address))}`,
    `${span('dim', 'status    ')}${online ? span('ok', 'mining') : span('warn', 'no worker online')}`,
    `${span('dim', 'hashrate  ')}${span('hl', fmtHash(a.hashrate))}`,
    `${span('dim', 'avg 24h   ')}${fmtHash(avg24)}`,
    `${span('dim', 'workers   ')}${online} online of ${a.workerList.length}`,
    `${span('dim', 'last share')} ${lastSeen === null ? 'never' : lastSeen + 's ago'}`,
  ].join('\n');
  const progress = Math.min(1, a.balance.confirmed / min);
  $('#pane-bal').innerHTML = [
    `${span('dim', 'pending   ')}${coins(a.balance.pending)}  ${span('dim', 'in immature blocks')}`,
    `${span('dim', 'payable   ')}${span('hl', coins(a.balance.confirmed))}`,
    `${span('dim', 'paid      ')}${coins(a.balance.paid)}`,
    '',
    `${span('dim', 'next payout at ')}${leptons(min)} leptons`,
    `${bar(progress, 20, progress >= 1 ? 'ok' : 'c1')} ${pct(progress)}`,
    span('dim', progress >= 1 ? 'due with the next payout run' : `${leptons(Math.max(0, min - a.balance.confirmed))} leptons to go`),
  ].join('\n');
  const rates = compress(a.history.map((x) => x.hashrate), 56);
  const sh = (m) => Object.entries(m).map(([k, v]) => `${modeLabel[k] || k} ${num(v.count)}`).join('  ') || 'none';
  $('#pane-hr').innerHTML = [
    rates.length ? span('c2', spark(rates, 56)) : span('dim', 'no shares in the last 24 hours'),
    `${span('dim', 'shares 1h   ')}${esc(sh(a.shares1h))}`,
    `${span('dim', 'shares 24h  ')}${esc(sh(a.shares24h))}`,
  ].join('\n');
  $('#pane-workers').innerHTML = [span('row-h', fit('WORKER', wide('workers') ? 40 : 16, wide('workers')) + pad('MODE', 7) + lpad('HASHRATE', 12) + lpad('DIFF', 9) + lpad('OK', 8) + lpad('STALE', 7) + lpad('BAD', 5) + '  LAST')]
    .concat(a.workerList.map((w) => `${wide('workers') ? pad(w.name, 40) : pad(trunc(w.name, 15), 16)}${span('c2', pad(modeLabel[w.mode] || w.mode, 7))}${lpad(fmtHash(w.hashrate), 12)}${lpad(num(w.difficulty), 9)}${lpad(num(w.accepted), 8)}${span(w.stale > 0 ? 'warn' : 'dim', lpad(num(w.stale), 7))}${lpad(num(w.invalid), 5)}  ${w.seen < 120 ? span('ok', w.seen + 's') : span('dim', ago(Date.now() / 1000 - w.seen))}`))
    .concat(a.workerList.length ? [] : [span('dim', 'no worker is connected right now')]).join('\n');
  $('#pane-pend').innerHTML = [span('row-h', pad('BLOCK', 9) + pad('AMOUNT', 14) + 'STATE')]
    .concat(a.credits.map((c) => {
      const st = c.status === 'pending' ? span('c3', `matures in ${c.maturesIn}`) : c.status === 'confirmed' ? span('ok', 'payable') : span('err', c.status);
      return `${linkPad(c.url, num(c.height), 9)}${pad(coins(c.amount), 14)}${st}`;
    }))
    .concat(a.credits.length ? [] : [span('dim', 'no PPLNS credits yet')]).join('\n');
  $('#pane-pay').innerHTML = [span('row-h', pad('AGE', 7) + pad('AMOUNT', 14) + 'TX')]
    .concat(a.payouts.map((x) => `${pad(ago(x.time), 7)}${pad(coins(x.amount), 14)}${link(txUrl(explorer, x.txid), wide('pay') ? (x.txid || 'pending') : short(x.txid || 'pending', 6, 4), 'c1')}`))
    .concat(a.payouts.length ? [] : [span('dim', 'nothing paid out yet')]).join('\n');
  $('#pane-found').innerHTML = [span('row-h', pad('BLOCK', 9) + pad('AGE', 7) + pad('MODE', 7) + 'STATE')]
    .concat(a.blocksFound.map((b) => `${linkPad(b.url, num(b.height), 9)}${pad(ago(b.time), 7)}${span('c2', pad(modeLabel[b.mode] || b.mode, 7))}${b.status}`))
    .concat(a.blocksFound.length ? [] : [span('dim', 'no block found by this address')]).join('\n');
}

/* ---------- console ---------- */

const tty = $('#tty');
function out(html) {
  const d = document.createElement('div');
  d.innerHTML = html;
  tty.appendChild(d);
  while (tty.childElementCount > 200) tty.firstChild.remove();
  tty.scrollTop = tty.scrollHeight;
}

async function setAddress(addr) {
  M.addr = addr.trim();
  M.acct = null;
  try { localStorage.setItem('photon-miner-address', M.addr); } catch (_) { /* storage unavailable */ }
  history.replaceState(null, '', '#' + encodeURIComponent(M.addr));
  try { await load(); out(span('ok', 'loaded ' + maskAddr(M.addr))); } catch (e) { out(span('err', e.message)); }
  render();
}

const cmds = {
  help() {
    out(span('c1', 'commands'));
    [['go <address>', 'show another address (or just paste it)'], ['refresh', 'reload now'], ['theme cyan|amber|green|magenta', 'colour scheme'], ['clear', 'clear console']]
      .forEach(([c, d]) => out(`  ${span('c3', pad(c, 34))}${span('dim', d)}`));
  },
  go(a) { return a[0] ? setAddress(a[0]) : out(span('warn', 'usage: go <address>')); },
  async refresh() { try { await load(); render(); out(span('dim', 'refreshed')); } catch (e) { out(span('err', e.message)); } },
  theme(a) {
    const t = a[0];
    if (!['cyan', 'amber', 'green', 'magenta'].includes(t)) return out(span('warn', 'usage: theme cyan|amber|green|magenta'));
    document.body.dataset.theme = t;
    try { localStorage.setItem('photon-theme', t); } catch (_) { /* storage unavailable */ }
  },
  clear() { tty.innerHTML = ''; },
};

const input = $('#cmd');
$('#prompt').addEventListener('submit', async (e) => {
  e.preventDefault();
  const raw = input.value.trim();
  input.value = '';
  if (!raw) return;
  out(`${span('u', 'miner')}${span('at', '@')}${span('h', 'photon')}${span('sep', ':~$')} ${esc(raw)}`);
  const [c, ...a] = raw.split(/\s+/);
  if (cmds[c]) { try { await cmds[c](a); } catch (err) { out(span('err', 'error: ' + err.message)); } }
  else if (/^[a-z]{1,8}1[0-9a-z]{20,}$/i.test(raw)) await setAddress(raw);
  else out(span('warn', `unknown command: ${c}. try help, or paste your address`));
});
document.addEventListener('keydown', (e) => { if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key.length === 1 && document.activeElement !== input) input.focus(); });

/* ---------- boot ---------- */

async function boot() {
  try { const t = localStorage.getItem('photon-theme'); if (t) document.body.dataset.theme = t; } catch (_) { /* storage unavailable */ }
  const params = new URLSearchParams(location.search);
  M.demo = params.has('demo') || location.protocol === 'file:';
  const tag = $('#mode-tag');
  tag.textContent = M.demo ? 'DEMO DATA' : 'LIVE';
  tag.className = 'tag ' + (M.demo ? 'demo' : 'live');
  let addr = decodeURIComponent(location.hash.replace(/^#/, ''));
  if (!addr) { try { addr = localStorage.getItem('photon-miner-address') || ''; } catch (_) { addr = ''; } }
  if (M.demo && !addr) addr = 'be1qdemodemodemodemodemodemodemodemodemo';
  out(span('c1', 'miner overview. paste your payout address to see balances, pending credits and payouts.'));
  if (addr) await setAddress(addr); else render();
  setInterval(async () => { if (!M.addr) return; try { await load(); render(); } catch (_) { /* keep the last data */ } }, 10000);
  setInterval(() => { $('#clock').textContent = new Date().toISOString().slice(11, 19) + ' UTC'; }, 500);
  if (matchMedia('(pointer: fine)').matches) input.focus();
}
boot();
