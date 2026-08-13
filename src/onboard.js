/* First-run flow.
 *
 * Reuses the settings window's commands wholesale — get_state, set_config,
 * download_model, pull_llm_model. Nothing here has its own backend surface
 * except the `onboarded` flag, so anything the user does in this flow is
 * reachable again from settings afterwards.
 *
 * The order is deliberate: microphone and model are the two things without
 * which the product does not work at all, so they come before the optional
 * rewrite step, and the try-out sits last where it can prove all three.
 */

const invoke = window.__TAURI__?.core?.invoke;
const listen = window.__TAURI__?.event?.listen;
const thisWindow = window.__TAURI__?.window?.getCurrentWindow?.();
const $ = id => document.getElementById(id);

/** The rewrite step removes itself when Ollama is not installed — offering a
 *  download for a server that is not there is a dead end, not a choice. */
const STEPS = ['welcome', 'mic', 'model', 'llm', 'try', 'done'];
let steps = [...STEPS];
let at = 0;

let s = null;      // last get_state payload
let cfg = null;
/** Set while a download is in flight so the footer can block on it. */
let busy = false;
/** Proven by the try-out step: a real dictation came back with text. */
let dictated = false;

function toast(msg) {
  const t = $('toast');
  t.textContent = msg;
  t.classList.add('on');
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => t.classList.remove('on'), 2600);
}

const save = () => invoke('set_config', { cfg }).catch(e => toast(`${e}`));

/* --------------------------------------------------------------- progress */

if (listen) {
  listen('verba:accent', ({ payload }) => applySystemTheme(payload.accent))
    .catch(e => console.error('accent listen rejected', e));

  listen('verba:download', ({ payload }) => {
    const card = $('rec-card');
    if (payload.file !== card.dataset.file) return;

    if (!payload.done) {
      const pct = payload.total ? (payload.received / payload.total) * 100 : 0;
      $('rec-fill').style.width = `${pct.toFixed(1)}%`;
      $('rec-txt').textContent = payload.stage === 'extracting'
        ? 'extracting…'
        : `${(payload.received / 1048576).toFixed(0)} / ${(payload.total / 1048576).toFixed(0)} MB`;
      return;
    }

    busy = false;
    card.classList.remove('downloading');
    if (payload.error) { toast(`Download failed: ${payload.error}`); render(); return; }

    // Downloading a model in this flow is only ever done in order to use it.
    cfg.model = payload.file;
    save();
    reload();
  }).catch(e => console.error('download listen rejected', e));

  listen('verba:llm-pull', ({ payload }) => {
    const wrap = $('llm-wrap');
    if (payload.name !== wrap.dataset.name) return;

    if (!payload.done) {
      const pct = payload.total ? (payload.completed / payload.total) * 100 : 0;
      $('lm-fill').style.width = `${pct.toFixed(1)}%`;
      $('lm-txt').textContent = payload.total
        ? `${(payload.completed / 1073741824).toFixed(1)} / ${(payload.total / 1073741824).toFixed(1)} GB`
        : payload.status;
      return;
    }

    busy = false;
    wrap.querySelector('.rec').classList.remove('downloading');
    if (payload.error) { toast(`Pull failed: ${payload.error}`); render(); return; }

    cfg.llm_model = payload.name;
    save();
    reload();
  }).catch(e => console.error('llm-pull listen rejected', e));

  // The try-out step. INSERTED is the only status that carries final text.
  listen('verba:state', ({ payload }) => {
    if (steps[at] !== 'try') return;
    const st = $('try-state'), heard = $('try-heard');

    if (payload.phase === 'listening') {
      st.textContent = 'Listening — keep holding, then let go.';
      st.className = 'tstate live';
      return;
    }
    if (payload.status === 'TRANSCRIBING' || payload.status === 'FORMATTING') {
      st.textContent = 'Transcribing…';
      st.className = 'tstate live';
      return;
    }
    if (payload.text) heard.textContent = payload.text;
    if (payload.status === 'INSERTED') {
      dictated = true;
      st.textContent = 'That worked.';
      st.className = 'tstate ok';
      render();
    }
  }).catch(e => console.error('state listen rejected', e));
}

/* ------------------------------------------------------------------- boot */

async function boot() {
  if (!invoke) {
    document.body.insertAdjacentHTML('afterbegin',
      '<p style="padding:20px;color:#f88">Tauri API unavailable — onboarding cannot load.</p>');
    return;
  }
  await reload();

  $('close').onclick = finish;      // closing early still counts as done
  $('back').onclick = () => { at = Math.max(0, at - 1); render(); };
  $('skip').onclick = () => advance();
  $('next').onclick = () => {
    if (at === steps.length - 1) finish();
    else advance();
  };
  $('startup').onclick = () => {
    cfg.launch_at_startup = !cfg.launch_at_startup;
    save();
    render();
  };
  $('mic').onchange = e => {
    cfg.microphone = e.target.value || null;
    save();
    $('mic-note').textContent = cfg.microphone
      ? 'Verba will record from this device.' : 'Using the Windows default.';
  };
}

function advance() {
  at = Math.min(steps.length - 1, at + 1);
  render();
}

async function reload() {
  s = await invoke('get_state');
  cfg = s.config;

  applySystemTheme(s.accent);

  // Drop the rewrite step when there is no server to talk to. Keep it if the
  // user has already chosen a model, so the flow does not silently hide a
  // setting that is switched on.
  const keepLlm = s.ollama_status !== 'missing' || !!cfg.llm_model;
  steps = STEPS.filter(k => k !== 'llm' || keepLlm);
  at = Math.min(at, steps.length - 1);

  buildMic();
  buildModel();
  buildLlm();
  buildKeys();
  buildSummary();
  render();
}

/* ------------------------------------------------------------------ steps */

function buildMic() {
  const sel = $('mic');
  sel.innerHTML = '<option value="">System default</option>';
  s.microphones.forEach(m => {
    const o = document.createElement('option');
    o.value = m;
    o.textContent = m;
    if (cfg.microphone === m) o.selected = true;
    sel.appendChild(o);
  });
  $('mic-note').textContent = cfg.microphone
    ? 'Verba will record from this device.' : 'Using the Windows default.';
}

/** The one model the hardware says to use, with the full list behind a fold. */
function buildModel() {
  const usable = s.models.filter(m => m.engine === 'whisper.cpp');
  // This step only offers whisper.cpp, because the other engines need a Python
  // sidecar installed first and that is not a first-run conversation. But the
  // flow also runs after an upgrade, where a working model may already be
  // chosen — possibly a Parakeet one. Leading with the hardware pick there
  // would prompt a several-hundred-megabyte download to replace something that
  // already works, so an installed current model wins.
  const current = s.models.find(m => m.file === cfg.model && m.installed);
  const rec = current || usable.find(m => m.file === s.recommendation.model) || usable[0];
  const card = $('rec-card');
  if (!rec) return;

  // The badge is a claim about hardware fit, so it only belongs on the model
  // the recommendation actually named.
  card.querySelector('.rec-tag').hidden = rec.file !== s.recommendation.model;

  // The two bars, and why this row was picked. The reason names the actual
  // hardware, which is what makes the recommendation feel like a decision
  // rather than a default.
  $('rec-rates').innerHTML = ratingBars(rec);
  $('rec-why').textContent = rec.file === s.recommendation.model
    ? `${s.recommendation.reason}.` : '';

  card.dataset.file = rec.file;
  $('rec-name').textContent = rec.name;
  $('rec-note').textContent =
    `${rec.size_mb} MB · ${rec.note}`;
  card.classList.toggle('have', rec.installed);

  // The hardware recommendation can be several hundred megabytes, and this is
  // a hard gate — nothing works until it lands. Say so, rather than leaving
  // the smaller option hidden behind a collapsed fold on a slow connection.
  const smaller = usable.filter(m => !m.installed && m.size_mb < rec.size_mb)
                        .sort((a, b) => a.size_mb - b.size_mb)[0];
  $('model-sub').textContent = rec.installed
    ? 'Verba needs one model file to transcribe. You already have this one.'
    : smaller && rec.size_mb > 250
      ? `Verba needs one model file to transcribe. This is the best match for your hardware; `
        + `if the download is too large, ${smaller.name} is ${smaller.size_mb} MB.`
      : 'Verba needs one model file to transcribe. This is the only download it requires.';
  $('rec-get').textContent = `Download ${rec.size_mb} MB`;
  $('rec-get').onclick = () => {
    busy = true;
    card.classList.add('downloading');
    render();
    invoke('download_model', { file: rec.file }).catch(err => {
      busy = false;
      card.classList.remove('downloading');
      toast(`${err}`);
      render();
    });
  };

  // Anything already on disk is selectable without a transfer; anything not is
  // listed for completeness but downloading it is a settings job, not a
  // first-run one.
  const host = $('model-list');
  host.innerHTML = '';
  usable.forEach(m => {
    const row = document.createElement('div');
    row.className = 'card row';
    // Bars here too: the fold is where someone goes specifically to weigh one
    // model against another, so it is the last place to hide the comparison.
    row.className = 'card';
    row.innerHTML =
      `<div class="row"><div class="grow"><b>${m.name}</b>` +
      `<small>${m.size_mb} MB · ${m.note}</small></div><span class="act"></span></div>` +
      ratingBars(m);
    const btn = document.createElement('button');
    btn.className = 'btn tiny';
    if (m.file === cfg.model) {
      btn.textContent = 'Selected';
      btn.disabled = true;
    } else if (m.installed) {
      btn.textContent = 'Use this';
      btn.onclick = () => { cfg.model = m.file; save(); reload(); };
    } else {
      btn.textContent = `Get ${m.size_mb} MB`;
      btn.onclick = () => {
        card.dataset.file = m.file;
        $('rec-name').textContent = m.name;
        $('rec-note').textContent = `${m.size_mb} MB · ${m.note}`;
        busy = true;
        card.classList.add('downloading');
        render();
        invoke('download_model', { file: m.file }).catch(err => {
          busy = false; card.classList.remove('downloading'); toast(`${err}`); render();
        });
      };
    }
    row.querySelector('.act').appendChild(btn);
    host.appendChild(row);
  });
}

function buildLlm() {
  const dot = $('ob-dot'), state = $('ob-state'), note = $('ob-note'),
        action = $('ob-action'), wrap = $('llm-wrap');
  dot.className = 'dot';
  action.hidden = true;
  wrap.hidden = s.ollama_status !== 'running';

  if (s.ollama_status === 'running') {
    dot.classList.add('up');
    state.textContent = 'RUNNING';
    note.textContent = 'Ready. Pick a model below and Verba will use it to polish dictation.';
  } else if (s.ollama_status === 'stopped') {
    dot.classList.add('down');
    state.textContent = 'STOPPED';
    note.textContent = 'Installed but not running.';
    action.hidden = false;
    action.textContent = 'Start it';
    action.onclick = () => {
      action.disabled = true;
      invoke('start_ollama')
        .then(() => reload())
        .catch(e => { action.disabled = false; toast(`${e}`); });
    };
  } else {
    dot.classList.add('gone');
    state.textContent = 'NOT INSTALLED';
    note.textContent = 'Install Ollama to enable this, then reopen Verba settings.';
    action.hidden = false;
    action.textContent = 'Open ollama.com';
    action.onclick = () => invoke('open_url', { url: 'https://ollama.com/download' })
      .catch(e => toast(`${e}`));
  }

  if (wrap.hidden) return;

  const rec = s.llm_models.find(m => m.name === s.llm_recommended)
           || s.llm_models.find(m => m.installed);
  if (!rec) { wrap.hidden = true; return; }

  wrap.dataset.name = rec.name;
  $('lm-name').textContent = rec.name;
  $('lm-note').textContent = rec.local_only
    ? 'Already on this machine.' : `${rec.size_gb.toFixed(1)} GB · ${rec.note}`;

  const card = wrap.querySelector('.rec');
  const chosen = cfg.llm_model === rec.name;
  card.classList.toggle('have', chosen);
  const get = $('lm-get');
  get.textContent = rec.installed ? 'Use this model' : `Download ${rec.size_gb.toFixed(1)} GB`;
  get.onclick = () => {
    if (rec.installed) { cfg.llm_model = rec.name; save(); reload(); return; }
    busy = true;
    card.classList.add('downloading');
    render();
    invoke('pull_llm_model', { name: rec.name }).catch(err => {
      busy = false; card.classList.remove('downloading'); toast(`${err}`); render();
    });
  };
}

function buildKeys() {
  const hk = cfg.hotkey;
  const parts = [];
  if (hk.ctrl) parts.push('Ctrl');
  if (hk.shift) parts.push('Shift');
  if (hk.alt) parts.push('Alt');
  if (hk.win) parts.push('Win');
  parts.push(hk.label);
  $('try-keys').innerHTML = parts.map(p => `<kbd>${p}</kbd>`).join('');
}

function buildSummary() {
  const hk = $('try-keys').textContent.replace(/([a-z])([A-Z])/g, '$1+$2');
  const model = s.models.find(m => m.file === cfg.model);
  const rows = [
    ['Hotkey', hk],
    ['Microphone', cfg.microphone || 'System default'],
    ['Speech model', model ? model.name : cfg.model],
    ['Rewrite model', cfg.llm_model || 'off'],
  ];
  $('summary').innerHTML = rows
    .map(([k, v]) => `<div><span>${k}</span><span>${v}</span></div>`).join('');
}

/* ----------------------------------------------------------------- render */

function render() {
  const key = steps[at];
  document.querySelectorAll('.step').forEach(el =>
    el.classList.toggle('on', el.dataset.step === key));

  const dots = $('dots');
  dots.innerHTML = steps
    .map((_, i) => `<i class="${i === at ? 'on' : i < at ? 'past' : ''}"></i>`).join('');

  $('back').style.visibility = at === 0 ? 'hidden' : 'visible';
  $('startup').classList.toggle('on', !!cfg.launch_at_startup);

  // The model step is the only hard gate: without a model file there is
  // nothing to transcribe with, and every later step would be a dead end.
  const noModel = key === 'model' && !s.models.some(m => m.file === cfg.model && m.installed);
  const next = $('next');
  next.disabled = busy || noModel;
  next.textContent = at === steps.length - 1 ? 'Start using Verba'
                   : key === 'try' && !dictated ? 'Skip the test'
                   : 'Continue';

  // Skip is for the steps where doing nothing is a legitimate choice.
  $('skip').hidden = !(key === 'llm' || (key === 'try' && !dictated)) || busy;
  if (key === 'try' && !dictated) $('skip').hidden = true;   // Next already says it
  buildSummary();
}

async function finish() {
  cfg.onboarded = true;
  await invoke('set_config', { cfg }).catch(e => toast(`${e}`));
  thisWindow?.close();
}

boot();
