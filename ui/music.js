'use strict';
// Original generative dark-synth score for the pool pages: synthesized live with the Web Audio API, no audio files
// (a few KB of code, no downloads). One full arrangement is 224 bars at 88 BPM (about 10 min 10 s), then it loops
// seamlessly. Every note is derived from fixed seeds, so the loop repeats exactly. Starts only on a click (browser rule).
const Music = (() => {
  const BPM = 88, STEP = 60 / BPM / 4, BARS = 224, STEPS = BARS * 16, TOTAL = STEPS * STEP;
  const hz = (n) => 440 * 2 ** ((n - 69) / 12);
  const rng = (seed) => () => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed / 4294967296; };
  const MIN = [0, 3, 7], MAJ = [0, 4, 7];
  // chord progressions: [bass root (MIDI), chord quality]
  const PROG = [
    [[38, MIN], [34, MAJ], [31, MIN], [33, MAJ]], // Dm Bb Gm A
    [[38, MIN], [36, MAJ], [34, MAJ], [33, MAJ]], // Dm C Bb A
  ];
  const PENT = [62, 65, 67, 69, 72, 74, 77]; // D minor pentatonic for the lead
  // 14 sections of 16 bars: drums, hats, bass pattern, arp, pad, lead, progression
  const S = (kick, hat, bass, arp, pad, lead, prog, snare = kick) => ({ kick, hat, bass, arp, pad, lead, prog, snare });
  const SECTIONS = [
    S(0, 0, 0, 0, 1, 0, 0), S(0, 0, 1, 0, 1, 0, 0), S(1, 1, 1, 0, 1, 0, 0), S(1, 1, 2, 1, 1, 0, 0),
    S(2, 2, 2, 1, 1, 1, 0), S(2, 2, 2, 1, 1, 1, 1), S(0, 0, 0, 1, 1, 1, 1), S(0, 1, 1, 1, 1, 0, 0, 3),
    S(2, 2, 3, 1, 1, 1, 1), S(2, 2, 3, 1, 1, 1, 0), S(0, 0, 1, 0, 1, 0, 1), S(2, 2, 3, 1, 1, 1, 0),
    S(1, 1, 2, 1, 1, 0, 1), S(0, 0, 0, 0, 1, 0, 0),
  ];
  const BASSPAT = ['................', 'x.......x.......', 'x..x..x.x..x..x.', 'x.xx.xx.x.xx.xxx'];

  function build(ctx, dest) {
    const out = ctx.createGain(); out.gain.value = 0.5;
    const comp = ctx.createDynamicsCompressor(); comp.threshold.value = -16; comp.ratio.value = 4; comp.attack.value = 0.01; comp.release.value = 0.2;
    out.connect(comp); comp.connect(dest);
    const analyser = ctx.createAnalyser(); analyser.fftSize = 64; analyser.smoothingTimeConstant = 0.8; comp.connect(analyser);
    const dry = ctx.createGain(); dry.connect(out);
    const delay = ctx.createDelay(1.5); delay.delayTime.value = STEP * 6; // dotted eighth
    const fb = ctx.createGain(); fb.gain.value = 0.38; const dlp = ctx.createBiquadFilter(); dlp.type = 'lowpass'; dlp.frequency.value = 2400;
    const wet = ctx.createGain(); wet.gain.value = 0.3;
    delay.connect(dlp); dlp.connect(fb); fb.connect(delay); dlp.connect(wet); wet.connect(out);
    const send = ctx.createGain(); send.connect(delay);
    const nb = ctx.createBuffer(1, ctx.sampleRate, ctx.sampleRate), nd = nb.getChannelData(0);
    for (let i = 0; i < nd.length; i++) nd[i] = Math.random() * 2 - 1;
    const noise = (t, dur, type, f, q, vol, to = dry) => {
      const src = ctx.createBufferSource(); src.buffer = nb; src.loop = true;
      const fl = ctx.createBiquadFilter(); fl.type = type; fl.frequency.value = f; fl.Q.value = q;
      const g = ctx.createGain(); g.gain.setValueAtTime(0.0001, t); g.gain.exponentialRampToValueAtTime(vol, t + 0.003); g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
      src.connect(fl); fl.connect(g); g.connect(to); src.start(t, Math.random() * 0.5); src.stop(t + dur + 0.05);
    };
    const tone = (t, dur, type, f, vol, to, o = {}) => {
      const osc = ctx.createOscillator(); osc.type = type; osc.frequency.setValueAtTime(f, t); if (o.det) osc.detune.value = o.det;
      if (o.to) osc.frequency.exponentialRampToValueAtTime(o.to, t + (o.glide || dur));
      const g = ctx.createGain(); const a = o.att || 0.004;
      g.gain.setValueAtTime(0.0001, t); g.gain.exponentialRampToValueAtTime(vol, t + a); g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
      let node = osc;
      if (o.lp) { const fl = ctx.createBiquadFilter(); fl.type = 'lowpass'; fl.Q.value = o.q || 1; fl.frequency.setValueAtTime(o.lp, t); if (o.lpTo) fl.frequency.exponentialRampToValueAtTime(o.lpTo, t + dur * 0.8); osc.connect(fl); node = fl; }
      node.connect(g); g.connect(to); osc.start(t); osc.stop(t + dur + 0.05);
      return osc;
    };
    const kick = (t, v) => tone(t, 0.32, 'sine', 125, v, dry, { to: 42, glide: 0.12 });
    const snare = (t, v) => { noise(t, 0.2, 'bandpass', 1900, 0.7, v * 0.5); tone(t, 0.12, 'triangle', 190, v * 0.35, dry, { to: 120 }); };
    const hat = (t, v, open) => noise(t, open ? 0.22 : 0.045, 'highpass', 7500, 0.5, v);
    const bass = (t, f, v) => { tone(t, STEP * 1.8, 'sawtooth', f, v, dry, { lp: 700, lpTo: 140, q: 4 }); tone(t, STEP * 1.8, 'sine', f, v * 1.1, dry); };
    const pluck = (t, f, v) => { tone(t, 0.3, 'sawtooth', f, v, dry, { lp: 2800, lpTo: 500, q: 2 }); tone(t, 0.3, 'square', f, v * 0.5, send, { lp: 2400, lpTo: 600, det: 6 }); };
    const pad = (t, dur, notes, v) => notes.forEach((n) => [-9, 0, 9].forEach((d) => { const o = tone(t, dur, 'sawtooth', hz(n), v / 3, send, { att: 1.6, det: d, lp: 650, lpTo: 1100 }); o.frequency.value = hz(n); }));
    const lead = (t, dur, n, v) => {
      const o = tone(t, dur, 'triangle', hz(n), v, send, { att: 0.02, lp: 3000 });
      const lfo = ctx.createOscillator(), lg = ctx.createGain(); lfo.frequency.value = 5; lg.gain.value = 7; lfo.connect(lg); lg.connect(o.detune); lfo.start(t); lfo.stop(t + dur + 0.05);
    };
    // rain on the glass: steady filtered noise, started separately
    const rain = ctx.createGain(); rain.gain.value = 0.045; rain.connect(out);
    let rainSrc = null;
    const startRain = () => {
      rainSrc = ctx.createBufferSource(); rainSrc.buffer = nb; rainSrc.loop = true;
      const bp = ctx.createBiquadFilter(); bp.type = 'bandpass'; bp.frequency.value = 3800; bp.Q.value = 0.35; rainSrc.connect(bp); bp.connect(rain); rainSrc.start();
    };

    function step(i, t) {
      const bar = Math.floor(i / 16), s = i % 16, sec = SECTIONS[Math.floor(bar / 16)], barIn = bar % 16, chord = PROG[sec.prog][bar % 4];
      const r = rng(bar * 7919 + 13);
      if (sec.kick) {
        if (s % 4 === 0) kick(t, 0.95);
        else if (sec.kick === 2 && s === 10 && barIn % 2 === 1) kick(t, 0.6);
        else if (sec.kick === 1 && s === 10) kick(t, 0.5);
      }
      if (sec.snare && (s === 4 || s === 12)) snare(t, 0.7);
      if (sec.snare === 3 && barIn >= 14 && s % 2 === 0) snare(t, 0.25 + 0.05 * ((barIn - 14) * 8 + s / 2));
      if (sec.hat === 1 && s % 2 === 0) hat(t, s % 4 === 2 ? 0.05 : 0.025);
      if (sec.hat === 2) { if (s % 2 === 0 || r() < 0.3) hat(t, s % 4 === 2 ? 0.06 : 0.03, s === 14 && barIn % 2 === 1); }
      if (barIn === 15 && sec.snare && s >= 12 && sec.snare !== 3) snare(t, 0.4);
      const bp = BASSPAT[sec.bass];
      if (bp[s] === 'x') bass(t, hz(chord[0] + (s === 11 || s === 14 ? 12 : 0)), 0.34);
      if (s === 0 && sec.pad) pad(t, STEP * 17, chord[1].map((o) => chord[0] + 24 + o), 0.075);
      if (sec.arp && (s % 2 === 0 || (sec.arp === 1 && r() < 0.28))) {
        const tones = chord[1].map((o) => chord[0] + 36 + o), k = (Math.floor(s / 2) + (barIn >> 2)) % tones.length;
        pluck(t, hz(tones[k] + (r() < 0.25 ? 12 : 0)), s % 4 === 0 ? 0.11 : 0.07);
      }
      if (sec.lead && s % 4 === 0 && barIn % 2 === 0 && r() < 0.75) lead(t, STEP * (4 + 4 * Math.floor(r() * 3)), PENT[Math.floor(r() * PENT.length)], 0.09);
    }
    return { step, out, analyser, startRain, stopRain() { if (rainSrc) rainSrc.stop(); } };
  }

  /* ---- player ---- */
  const KEY = 'photon.music.volume';
  const P = { ctx: null, b: null, master: null, timer: 0, next: 0, step: 0, t0: 0, state: 'stopped', vol: 0.35 };
  try { const v = parseFloat(localStorage.getItem(KEY)); if (v >= 0 && v <= 1) P.vol = v; } catch (_) { /* storage unavailable */ }
  const gainOf = (v) => v * v;

  function pump() {
    while (P.next < P.ctx.currentTime + 0.25) { P.b.step(P.step, P.next); P.step = (P.step + 1) % STEPS; P.next += STEP; }
  }
  function play() {
    if (P.state === 'playing') return;
    if (P.state === 'paused') { P.ctx.resume(); P.state = 'playing'; return; }
    const AC = window.AudioContext || window.webkitAudioContext; if (!AC) return;
    P.ctx = new AC(); P.master = P.ctx.createGain(); P.master.gain.value = gainOf(P.vol); P.master.connect(P.ctx.destination);
    P.b = build(P.ctx, P.master); P.b.startRain();
    P.step = 0; P.next = P.t0 = P.ctx.currentTime + 0.1; pump(); P.timer = setInterval(pump, 60); P.state = 'playing';
  }
  function pause() { if (P.state !== 'playing') return; P.ctx.suspend(); P.state = 'paused'; }
  function stop() {
    if (P.state === 'stopped') return;
    clearInterval(P.timer); const ctx = P.ctx; P.state = 'stopped';
    try { P.master.gain.cancelScheduledValues(ctx.currentTime); P.master.gain.setTargetAtTime(0, ctx.currentTime, 0.05); } catch (_) { /* already closed */ }
    setTimeout(() => ctx.close().catch(() => {}), 400); P.ctx = P.b = P.master = null;
  }
  function volume(v) {
    P.vol = Math.max(0, Math.min(1, v));
    if (P.master) P.master.gain.setTargetAtTime(gainOf(P.vol), P.ctx.currentTime, 0.03);
    try { localStorage.setItem(KEY, String(P.vol)); } catch (_) { /* ignore */ }
  }
  const time = () => (P.ctx && P.state !== 'stopped' ? Math.max(0, P.ctx.currentTime - P.t0) % TOTAL : 0);

  /* ---- widget in the title bar ---- */
  function mount() {
    const bar = document.querySelector('.titlebar'); if (!bar || bar.querySelector('.music')) return;
    const w = document.createElement('span'); w.className = 'music';
    const mk = (cls, txt, title) => { const b = document.createElement('button'); b.type = 'button'; b.className = cls; b.textContent = txt; b.title = title; b.setAttribute('aria-label', title); w.append(b); return b; };
    const pp = mk('mbtn', '▶', 'play music'), st = mk('mbtn', '■', 'stop music');
    const eq = document.createElement('canvas'); eq.className = 'meq'; eq.width = 40; eq.height = 14; w.append(eq);
    const vol = document.createElement('input'); vol.type = 'range'; vol.min = 0; vol.max = 100; vol.value = Math.round(P.vol * 100); vol.className = 'mvol'; vol.title = 'volume'; vol.setAttribute('aria-label', 'music volume'); w.append(vol);
    const tm = document.createElement('span'); tm.className = 'mtime'; tm.textContent = '--:-- / ' + fmt(TOTAL); w.append(tm);
    const clock = bar.querySelector('.clock'); bar.insertBefore(w, clock);
    const g = eq.getContext('2d'), data = new Uint8Array(32);
    const sync = () => { pp.textContent = P.state === 'playing' ? '❚❚' : '▶'; pp.title = P.state === 'playing' ? 'pause music' : 'play music'; w.classList.toggle('on', P.state === 'playing'); };
    pp.addEventListener('click', () => { if (P.state === 'playing') pause(); else play(); sync(); });
    st.addEventListener('click', () => { stop(); sync(); tm.textContent = '00:00 / ' + fmt(TOTAL); });
    vol.addEventListener('input', () => volume(vol.value / 100));
    const color = () => getComputedStyle(document.body).getPropertyValue('--c1').trim() || '#0ff';
    setInterval(() => {
      if (P.state !== 'stopped') tm.textContent = `${fmt(time())} / ${fmt(TOTAL)}`;
      g.clearRect(0, 0, 40, 14);
      if (P.state === 'playing' && P.b) {
        P.b.analyser.getByteFrequencyData(data); g.fillStyle = color(); g.shadowColor = g.fillStyle; g.shadowBlur = 4;
        for (let i = 0; i < 8; i++) { const v = data[i * 2 + 1] / 255, h = Math.max(1, v * 13); g.fillRect(i * 5, 14 - h, 3, h); }
      }
    }, 66);
  }
  const fmt = (s) => `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mount); else mount();

  return { play, pause, stop, volume, time, total: TOTAL, state: () => P.state, _build: build, _STEP: STEP, _STEPS: STEPS };
})();
