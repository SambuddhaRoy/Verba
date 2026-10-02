/* Assertions for the three Verba windows, run in headless Edge.
 *
 * A third of this codebase is frontend and until now it had one self-check, so
 * every UI regression has been found by a human looking at a screenshot. These
 * are the checks that would have caught the ones that actually happened:
 * onboarding steps stretched to full height by an inherited `flex: 1`, a
 * vocabulary cache keyed on nothing, "Print ( hello)", a model step offering a
 * 547MB download to someone who already had a working model.
 *
 * Each window is loaded with `window.__TAURI__` stubbed and a real `--state`
 * payload, its own script inlined, then this file. Results are written into
 * #uicheck so `--dump-dom` can carry them back out.
 */

const CHECKS = {};

/* ------------------------------------------------------------- settings */

CHECKS.settings = async (t, s) => {
  await t.boot(() => document.querySelectorAll('.model').length > 0);

  t.is('no JS errors', window.__errors.length === 0, window.__errors.join('; '));

  const panels = [...document.querySelectorAll('.nav')].map(n => n.dataset.panel);
  t.is('every nav has a panel', panels.every(p => document.getElementById(`p-${p}`)),
       panels.join(','));

  const models = [...document.querySelectorAll('.model')];
  t.is('models listed', models.length > 0, `${models.length}`);
  t.is('every model has two rating bars',
       models.every(m => m.querySelectorAll('.rates .rate').length === 2),
       `${models.filter(m => m.querySelectorAll('.rates .rate').length !== 2).length} wrong`);

  // The bars are the whole point of the accuracy/speed work: a blended score
  // would hide the trade-off, so there must be two distinctly-classed fills.
  const first = models[0];
  t.is('accuracy and speed are distinct fills',
       !!first.querySelector('.rt > i.acc') && !!first.querySelector('.rt > i.spd'));

  t.is('exactly one model recommended',
       document.querySelectorAll('.model .rec').length === 1,
       `${document.querySelectorAll('.model .rec').length}`);

  t.is('version is not hardcoded',
       document.getElementById('about-sub').textContent.includes(s.version),
       document.getElementById('about-sub').textContent);

  // Light mode: the tokens must actually flip, or the theme is cosmetic only.
  const root = document.documentElement;
  const tintOf = () => getComputedStyle(root).getPropertyValue('--tint-rgb').trim();
  applySystemTheme({ ...s.accent, theme: 'dark' });
  const dark = tintOf();
  applySystemTheme({ ...s.accent, theme: 'light' });
  const light = tintOf();
  t.is('light mode inverts the surface tint', dark !== light && light.startsWith('0'),
       `dark=${dark} light=${light}`);
  t.is('light mode sets data-theme', root.dataset.theme === 'light', root.dataset.theme);
  applySystemTheme(s.accent);

  // Packs and learning render from their own commands, not get_state.
  await t.settle();
  t.is('packs listed', document.querySelectorAll('#pack-list .card').length > 0,
       `${document.querySelectorAll('#pack-list .card').length}`);

  // The network panel is the thing that makes the local-first claim checkable,
  // so an empty log has to read as reassurance rather than as a broken widget.
  window.__net = [];
  buildNetwork();
  await t.settle();
  t.is('an empty log says so',
       /No outbound requests/.test(document.getElementById('net-count').textContent),
       document.getElementById('net-count').textContent);
  t.is('an empty log lists nothing',
       document.querySelectorAll('#net-list .net-row').length === 0);

  // Loopback must be marked, not hidden: a call to a local Ollama is not the
  // app phoning home, and showing them identically would be alarming or
  // dishonest depending on which way it erred.
  window.__net = [
    { at: 1770000000, method: 'GET', host: 'api.github.com',
      url: 'https://api.github.com/x', purpose: 'check for a new version', local: false },
    { at: 1770000001, method: 'POST', host: '127.0.0.1:11434',
      url: 'http://127.0.0.1:11434/api/generate', purpose: 'rewrite', local: true },
  ];
  buildNetwork();
  await t.settle();
  const rows = [...document.querySelectorAll('#net-list .net-row')];
  t.is('requests are listed', rows.length === 2, `${rows.length}`);
  t.is('loopback is distinguished from off-machine',
       rows.filter(r => r.classList.contains('local')).length === 1,
       `${rows.filter(r => r.classList.contains('local')).length} marked local`);
  t.is('the count names how many left the machine',
       /1 off this PC/.test(document.getElementById('net-count').textContent),
       document.getElementById('net-count').textContent);
  t.is('a destination is shown',
       rows.some(r => r.querySelector('.h').textContent === 'api.github.com'));

  // The overlay used to have four treatments and a picker. It has one now, so
  // the picker, its panel and its nav entry must all be gone.
  t.is('there is no appearance panel',
       !document.getElementById('p-appearance') && !document.querySelector('.nav[data-panel="appearance"]'));
  t.is('there is no overlay picker', document.querySelectorAll('.vis').length === 0);

  // Live typing is a plain toggle that writes straight through to the config.
  const live = document.getElementById('live_typing');
  t.is('live typing has a toggle in the dictation panel',
       !!live && document.getElementById('p-dictation').contains(live));
  const before = cfg.live_typing;
  live.click();
  t.is('the live typing toggle flips the setting',
       cfg.live_typing === !before && live.classList.contains('on') === cfg.live_typing,
       `cfg=${cfg.live_typing}`);
  live.click();

  // The window shrink workaround moved to General, and your own vocabulary
  // terms moved from the long Modes page to the Vocabulary page.
  t.is('the shrink workaround is under General',
       document.getElementById('p-general').contains(document.getElementById('tight_overlay_window')));
  t.is('your vocabulary terms are on the vocabulary page',
       document.getElementById('p-learning').contains(document.getElementById('vocabulary')));

  // Copy: no em dashes in the visible text of any panel.
  const dashed = [...document.querySelectorAll('section *')]
    .filter(n => n.children.length === 0 && n.textContent.includes('\u2014'))
    .map(n => n.textContent.trim().slice(0, 40));
  t.is('no em dashes in the settings copy', dashed.length === 0, dashed.join(' | '));
};

/* ------------------------------------------------------------ onboarding */

CHECKS.onboard = async (t, s) => {
  await t.boot(() => document.querySelectorAll('.step').length > 0);
  t.is('no JS errors', window.__errors.length === 0, window.__errors.join('; '));

  t.is('starts on the first step',
       document.querySelector('.step.on')?.dataset.step === 'welcome',
       document.querySelector('.step.on')?.dataset.step);

  // The bug this catches: settings.css styles bare `section` as its scrolling
  // pane, and inheriting flex: 1 stretched every step to the full column,
  // which silently defeated the centring.
  const step = document.querySelector('.step.on');
  const main = document.querySelector('main.ob');
  const mcs = getComputedStyle(main);
  // Against the *content* height, not the border box. Comparing to the border
  // box made this check unfalsifiable: main's 16px of vertical padding meant a
  // fully stretched step still measured smaller, so it passed with the bug
  // deliberately reintroduced.
  const avail = main.clientHeight
    - parseFloat(mcs.paddingTop) - parseFloat(mcs.paddingBottom);
  const h = step.getBoundingClientRect().height;
  t.is('a step does not stretch to fill the column', h < avail - 2,
       `step=${Math.round(h)} available=${Math.round(avail)}`);
  t.is('a step does not grow', getComputedStyle(step).flexGrow === '0',
       `flex-grow=${getComputedStyle(step).flexGrow}`);

  // A fresh machine: nothing downloaded. Continue must be blocked, because
  // every later step is a dead end without a model.
  const fresh = JSON.parse(JSON.stringify(s));
  fresh.models.forEach(m => (m.installed = false));
  fresh.config.model = 'ggml-small.en-q5_1.bin';
  window.__state = fresh;
  at = 0;
  await reload();
  at = steps.indexOf('model');
  render();
  t.is('model step gates Continue when nothing is installed',
       document.getElementById('next').disabled === true);
  t.is('recommended model offers a download',
       /Download/.test(document.getElementById('rec-get').textContent),
       document.getElementById('rec-get').textContent);
  t.is('recommendation shows rating bars',
       document.querySelectorAll('#rec-rates .rate').length === 2);

  // An upgrade with a working model must not be told to download another.
  const have = JSON.parse(JSON.stringify(s));
  have.models.forEach(m => (m.installed = m.file === have.config.model));
  window.__state = have;
  at = 0;
  await reload();
  at = steps.indexOf('model');
  render();
  t.is('an installed model does not gate Continue',
       document.getElementById('next').disabled === false);
};

/* -------------------------------------------------------------- overlay */

CHECKS.overlay = async (t, s) => {
  await t.boot(() => typeof tick === 'function');
  t.is('no JS errors', window.__errors.length === 0, window.__errors.join('; '));

  const stage = document.getElementById('stage-orb');
  const orb = document.getElementById('orb');
  const wave = () => document.querySelector('#orb-ribbons path').getAttribute('d');

  // The render loop is driven here rather than waited on. Headless Chrome does
  // not reliably run requestAnimationFrame under --virtual-time-budget, so the
  // geometry would never be written and the check would pass or fail for the
  // wrong reason. tick() is the same function the rAF loop calls.
  let now = performance.now();
  const advance = frames => { for (let i = 0; i < frames; i++) { now += 16; tick(now); } };
  // Every real payload carries all 24 bands; an empty array would resize the
  // spectrum to nothing and turn the wave into NaN.
  const said = payload =>
    window.__subs['verba:state']({ payload: { bands: new Array(24).fill(0), ...payload } });

  // The visualiser not reacting to speech was reported twice. The check is that
  // the geometry actually changes with the spectrum.
  said({ phase: 'listening', status: 'LISTENING', bands: new Array(24).fill(0.02) });
  advance(30);
  const quiet = wave();
  said({ phase: 'listening', status: 'LISTENING', bands: new Array(24).fill(0.95) });
  advance(30);
  const loud = wave();
  t.is('the wave is drawn at all', !!quiet && quiet.length > 20, `d=${quiet}`);
  t.is('the wave responds to the spectrum', quiet !== loud,
       `quiet=${(quiet || '').slice(0, 40)} loud=${(loud || '').slice(0, 40)}`);
  t.is('the capsule is small while listening', orb.classList.contains('s-listen'), orb.className);

  said({ phase: 'transcribing', status: 'TRANSCRIBING' });
  advance(30);
  t.is('the capsule narrows while working', orb.classList.contains('s-think'), orb.className);
  // Height of the wave about its centre line (80 in a 160 viewBox). With no
  // voice to draw the working pulse must still be visible, not a flat line.
  const swing = () => Math.max(...wave().match(/,(-?[\d.]+|NaN)/g).map(m => Math.abs(parseFloat(m.slice(1)) - 80)));
  t.is('the wave shows activity while working', swing() > 12, `swing=${swing()}`);
  t.is('the label shimmers while working', stage.classList.contains('thinking'));

  said({ phase: 'transcribing', status: 'INSERTED', mode: 'Email', text: 'Hi John, is the report ready?' });
  t.is('the capsule opens into a panel for the text', orb.classList.contains('s-panel'), orb.className);
  t.is('the label names the mode', document.getElementById('orb-label').textContent === 'Email',
       document.getElementById('orb-label').textContent);
  t.is('the label stops shimmering once inserted', !stage.classList.contains('thinking'));

  // Live typing sends no text at all: the words go into the app instead, so
  // the overlay must stay a capsule rather than opening a panel to nothing.
  said({ phase: 'idle', status: '' });
  said({ phase: 'listening', status: 'LISTENING' });
  said({ phase: 'transcribing', status: 'INSERTED' });
  t.is('a dictation with no text never opens the panel', !orb.classList.contains('s-panel'), orb.className);
  said({ phase: 'idle', status: '' });
  t.is('the capsule tucks away when idle', orb.classList.contains('s-idle'), orb.className);

  // Full spectrum: violet to red along the wave, in both themes. Every ribbon
  // uses the one gradient, so the colours cannot wash out where ribbons cross.
  const hue = rgb => {
    const [r, g, b] = rgb.match(/[\d.]+/g).slice(0, 3).map(Number).map(v => v / 255);
    const max = Math.max(r, g, b), d = max - Math.min(r, g, b);
    if (d === 0) return 0;
    const h = max === r ? ((g - b) / d + (g < b ? 6 : 0)) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    return h * 60;
  };
  const ps = [...document.querySelectorAll('#orb-ribbons path')];
  t.is('seven ribbons', ps.length === 7, `${ps.length}`);
  t.is('every ribbon is filled with the spectrum gradient',
       ps.every(p => getComputedStyle(p).fill.includes('url') && getComputedStyle(p).fill.includes('#spectrum')),
       ps.map(p => getComputedStyle(p).fill).join(' '));
  t.is('ribbons are not blended into each other',
       ps.every(p => getComputedStyle(p).mixBlendMode === 'normal'),
       ps.map(p => getComputedStyle(p).mixBlendMode).join(' '));
  for (const theme of ['dark', 'light']) {
    said({ phase: 'listening', status: 'LISTENING', accent: { theme } });
    const stops = [...document.querySelectorAll('#spectrum stop')].map(n => getComputedStyle(n).stopColor);
    const hues = stops.map(hue);
    const near = (lo, hi) => hues.some(h => h >= lo && h <= hi);
    t.is(`${theme}: seven stops, all different`, stops.length === 7 && new Set(stops).size === 7, stops.join(' '));
    t.is(`${theme}: the wave covers violet to red`,
         (near(0, 15) || near(345, 360)) && near(35, 70) && near(100, 170) && near(195, 235) && near(250, 295),
         hues.map(h => Math.round(h)).join(','));
    t.is(`${theme}: violet is at the left and red at the right`,
         hue(stops[0]) > 240 && (hue(stops[6]) < 20 || hue(stops[6]) > 340),
         `${Math.round(hue(stops[0]))} to ${Math.round(hue(stops[6]))}`);
    t.is(`${theme}: the theme attribute is set`, document.documentElement.dataset.theme === theme);
  }

  // Text flips to dark on light glass.
  said({ phase: 'listening', status: 'LISTENING', accent: { theme: 'dark' } });
  const darkInk = getComputedStyle(document.getElementById('orb-line')).color;
  said({ phase: 'listening', status: 'LISTENING', accent: { theme: 'light' } });
  const lightInk = getComputedStyle(document.getElementById('orb-line')).color;
  t.is('the text flips to dark on light glass',
       parseFloat(darkInk.match(/[\d.]+/)[0]) > 200 && parseFloat(lightInk.match(/[\d.]+/)[0]) < 80,
       `dark=${darkInk} light=${lightInk}`);

  // Translucent, not a slab: the desktop has to show through in both themes.
  for (const theme of ['dark', 'light']) {
    said({ phase: 'listening', status: 'LISTENING', accent: { theme } });
    const a = parseFloat(getComputedStyle(stage).getPropertyValue('--tint-a'));
    t.is(`${theme}: the glass is translucent`, a > 0 && a < 0.6, `tint alpha ${a}`);
  }
};

/* ------------------------------------------------------------- harness */

async function runChecks(which, state) {
  const results = [];
  const t = {
    is(name, ok, detail) {
      results.push({ name, ok: !!ok, detail: ok ? '' : detail || '' });
    },
    /** Wait for the window's own async boot to finish. */
    async boot(ready) {
      for (let i = 0; i < 120 && !ready(); i++) await new Promise(r => setTimeout(r, 25));
      if (!ready()) results.push({ name: 'window booted', ok: false, detail: 'timed out' });
    },
    settle: () => new Promise(r => setTimeout(r, 120)),
    /** One rendered frame, two rAFs deep so work scheduled by the first tick
     *  has also been drawn.
     *
     *  Raced against a timer because headless Chrome under
     *  --virtual-time-budget does not always drive requestAnimationFrame, and
     *  a frame wait that never resolves takes the whole page's results with
     *  it — the checks simply never report. */
    frame: () => new Promise(r => {
      let settled = false;
      const done = () => { if (!settled) { settled = true; r(); } };
      requestAnimationFrame(() => requestAnimationFrame(done));
      setTimeout(done, 60);
    }),
  };

  try {
    await CHECKS[which](t, state);
  } catch (e) {
    results.push({ name: `${which} threw`, ok: false, detail: `${e && e.stack || e}` });
  }

  const out = document.createElement('pre');
  out.id = 'uicheck';
  out.textContent = results
    .map(r => `${r.ok ? 'PASS' : 'FAIL'} ${which}: ${r.name}${r.detail ? ` -- ${r.detail}` : ''}`)
    .join('\n') + `\nDONE ${which} ${results.filter(r => !r.ok).length}`;
  document.body.appendChild(out);
}
