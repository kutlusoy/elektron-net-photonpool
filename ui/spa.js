'use strict';
// Single-page shell of the public pages: one document with a persistent title bar (music player, clock), views for the pool and the
// miner page that are swapped in place, so the music keeps playing across pages. Paths: / (pool), /miner and /miner/<address>.
window.SPA = (() => {
  let current = null, token = 0;
  const routeOf = (path) => (/^\/miner(\.html|\/|$)/.test(path) ? 'miner' : 'pool');
  const root = () => document.getElementById('view');

  function cascade(r) {
    const grid = r.querySelector('main.grid');
    if (!grid) return;
    [...grid.querySelectorAll('.pane')].forEach((p, i) => p.style.setProperty('--i', i));
    grid.classList.add('entering');
    setTimeout(() => grid.classList.remove('entering'), 1200);
  }

  async function show(path, instant) {
    const name = routeOf(path), mine = ++token;
    const swap = () => {
      if (mine !== token) return;
      if (current) { try { Views[current].unmount(); } catch (_) { /* view already gone */ } }
      if (window.Depth) Depth.detach();
      const r = root();
      r.textContent = '';
      r.className = 'view v-' + name;
      current = name;
      document.querySelectorAll('[data-nav]').forEach((a) => a.classList.toggle('on', a.dataset.nav === name));
      Views[name].mount(r);
      if (window.Depth) Depth.attach();
      cascade(r);
    };
    if (!instant && current && document.startViewTransition && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
      try { await document.startViewTransition(swap).finished; } catch (_) { /* transition skipped */ }
    } else swap();
  }

  function go(path, replace) {
    if (path === location.pathname + location.search) return;
    history[replace ? 'replaceState' : 'pushState'](null, '', path);
    show(path);
  }

  document.addEventListener('click', (e) => {
    const a = e.target.closest && e.target.closest('a[href]');
    if (!a || a.target || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const u = new URL(a.href, location.href);
    if (u.origin !== location.origin || !(u.pathname === '/' || /^\/miner/.test(u.pathname))) return;
    e.preventDefault();
    go(u.pathname + u.search);
  });
  addEventListener('popstate', () => show(location.pathname));
  document.addEventListener('DOMContentLoaded', () => {
    setInterval(() => { const c = document.getElementById('clock'); if (c) c.textContent = new Date().toISOString().slice(11, 19) + ' UTC'; }, 500);
    show(location.pathname, true);
  });
  return { go };
})();
