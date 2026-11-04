'use strict';
// Depth view: a pane can be maximized (button in its title, double-click on the title, click on a pane in
// the background); the other panes then recede in 3D and blur. Esc, the button or a double-click goes back.
(() => {
  const grid = document.querySelector('main.grid');
  if (!grid) return;
  const narrow = matchMedia('(max-width: 720px)');
  let cur = null, ph = null, timer = 0;

  const px = (v) => `${Math.round(v)}px`;
  const place = (el, r) => { el.style.left = px(r.left); el.style.top = px(r.top); el.style.width = px(r.width); el.style.height = px(r.height); };
  // layout box relative to the grid, unaffected by the 3D transforms
  const rel = (el) => ({ left: el.offsetLeft, top: el.offsetTop, width: el.offsetWidth, height: el.offsetHeight });
  const full = () => {
    const cs = getComputedStyle(grid), g = { width: grid.clientWidth, height: grid.clientHeight };
    const l = parseFloat(cs.paddingLeft), t = parseFloat(cs.paddingTop);
    const w = g.width - l - parseFloat(cs.paddingRight), h = g.height - t - parseFloat(cs.paddingBottom);
    // leave a margin so that the receding panes stay visible around the front one
    const mx = w * 0.11, my = h * 0.12;
    return { left: l + mx, top: t + my, width: w - 2 * mx, height: h - 2 * my };
  };
  const clean = () => {
    if (!cur) return;
    cur.classList.remove('front');
    cur.style.left = cur.style.top = cur.style.width = cur.style.height = '';
    if (ph) ph.remove();
    cur = ph = null;
  };

  function focus(p) {
    if (narrow.matches || p === cur) return;
    clearTimeout(timer);
    clean();
    const gw = grid.clientWidth, gh = grid.clientHeight;
    for (const q of grid.querySelectorAll('.pane')) {
      const r = rel(q === cur ? ph : q);
      q.style.setProperty('--dx', (((r.left + r.width / 2) - gw / 2) / gw * 2).toFixed(3));
      q.style.setProperty('--dy', (((r.top + r.height / 2) - gh / 2) / gh * 2).toFixed(3));
    }
    const start = rel(p);
    ph = document.createElement('div');
    ph.style.gridArea = getComputedStyle(p).gridArea;
    p.before(ph);
    cur = p;
    place(p, start);
    p.classList.add('front');
    p.getBoundingClientRect();
    grid.classList.add('depth');
    place(p, full());
  }

  function restore() {
    if (!cur) return;
    const p = cur, home = rel(ph);
    grid.classList.remove('depth');
    place(p, home);
    timer = setTimeout(() => { if (cur === p) clean(); }, 650);
  }

  const toggle = (p) => (cur === p && grid.classList.contains('depth') ? restore() : focus(p));

  for (const p of grid.querySelectorAll('.pane')) {
    const h = p.querySelector('h2');
    if (!h) continue;
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'max';
    b.textContent = '⤢';
    b.title = 'maximize (Esc to go back)';
    b.setAttribute('aria-label', 'maximize ' + (p.getAttribute('aria-label') || 'pane'));
    b.addEventListener('click', (e) => { e.stopPropagation(); toggle(p); });
    h.append(b);
    h.addEventListener('dblclick', () => toggle(p));
    p.addEventListener('click', () => { if (grid.classList.contains('depth') && p !== cur) focus(p); });
  }
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') restore(); });
  grid.addEventListener('mousemove', (e) => {
    if (!grid.classList.contains('depth')) return;
    const g = grid.getBoundingClientRect();
    grid.style.setProperty('--mx', (((e.clientX - g.left) / g.width) * 2 - 1).toFixed(3));
    grid.style.setProperty('--my', (((e.clientY - g.top) / g.height) * 2 - 1).toFixed(3));
  });
  narrow.addEventListener('change', () => { if (narrow.matches) { grid.classList.remove('depth'); clean(); } });
  addEventListener('resize', () => { if (cur && grid.classList.contains('depth')) place(cur, full()); });
})();
