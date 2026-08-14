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
  await t.boot(() => typeof setVisual === 'function');
  t.is('accent palette self-test', accentSelfTest().includes('passed'), accentSelfTest());

  for (const v of ['ribbons', 'glow', 'minimal']) {
    setVisual(v);
    t.is(`visual ${v} shows its stage`,
         !document.getElementById(`stage-${v}`).hidden);
  }

  // The visualisers not reacting to speech was reported twice. The check is
  // that the geometry actually changes with the spectrum.
  setVisual('ribbons');
  setPhase('listening');
  const path = () => document.querySelector('#ribbons path').getAttribute('d');

  // The render loop is driven here rather than waited on. Headless Chrome does
  // not reliably run requestAnimationFrame under --virtual-time-budget, so the
  // geometry would never be written and the check would pass or fail for the
  // wrong reason. tick() is the same function the rAF loop calls.
  let now = performance.now();
  const advance = frames => {
    for (let i = 0; i < frames; i++) { now += 16; tick(now); }
  };

  setBands(new Array(24).fill(0.02));
  advance(30);
  const quiet = path();
  setBands(new Array(24).fill(0.95));
  advance(30);
  const loud = path();

  t.is('the wave is drawn at all', !!quiet && quiet.length > 20, `d=${quiet}`);
  t.is('ribbons respond to the spectrum', !!loud && quiet !== loud,
       `quiet=${(quiet || '').slice(0, 40)} loud=${(loud || '').slice(0, 40)}`);

  // Accent tinting, including the two edge cases that were real bugs: a grey
  // accent must not gain a hue, a saturated one must not go neon.
  applyAccent('#6B6B6B', 'dark');
  const grey = document.querySelector('#ribbons path').getAttribute('fill').match(/\d+/g).map(Number);
  t.is('a grey accent stays grey', Math.max(...grey) - Math.min(...grey) === 0, grey.join(','));

  applyAccent('#0078D4', 'dark');
  const blue = document.querySelector('#ribbons path').getAttribute('fill').match(/\d+/g).map(Number);
  t.is('a saturated accent is not neon', Math.max(...blue) <= 245, blue.join(','));

  applyAccent(s.accent.base, 'light');
  t.is('overlay follows the theme', document.documentElement.dataset.theme === 'light');
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
