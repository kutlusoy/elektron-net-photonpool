'use strict';

const $ = (s) => document.querySelector(s);
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const pad = (s, n) => String(s).padEnd(n).slice(0, n);
const lpad = (s, n) => String(s).padStart(n);
const span = (cls, s) => `<span class="${cls}">${esc(s)}</span>`;
const num = (n) => Math.round(n).toLocaleString('en-US');
const pct = (x) => (x * 100).toFixed(1) + '%';
const hms = (s) => { s = Math.max(0, Math.floor(s)); return [Math.floor(s / 3600), Math.floor(s / 60) % 60, s % 60].map((v) => String(v).padStart(2, '0')).join(':'); };
const dhms = (s) => `${Math.floor(s / 86400)}d ${hms(s % 86400)}`;
const safeUrl = (u) => typeof u === 'string' && /^https?:\/\/[^\s"'<>]+$/.test(u);
const link = (url, text, cls) => (safeUrl(url) ? `<a class="lnk ${cls || ''}" href="${esc(url)}" target="_blank" rel="noopener noreferrer">${esc(text)}</a>` : span(cls || '', text));
const linkPad = (url, text, w, cls) => link(url, text, cls) + ' '.repeat(Math.max(0, w - text.length));
const blockUrl = (base, hash) => (safeUrl(base) && /^[0-9a-f]{64}$/.test(hash || '') ? `${base.replace(/\/+$/, '')}/block/${hash}` : '');
const trunc = (s, n) => (s.length > n ? s.slice(0, n - 2) + '..' : s);
const workerLabel = (name) => {
  const i = name.indexOf('.');
  const addr = i < 0 ? name : name.slice(0, i);
  const rest = i < 0 ? '' : name.slice(i);
  return (addr.length > 12 ? addr.slice(0, 4) + '..' + addr.slice(-4) : addr) + rest;
};
// A maximized pane (depth.js) shows complete texts instead of the shortened ones.
const wide = (id) => !!document.querySelector('.p-' + id + '.front');
// Column width that fits the longest value (maximized panes only).
const colw = (vals, min) => Math.max(min, ...vals.map((v) => String(v).length)) + 2;
const fit = (s, n, w) => (w ? String(s).padEnd(n) : pad(s, n));
const fmtDiff = (n) => { if (!(n > 0)) return '-'; const u = [[1e15, 'P'], [1e12, 'T'], [1e9, 'G'], [1e6, 'M'], [1e3, 'k']]; for (const [v, c] of u) if (n >= v) return (n / v).toFixed(2) + c; return n.toFixed(n < 10 ? 2 : 0); };
const short = (h, a = 8, b = 6) => (h.length > a + b + 1 ? h.slice(0, a) + '..' + h.slice(-b) : h);

function fmtHash(h) {
  const u = ['H/s', 'kH/s', 'MH/s', 'GH/s', 'TH/s', 'PH/s', 'EH/s'];
  let i = 0;
  while (h >= 1000 && i < u.length - 1) { h /= 1000; i++; }
  return `${h.toFixed(h >= 100 ? 1 : 2)} ${u[i]}`;
}
function spark(vals, width) {
  const ch = '▁▂▃▄▅▆▇█';
  const v = vals.slice(-width);
  if (!v.length) return '';
  const lo = Math.min(...v), hi = Math.max(...v), r = hi - lo || 1;
  return v.map((x) => ch[Math.min(7, Math.floor(((x - lo) / r) * 7.999))]).join('');
}
function bar(frac, w, cls) {
  const f = Math.max(0, Math.min(1, frac));
  const n = Math.round(f * w);
  return `<span class="${cls || 'c1'}">${'█'.repeat(n)}</span><span class="dim">${'░'.repeat(w - n)}</span>`;
}
function rng(seed) {
  return () => { seed |= 0; seed = (seed + 0x6D2B79F5) | 0; let t = Math.imul(seed ^ (seed >>> 15), 1 | seed); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
}
const hex = (r, n) => Array.from({ length: n }, () => '0123456789abcdef'[Math.floor(r() * 16)]).join('');

const LEPTON = 1e8;
const coins = (l) => (l / LEPTON).toFixed(8);
const leptons = (l) => Math.round(l).toLocaleString('en-US');
const maskAddr = (a) => (a.length > 14 ? a.slice(0, 6) + '..' + a.slice(-4) : a);
const ago = (t) => { const a = Math.max(0, Date.now() / 1000 - t); return a < 90 ? Math.floor(a) + 's' : a < 3600 ? Math.floor(a / 60) + 'm' : a < 172800 ? (a / 3600).toFixed(1) + 'h' : Math.floor(a / 86400) + 'd'; };

/* ---------- sortable columns ---------- */
// Header cells made with hdr() sort their table when clicked (again: reverse). sortRows() applies the order.
const SortS = {};
const sortOf = (id, def, dir = -1) => SortS[id] || (SortS[id] = { k: def, d: dir });
// cols: [key, label, width, right-aligned, first direction (1 = ascending)]
function hdr(id, cols, def) {
  const s = sortOf(id, def);
  return cols.map(([k, label, w, right, d]) => {
    const on = s.k === k, t = on ? (right ? (s.d > 0 ? '\u25b2' : '\u25bc') + label : label + (s.d > 0 ? '\u25b2' : '\u25bc')) : label;
    return `<span class="th${on ? ' on' : ''}" data-s="${id}" data-k="${k}" data-d="${d || -1}" title="sort by ${esc(label.toLowerCase())}">${esc(right ? lpad(t, w) : pad(t, w))}</span>`;
  }).join('');
}
function sortRows(id, rows, get, def) {
  const s = sortOf(id, def), g = get[s.k] || get[def];
  return [...rows].sort((a, b) => { const x = g(a), y = g(b); return (x < y ? -1 : x > y ? 1 : 0) * s.d; });
}
document.addEventListener('click', (e) => {
  const t = e.target.closest && e.target.closest('.th'); if (!t) return;
  const s = sortOf(t.dataset.s, t.dataset.k);
  if (s.k === t.dataset.k) s.d = -s.d; else { s.k = t.dataset.k; s.d = +t.dataset.d; }
  document.dispatchEvent(new Event('depthchange'));
});

// Long hashes in a log line: short while the pane is small, complete while it is maximized (both are in the line, CSS picks one).
const hashSpans = (text) => esc(text).replace(/\b([0-9a-f]{12})([0-9a-f]{52})\b/g, '<span class="h-short">$1..</span><span class="h-full">$1$2</span>');

// Color themes, from cool and technical to dangerous and hacker-like. The palette lives in style.css (body[data-theme]); the swatches here only draw the menu.
const THEMES = [
  { id: 'cyan', name: 'TECH', mood: 'cool and clean', sw: ['#00e5ff', '#ff2bd6', '#f9f002'] },
  { id: 'ice', name: 'ARCTIC', mood: 'calm, icy blue', sw: ['#7cc7ff', '#b9a8ff', '#eaf6ff'] },
  { id: 'abyss', name: 'ABYSS', mood: 'deep sea, bioluminescent', sw: ['#00ffd5', '#2f7bff', '#b6fff0'] },
  { id: 'magenta', name: 'SYNTHWAVE', mood: 'neon night drive', sw: ['#ff2bd6', '#00e5ff', '#f9f002'] },
  { id: 'amber', name: 'RETRO', mood: 'old phosphor terminal', sw: ['#ffb000', '#ff6a00', '#ffe08a'] },
  { id: 'gold', name: 'GOLD', mood: 'black and gold, luxury', sw: ['#f5c542', '#fff0b0', '#ff8a3d'] },
  { id: 'candy', name: 'CANDY', mood: 'bubblegum, pastel pop', sw: ['#ff7ac8', '#7ad8ff', '#ffe27a'] },
  { id: 'toxic', name: 'BIOHAZARD', mood: 'acid green, warning stripes', sw: ['#c6ff00', '#ff00e6', '#00ffd0'] },
  { id: 'crimson', name: 'DANGER', mood: 'red alert', sw: ['#ff2a3d', '#ff7a00', '#ffd400'] },
  { id: 'matrix', name: 'MATRIX', mood: 'green on black, hacker', sw: ['#00ff41', '#00b32c', '#b6ff9e'] },
  { id: 'mono', name: 'GHOST', mood: 'monochrome, one red wire', sw: ['#f2f2f2', '#ff3b3b', '#9aa0a6'] },
  { id: 'green', name: 'FOREST', mood: 'green with cyan', sw: ['#39ff88', '#00e5ff', '#d6ff3d'] },
];
const Themes = (() => {
  // Themes are chosen in the terminal (`theme <name>`, `theme` lists them). The default is always cyan: nothing is stored, the choice lasts until the page is reloaded.
  const has = (id) => THEMES.some((t) => t.id === id);
  function set(id) {
    if (!has(id)) return false;
    document.body.dataset.theme = id;
    document.dispatchEvent(new Event('themechange'));
    return true;
  }
  const listHtml = () => THEMES.map((t) => `${span(document.body.dataset.theme === t.id ? 'ok' : 'c1', pad(t.id, 9))}${span('dim', t.mood)}`).join('\n');
  return { set, list: () => THEMES.map((t) => t.id), listHtml };
})();
