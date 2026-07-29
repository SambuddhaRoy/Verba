/* Interference-ribbon visualiser.
 *
 * Seven ribbons, each the filled area between +f(x) and -f(x) where f is a
 * spindle envelope times a sine at its own frequency and drift speed. Where two
 * ribbons cross, screen blending whitens — that is what produces the lens shapes.
 * Constants are lifted verbatim from the design; do not "tidy" them.
 *
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

const el = {
  glass:  document.getElementById('glass'),
  rim:    document.getElementById('rim'),
  wave:   document.getElementById('wave'),
  blend:  document.getElementById('blend'),
  body:   document.getElementById('body'),
  status: document.getElementById('status'),
  timer:  document.getElementById('timer'),
  line:   document.getElementById('line'),
};
const paths = [...document.querySelectorAll('#ribbons path')];

let phase = 'idle';
let level = 0;        // smoothed, drives amplitude
let target = 0;       // where level is heading
let t = 0;            // animation clock, advances at phase-dependent speed
let words = [];
let revealStart = 0;

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

function setPhase(next) {
  phase = next;

  if (next === 'idle') {
    Object.assign(el.glass.style, {
      width: '180px', opacity: '0', background: 'transparent',
      backdropFilter: 'none', boxShadow: 'none', marginTop: '0px',
    });
    el.wave.style.transform = 'scaleX(.06)';
    el.wave.style.opacity = '0';
    el.rim.style.opacity = '0';
    el.blend.style.opacity = '0';
    el.body.style.height = '0px';
    target = 0;

  } else if (next === 'listening') {
    // Glass stays fully transparent here: the ribbons float in air with nothing
    // behind them. This is deliberate in the design, and it means the listening
    // state needs no desktop backdrop at all.
    Object.assign(el.glass.style, {
      width: '560px', opacity: '1', background: 'transparent',
      backdropFilter: 'none', boxShadow: 'none', marginTop: '10px',
    });
    el.wave.style.transform = 'scaleX(1)';
    el.wave.style.opacity = '1';
    el.rim.style.opacity = '0';
    el.blend.style.opacity = '0';
    el.body.style.height = '0px';
    target = 1;

  } else {
    // transcribing / done — the body extends and the surface darkens into glass.
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
    target = 0.34;
  }
}

function setText(text) {
  el.line.textContent = '';
  words = (text || '').split(/(\s+)/).filter(Boolean).map(w => {
    const s = document.createElement('span');
    s.textContent = w;
    el.line.appendChild(s);
    return s;
  });
  const caret = document.createElement('span');
  caret.id = 'caret';
  el.line.appendChild(caret);
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

let last = performance.now();
function tick(now) {
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;

  level += (target - level) * Math.min(1, dt * 3.4);
  t += dt * (phase === 'listening' ? 1 : 0.45);

  for (let i = 0; i < RIBBONS.length; i++) {
    paths[i].setAttribute('d', ribbonPath(RIBBONS[i], t, level));
  }

  if (words.length) reveal(Math.min(1, (now - revealStart) / 3600));

  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);

// --- driven by the Rust engine -------------------------------------------

const listen = window.__TAURI__?.event?.listen;
if (listen) {
  listen('verba://state', ({ payload }) => {
    if (payload.phase !== phase) setPhase(payload.phase);

    // Real microphone amplitude, not the mockup's synthetic jitter.
    if (payload.phase === 'listening' && typeof payload.level === 'number') {
      target = Math.min(1, payload.level);
    }
    if (typeof payload.elapsed === 'number') {
      const s = Math.floor(payload.elapsed);
      el.timer.textContent = `0:${String(s).padStart(2, '0')}`;
    }
    if (payload.status) {
      el.status.textContent = payload.status;
      el.status.style.color =
        payload.phase === 'listening' ? 'oklch(80% .12 350)' : 'oklch(76% .12 300)';
    }
    if (payload.text !== undefined && payload.text !== null) setText(payload.text);
  });
}

setPhase('idle');
