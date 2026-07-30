/* Verba overlay renderer.
 *
 * Two treatments driven by the same state:
 *   ribbons — the design's interference wave
 *   glow    — dark box inside a moving blue aura
 *
 * Both react to per-band audio energy from the engine, so the motion tracks the
 * shape of the voice rather than only its loudness.
 *
 * Ribbon geometry, constants lifted verbatim from the design — do not "tidy":
 *   f(x)   = env(x) * sin(2*PI*k*x + phase + t*speed)
 *   env(x) = sin(PI*x)^1.4          spindle taper to zero at both ends
 *   amp    = a * (0.16 + 0.84*level) * H * 0.46
 */

const RIBBONS = [
  { k: 1.00, s:  0.85, a: 1.00, p: 0.0 },
  { k: 1.45, s: -0.70, a: 0.88, p: 1.1 },
  { k: 2.05, s:  1.25, a: 0.72, p: 2.2 },
  { k: 2.55, s: -1.10, a: 0.56, p: 0.6 },
  { k: 3.20, s:  0.60, a: 0.44, p: 3.4 },
  { k: 0.72, s: -0.45, a: 0.52, p: 1.8 },
  { k: 1.15, s:  0.30, a: 0.16, p: 0.4 },
];

const W = 1000, H = 160, CY = 80, N = 56;
const NBANDS = 7;

/* One entry per halo layer: sweep speed in degrees/second, the band slice it
 * listens to, and its resting blur. Splitting the spectrum across the three
 * means low rumble drives the tight inner halo while sibilance flares the
 * outer haze, so the glow moves rather than merely pulsing as one.
 * Speeds are mutually prime-ish so the layers never resynchronise. */
const HALOS = [
  { speed:  24, lo: 0, hi: 3, blur: 18, base: 0.55, shift: 0, noise: true },
  { speed: -15, lo: 2, hi: 5, blur: 34, base: 0.42, shift: 3, noise: true },
  // Outer layer stays un-displaced: it is ambient bloom, and a third SVG
  // filter at 30fps costs more than it adds.
  { speed:   9, lo: 4, hi: 7, blur: 58, base: 0.30, shift: 5, noise: false },
];

/* One colour per band, so a given frequency always lights the same part of the
 * ring. Blues and violets only — the accent stays a single family. */
const GLOW_HUES = [
  [ 79, 168, 255], [ 23, 200, 255], [170, 225, 255], [ 96, 150, 255],
  [106,  92, 255], [ 43, 123, 255], [ 20, 180, 255],
];

const $ = id => document.getElementById(id);
const el = {
  stageRibbons: $('stage-ribbons'), stageGlow: $('stage-glow'),
  glass: $('glass'), rim: $('rim'), wave: $('wave'), blend: $('blend'),
  body: $('body'), status: $('status'), timer: $('timer'), line: $('line'),
  shell: $('glow-shell'), aura: $('aura'), box: $('glow-box'),
  gStatus: $('g-status'), gTimer: $('g-timer'), gLine: $('g-line'),
  engine: $('engine'), gEngine: $('g-engine'),
};
const paths = [...document.querySelectorAll('#ribbons path')];
const halos = [...document.querySelectorAll('#aura .halo')];
const gnTurb = $('gn-turb');
const gnDisp = $('gn-disp');

let phase = 'idle';
let visual = 'ribbons';
let level = 0, levelTarget = 0;
let t = 0;
const band = new Array(NBANDS).fill(0);        // smoothed, what we draw
const bandTarget = new Array(NBANDS).fill(0);  // latest from the engine
let words = [], revealStart = 0;

function ribbonPath(r, t, lvl) {
  const amp = r.a * (0.16 + 0.84 * lvl) * H * 0.46;
  const top = [], bot = [];
  for (let i = 0; i <= N; i++) {
    const u = i / N;
    const x = u * W;
    const env = Math.pow(Math.sin(Math.PI * u), 1.4);
    const w = Math.sin(u * Math.PI * 2 * r.k + r.p + t * r.s);
    const dy = amp * env * w;
    top.push(x.toFixed(1) + ',' + (CY - dy).toFixed(2));
    bot.push(x.toFixed(1) + ',' + (CY + dy).toFixed(2));
  }
  return 'M' + top.join('L') + 'L' + bot.reverse().join('L') + 'Z';
}

const GLASS_FILL =
  'linear-gradient(157deg,rgba(255,255,255,.1),rgba(255,255,255,.026) 44%,rgba(255,255,255,.062))';
const GLASS_SHADOW =
  '0 42px 96px -26px rgba(0,0,0,.9),0 0 64px -18px rgba(160,130,255,.18),' +
  'inset 0 1px 0 rgba(255,255,255,.32),inset 0 0 0 1px rgba(255,255,255,.06)';

function setVisual(name) {
  visual = name === 'glow' ? 'glow' : 'ribbons';
  el.stageRibbons.hidden = visual !== 'ribbons';
  el.stageGlow.hidden = visual !== 'glow';
  setPhase(phase);
}

function setPhase(next) {
  phase = next;
  const listening = next === 'listening';
  const idle = next === 'idle';

  if (idle) levelTarget = 0;
  else if (listening) levelTarget = 1;
  else levelTarget = 0.34;

  // ribbons
  if (idle) {
    Object.assign(el.glass.style, {
      width: '180px', opacity: '0', background: 'transparent',
      backdropFilter: 'none', boxShadow: 'none', marginTop: '0px',
    });
    el.wave.style.transform = 'scaleX(.06)';
    el.wave.style.opacity = '0';
    el.rim.style.opacity = '0';
    el.blend.style.opacity = '0';
    el.body.style.height = '0px';
  } else if (listening) {
    // Glass stays fully transparent here: the ribbons float in air with nothing
    // behind them. Deliberate in the design, and it means this state needs no
    // desktop backdrop at all.
    Object.assign(el.glass.style, {
      width: '560px', opacity: '1', background: 'transparent',
      backdropFilter: 'none', boxShadow: 'none', marginTop: '10px',
    });
    el.wave.style.transform = 'scaleX(1)';
    el.wave.style.opacity = '1';
    el.rim.style.opacity = '0';
    el.blend.style.opacity = '0';
    el.body.style.height = '0px';
  } else {
    Object.assign(el.glass.style, {
      width: '620px', opacity: '1', background: GLASS_FILL,
      backdropFilter: 'blur(38px) saturate(180%) brightness(.8)',
      boxShadow: GLASS_SHADOW, marginTop: '14px',
    });
    el.wave.style.transform = 'scaleX(1)';
    el.wave.style.opacity = '1';
    el.rim.style.opacity = '1';
    el.blend.style.opacity = '1';
    el.body.style.height = '232px';
  }

  // glow — narrower while listening, since the box holds only the meta row
  // until a transcript arrives.
  el.shell.style.width = idle ? '300px' : listening ? '380px' : '560px';
  el.shell.style.opacity = idle ? '0' : '1';
  el.aura.style.opacity = idle ? '0' : '1';
  el.gLine.classList.toggle('open', !idle && !listening);
}

function setText(text) {
  const host = visual === 'glow' ? el.gLine : el.line;
  const other = visual === 'glow' ? el.line : el.gLine;
  other.textContent = '';
  host.textContent = '';
  words = (text || '').split(/(\s+)/).filter(Boolean).map(w => {
    const s = document.createElement('span');
    s.textContent = w;
    host.appendChild(s);
    return s;
  });
  const caret = document.createElement('span');
  caret.className = 'caret';
  host.appendChild(caret);
  revealStart = performance.now();
}

/* Opacity cascade from the design: words settle 0 -> .18 -> .34 -> .72 -> 1.
 * Whisper is not streaming, so this replays a finished transcript rather than
 * showing live partials — visually identical, and it costs nothing. */
function reveal(p) {
  const n = words.length;
  if (!n) return;
  const shown = p * (n + 2);
  for (let i = 0; i < n; i++) {
    const d = shown - i;
    words[i].style.opacity =
      d >= 1.6 ? '1' : d >= 1 ? '.72' : d >= 0.5 ? '.34' : d > 0 ? '.18' : '0';
  }
}

/** Conic gradient built from the live band energies, so brightness varies
 *  around the perimeter with what is being said rather than sweeping uniformly.
 *  `shift` rotates which band owns which arc, giving each layer its own
 *  distribution. The final stop repeats the first so the ring closes seamlessly. */
function conicFromBands(angleDeg, shift) {
  let stops = '';
  for (let i = 0; i <= NBANDS; i++) {
    const idx = (i + shift) % NBANDS;
    const c = GLOW_HUES[idx];
    const a = (0.03 + 0.97 * band[idx]).toFixed(3);
    stops += `rgba(${c[0]},${c[1]},${c[2]},${a}) ${((i / NBANDS) * 100).toFixed(1)}%`;
    if (i < NBANDS) stops += ',';
  }
  return `conic-gradient(from ${angleDeg.toFixed(1)}deg,${stops})`;
}

let last = performance.now();
let lastPaint = 0;
function tick(now) {
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;

  level += (levelTarget - level) * Math.min(1, dt * 3.4);
  t += dt * (phase === 'listening' ? 1 : 0.45);

  // Fast attack, slow decay — a symmetric filter makes speech look like mush,
  // because consonant transients are gone before a slow attack can reach them.
  // Decay is quick enough that the glow visibly drops between syllables.
  for (let i = 0; i < NBANDS; i++) {
    const tgt = phase === 'listening' ? bandTarget[i] : 0;
    const k = tgt > band[i] ? dt * 30 : dt * 10;
    band[i] += (tgt - band[i]) * Math.min(1, k);
  }

  if (visual === 'ribbons') {
    for (let i = 0; i < RIBBONS.length; i++) {
      // Blend: the global level keeps every ribbon alive so the shape still
      // reads as one object, while its own band gives it independent motion.
      const lvl = level * (0.4 + 0.6 * band[i]);
      paths[i].setAttribute('d', ribbonPath(RIBBONS[i], t, lvl));
    }
  } else {
    let total = 0, peak = 0;
    for (let i = 0; i < NBANDS; i++) {
      total += band[i];
      if (band[i] > peak) peak = band[i];
    }
    const avg = total / NBANDS;

    // Rebuilding a conic gradient forces a repaint of a blurred, filtered
    // element, so it is throttled to the rate bands actually arrive at. At
    // 60fps half the work would be redundant.
    const rebuild = now - lastPaint > 32;
    if (rebuild) lastPaint = now;

    if (rebuild) {
      // Displacement amount follows the loudest band, so a plosive visibly
      // tears the outline rather than merely brightening it.
      gnDisp.setAttribute('scale', (8 + 74 * peak).toFixed(1));
      // Two slow, mutually incommensurate drifts plus an energy term: the
      // texture keeps evolving instead of cycling, and tightens when louder.
      const bx = 0.008 + 0.005 * Math.sin(t * 0.37) + 0.011 * avg;
      const by = 0.013 + 0.006 * Math.cos(t * 0.29) + 0.013 * avg;
      gnTurb.setAttribute('baseFrequency', `${bx.toFixed(4)} ${by.toFixed(4)}`);
    }

    for (let i = 0; i < halos.length; i++) {
      const spec = HALOS[i];
      let e = 0;
      for (let b = spec.lo; b < spec.hi; b++) e += band[b];
      e /= spec.hi - spec.lo;

      if (rebuild) {
        halos[i].style.background = conicFromBands(t * spec.speed, spec.shift);
        halos[i].style.filter =
          `${spec.noise ? 'url(#glowNoise) ' : ''}blur(${(spec.blur + 12 * (1 - e)).toFixed(1)}px)`;
      }
      // Driven by band energy alone. `level` is pinned to 1 for the whole
      // listening phase, so folding it in here is what made the glow look
      // inert — it only ever contributed a constant.
      halos[i].style.opacity = (spec.base * (0.12 + 0.88 * e)).toFixed(3);
      halos[i].style.transform = `scale(${(0.95 + 0.11 * e).toFixed(4)})`;
    }

    // The box itself breathes, so the glow reads as belonging to it rather
    // than sitting behind it. Throttled with the rest — a box-shadow change
    // repaints the whole element.
    if (rebuild) {
      const g = avg;
      el.box.style.boxShadow =
        `0 0 ${(28 + 90 * g).toFixed(0)}px ${(-8 + 26 * g).toFixed(0)}px rgba(52,134,255,${(0.2 + 0.55 * g).toFixed(3)}),` +
        `0 0 ${(10 + 32 * g).toFixed(0)}px rgba(150,215,255,${(0.1 + 0.34 * g).toFixed(3)}),` +
        'inset 0 1px 0 rgba(255,255,255,.18),' +
        'inset 0 0 0 1px rgba(140,200,255,.16),' +
        '0 30px 80px -30px rgba(0,0,0,.95)';
    }
  }

  if (words.length) reveal(Math.min(1, (now - revealStart) / 3600));
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);

// --- driven by the Rust engine -------------------------------------------

const api = window.__TAURI__?.event;
if (!api?.listen) {
  // Standalone in a browser, or the ACL denied the API. Leave a trace rather
  // than sitting invisibly at opacity 0, which is indistinguishable from the
  // window failing to show at all.
  console.error('Verba: __TAURI__.event.listen unavailable — overlay will not update');
} else {
  api.listen('verba:state', ({ payload }) => {
    if (payload.visual && payload.visual !== visual) setVisual(payload.visual);
    if (payload.phase !== phase) setPhase(payload.phase);

    if (Array.isArray(payload.bands)) {
      for (let i = 0; i < NBANDS; i++) bandTarget[i] = payload.bands[i] ?? 0;
    }
    if (typeof payload.elapsed === 'number') {
      const s = Math.floor(payload.elapsed);
      const txt = `0:${String(s).padStart(2, '0')}`;
      el.timer.textContent = txt;
      el.gTimer.textContent = txt;
    }
    if (payload.status) {
      el.status.textContent = payload.status;
      el.gStatus.textContent = payload.status;
      el.status.style.color =
        payload.phase === 'listening' ? 'oklch(80% .12 350)' : 'oklch(76% .12 300)';
    }
    if (payload.model) {
      const name = payload.model.replace(/^ggml-/, '').replace(/\.bin$/, '').toUpperCase();
      const label = `${name} · ${payload.gpu ? 'GPU' : 'LOCAL'}`;
      el.engine.textContent = label;
      el.gEngine.textContent = label;
    }
    if (payload.text !== undefined && payload.text !== null) setText(payload.text);
  }).catch(err => console.error('Verba: listen() rejected', err));
}

setVisual('ribbons');
setPhase('idle');
