/* Verba overlay renderer.
 *
 * A live spectrum from the engine drives seven ribbons, violet to red, inside a
 * glass capsule. The spectrum sets the wave's height along its length, so the
 * shape changes with the voice and not just the size:
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
/** Samples per ribbon path. */
const N = 96;
const STATES = ['s-idle', 's-listen', 's-think', 's-panel'];

const $ = id => document.getElementById(id);
const el = {
  stage: $('stage-orb'), orb: $('orb'), bloom: $('orb-bloom'), behind: $('orb-behind'),
  label: $('orb-label'), time: $('orb-time'), line: $('orb-line'),
};
const paths = [...document.querySelectorAll('#orb-ribbons path')];

let phase = 'idle';
let level = 0, levelTarget = 0;   // phase envelope: fades the whole thing in and out
let t = 0;                        // animation clock
let hasText = false;

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

/** Paint the captured desktop into the backdrop canvas. It arrives at 1/8
 *  scale and CSS stretches it back up; the upscale plus the blur is what makes
 *  it read as frosted glass rather than a thumbnail. */
function setBackdrop(bd) {
  if (!bd) return;
  const bin = atob(bd.rgba);
  const buf = new Uint8ClampedArray(bin.length);
  for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
  if (buf.length !== bd.width * bd.height * 4) return;
  el.behind.width = bd.width;
  el.behind.height = bd.height;
  el.behind.getContext('2d').putImageData(new ImageData(buf, bd.width, bd.height), 0, 0);
}

/** The capsule is glass in both themes: dark over a dark desktop, pale over a
 *  light one. All the colour changes live in overlay.css. */
function setTheme(theme) {
  document.documentElement.dataset.theme = theme === 'light' ? 'light' : 'dark';
}

/** Spectrum sampled at an arbitrary position, linearly interpolated. */
function specAt(u) {
  const n = spec.length;
  const x = Math.min(0.9999, Math.max(0, u)) * (n - 1);
  const i = Math.floor(x);
  const f = x - i;
  return spec[i] * (1 - f) + spec[Math.min(n - 1, i + 1)] * f;
}

function ribbonPath(r, t, amp = level) {
  const top = [], bot = [];
  for (let i = 0; i <= N; i++) {
    const u = i / N;
    // Spindle taper, so every ribbon meets the centre line at both ends.
    const env = Math.pow(Math.sin(Math.PI * u), 1.4);
    // Low frequencies on the left, sibilance on the right.
    const s = specAt(u + r.off);
    // A travelling sine keeps the ribbons distinct from one another and alive
    // between syllables; the spectrum modulates its amplitude.
    const wob = Math.sin(u * Math.PI * 2 * r.k + r.p + t * r.s);
    const dy = env * wob * r.a * H * 0.46 * (0.06 + 0.94 * s) * amp;
    top.push((u * W).toFixed(1) + ',' + (CY - dy).toFixed(2));
    bot.push((u * W).toFixed(1) + ',' + (CY + dy).toFixed(2));
  }
  return 'M' + top.join('L') + 'L' + bot.reverse().join('L') + 'Z';
}

function setPhase(next) {
  phase = next;
  if (next === 'idle') hasText = false;
  applyLayout();
}

/* The shape depends on both the phase and whether a transcript has arrived:
 * interim passes deliver text mid-dictation, so the panel opens while still
 * listening, not only once the key is released. */
function applyLayout() {
  const idle = phase === 'idle';
  const listening = phase === 'listening';
  levelTarget = idle ? 0 : listening ? 1 : 0.34;

  const state = idle ? 's-idle' : hasText ? 's-panel' : listening ? 's-listen' : 's-think';
  for (const node of [el.stage, el.orb]) {
    node.classList.remove(...STATES);
    node.classList.add(state);
  }
  // The clock only means something while recording; afterwards it reads 0:00.
  el.stage.classList.toggle('live', listening);
}

/* Render a transcript. `partial` marks it as an interim pass that a later one
 * will replace, so the tail is tapered: the newest words are the least
 * settled. */
function setText(text, partial) {
  el.line.textContent = '';
  const words = (text || '').split(/(\s+)/).filter(Boolean).map(w => {
    const s = document.createElement('span');
    s.textContent = w;
    el.line.appendChild(s);
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
  el.line.appendChild(caret);

  if ((n > 0) !== hasText) {
    hasText = n > 0;
    applyLayout();
  }

  // The text box has a fixed ceiling, so a long dictation keeps its newest
  // words in view and fades the top edge instead.
  el.line.scrollTop = el.line.scrollHeight;
  el.line.classList.toggle('clip', el.line.scrollHeight > el.line.clientHeight);
}

let last = performance.now();

/** One animation step. It does not schedule the next, so a test can call it
 *  directly: when it did, every call started another loop, and a check that
 *  stepped a few hundred times left a few hundred loops redrawing in parallel. */
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

  // While working there is no voice to draw, so a slow pulse travels along the
  // wave instead: the activity indicator Siri shows in the same place.
  if (phase === 'transcribing') {
    for (let i = 0; i < spec.length; i++) {
      spec[i] = Math.max(spec[i], 0.3 + 0.3 * Math.sin(t * 6 - i * 0.5));
    }
  }
  // The thinking wave is small in a small pill, so it is drawn at more than
  // the listening level or it reads as a flat line.
  const amp = phase === 'listening' ? level : level * 2;
  for (let i = 0; i < paths.length; i++) {
    paths[i].setAttribute('d', ribbonPath(RIBBONS[i % RIBBONS.length], t, amp));
  }

  let sum = 0;
  for (let i = 0; i < spec.length; i++) sum += spec[i];
  const e = Math.min(1, (sum / spec.length) * 1.6) * level;
  // The capsule breathes with the voice, a few percent at most, and the glow it
  // casts brightens. `scale` rather than `transform`, which the CSS state
  // transitions own.
  el.orb.style.scale = (1 + 0.045 * e).toFixed(4);
  el.bloom.style.opacity = `calc(var(--bloom-a) * ${(level * (0.1 + 0.45 * e)).toFixed(3)})`;
  el.bloom.style.scale = `${(0.9 + 0.3 * e).toFixed(4)} ${(0.85 + 0.5 * e).toFixed(4)}`;
}

function frame(now) {
  tick(now);
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

// --- driven by the Rust engine ---------------------------------------------

const api = window.__TAURI__?.event;
if (!api?.listen) {
  // Standalone in a browser, or the ACL denied the API. Leave a trace rather
  // than sitting invisibly at opacity 0, which is indistinguishable from the
  // window failing to show at all.
  console.error('Verba: __TAURI__.event.listen unavailable, overlay will not update');
} else {
  api.listen('verba:state', ({ payload }) => {
    if (payload.phase !== phase) setPhase(payload.phase);
    if (Array.isArray(payload.bands)) setBands(payload.bands);
    if (payload.backdrop) setBackdrop(payload.backdrop);
    if (payload.accent?.theme) setTheme(payload.accent.theme);

    if (typeof payload.elapsed === 'number') {
      el.time.textContent = `0:${String(Math.floor(payload.elapsed)).padStart(2, '0')}`;
    }
    if (payload.status) {
      // Once a mode has formatted the text, naming it is more use than
      // "Inserted": it is the only place the routing decision is visible, and
      // a wrong rule is otherwise invisible until you read the output.
      const status = payload.status.charAt(0) + payload.status.slice(1).toLowerCase();
      el.label.textContent = payload.mode || status;
      el.stage.classList.toggle('thinking',
        payload.status === 'TRANSCRIBING' || payload.status === 'FORMATTING');
    }
    if (payload.text !== undefined && payload.text !== null) {
      setText(payload.text, !!payload.partial);
    }
  }).catch(err => console.error('Verba: listen() rejected', err));

  // Sent on its own when the Windows theme changes, so the glass switches
  // between dark and light without waiting for the next dictation.
  api.listen('verba:accent', ({ payload }) => {
    if (payload.accent?.theme) setTheme(payload.accent.theme);
  }).catch(err => console.error('Verba: accent listen rejected', err));
}

setTheme('dark');
setPhase('idle');
