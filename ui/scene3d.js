'use strict';
// The engine as a small 3D scene (Three.js, loaded only for the ENGINE view): the node and the chain behind it, the block template that is assembled
// from the candidates, the allocator with its work units, the gate where shares are checked, and the miners. Jobs fly from the template through the
// allocator and the gate to the miners, shares fly back; accepted shares reach the node, stale and invalid ones fall at the gate. All of it is driven by
// the real counters of the pool (candidates, epoch use, jobs/s, shares/s and their outcome ratio, miners online, new tips, found blocks).
// Cost: one draw call per object kind (instanced meshes), basic materials without lights, ~30 frames per second, paused while hidden.
window.Flow3D = (() => {
  const T = window.THREE;
  const MAXJ = 150, MAXS = 90, MAXM = 150, TILES = 64, MAXC = 16, CHAIN = 7;
  const X = { node: -17, attest: -12.5, tpl: -8, alloc: -3, gate: 2, miners: 8 };
  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
  const ease = (t) => 1 - Math.pow(1 - clamp(t, 0, 1), 3);

  function label(text, color, scale = 1) {
    const c = document.createElement('canvas'); c.width = 256; c.height = 64;
    const g = c.getContext('2d'); g.font = '600 28px ui-monospace, Menlo, monospace'; g.textAlign = 'center'; g.textBaseline = 'middle';
    g.fillStyle = color; g.fillText(text, 128, 34);
    const tex = new T.CanvasTexture(c);
    const m = new T.Sprite(new T.SpriteMaterial({ map: tex, transparent: true, depthWrite: false }));
    m.scale.set(4 * scale, 1 * scale, 1); m.userData.tex = tex;
    return m;
  }


  // Smooth glowing lines of constant pixel width (the idea of Line2/LineMaterial): every segment is an instanced quad that is widened in screen space,
  // has soft edges (no staircase), a fade along its length and with the distance, and is added up additively. One draw call per set of lines.
  const VS = `
    uniform vec2 res; uniform float pxr; uniform float near;
    attribute vec2 corner; attribute vec3 iS; attribute vec3 iE; attribute vec4 iC; attribute vec3 iP; // iP: width in css px, alpha at start, alpha at end
    varying vec4 vC; varying vec2 vA; varying float vY; varying float vD; varying float vX;
    void main() {
      vec4 vs = modelViewMatrix * vec4(iS, 1.0), ve = modelViewMatrix * vec4(iE, 1.0);
      if (vs.z > -near && ve.z > -near) { gl_Position = vec4(2.0, 2.0, 2.0, 1.0); return; }
      if (vs.z > -near) vs += (ve - vs) * ((-near - vs.z) / (ve.z - vs.z));
      if (ve.z > -near) ve += (vs - ve) * ((-near - ve.z) / (vs.z - ve.z));
      vec4 cs = projectionMatrix * vs, ce = projectionMatrix * ve;
      vec2 ps = cs.xy / cs.w * res * 0.5, pe = ce.xy / ce.w * res * 0.5, d = pe - ps;
      float len = length(d); vec2 dir = len > 0.0001 ? d / len : vec2(1.0, 0.0), nrm = vec2(-dir.y, dir.x);
      float t = corner.x, w = iP.x * pxr;
      vec4 c = mix(cs, ce, t);
      vec2 off = (nrm * corner.y * w * 0.5 + dir * (t * 2.0 - 1.0) * w * 0.5) / (res * 0.5);
      gl_Position = vec4(c.xy + off * c.w, c.z, c.w);
      vC = iC; vA = iP.yz; vY = corner.y; vX = t; vD = -mix(vs.z, ve.z, t);
    }`;
  const FS = `
    varying vec4 vC; varying vec2 vA; varying float vY; varying float vD; varying float vX;
    void main() {
      float edge = 1.0 - smoothstep(0.35, 1.0, abs(vY));
      float a = edge * edge * mix(vA.x, vA.y, vX) * (1.0 - smoothstep(25.0, 70.0, vD));
      gl_FragColor = vec4(vC.rgb * a, a);
    }`;
  function fatLines(cap) {
    const g = new T.InstancedBufferGeometry();
    g.setAttribute('position', new T.Float32BufferAttribute(new Float32Array(12), 3)); // unused, keeps three.js happy
    g.setAttribute('corner', new T.Float32BufferAttribute([0, -1, 1, -1, 1, 1, 0, 1], 2)); g.setIndex([0, 1, 2, 0, 2, 3]);
    const mk = (n) => new T.InstancedBufferAttribute(new Float32Array(cap * n), n);
    const aS = mk(3), aE = mk(3), aC = mk(4), aP = mk(3);
    g.setAttribute('iS', aS); g.setAttribute('iE', aE); g.setAttribute('iC', aC); g.setAttribute('iP', aP);
    const mat = new T.ShaderMaterial({ vertexShader: VS, fragmentShader: FS, transparent: true, depthWrite: false, blending: T.AdditiveBlending, uniforms: { res: { value: new T.Vector2(1, 1) }, pxr: { value: 1 }, near: { value: 0.2 } } });
    const mesh = new T.Mesh(g, mat); mesh.frustumCulled = false; let n = 0;
    return {
      mesh, mat, geo: g,
      clear() { n = 0; },
      // a: start, b: end ({x,y,z}), c: THREE.Color, w: width in css px, a0/a1: alpha at the ends
      add(a, b, c, w, a0 = 1, a1 = 1) {
        if (n >= cap) return; const i = n++;
        aS.setXYZ(i, a.x, a.y, a.z); aE.setXYZ(i, b.x, b.y, b.z); aC.setXYZW(i, c.r, c.g, c.b, 1); aP.setXYZ(i, w, a0, a1);
      },
      commit() { g.instanceCount = n; aS.needsUpdate = aE.needsUpdate = aC.needsUpdate = aP.needsUpdate = true; },
    };
  }

  function create(canvas) {
    const renderer = new T.WebGLRenderer({ canvas, antialias: true, alpha: true, powerPreference: 'default' });
    renderer.setClearColor(0x000000, 0);
    const scene = new T.Scene(), world = new T.Group(); world.rotation.y = -0.6; scene.add(world); // the pipeline runs diagonally into the depth: the node at the horizon, the miners in front
    const cam = new T.PerspectiveCamera(52, 1, 0.1, 140);
    const D = new T.Object3D(), tmp = new T.Color();
    const col = {}, disposables = [];
    const cs = getComputedStyle(document.body);
    for (const k of ['c1', 'c2', 'c3', 'ok', 'warn', 'err', 'dim']) col[k] = new T.Color(cs.getPropertyValue('--' + k).trim() || '#00e5ff');
    col.line = col.c1.clone().multiplyScalar(0.35); // the CSS line color is a color-mix() that Three.js cannot read
    const track = (o) => { disposables.push(o); return o; };
    const add = (o) => { world.add(o); return o; };
    const glow = (c, o = 1) => track(new T.MeshBasicMaterial({ color: c, transparent: true, opacity: o, blending: T.AdditiveBlending, depthWrite: false }));

    // floor and station frames
    const lines = fatLines(700), trails = fatLines(420);
    add(lines.mesh); add(trails.mesh); track(lines.geo); track(lines.mat); track(trails.geo); track(trails.mat);
    const V = (x, y, z) => ({ x, y, z });
    // floor grid that fades into the distance, with a brighter beam along the pipeline
    for (let x = -26; x <= 14; x += 2) lines.add(V(x, -1.7, -14), V(x, -1.7, 14), col.line, 1.1, 0.5, 0.5);
    for (let z = -14; z <= 14; z += 2) lines.add(V(-26, -1.7, z), V(14, -1.7, z), col.line, 1.1, 0.5, 0.5);
    lines.add(V(X.node, -1.7, 0), V(X.miners, -1.7, 0), col.c1, 2.2, 0.9, 0.35); lines.add(V(X.node, -1.7, 0), V(X.miners, -1.7, 0), col.c1, 9, 0.18, 0.06);
    // station frames: a sharp line and a wide faint one on top give the glow
    const box = (x, w, h, d, c) => {
      const xs = [x - w / 2, x + w / 2], ys = [-0.2 - h / 2, -0.2 + h / 2], zs = [-d / 2, d / 2], P8 = [];
      for (const X1 of xs) for (const Y1 of ys) for (const Z1 of zs) P8.push(V(X1, Y1, Z1));
      const E = [[0, 1], [2, 3], [4, 5], [6, 7], [0, 2], [1, 3], [4, 6], [5, 7], [0, 4], [1, 5], [2, 6], [3, 7]];
      for (const [i, j] of E) { lines.add(P8[i], P8[j], c, 1.4, 0.8, 0.8); lines.add(P8[i], P8[j], c, 7, 0.14, 0.14); }
    };
    box(X.node, 2.4, 2.4, 2.4, col.c1); box(X.attest, 0.6, 3.2, 3.2, col.ok); box(X.tpl, 3.6, 2.4, 3.6, col.c2); box(X.alloc, 3.6, 0.6, 3.6, col.c3); box(X.gate, 0.5, 3.2, 4.4, col.ok); box(X.miners, 6, 3.6, 10, col.c1);
    lines.commit();
    const labels = [['NODE', X.node, 2.6, col.c1], ['ATTEST', X.attest, 4.1, col.ok], ['TEMPLATE', X.tpl, 2.9, col.c2], ['ALLOCATOR', X.alloc, 1.6, col.c3], ['SHARE CHECK', X.gate, 3.8, col.ok], ['MINERS', X.miners, 3.4, col.c1]];
    // soft halos behind the stations (one radial texture, additive sprites)
    const hc = document.createElement('canvas'); hc.width = hc.height = 64; { const g = hc.getContext('2d'), r = g.createRadialGradient(32, 32, 0, 32, 32, 32); r.addColorStop(0, 'rgba(255,255,255,.9)'); r.addColorStop(1, 'rgba(255,255,255,0)'); g.fillStyle = r; g.fillRect(0, 0, 64, 64); }
    const haloTex = track(new T.CanvasTexture(hc));
    const halo = (x, y, c, size, o) => { const h = new T.Sprite(track(new T.SpriteMaterial({ map: haloTex, color: c, transparent: true, opacity: o, blending: T.AdditiveBlending, depthWrite: false }))); h.position.set(x, y, 0); h.scale.set(size, size, 1); add(h); return h; };
    const halos = [halo(X.node, -0.2, col.c1, 5, 0.55), halo(X.attest, -0.2, col.ok, 4, 0.4), halo(X.tpl, -0.2, col.c2, 6, 0.4), halo(X.alloc, -0.4, col.c3, 6, 0.35), halo(X.gate, -0.2, col.ok, 4.5, 0.3), halo(X.miners, -0.2, col.c1, 10, 0.18)];
    labels.forEach(([t, x, y, c]) => { const s = label(t, '#' + c.getHexString(), 0.9 + Math.max(0, (8 - x) / 22)); s.position.set(x, y, 0); add(s); track(s.material); track(s.userData.tex); });

    // attestation: the candidates pass this ring on their way from the node to the template and come out checked
    const attest = add(new T.Mesh(track(new T.TorusGeometry(1.5, 0.07, 8, 40)), glow(col.ok, 0.9))); attest.position.set(X.attest, -0.2, 0); attest.rotation.y = Math.PI / 2;
    const attest2 = add(new T.Mesh(track(new T.TorusGeometry(1.1, 0.04, 6, 32)), glow(col.ok, 0.6))); attest2.position.set(X.attest, -0.2, 0); attest2.rotation.y = Math.PI / 2;
    let attestFlash = 0, infoLabel = label('', '#' + col.ok.getHexString(), 0.8); infoLabel.position.set(X.attest, -2.3, 0); add(infoLabel); track(infoLabel.material);
    // node core and chain behind it
    const core = add(new T.Mesh(track(new T.IcosahedronGeometry(0.7, 1)), glow(col.c1, 0.85)));
    core.position.set(X.node, -0.2, 0);
    // glimmer at the horizon: rays rotating around the node, orbiting sparkles, a pulsing corona; everything flares up for a new tip
    const sc = document.createElement('canvas'); sc.width = 32; sc.height = 256; { const g = sc.getContext('2d'), v = g.createLinearGradient(0, 0, 0, 256); v.addColorStop(0, 'rgba(255,255,255,0)'); v.addColorStop(0.5, 'rgba(255,255,255,1)'); v.addColorStop(1, 'rgba(255,255,255,0)'); g.fillStyle = v; g.fillRect(0, 0, 32, 256); g.globalCompositeOperation = 'destination-in'; const h = g.createLinearGradient(0, 0, 32, 0); h.addColorStop(0, 'rgba(0,0,0,0)'); h.addColorStop(0.5, 'rgba(0,0,0,1)'); h.addColorStop(1, 'rgba(0,0,0,0)'); g.fillStyle = h; g.fillRect(0, 0, 32, 256); }
    const streak = track(new T.CanvasTexture(sc));
    const rays = Array.from({ length: 12 }, (_, i) => { const r = new T.Sprite(track(new T.SpriteMaterial({ map: streak, color: i % 3 === 0 ? col.c2 : col.c1, transparent: true, opacity: 0.25, blending: T.AdditiveBlending, depthWrite: false }))); r.position.set(X.node, -0.2, 0); r.scale.set(0.5, 7 + (i % 4) * 1.6, 1); add(r); return r; });
    const SPK = 70, spk = new Float32Array(SPK * 3); for (let i = 0; i < SPK; i++) { const a = Math.random() * 6.283, r = 1.2 + Math.random() * 2.6; spk[i * 3] = 0; spk[i * 3 + 1] = Math.sin(a) * r * 0.7; spk[i * 3 + 2] = Math.cos(a) * r; }
    const spg = track(new T.BufferGeometry()); spg.setAttribute('position', new T.BufferAttribute(spk, 3));
    const sparkles = add(new T.Points(spg, track(new T.PointsMaterial({ color: col.c1, size: 0.12, transparent: true, opacity: 0.9, blending: T.AdditiveBlending, depthWrite: false })))); sparkles.position.set(X.node, -0.2, 0);
    let flare = 0;
    // photon fire: a stream of glowing particles leaves the node toward the miners, white-hot at the source, cooling to cyan and magenta, flickering
    // upward like flames; the number of particles follows the hash rate and a new tip makes a burst. One Points object, positions updated on the CPU.
    const FN = 600, fp = new Float32Array(FN * 3), fv = new Float32Array(FN * 3), fl = new Float32Array(FN), fm = new Float32Array(FN), fc = new Float32Array(FN * 3);
    for (let i = 0; i < FN; i++) { fp[i * 3 + 1] = -50; fl[i] = 0; }
    const fg = track(new T.BufferGeometry()); fg.setAttribute('position', new T.BufferAttribute(fp, 3)); fg.setAttribute('color', new T.BufferAttribute(fc, 3));
    const fire = add(new T.Points(fg, track(new T.PointsMaterial({ size: 0.45, map: haloTex, vertexColors: true, transparent: true, opacity: 1, blending: T.AdditiveBlending, depthWrite: false })))); fire.frustumCulled = false;
    const white = new T.Color(1, 1, 1); let fireAcc = 0;
    const emit = (i) => {
      const a = Math.random() * 6.283, r = Math.random() * 0.55;
      fp[i * 3] = X.node + 0.4; fp[i * 3 + 1] = -0.2 + Math.cos(a) * r; fp[i * 3 + 2] = Math.sin(a) * r;
      const sp = 2.2 + Math.random() * 4.5;
      fv[i * 3] = sp; fv[i * 3 + 1] = (Math.random() - 0.35) * 1.2; fv[i * 3 + 2] = (Math.random() - 0.5) * 1.6;
      fm[i] = 1.6 + Math.random() * 2.4; fl[i] = fm[i];
    };
    const chainGeo = track(new T.BoxGeometry(0.9, 0.9, 0.9)), chainMat = track(new T.MeshBasicMaterial({ color: col.c1, wireframe: true, transparent: true, opacity: 0.6 }));
    const chain = [];
    const placeChain = () => chain.forEach((c, i) => { c.tx = X.node - 2.6 - i * 1.5; });
    const pushBlock = (gold) => {
      const m = new T.Mesh(chainGeo, gold ? track(new T.MeshBasicMaterial({ color: col.warn, wireframe: true })) : chainMat);
      m.position.set(X.node, -0.2, 0); m.scale.setScalar(0.01); add(m);
      chain.unshift({ m, tx: X.node, born: performance.now() });
      while (chain.length > CHAIN) { const o = chain.pop(); world.remove(o.m); }
      placeChain();
    };
    for (let i = 0; i < 4; i++) pushBlock(false);

    // template: candidate blocks assembled from parts
    const tplGeo = track(new T.BoxGeometry(0.62, 0.62, 0.62)), tplMat = glow(col.c2, 0.8);
    const tpl = add(new T.InstancedMesh(tplGeo, tplMat, MAXC)); tpl.position.set(X.tpl, -0.2, 0); for (let i = 0; i < MAXC; i++) tpl.setColorAt(i, col.c2);
    const crossed = new Array(MAXC).fill(true);
    const slot = (i) => [((i % 4) - 1.5) * 0.85, (Math.floor(i / 8) - 0.5) * 0.85, (Math.floor((i % 8) / 4) - 0.5) * 0.85];
    const seeds = Array.from({ length: MAXC }, () => [-3.2 - Math.random() * 2.2, (Math.random() - 0.5) * 2.4, (Math.random() - 0.5) * 3]);
    const parts = ['coinbase', 'merkle', 'ntime', 'nonce'].map((t, i) => { const s = label(t, '#' + col.c2.getHexString(), 0.5); s.position.set(X.tpl + Math.cos(i * 1.57) * 2.6, 0.9 + (i % 2) * 0.5, Math.sin(i * 1.57) * 2.6); add(s); track(s.material); track(s.userData.tex); return s; });

    // allocator: 8x8 work units, filled as the epoch is used up
    const tileGeo = track(new T.BoxGeometry(0.36, 0.1, 0.36)), tiles = add(new T.InstancedMesh(tileGeo, track(new T.MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.9 })), TILES));
    for (let i = 0; i < TILES; i++) { D.position.set(X.alloc + ((i % 8) - 3.5) * 0.42, -0.45, (Math.floor(i / 8) - 3.5) * 0.42); D.rotation.set(0, 0, 0); D.scale.setScalar(1); D.updateMatrix(); tiles.setMatrixAt(i, D.matrix); tiles.setColorAt(i, col.dim); }
    let usedShown = -1;

    // miners: a faint field of slots, the online ones lit
    const mpos = Array.from({ length: MAXM }, (_, i) => { const a = i * 2.399963, r = 0.6 + 3.8 * Math.sqrt((i + 0.5) / MAXM); return new T.Vector3(X.miners + Math.cos(a) * r * 0.55, -1.2 + ((i * 37) % 17) / 17 * 2.6, Math.sin(a) * r); });
    const mGeo = track(new T.OctahedronGeometry(0.22)), miners = add(new T.InstancedMesh(mGeo, glow(col.ok, 0.95), MAXM));
    const pg = track(new T.BufferGeometry()); pg.setAttribute('position', new T.Float32BufferAttribute(mpos.flatMap((p) => [p.x, p.y, p.z]), 3));
    add(new T.Points(pg, track(new T.PointsMaterial({ color: col.dim, size: 0.09, transparent: true, opacity: 0.45 }))));
    const pulse = new Float32Array(MAXM);

    // gateways (Photon): mining gateways of other operators that connect to the pool and bring the shares of their own miners
    const MAXG = 12, gwGeo = track(new T.OctahedronGeometry(0.38)), gwMesh = add(new T.InstancedMesh(gwGeo, glow(col.c3, 0.95), MAXG));
    const gwPos = Array.from({ length: MAXG }, (_, i) => new T.Vector3(X.miners - 3.2 + (i % 3) * 1.5, -0.4 + (Math.floor(i / 3) % 2) * 1.1, -7.2 - Math.floor(i / 3) * 1.5));
    const CH = 6, chArr = new Float32Array(MAXG * CH * 3), chGeo = track(new T.BufferGeometry()); chGeo.setAttribute('position', new T.BufferAttribute(chArr, 3));
    const chPts = add(new T.Points(chGeo, track(new T.PointsMaterial({ color: col.c3, size: 0.1, transparent: true, opacity: 0.8, blending: T.AdditiveBlending, depthWrite: false }))));
    let gwLabel = label('', '#' + col.c3.getHexString(), 0.8); gwLabel.position.set(X.miners - 1.6, 2.4, -8.6); add(gwLabel); track(gwLabel.material);
    const gwHalo = halo(X.miners - 1.6, -0.2, col.c3, 6, 0); let gwShown = -1, gateFlash = 0;
    // particles
    const sph = track(new T.SphereGeometry(0.11, 6, 5));
    const jobs = add(new T.InstancedMesh(sph, glow(col.c1, 0.95), MAXJ)), shares = add(new T.InstancedMesh(sph, track(new T.MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.95, blending: T.AdditiveBlending, depthWrite: false })), MAXS));
    const J = Array.from({ length: MAXJ }, () => ({ on: false, t: 0, v: 0, m: 0 })), S = Array.from({ length: MAXS }, () => ({ on: false, t: 0, v: 0, m: 0, o: 0 }));
    const hide = (mesh, i) => { D.position.set(0, -50, 0); D.scale.setScalar(0.001); D.updateMatrix(); mesh.setMatrixAt(i, D.matrix); };
    for (let i = 0; i < MAXJ; i++) hide(jobs, i);
    for (let i = 0; i < MAXS; i++) { hide(shares, i); shares.setColorAt(i, col.ok); }
    // the tail of a streak lies a fixed length behind the particle, in its direction of travel (independent of the frame rate)
    const trailTo = (q) => { const dx = P.x - q.px, dy = P.y - q.py, dz = P.z - q.pz, l = Math.hypot(dx, dy, dz) || 1; tail.set(P.x - dx / l * 1.5, P.y - dy / l * 1.5, P.z - dz / l * 1.5); };
    const W = [new T.Vector3(X.tpl + 1.9, -0.2, 0), new T.Vector3(X.alloc, -0.2, 0), new T.Vector3(X.gate, -0.2, 0)];
    const lerp3 = (a, b, k, out) => out.set(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k, a.z + (b.z - a.z) * k);
    const P = new T.Vector3(), tail = new T.Vector3(), nodePos = new T.Vector3(X.node + 1, -0.2, 0);

    // found block and tip rings
    const ringGeo = track(new T.RingGeometry(0.9, 1, 48)), rings = [];
    const ring = (c) => { const m = new T.Mesh(ringGeo, track(new T.MeshBasicMaterial({ color: c, transparent: true, opacity: 0.9, side: T.DoubleSide, blending: T.AdditiveBlending, depthWrite: false }))); m.rotation.x = -Math.PI / 2; m.position.set(X.node, -1.6, 0); add(m); rings.push({ m, t: 0 }); };
    let gold = null, assemble = 1, heightLabel = label('', '#' + col.c3.getHexString(), 0.9);
    heightLabel.position.set(X.tpl, 1.7, 0); add(heightLabel); track(heightLabel.material);

    const st = { hr: 0, gw: 0, chn: 0, photon: false, pshares: 0, pRej: 0, checks: 0, accG: 0, w: 0, h: 0, run: false, raf: 0, last: 0, vis: true, jobsT: 0, sharesT: 0, jobs: 0, shares: 0, accJ: 0, accS: 0, cands: 8, used: 0, online: 0, okF: 1, staleF: 0, badF: 0, tip: 0, t: 0 };
    const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
    // Quality levels. 0: the small pane (few particles, 12 fps, no bloom), 1: maximized on a modest device, 2: maximized (bloom, streaks, 30 fps).
    // The controller never goes above 0 while the pane is small and steps down by itself when the frame rate cannot keep up.
    const LV = [
      { fps: 12, dpr: 1, bloom: false, jobs: 36, shares: 24, fire: 140, rate: 0.5, trails: false, sway: 0.35 },
      { fps: 24, dpr: 1.25, bloom: true, jobs: 100, shares: 60, fire: 320, rate: 0.8, trails: true, sway: 0.7 },
      { fps: 30, dpr: 1.75, bloom: true, jobs: 150, shares: 90, fire: 600, rate: 1, trails: true, sway: 1 },
    ];
    canvas.dataset.level = '0'; st.lv = 0; st.cap = 2; st.slow = 0; st.avg = 0; st.t0 = performance.now(); let q = LV[0];
    const pane = canvas.closest('.pane'), isFront = () => !!(pane && pane.classList.contains('front'));
    const size = () => { const r = canvas.getBoundingClientRect(); st.w = Math.max(10, r.width); st.h = Math.max(10, r.height); const dpr = Math.min(q.dpr, devicePixelRatio || 1); st.dpr = dpr; renderer.setPixelRatio(dpr); renderer.setSize(st.w, st.h, false); cam.aspect = st.w / st.h; cam.updateProjectionMatrix(); for (const f of [lines, trails]) { f.mat.uniforms.res.value.set(st.w * dpr, st.h * dpr); f.mat.uniforms.pxr.value = dpr; } bloomSize(); };
    const ro = new ResizeObserver(size); ro.observe(canvas);
    const io = new IntersectionObserver((e) => { st.vis = e[0].isIntersecting; kick(); }); io.observe(canvas);
    const onVis = () => kick(); document.addEventListener('visibilitychange', onVis);

    function spawnJob() { const j = J.slice(0, q.jobs).find((o) => !o.on); if (!j) return; Object.assign(j, { px: undefined, on: true, t: 0, v: 0.22 + Math.random() * 0.16, m: (Math.random() * Math.max(1, Math.min(MAXM, st.online))) | 0 }); }
    function spawnShare(viaGateway) {
      const idx = S.slice(0, q.shares).findIndex((o) => !o.on); if (idx < 0) return;
      const s = S[idx], n = Math.max(1, Math.min(MAXM, st.online)), r = Math.random();
      const o = viaGateway ? (r < st.pRej ? 2 : 0) : (r < st.okF ? 0 : r < st.okF + st.staleF ? 1 : 2);
      Object.assign(s, { px: undefined, on: true, t: 0, v: 0.3 + Math.random() * 0.2, m: viaGateway ? 0 : (Math.random() * n) | 0, o, c: false, g: viaGateway ? (Math.random() * Math.min(MAXG, Math.max(1, st.gw))) | 0 : -1 });
      shares.setColorAt(idx, viaGateway ? col.c3 : col.c1); if (shares.instanceColor) shares.instanceColor.needsUpdate = true;
      if (!viaGateway) pulse[s.m] = 1;
    }

    function update(dt) {
      st.t += dt;
      const a = 1 - Math.exp(-dt * 2);
      st.jobs += (st.jobsT - st.jobs) * a; st.shares += (st.sharesT - st.shares) * a;
      // a small base keeps the scene alive; above it the number of particles grows logarithmically with the real rate
      st.accJ += dt * q.rate * (2.5 + 8 * Math.log10(1 + st.jobs)); st.accS += dt * q.rate * (0.8 + 6 * Math.log10(1 + st.shares * 4));
      while (st.accJ >= 1) { st.accJ -= 1; spawnJob(); }
      while (st.accS >= 1 && st.online > 0) { st.accS -= 1; spawnShare(false); }
      st.accS = Math.min(st.accS, 2);
      if (st.gw > 0) { st.accG += dt * q.rate * (0.5 + 6 * Math.log10(1 + st.pshares * 4)); while (st.accG >= 1) { st.accG -= 1; spawnShare(true); } st.accG = Math.min(st.accG, 2); }
      // camera sways slowly around the scene
      const k = st.t * 0.12, intro = Math.min(1, (performance.now() - st.t0) / 2200), dolly = (1 + 0.45 * Math.pow(1 - intro, 3)) * Math.min(1.75, Math.max(1, 1.85 / (st.w / st.h))); // the camera glides in when the view opens and keeps the whole scene in narrow panes
      cam.position.set((1 + Math.sin(k) * 2.2 * q.sway) * dolly, (5.8 + Math.sin(k * 1.7) * 0.5 * q.sway) * dolly, (17 + Math.cos(k * 0.8) * 0.6 * q.sway) * dolly); cam.lookAt(0.5, -0.2, -2);
      core.rotation.y += dt * 0.8; core.scale.setScalar(1 + 0.06 * Math.sin(st.t * 2.4));

      // template assembly
      assemble = Math.min(1, assemble + dt * 0.7);
      const nc = clamp(st.cands | 0, 1, MAXC);
      for (let i = 0; i < MAXC; i++) {
        if (i >= nc) { D.position.set(0, -50, 0); D.scale.setScalar(0.001); } else {
          const e = ease(assemble * 1.6 - i * 0.045), s = slot(i);
          D.position.set(s[0] + seeds[i][0] * (1 - e), s[1] + seeds[i][1] * (1 - e), s[2] + seeds[i][2] * (1 - e)); D.scale.setScalar(0.4 + 0.6 * e);
          // unattested parts are amber, they turn green when they pass the ring (x of the ring relative to the template) and settle into the template colour
          const wx = X.tpl + D.position.x, pass = wx >= X.attest;
          if (assemble < 1 && !pass) tmp.copy(col.warn); else if (assemble < 1 && e < 0.97) tmp.copy(col.ok); else tmp.copy(col.c2);
          if (assemble < 1 && pass && crossed[i] === false) { crossed[i] = true; attestFlash = 1; }
          if (assemble === 0 || assemble < 1) { if (!pass) crossed[i] = false; }
          tpl.setColorAt(i, tmp);
        }
        D.rotation.set(0, st.t * 0.25 * (i % 2 ? 1 : -1), 0); D.updateMatrix(); tpl.setMatrixAt(i, D.matrix);
      }
      tpl.instanceMatrix.needsUpdate = true; if (tpl.instanceColor) tpl.instanceColor.needsUpdate = true;
      attest.rotation.x += dt * 1.6; attest2.rotation.x -= dt * 2.4; attestFlash = Math.max(0, attestFlash - dt * 3);
      attest.material.opacity = 0.7 + 0.3 * attestFlash; attest.scale.setScalar(1 + 0.18 * attestFlash); halos[1].material.opacity = 0.35 + 0.5 * attestFlash;
      parts.forEach((p, i) => { p.position.x = X.tpl + Math.cos(st.t * 0.5 + i * 1.57) * 2.6; p.position.z = Math.sin(st.t * 0.5 + i * 1.57) * 2.6; });

      // allocator tiles follow the epoch use
      const used = Math.round(clamp(st.used, 0, 1) * TILES);
      if (used !== usedShown) { for (let i = 0; i < TILES; i++) tiles.setColorAt(i, i < used ? col.c3 : col.dim); tiles.instanceColor.needsUpdate = true; usedShown = used; }

      // miners
      const on = Math.min(MAXM, st.online | 0);
      for (let i = 0; i < MAXM; i++) {
        pulse[i] = Math.max(0, pulse[i] - dt * 2.5);
        if (i >= on) { D.position.set(0, -50, 0); D.scale.setScalar(0.001); } else { D.position.copy(mpos[i]); D.scale.setScalar(0.8 + 0.9 * pulse[i]); }
        D.rotation.set(st.t * 0.6 + i, st.t * 0.4, 0); D.updateMatrix(); miners.setMatrixAt(i, D.matrix);
      }
      miners.instanceMatrix.needsUpdate = true;

      trails.clear();
      // jobs: template -> allocator -> gate -> miner
      for (let i = 0; i < MAXJ; i++) {
        const j = J[i]; if (!j.on) continue;
        j.t += dt * j.v;
        if (j.t >= 1 || j.m >= on) { j.on = false; hide(jobs, i); continue; }
        const t = j.t;
        if (t < 0.25) lerp3(W[0], W[1], t / 0.25, P); else if (t < 0.5) lerp3(W[1], W[2], (t - 0.25) / 0.25, P); else lerp3(W[2], mpos[j.m], (t - 0.5) / 0.5, P);
        P.y += Math.sin(t * Math.PI) * 0.7;
        if (q.trails && j.px !== undefined) { trailTo(j); trails.add(tail, P, col.c1, 2.2, 0, 1); trails.add(tail, P, col.c1, 8, 0, 0.25); }
        j.px = P.x; j.py = P.y; j.pz = P.z;
        D.position.copy(P); D.scale.setScalar(0.7 + 0.5 * Math.sin(t * Math.PI)); D.rotation.set(0, 0, 0); D.updateMatrix(); jobs.setMatrixAt(i, D.matrix);
      }
      jobs.instanceMatrix.needsUpdate = true;
      // shares: miner -> gate -> node (accepted) or down (stale, invalid)
      let recolor = false;
      for (let i = 0; i < MAXS; i++) {
        const s = S[i]; if (!s.on) continue;
        s.t += dt * s.v;
        if (s.t >= 1 || (s.g < 0 && s.m >= on)) { s.on = false; hide(shares, i); continue; }
        const t = s.t;
        if (t < 0.5) lerp3(s.g >= 0 ? gwPos[s.g] : mpos[s.m], W[2], t / 0.5, P);
        else if (s.o === 0) { lerp3(W[2], nodePos, (t - 0.5) / 0.5, P); P.y += Math.sin((t - 0.5) * 2 * Math.PI) * 1.4; }
        else { P.copy(W[2]); P.y -= (t - 0.5) * 4; P.x += (t - 0.5) * 1.2; }
        if (t >= 0.5 && !s.c) { shares.setColorAt(i, s.o === 0 ? col.ok : s.o === 1 ? col.warn : col.err); recolor = true; s.c = true; }
        if (t < 0.5 && s.c === undefined) { /* neutral until the gate */ }
        { const cc = t < 0.5 ? (s.g >= 0 ? col.c3 : col.c1) : s.o === 0 ? col.ok : s.o === 1 ? col.warn : col.err; if (q.trails && s.px !== undefined) { trailTo(s); trails.add(tail, P, cc, 2.2, 0, 1); trails.add(tail, P, cc, 8, 0, 0.25); } s.px = P.x; s.py = P.y; s.pz = P.z; }
        D.position.copy(P); D.scale.setScalar(t >= 0.5 && s.o ? Math.max(0.1, 1 - (t - 0.5) * 2) : 1); D.rotation.set(0, 0, 0); D.updateMatrix(); shares.setMatrixAt(i, D.matrix);
      }
      trails.commit(); shares.instanceMatrix.needsUpdate = true; if (recolor && shares.instanceColor) shares.instanceColor.needsUpdate = true;

      // photon fire
      fireAcc += dt * q.rate * (70 + 70 * Math.log10(1 + st.hr / 1e5) + 40 * Math.log10(1 + st.jobs) + 320 * flare);
      for (let i = 0; i < q.fire; i++) {
        if (fl[i] <= 0) { if (fireAcc >= 1) { fireAcc -= 1; emit(i); } else { fp[i * 3 + 1] = -50; continue; } }
        fl[i] -= dt; const k = 1 - Math.max(0, fl[i]) / fm[i], o = i * 3;
        fv[o + 1] += (0.3 + Math.sin(st.t * 7 + i) * 0.6) * dt; // flames flicker upward
        fp[o] += fv[o] * dt * (1 - 0.35 * k); fp[o + 1] += fv[o + 1] * dt; fp[o + 2] += fv[o + 2] * dt;
        if (fp[o] > X.gate) { fl[i] = 0; fp[o + 1] = -50; continue; } // burnt out at the share check
        tmp.copy(k < 0.25 ? white : k < 0.6 ? col.c1 : col.c2); const b = k < 0.8 ? 1 : (1 - k) * 5; fc[o] = tmp.r * b; fc[o + 1] = tmp.g * b; fc[o + 2] = tmp.b * b;
      }
      fg.attributes.position.needsUpdate = true; fg.attributes.color.needsUpdate = true;
      // node glimmer
      flare = Math.max(0, flare - dt * 0.8);
      rays.forEach((r, i) => { r.material.rotation = st.t * (0.05 + (i % 4) * 0.02) * (i % 2 ? 1 : -1) + i * 0.5236; r.material.opacity = 0.14 + 0.1 * Math.sin(st.t * 1.3 + i * 1.9) + 0.55 * flare; r.scale.x = 0.5 + 0.8 * flare; });
      sparkles.rotation.x += dt * 0.25; sparkles.material.opacity = 0.55 + 0.4 * Math.sin(st.t * 5.1) + 0.3 * flare; sparkles.material.size = 0.1 + 0.08 * Math.sin(st.t * 3.3) + 0.12 * flare;
      halos[0].material.opacity = 0.5 + 0.12 * Math.sin(st.t * 1.7) + 0.4 * flare; halos[0].scale.setScalar(5 + 0.6 * Math.sin(st.t * 1.7) + 2 * flare);
      // gateways
      const gn = st.photon ? Math.min(MAXG, Math.max(st.gw, 0)) : 0;
      for (let i = 0; i < MAXG; i++) {
        if (i >= gn) { D.position.set(0, -50, 0); D.scale.setScalar(0.001); } else { D.position.copy(gwPos[i]); D.scale.setScalar(1 + 0.15 * Math.sin(st.t * 2 + i)); }
        D.rotation.set(0, st.t * 0.9 + i, st.t * 0.5); D.updateMatrix(); gwMesh.setMatrixAt(i, D.matrix);
        for (let c = 0; c < CH; c++) {
          const o = (i * CH + c) * 3;
          if (i >= gn || c >= Math.min(CH, Math.ceil(st.chn / Math.max(1, st.gw)))) { chArr[o] = chArr[o + 1] = chArr[o + 2] = -50; continue; }
          const a = st.t * 1.4 + c * (6.283 / CH) + i; chArr[o] = gwPos[i].x + Math.cos(a) * 0.65; chArr[o + 1] = gwPos[i].y + Math.sin(a * 0.7) * 0.3; chArr[o + 2] = gwPos[i].z + Math.sin(a) * 0.65;
        }
      }
      gwMesh.instanceMatrix.needsUpdate = true; chGeo.attributes.position.needsUpdate = true;
      gwHalo.material.opacity = st.photon ? 0.15 + 0.1 * Math.min(1, gn / 4) + 0.3 * gateFlash : 0; gateFlash = Math.max(0, gateFlash - dt * 2.5); halos[4].material.opacity = 0.3 + 0.5 * gateFlash;
      if (st.checks > 0) { st.accC = (st.accC || 0) + dt * Math.min(4, st.checks); if (st.accC >= 1) { st.accC = 0; gateFlash = 1; } }
      // chain, rings, found block
      chain.forEach((c) => { c.m.position.x += (c.tx - c.m.position.x) * (1 - Math.exp(-dt * 4)); c.m.scale.setScalar(Math.min(1, c.m.scale.x + dt * 2)); c.m.rotation.y += dt * 0.3; });
      for (const r of rings) { r.t += dt; r.m.scale.setScalar(1 + r.t * 5); r.m.material.opacity = Math.max(0, 0.9 - r.t * 0.7); }
      for (let i = rings.length - 1; i >= 0; i--) if (rings[i].t > 1.4) { world.remove(rings[i].m); rings.splice(i, 1); }
      if (gold) {
        gold.t += dt * 0.45;
        const k = ease(gold.t); gold.m.position.set(X.miners + (X.node - X.miners) * k, -0.2 + Math.sin(k * Math.PI) * 3, 0); gold.m.rotation.y += dt * 3; gold.m.scale.setScalar(1.4 - 0.4 * k);
        if (gold.t >= 1) { world.remove(gold.m); gold = null; pushBlock(true); ring(col.warn); }
      }
    }
    // Bloom without the post-processing add-ons of three.js: the scene goes into a render target, the bright parts are shrunk to a quarter of the size,
    // blurred (two separable passes) and added back. Only used from level 1 on; below that the scene is drawn straight to the canvas.
    const gl2 = renderer.capabilities.isWebGL2;
    const ortho = new T.OrthographicCamera(-1, 1, 1, -1, 0, 1), quadGeo = track(new T.PlaneGeometry(2, 2)), quadScene = new T.Scene();
    const quad = new T.Mesh(quadGeo, null); quad.frustumCulled = false; quadScene.add(quad);
    const pass = (frag, uniforms) => new T.ShaderMaterial({ vertexShader: 'varying vec2 vUv; void main() { vUv = uv; gl_Position = vec4(position.xy, 0.0, 1.0); }', fragmentShader: frag, uniforms, depthTest: false, depthWrite: false, blending: T.NoBlending });
    const matBright = pass('uniform sampler2D t; uniform vec2 px; varying vec2 vUv; void main() { vec4 c = (texture2D(t, vUv + px * vec2(-1., -1.)) + texture2D(t, vUv + px * vec2(1., -1.)) + texture2D(t, vUv + px * vec2(-1., 1.)) + texture2D(t, vUv + px * vec2(1., 1.))) * 0.25; float m = max(max(c.r, c.g), c.b); gl_FragColor = vec4(c.rgb * smoothstep(0.32, 0.8, m), 1.0); }', { t: { value: null }, px: { value: new T.Vector2() } });
    const matBlur = pass('uniform sampler2D t; uniform vec2 dir; varying vec2 vUv; void main() { vec4 s = texture2D(t, vUv) * 0.2270270; s += (texture2D(t, vUv + dir * 1.3846) + texture2D(t, vUv - dir * 1.3846)) * 0.3162162; s += (texture2D(t, vUv + dir * 3.2308) + texture2D(t, vUv - dir * 3.2308)) * 0.0702703; gl_FragColor = s; }', { t: { value: null }, dir: { value: new T.Vector2() } });
    const matMix = pass('uniform sampler2D t; uniform sampler2D b; uniform float k; varying vec2 vUv; void main() { vec4 s = texture2D(t, vUv); vec3 c = s.rgb + texture2D(b, vUv).rgb * k; gl_FragColor = vec4(c, clamp(max(max(c.r, c.g), max(c.b, s.a)), 0.0, 1.0)); }', { t: { value: null }, b: { value: null }, k: { value: 1 } });
    track(matBright); track(matBlur); track(matMix);
    let rtS = null, rtA = null, rtB = null;
    const mkRT = (w, h, samples) => new T.WebGLRenderTarget(Math.max(2, w | 0), Math.max(2, h | 0), { minFilter: T.LinearFilter, magFilter: T.LinearFilter, format: T.RGBAFormat, type: T.UnsignedByteType, depthBuffer: true, samples });
    function bloomSize() {
      [rtS, rtA, rtB].forEach((r) => r && r.dispose()); rtS = rtA = rtB = null;
      if (!q.bloom) return;
      const w = st.w * st.dpr, h = st.h * st.dpr;
      rtS = mkRT(w, h, gl2 ? 4 : 0); rtA = mkRT(w / 4, h / 4, 0); rtB = mkRT(w / 4, h / 4, 0);
      rtA.depthBuffer = rtB.depthBuffer = false;
    }
    const full = (mat, target) => { quad.material = mat; renderer.setRenderTarget(target); renderer.render(quadScene, ortho); };
    function renderScene() {
      renderer.setClearColor(0x000000, 0);
      if (!q.bloom || !rtS) { renderer.setRenderTarget(null); renderer.render(scene, cam); return; }
      renderer.setRenderTarget(rtS); renderer.clear(); renderer.render(scene, cam);
      matBright.uniforms.t.value = rtS.texture; matBright.uniforms.px.value.set(1 / rtS.width, 1 / rtS.height); full(matBright, rtA);
      matBlur.uniforms.t.value = rtA.texture; matBlur.uniforms.dir.value.set(1 / rtA.width, 0); full(matBlur, rtB);
      matBlur.uniforms.t.value = rtB.texture; matBlur.uniforms.dir.value.set(0, 1 / rtA.height); full(matBlur, rtA);
      matBlur.uniforms.t.value = rtA.texture; matBlur.uniforms.dir.value.set(2 / rtA.width, 0); full(matBlur, rtB); // a second, wider round for a soft halo
      matBlur.uniforms.t.value = rtB.texture; matBlur.uniforms.dir.value.set(0, 2 / rtA.height); full(matBlur, rtA);
      matMix.uniforms.t.value = rtS.texture; matMix.uniforms.b.value = rtA.texture; matMix.uniforms.k.value = 1.15; full(matMix, null);
    }
    function setLevel(l) {
      if (l === st.lv) return;
      st.lv = l; q = LV[l]; canvas.dataset.level = String(l);
      size(); // pixel ratio and bloom targets follow the level
      for (let i = q.fire; i < FN; i++) { fl[i] = 0; fp[i * 3 + 1] = -50; }
      st.slow = 0; st.avg = 0;
    }
    let lvChecked = 0;
    function control(ts, frameMs) {
      if (ts - lvChecked > 300) { lvChecked = ts; const want = Math.min(isFront() ? 2 : 0, st.cap); if (want !== st.lv) setLevel(want); }
      // the frame rate cannot keep up: step down for good (until the page is reloaded)
      st.avg = st.avg ? st.avg * 0.94 + frameMs * 0.06 : frameMs;
      if (st.lv > 0 && st.avg > 1000 / q.fps * 1.7) { if (++st.slow > 45) { st.cap = st.lv - 1; setLevel(Math.min(st.lv, st.cap)); } } else st.slow = Math.max(0, st.slow - 1);
    }

    function frame(ts) {
      st.raf = 0; if (!st.run) return;
      if (document.hidden || !st.vis) { st.last = 0; return; }
      if (!st.last || ts - st.last >= 1000 / q.fps - 2) {
        const t0 = performance.now(), gap = st.last ? ts - st.last : 1000 / q.fps, dt = st.last ? Math.min(0.1, gap / 1000) : 0.033;
        update(dt); renderScene(); st.last = ts;
        if (canvas.style.opacity !== '1') canvas.style.opacity = '1'; // fades in with the first frame
        control(ts, gap);
        void t0;
      }
      st.raf = requestAnimationFrame(frame);
    }
    function kick() { if (st.run && !st.raf && st.vis && !document.hidden) st.raf = requestAnimationFrame(frame); }
    size();
    return {
      start() { st.run = true; canvas.style.opacity = '0'; canvas.style.transition = 'opacity .9s ease'; st.t0 = performance.now(); if (reduce) { update(0.5); renderScene(); canvas.style.opacity = '1'; return; } kick(); },
      stop() {
        st.run = false; cancelAnimationFrame(st.raf); ro.disconnect(); io.disconnect(); document.removeEventListener('visibilitychange', onVis);
        disposables.forEach((d) => { try { d.dispose(); } catch (_) { /* already gone */ } });
        [tpl, tiles, miners, jobs, shares].forEach((m) => m.dispose()); renderer.dispose();
        try { renderer.forceContextLoss(); } catch (_) { /* not supported */ }
      },
      feed(d) {
        st.hr = d.hr || 0; st.batchMs = d.batchMs || 0; st.backend = d.backend || ''; st.jobsT = d.jobs || 0; st.sharesT = d.shares || 0; st.cands = d.cands || st.cands; st.used = d.epochUse || 0; st.online = d.miners || 0;
        const tot = (d.ok || 0) + (d.stale || 0) + (d.bad || 0);
        if (tot > 0) { st.okF = d.ok / tot; st.staleF = d.stale / tot; st.badF = d.bad / tot; }
      },
      tip(height) {
        if (st.tip && height !== st.tip) { assemble = 0; pushBlock(false); ring(col.c3); flare = 1; }
        if (height !== st.tip) { const l = label('height ' + height, '#' + col.c3.getHexString(), 0.9); l.position.copy(heightLabel.position); world.remove(heightLabel); heightLabel.material.map.dispose(); heightLabel = add(l); track(l.material); }
        if (height !== st.tip || !st.infoDone) {
          const t = `${st.cands | 0} candidates attested${st.batchMs ? ' in ' + Math.round(st.batchMs) + ' ms' : ''}`;
          const l = label(t, '#' + col.ok.getHexString(), 0.8); l.position.copy(infoLabel.position); world.remove(infoLabel); infoLabel.material.map.dispose(); infoLabel = add(l); track(l.material); st.infoDone = true;
        }
        st.tip = height;
      },
      block() { if (gold) return; const m = new T.Mesh(chainGeo, track(new T.MeshBasicMaterial({ color: col.warn }))); m.position.set(X.miners, -0.2, 0); add(m); gold = { m, t: 0 }; ring(col.warn); },
    };
  }
  return { create, supported: () => { try { const c = document.createElement('canvas'); return !!(window.THREE && (c.getContext('webgl2') || c.getContext('webgl'))); } catch (_) { return false; } } };
})();
