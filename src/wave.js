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
const NBARS = 15;

const $ = id => document.getElementById(id);
const el = {
  stageRibbons: $('stage-ribbons'), stageGlow: $('stage-glow'),
  glass: $('glass'), rim: $('rim'), wave: $('wave'), blend: $('blend'),
  body: $('body'), status: $('status'), timer: $('timer'), line: $('line'),
  shell: $('glow-shell'), aura: $('aura'), gStatus: $('g-status'),
  gTimer: $('g-timer'), gBars: $('g-bars'), gLine: $('g-line'),
};
const paths = [...document.querySelectorAll('#ribbons path')];
const blobs = [...document.querySelectorAll('#aura .blob')];

// Build the bar meter once.
const bars = [];
for (let i = 0; i < NBARS; i++) {
  const b = document.createElement('i');
  el.gBars.appendChild(b);
  bars.push(b);
}

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

  // glow
  el.shell.style.width = idle ? '300px' : listening ? '460px' : '560px';
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

/** Sample the band curve at an arbitrary position, so the meter can have more
 *  bars than there are bands. */
function bandAt(u) {
  const x = u * (NBANDS - 1);
  const i = Math.min(NBANDS - 2, Math.floor(x));
  const f = x - i;
  return band[i] * (1 - f) + band[i + 1] * f;
}

let last = performance.now();
function tick(now) {
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;

  level += (levelTarget - level) * Math.min(1, dt * 3.4);
  t += dt * (phase === 'listening' ? 1 : 0.45);

  // Fast attack, slow decay — a symmetric filter makes speech look like mush,
  // because consonant transients are gone before a slow attack can reach them.
  for (let i = 0; i < NBANDS; i++) {
    const tgt = phase === 'listening' ? bandTarget[i] : 0;
    const k = tgt > band[i] ? dt * 24 : dt * 7;
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
    for (let i = 0; i < bars.length; i++) {
      const v = bandAt(i / (bars.length - 1)) * level;
      bars[i].style.transform = `scaleY(${(0.04 + 0.96 * v).toFixed(3)})`;
    }
    // Blobs drift on incommensurate sine pairs so the aura never visibly loops.
    for (let i = 0; i < blobs.length; i++) {
      const e = bandAt((i + 0.5) / blobs.length);
      const x = Math.sin(t * (0.62 + i * 0.17) + i * 2.1) * (18 + 22 * e);
      const y = Math.cos(t * (0.44 + i * 0.13) + i * 1.7) * (12 + 16 * e);
      const s = 0.72 + 0.85 * e * level;
      blobs[i].style.transform =
        `translate(-50%,-50%) translate(${x.toFixed(1)}%, ${y.toFixed(1)}%) scale(${s.toFixed(3)})`;
    }
    el.aura.style.filter = `blur(${(26 + 16 * (1 - level)).toFixed(1)}px)`;
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
    if (payload.text !== undefined && payload.text !== null) setText(payload.text);
  }).catch(err => console.error('Verba: listen() rejected', err));
}

setVisual('ribbons');
setPhase('idle');
