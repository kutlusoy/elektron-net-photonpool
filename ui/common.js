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
