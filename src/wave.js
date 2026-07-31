/* Verba overlay renderer.
 *
 * Two treatments, both driven by a live spectrum from the engine:
 *   ribbons — a wave visualiser; the spectrum sets the wave's height along its
 *             length, so the shape changes with the voice, not just the size
 *   glow    — dark box inside a noise-displaced aura whose brightness varies
 *             around the perimeter by frequency
 *
 * Ribbon geometry comes from the design; the spectrum term is the addition
 * that makes it a visualiser rather than an animation:
 *   f(x)   = env(x) * spec(x) * sin(2*PI*k*x + phase + t*speed)
 *   env(x) = sin(PI*x)^1.4          spindle taper to zero at both ends
 */

const RIBBONS = [
  { k: 1.00, s:  0.85, a: 1.00, p: 0.0, off: 0.00 },
  { k: 1.45, s: -0.70, a: 0.88, p: 1.1, off: 0.06 },
  { k: 2.05, s:  1.25, a: 0.72, p: 2.2, off: 0.12 },
  { k: 2.55, s: -1.10, a: 0.56, p: 0.6, off: 0.03 },
  { k: 3.20, s:  0.60, a: 0.44, p: 3.4, off: 0.18 },
  { k: 0.72, s: -0.45, a: 0.52, p: 1.8, off: 0.09 },
  { k: 1.15, s:  0.30, a: 0.16, p: 0.4, off: 0.00 },
];

const W = 1000, H = 160, CY = 80;
/** Samples per ribbon path. Higher than the design's 56 because the spectrum
 *  now adds detail along the length that a coarse path would step over. */
const N = 96;

/* One entry per aura layer: sweep speed in degrees/second, the fraction of the
 * spectrum it listens to, and its resting blur. Splitting the spectrum three
 * ways means low rumble drives the tight inner halo while sibilance flares the
 * outer haze, so the glow moves rather than pulsing as one. */
const HALOS = [
  { speed:  24, from: 0.00, to: 0.40, blur: 16, base: 0.95, shift: 0,  noise: true },
  { speed: -15, from: 0.30, to: 0.75, blur: 32, base: 0.78, shift: 8,  noise: true },
  // Outer layer stays un-displaced: it is ambient bloom, and a third SVG
  // filter at 30fps costs more than it adds.
  { speed:   9, from: 0.60, to: 1.00, blur: 56, base: 0.58, shift: 16, noise: false },
];

/** Blues and violets only — the glow stays one colour family. Sampled across
 *  however many bands arrive. */
const GLOW_STOPS = [
  [ 79, 168, 255], [ 23, 200, 255], [170, 225, 255],
  [ 96, 150, 255], [106,  92, 255], [ 43, 123, 255],
];

/* Minimal treatment: a scrolling record of loudness over time rather than a
 * spectrum. One bar per slice, mirrored about the centre line, with a playhead
 * where the next slice lands. */
const MIN_SLOTS = 85;
const MIN_PITCH = 5;      // 2px bar + 3px gap, matching the CSS
/** One bar per this many ms — 85 slots is then about 7.6s before it scrolls. */
const MIN_PUSH_MS = 90;

const $ = id => document.getElementById(id);
const el = {
  stageRibbons: $('stage-ribbons'), stageGlow: $('stage-glow'),
  stageMinimal: $('stage-minimal'),
  minCard: $('min-card'), minBars: $('min-bars'), minDots: $('min-dots'),
  minCursor: $('min-cursor'), minTitle: $('min-title'), minTime: $('min-time'),
  minLine: $('min-line'),
  glass: $('glass'), field: $('field'), behind: $('behind'),
  wave: $('wave'), blend: $('blend'), body: $('body'),
  status: $('status'), timer: $('timer'), line: $('line'), engine: $('engine'),
  shell: $('glow-shell'), aura: $('aura'), box: $('glow-box'),
  gStatus: $('g-status'), gTimer: $('g-timer'), gLine: $('g-line'), gEngine: $('g-engine'),
};
const paths = [...document.querySelectorAll('#ribbons path')];
const halos = [...document.querySelectorAll('#aura .halo')];
const gnTurb = $('gn-turb');
const gnDisp = $('gn-disp');

// Bars are built once; only their scaleY changes per frame.
const minBars = [];
for (let i = 0; i < MIN_SLOTS; i++) {
  const b = document.createElement('i');
  el.minBars.appendChild(b);
  minBars.push(b);
}
const minHistory = new Float32Array(MIN_SLOTS);
let minHead = 0;
let minLastPush = 0;
/// Set when a slice lands, so the bar redraw runs on change rather than per frame.
let minDirty = true;

function minReset() {
  minHistory.fill(0);
  minHead = 0;
  minLastPush = 0;
  minDirty = true;
}

let phase = 'idle';
let visual = 'ribbons';
let level = 0, levelTarget = 0;   // phase envelope: fades the whole thing in and out
let t = 0;                        // animation clock
let words = [], hasText = false;

/* Spectrum. Sized from whatever the engine sends rather than a constant, so a
 * change to the band count on the Rust side cannot silently leave the top of
 * the spectrum unread. */
let spec = new Float32Array(24);   // smoothed, what we draw
let specTarget = new Float32Array(24);

function setBands(arr) {
  if (arr.length !== spec.length) {
    spec = new Float32Array(arr.length);
    specTarget = new Float32Array(arr.length);
  }
  for (let i = 0; i < arr.length; i++) specTarget[i] = arr[i] || 0;
}

/** Paint the captured desktop into the backdrop canvas.
 *
 *  The image arrives at 1/8 scale; CSS stretches it back up. That upscale plus
 *  the CSS blur is what makes it read as frosted glass rather than a thumbnail. */
function setBackdrop(bd) {
  if (!bd || !el.behind) return;
  const bin = atob(bd.rgba);
  const buf = new Uint8ClampedArray(bin.length);
  for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
  if (buf.length !== bd.width * bd.height * 4) return;

  el.behind.width = bd.width;
  el.behind.height = bd.height;
  el.behind.getContext('2d').putImageData(new ImageData(buf, bd.width, bd.height), 0, 0);
}

/** Spectrum sampled at an arbitrary position, linearly interpolated. */
function specAt(u) {
  const n = spec.length;
  const x = Math.min(0.9999, Math.max(0, u)) * (n - 1);
  const i = Math.floor(x);
  const f = x - i;
  return spec[i] * (1 - f) + spec[Math.min(n - 1, i + 1)] * f;
}

/** Mean over a fractional slice of the spectrum. */
function specBand(from, to) {
  const n = spec.length;
  const a = Math.floor(from * n), b = Math.max(a + 1, Math.ceil(to * n));
  let sum = 0;
  for (let i = a; i < b && i < n; i++) sum += spec[i];
  return sum / (b - a);
}

// --- ribbons --------------------------------------------------------------

function ribbonPath(r, t) {
  const top = [], bot = [];
  for (let i = 0; i <= N; i++) {
    const u = i / N;
    const x = u * W;
    // Spindle taper, so every ribbon meets the centre line at both ends.
    const env = Math.pow(Math.sin(Math.PI * u), 1.4);
    // The spectrum sets the height *along the length*: low frequencies on the
    // left, sibilance on the right. This is what makes the wave change shape
    // with the voice instead of merely getting bigger.
    const s = specAt(u + r.off);
    // A travelling sine keeps the ribbons distinct from one another and alive
    // between syllables; the spectrum modulates its amplitude.
    const wob = Math.sin(u * Math.PI * 2 * r.k + r.p + t * r.s);
    const dy = env * wob * r.a * H * 0.46 * (0.06 + 0.94 * s) * level;
    top.push(x.toFixed(1) + ',' + (CY - dy).toFixed(2));
    bot.push(x.toFixed(1) + ',' + (CY + dy).toFixed(2));
  }
  return 'M' + top.join('L') + 'L' + bot.reverse().join('L') + 'Z';
}

// --- glow -----------------------------------------------------------------

/** Conic gradient built from the live spectrum, so brightness varies around
 *  the perimeter with what is being said. `shift` rotates which frequency owns
 *  which arc, giving each layer its own distribution. The last stop repeats
 *  the first so the ring closes seamlessly. */
function conicFromSpectrum(angleDeg, shift) {
  const n = spec.length;
  let stops = '';
  for (let i = 0; i <= n; i++) {
    const idx = (i + shift) % n;
    const c = GLOW_STOPS[idx % GLOW_STOPS.length];
    const a = (0.02 + 0.98 * spec[idx]).toFixed(3);
    stops += `rgba(${c[0]},${c[1]},${c[2]},${a}) ${((i / n) * 100).toFixed(1)}%`;
    if (i < n) stops += ',';
  }
  return `conic-gradient(from ${angleDeg.toFixed(1)}deg,${stops})`;
}

// --- layout ---------------------------------------------------------------

// Dark, not light. The previous white-tinted gradient made the panel brighter
// than whatever was behind it, so light text sat on light ground and the
// transcript was unreadable over most desktops.
const GLASS_FILL =
  'linear-gradient(157deg,rgba(17,19,27,.88),rgba(10,11,17,.93) 44%,rgba(15,17,25,.90))';
const GLASS_SHADOW =
  '0 42px 96px -26px rgba(0,0,0,.9),0 0 64px -18px rgba(160,130,255,.18),' +
  'inset 0 1px 0 rgba(255,255,255,.32),inset 0 0 0 1px rgba(255,255,255,.06)';

const VISUALS = ['ribbons', 'glow', 'minimal'];

function setVisual(name) {
  visual = VISUALS.includes(name) ? name : 'ribbons';
  el.stageRibbons.hidden = visual !== 'ribbons';
  el.stageGlow.hidden = visual !== 'glow';
  el.stageMinimal.hidden = visual !== 'minimal';
  applyLayout();
}

function setPhase(next) {
  const was = phase;
  phase = next;
  if (next === 'idle') hasText = false;
  // A new dictation starts a new recording, so the history restarts with it.
  if (next === 'listening' && was !== 'listening') minReset();
  applyLayout();
}

/* Layout depends on both the phase and whether a transcript has arrived, since
 * interim passes deliver text mid-dictation: the panel has to open while still
 * listening, not only once the key is released. */
function applyLayout() {
  const idle = phase === 'idle';
  const listening = phase === 'listening';
  const expanded = !idle && (hasText || !listening);

  levelTarget = idle ? 0 : listening ? 1 : 0.34;

  if (idle) {
    Object.assign(el.glass.style, {
      width: '180px', opacity: '0', background: 'transparent',
      boxShadow: 'none', marginTop: '0px',
    });
    el.wave.style.transform = 'scaleX(.06)';
    el.wave.style.opacity = '0';
    el.field.style.opacity = '0';
    el.behind.style.opacity = '0';
    el.blend.style.opacity = '0';
    el.body.style.height = '0px';
  } else if (!expanded) {
    // Glass stays fully transparent here: the ribbons float in air with nothing
    // behind them. Deliberate in the design, and it means this state needs no
    // desktop backdrop at all.
    Object.assign(el.glass.style, {
      width: '560px', opacity: '1', background: 'transparent',
      boxShadow: 'none', marginTop: '10px',
    });
    el.wave.style.transform = 'scaleX(1)';
    el.wave.style.opacity = '1';
    el.field.style.opacity = '0';
    el.behind.style.opacity = '0';
    el.blend.style.opacity = '0';
    el.body.style.height = '0px';
  } else {
    Object.assign(el.glass.style, {
      width: '620px', opacity: '1', background: GLASS_FILL,
      boxShadow: GLASS_SHADOW, marginTop: '14px',
    });
    el.wave.style.transform = 'scaleX(1)';
    el.wave.style.opacity = '1';
    el.field.style.opacity = '1';
    el.behind.style.opacity = '1';
    el.blend.style.opacity = '1';
    el.body.style.height = '210px';
  }

  el.shell.style.width = idle ? '300px' : expanded ? '560px' : '380px';
  el.shell.style.opacity = idle ? '0' : '1';
  el.aura.style.opacity = idle ? '0' : '1';
  el.gLine.classList.toggle('open', expanded);

  // minimal — one width, since the waveform is a fixed number of slots and
  // resizing it mid-recording would rescale the history.
  el.minCard.style.width = idle ? '300px' : '470px';
  el.minCard.style.opacity = idle ? '0' : '1';
  el.minLine.classList.toggle('open', expanded);
}

/* Render a transcript. `partial` marks it as an interim pass that a later one
 * will replace, so the tail is tapered the way the design's confirmation
 * cascade does — the most recent words are the least settled. */
function setText(text, partial) {
  const hosts = { ribbons: el.line, glow: el.gLine, minimal: el.minLine };
  const host = hosts[visual];
  // Clear the inactive ones, or switching treatment mid-session leaves a stale
  // transcript behind in the hidden stage.
  for (const [k, node] of Object.entries(hosts)) {
    if (k !== visual) node.textContent = '';
  }
  host.textContent = '';

  words = (text || '').split(/(\s+)/).filter(Boolean).map(w => {
    const s = document.createElement('span');
    s.textContent = w;
    host.appendChild(s);
    return s;
  });

  const n = words.length;
  words.forEach((w, i) => {
    if (!partial) { w.style.opacity = '1'; return; }
    const back = n - 1 - i;
    w.style.opacity = back === 0 ? '.34' : back === 1 ? '.62' : back === 2 ? '.85' : '1';
  });

  const caret = document.createElement('span');
  caret.className = 'caret';
  host.appendChild(caret);

  if ((n > 0) !== hasText) {
    hasText = n > 0;
    applyLayout();
  }
}

// --- frame ----------------------------------------------------------------

let last = performance.now();
let lastPaint = 0;

function tick(now) {
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;

  level += (levelTarget - level) * Math.min(1, dt * 3.4);
  t += dt * (phase === 'listening' ? 1 : 0.45);

  // Fast attack, slow decay. A symmetric filter makes speech look like mush,
  // because consonant transients are gone before a slow attack can reach them.
  const gate = phase === 'listening' ? 1 : 0;
  for (let i = 0; i < spec.length; i++) {
    const tgt = specTarget[i] * gate;
    const k = tgt > spec[i] ? dt * 34 : dt * 11;
    spec[i] += (tgt - spec[i]) * Math.min(1, k);
  }

  if (visual === 'minimal') {
    // Append a slice on a fixed clock rather than per frame, so the waveform
    // scrolls at a rate that does not depend on the display's refresh.
    if (phase === 'listening' && now - minLastPush >= MIN_PUSH_MS) {
      minLastPush = now;
      let peak = 0, sum = 0;
      for (let i = 0; i < spec.length; i++) {
        sum += spec[i];
        if (spec[i] > peak) peak = spec[i];
      }
      // Peak-weighted: the mean alone flattens syllables into a steady band,
      // and a recorder waveform lives on its transients.
      const v = Math.min(1, peak * 0.75 + (sum / spec.length) * 0.9);
      if (minHead < MIN_SLOTS) {
        minHistory[minHead++] = v;
      } else {
        minHistory.copyWithin(0, 1);
        minHistory[MIN_SLOTS - 1] = v;
      }
      minDirty = true;
    }

    // Redraw only when the history actually moved. It advances 11 times a
    // second while this loop runs at the display's refresh rate, so repainting
    // all 85 bars every frame was ~10,000 style writes per second for data
    // that changed 11 times.
    if (minDirty) {
      minDirty = false;
      for (let i = 0; i < MIN_SLOTS; i++) {
        // Past slices keep their recorded height; the rest are not drawn.
        minBars[i].style.transform =
          i < minHead ? `scaleY(${(0.035 + 0.965 * minHistory[i]).toFixed(3)})` : 'scaleY(0)';
      }
      const x = minHead * MIN_PITCH;
      el.minCursor.style.left = `${x}px`;
      el.minDots.style.left = `${x + 7}px`;
      el.minDots.style.right = '0px';
    }
    const title = phase === 'listening' ? 'NEW RECORDING' : 'TRANSCRIBING';
    if (el.minTitle.textContent !== title) el.minTitle.textContent = title;
    el.minCursor.style.opacity = phase === 'listening' ? '1' : '0';

  } else if (visual === 'ribbons') {
    for (let i = 0; i < RIBBONS.length; i++) {
      paths[i].setAttribute('d', ribbonPath(RIBBONS[i], t));
    }
  } else {
    let total = 0, peak = 0;
    for (let i = 0; i < spec.length; i++) {
      total += spec[i];
      if (spec[i] > peak) peak = spec[i];
    }
    const avg = total / spec.length;

    // Rebuilding a conic gradient forces a repaint of a blurred, filtered
    // element, so it is throttled to the rate the spectrum actually arrives at.
    const rebuild = now - lastPaint > 32;
    if (rebuild) lastPaint = now;

    if (rebuild) {
      // Displacement follows the loudest band, so a plosive tears the outline
      // rather than merely brightening it.
      gnDisp.setAttribute('scale', (10 + 140 * peak * level).toFixed(1));
      // Two slow, incommensurate drifts plus an energy term: the texture keeps
      // evolving instead of cycling, and breaks into finer filaments when loud.
      const bx = 0.010 + 0.006 * Math.sin(t * 0.37) + 0.022 * avg;
      const by = 0.016 + 0.007 * Math.cos(t * 0.29) + 0.026 * avg;
      gnTurb.setAttribute('baseFrequency', `${bx.toFixed(4)} ${by.toFixed(4)}`);
    }

    for (let i = 0; i < halos.length; i++) {
      const h = HALOS[i];
      const e = specBand(h.from, h.to) * level;
      if (rebuild) {
        halos[i].style.background = conicFromSpectrum(t * h.speed, h.shift);
        halos[i].style.filter =
          `${h.noise ? 'url(#glowNoise) ' : ''}blur(${(h.blur + 12 * (1 - e)).toFixed(1)}px)`;
      }
      halos[i].style.opacity = (h.base * (0.05 + 0.95 * e)).toFixed(3);
      halos[i].style.transform = `scale(${(0.93 + 0.18 * e).toFixed(4)})`;
    }

    if (rebuild) {
      const g = avg * level;
      el.box.style.boxShadow =
        `0 0 ${(24 + 130 * g).toFixed(0)}px ${(-10 + 40 * g).toFixed(0)}px rgba(52,134,255,${(0.16 + 0.72 * g).toFixed(3)}),` +
        `0 0 ${(8 + 44 * g).toFixed(0)}px rgba(160,220,255,${(0.08 + 0.5 * g).toFixed(3)}),` +
        'inset 0 1px 0 rgba(255,255,255,.18),' +
        'inset 0 0 0 1px rgba(140,200,255,.16),' +
        '0 30px 80px -30px rgba(0,0,0,.95)';
    }
  }

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
    if (Array.isArray(payload.bands)) setBands(payload.bands);
    if (payload.backdrop) setBackdrop(payload.backdrop);

    if (typeof payload.elapsed === 'number') {
      const s = Math.floor(payload.elapsed);
      const txt = `0:${String(s).padStart(2, '0')}`;
      el.timer.textContent = txt;
      el.gTimer.textContent = txt;
      el.minTime.textContent = `00:${String(s).padStart(2, '0')}`;
    }
    if (payload.status) {
      // Once a mode has formatted the text, naming it is more use than
      // "INSERTED" — it is the only place the routing decision is visible,
      // and a wrong rule is otherwise invisible until you read the output.
      const label = payload.mode ? payload.mode.toUpperCase() : payload.status;
      el.status.textContent = label;
      el.gStatus.textContent = label;
      el.status.style.color =
        payload.phase === 'listening' ? 'oklch(80% .12 350)' : 'oklch(76% .12 300)';
    }
    if (payload.model) {
      const name = payload.model.replace(/^ggml-/, '').replace(/\.bin$/, '').toUpperCase();
      const label = `${name} · ${payload.gpu ? 'GPU' : 'LOCAL'}`;
      el.engine.textContent = label;
      el.gEngine.textContent = label;
    }
    if (payload.text !== undefined && payload.text !== null) {
      setText(payload.text, !!payload.partial);
    }
  }).catch(err => console.error('Verba: listen() rejected', err));
}

setVisual('ribbons');
setPhase('idle');
