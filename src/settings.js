/* Settings window.
 *
 * Every control writes straight through to the config file; there is no Save
 * button and no dirty state to get out of sync. The engine picks changes up on
 * the next dictation.
 */

const invoke = window.__TAURI__?.core?.invoke;
const currentWindow = window.__TAURI__?.window?.getCurrentWindow?.();

const listen = window.__TAURI__?.event?.listen;
const $ = id => document.getElementById(id);
let cfg = null;
let saveTimer = null;
/** Model to select automatically once its download finishes. */
let pendingSelect = null;

/* Download progress. Wired before boot() so a transfer already running when the
 * window opens still reports. */
if (listen) {
  listen('verba:download', ({ payload }) => {
    const row = document.querySelector(`.model[data-file="${CSS.escape(payload.file)}"]`);
    if (!row) return;

    if (!payload.done) {
      const pct = payload.total ? (payload.received / payload.total) * 100 : 0;
      row.querySelector('.dl-fill').style.width = `${pct.toFixed(1)}%`;
      row.querySelector('.dl-txt').textContent = payload.stage === 'extracting'
        ? 'extracting…'
        : `${(payload.received / 1048576).toFixed(0)} / ${(payload.total / 1048576).toFixed(0)} MB`;
      return;
    }

    row.classList.remove('downloading');
    if (payload.error) {
      toast(`Download failed: ${payload.error}`);
      pendingSelect = null;
      reload();
      return;
    }
    toast('Downloaded');
    // Selecting it here is the point of remembering: download then use, in one
    // action rather than two.
    if (pendingSelect === payload.file) {
      cfg.model = payload.file;
      pendingSelect = null;
      save('Model downloaded and selected');
    }
    reload();
  }).catch(e => console.error('download listen rejected', e));

  listen('verba:engine', ({ payload }) => {
    $('engine-hint').textContent = payload.error
      ? `${payload.message}: ${payload.error}`
      : payload.message;
    if (!payload.done) return;
    if (payload.error) toast(`Install failed: ${payload.error}`);
    else toast(`${payload.id} installed`);
    // Availability changed, so the engine list and model rows are both stale.
    reload();
  }).catch(e => console.error('engine listen rejected', e));
}

function toast(msg) {
  const t = $('toast');
  t.textContent = msg;
  t.classList.add('on');
  clearTimeout(toast.t);
  toast.t = setTimeout(() => t.classList.remove('on'), 1600);
}

/* Coalesce writes: dragging a slider would otherwise hit the disk on every
 * pixel of travel. */
function save(note) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      await invoke('set_config', { cfg });
      if (note) toast(note);
    } catch (e) {
      toast(`Could not save: ${e}`);
    }
  }, 180);
}

// --- panels ---------------------------------------------------------------

document.querySelectorAll('.nav').forEach(btn => {
  btn.onclick = () => {
    document.querySelectorAll('.nav').forEach(b => b.classList.toggle('on', b === btn));
    const id = `p-${btn.dataset.panel}`;
    document.querySelectorAll('.panel').forEach(p => p.classList.toggle('on', p.id === id));
  };
});

if (currentWindow) {
  $('close').onclick = () => currentWindow.hide();
  $('min').onclick = () => currentWindow.minimize();
}

// --- controls -------------------------------------------------------------

function bindToggle(key, note) {
  const el = $(key);
  el.onclick = () => {
    cfg[key] = !cfg[key];
    el.classList.toggle('on', cfg[key]);
    save(note);
  };
}

function fmtEject(secs) {
  if (secs === 0) return 'never';
  if (secs < 60) return `${secs}s`;
  return `${Math.round(secs / 60)} min`;
}

/* --- hotkey capture ------------------------------------------------------
 *
 * Browsers give us `event.code`, Windows wants a virtual-key code. Only the
 * keys anyone actually binds are mapped; anything else is rejected with a
 * message rather than silently stored as 0.
 */
function codeToVk(code) {
  if (/^Key[A-Z]$/.test(code)) return { vk: 0x41 + code.charCodeAt(3) - 65, label: code[3] };
  if (/^Digit[0-9]$/.test(code)) return { vk: 0x30 + +code[5], label: code[5] };
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return { vk: 0x70 + (+code.slice(1) - 1), label: code };
  const fixed = {
    Space: [0x20, 'Space'], Enter: [0x0D, 'Enter'], Tab: [0x09, 'Tab'],
    Backquote: [0xC0, '`'], Minus: [0xBD, '-'], Equal: [0xBB, '='],
    BracketLeft: [0xDB, '['], BracketRight: [0xDD, ']'], Backslash: [0xDC, '\\'],
    Semicolon: [0xBA, ';'], Quote: [0xDE, "'"], Comma: [0xBC, ','],
    Period: [0xBE, '.'], Slash: [0xBF, '/'], Insert: [0x2D, 'Insert'],
    Home: [0x24, 'Home'], End: [0x23, 'End'], PageUp: [0x21, 'PgUp'],
    PageDown: [0x22, 'PgDn'],
  }[code];
  return fixed ? { vk: fixed[0], label: fixed[1] } : null;
}

function drawHotkey(hk, arming) {
  const keys = $('hk-keys');
  keys.classList.toggle('arm', !!arming);
  const parts = [];
  if (hk.ctrl) parts.push('Ctrl');
  if (hk.shift) parts.push('Shift');
  if (hk.alt) parts.push('Alt');
  if (hk.win) parts.push('Win');
  parts.push(hk.label);
  keys.innerHTML = parts.map(p => `<kbd>${p}</kbd>`).join('<s>+</s>');
}

let arming = false;
function armCapture() {
  arming = true;
  $('hk-capture').classList.add('arm');
  $('hk-capture').textContent = 'Press keys…';
  $('hk-note').textContent = 'Hold your modifiers and press the main key. Esc cancels.';
}
function disarmCapture() {
  arming = false;
  $('hk-capture').classList.remove('arm');
  $('hk-capture').textContent = 'Change';
  $('hk-note').textContent =
    'Held to dictate. Swallowed while held, so the focused app never sees it.';
  drawHotkey(cfg.hotkey, false);
}

window.addEventListener('keydown', e => {
  if (!arming) return;
  e.preventDefault();
  if (e.code === 'Escape') { disarmCapture(); return; }
  // Modifier-only presses just update the preview; wait for a real key.
  if (['ControlLeft','ControlRight','ShiftLeft','ShiftRight','AltLeft','AltRight','MetaLeft','MetaRight'].includes(e.code)) {
    drawHotkey({ ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, win: e.metaKey, label: '…' }, true);
    return;
  }
  const mapped = codeToVk(e.code);
  if (!mapped) { toast(`${e.code} can't be used as a hotkey`); return; }
  if (!e.ctrlKey && !e.altKey && !e.metaKey) {
    // A bare key, or Shift+key, would fire during ordinary typing.
    toast('Include Ctrl, Alt or Win, or the hotkey will fire while you type');
    return;
  }
  cfg.hotkey = {
    ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, win: e.metaKey,
    vk: mapped.vk, label: mapped.label,
  };
  disarmCapture();
  save('Hotkey set');
}, true);

function render() {
  drawHotkey(cfg.hotkey, false);
  $('launch_at_startup').classList.toggle('on', cfg.launch_at_startup);
  $('preload_model').classList.toggle('on', cfg.preload_model);
  $('language').value = cfg.language;
  $('microphone').value = cfg.microphone ?? '';

  $('threads').value = cfg.threads ?? 0;
  $('threads-val').textContent = cfg.threads ? `${cfg.threads} threads` : 'auto';

  $('model_idle_eject_secs').value = cfg.model_idle_eject_secs;
  $('eject-val').textContent = fmtEject(cfg.model_idle_eject_secs);

  document.querySelectorAll('#engine button')
    .forEach(b => b.classList.toggle('on', b.dataset.v === cfg.engine));
  document.querySelectorAll('.model')
    .forEach(b => b.hidden = b.dataset.engine !== cfg.engine);
  document.querySelectorAll('.vis')
    .forEach(b => b.classList.toggle('on', b.dataset.v === cfg.visual));
  document.querySelectorAll('.model')
    .forEach(b => b.classList.toggle('on', b.dataset.file === cfg.model));
}

// --- boot -----------------------------------------------------------------

async function boot() {
  if (!invoke) {
    document.body.insertAdjacentHTML('afterbegin',
      '<p style="padding:20px;color:#f88">Tauri API unavailable — settings cannot load.</p>');
    return;
  }

  const s = await invoke('get_state');
  cfg = s.config;

  // Windows accent, applied before anything paints with it. Everything else in
  // this window is grey, so these four values are the entire palette.
  if (s.accent) {
    const r = document.documentElement.style;
    r.setProperty('--accent', s.accent.base);
    // The base accent can be dark enough to be unreadable on a dark surface;
    // Windows uses the light variants for exactly that reason.
    r.setProperty('--accent-light', s.accent.light2);
    r.setProperty('--accent-dim', s.accent.dark1);
    r.setProperty('--accent-rgb', s.accent.rgb);
  }

  // Hardware summary in the sidebar.
  const hw = s.hardware;
  const vram = hw.vram_mb >= 1024 ? `${(hw.vram_mb / 1024).toFixed(0)} GB` : `${hw.vram_mb} MB`;
  $('rig-line').innerHTML =
    `${hw.cores}C/${hw.threads}T · ${(hw.ram_mb / 1024).toFixed(0)} GB RAM<br>` +
    `${hw.gpu}<br>${vram} · ${hw.gpu_backend.toUpperCase()}`;
  $('rig-note').textContent = hw.gpu_backend === 'cpu'
    ? 'CPU only — this build has no GPU backend.'
    : 'GPU offload active. All processing on-device.';

  $('rec-line').textContent = `Recommended for this machine: ${s.recommendation.reason}.`;

  // Microphones.
  const mic = $('microphone');
  s.microphones.forEach(name => {
    const o = document.createElement('option');
    o.value = name; o.textContent = name;
    mic.appendChild(o);
  });
  mic.onchange = () => { cfg.microphone = mic.value || null; save('Microphone set'); };

  $('language').onchange = e => { cfg.language = e.target.value; save('Language set'); };

  bindToggle('launch_at_startup', 'Startup preference saved');
  bindToggle('preload_model', 'Preload preference saved');

  $('threads').max = hw.threads;
  $('threads').oninput = e => {
    const v = +e.target.value;
    cfg.threads = v === 0 ? null : v;
    $('threads-val').textContent = v === 0 ? 'auto' : `${v} threads`;
    save();
  };

  $('model_idle_eject_secs').oninput = e => {
    cfg.model_idle_eject_secs = +e.target.value;
    $('eject-val').textContent = fmtEject(cfg.model_idle_eject_secs);
    save();
  };

  // Engines. Unavailable ones stay visible but unselectable, and clicking one
  // explains why rather than doing nothing.
  const segs = $('engine');
  s.engines.forEach(en => {
    const b = document.createElement('button');
    b.dataset.v = en.id;
    b.textContent = en.name;
    b.title = en.note;
    if (!en.available) b.classList.add('off');
    b.onclick = () => {
      $('engine-hint').textContent = en.note;
      if (en.available) {
        cfg.engine = en.id;
        // Engines name their models differently, so carry the selection over
        // to something this one can actually load.
        const first = s.models.find(m => m.engine === en.id);
        if (first && !s.models.some(m => m.file === cfg.model && m.engine === en.id)) {
          cfg.model = first.file;
        }
        render();
        save('Engine set');
        return;
      }
      if (en.installable) {
        b.disabled = true;
        b.textContent = 'Installing…';
        invoke('install_engine', { id: en.id }).catch(err => {
          b.disabled = false;
          b.textContent = en.name;
          toast(`${err}`);
        });
      } else {
        toast(`${en.name} is not built into this version`);
      }
    };
    segs.appendChild(b);
  });
  $('engine-hint').textContent =
    s.engines.find(e => e.id === cfg.engine)?.note ?? '';

  // Models.
  const wrap = $('models');
  const widest = Math.max(...s.models.map(x => x.size_mb));
  const engineOf = id => s.engines.find(e => e.id === id);

  s.models.forEach(m => {
    const row = document.createElement('div');
    row.className = 'model';
    row.dataset.file = m.file;
    row.dataset.engine = m.engine;
    const built = engineOf(m.engine)?.available;
    const recommended = m.file === s.recommendation.model;
    const tag = !built ? 'ENGINE NOT BUILT' : m.installed ? 'INSTALLED' : 'NOT DOWNLOADED';

    row.innerHTML = `
      <div class="top">
        <span class="nm">${m.name}</span>
        <span class="tag">${tag}</span>
        ${m.streaming ? '<span class="stream">STREAMING</span>' : ''}
        ${recommended ? '<span class="rec">RECOMMENDED</span>' : ''}
        <span class="act"></span>
      </div>
      <div class="bar"><div style="width:${Math.round(m.size_mb / widest * 100)}%"></div></div>
      <div class="note${built && m.installed ? '' : ' miss'}">
        ${m.size_mb} MB · needs ~${(m.needs_mb / 1024).toFixed(1)} GB · ${m.license} · ${m.note}
      </div>
      <div class="dl"><div class="dl-fill"></div><span class="dl-txt"></span></div>`;

    const act = row.querySelector('.act');
    if (!built) {
      const why = document.createElement('span');
      why.className = 'why';
      why.textContent = 'why?';
      why.onclick = () => toast(`${engineOf(m.engine)?.name}: ${engineOf(m.engine)?.note}`);
      act.appendChild(why);
    } else if (!m.installed) {
      const dl = document.createElement('button');
      dl.className = 'btn tiny';
      dl.textContent = `Download ${m.size_mb} MB`;
      dl.onclick = e => {
        e.stopPropagation();
        dl.disabled = true;
        dl.textContent = 'Starting…';
        row.classList.add('downloading');
        // Remember what to select once it lands, so downloading a model and
        // then using it is one click rather than two.
        pendingSelect = m.file;
        invoke('download_model', { file: m.file }).catch(err => {
          row.classList.remove('downloading');
          dl.disabled = false;
          dl.textContent = `Download ${m.size_mb} MB`;
          toast(`${err}`);
        });
      };
      act.appendChild(dl);
    }

    row.onclick = () => {
      if (!built) { toast(`Needs the ${engineOf(m.engine)?.name} engine, which isn't built yet`); return; }
      if (!m.installed) { toast('Download it first'); return; }
      cfg.model = m.file;
      render();
      save('Model set — takes effect on next dictation');
    };
    wrap.appendChild(row);
  });

  buildModes(s);

  $('models-dir').textContent = s.models_dir;
  $('open-models').onclick = () =>
    invoke('reveal_models_dir').catch(e => toast(`${e}`));
  $('hk-capture').onclick = () => (arming ? disarmCapture() : armCapture());

  document.querySelectorAll('.vis').forEach(b => {
    b.onclick = () => { cfg.visual = b.dataset.v; render(); save('Overlay style set'); };
  });

  $('logpath').textContent = s.log_path;
  $('cfgpath').textContent = s.config_path;

  render();
}

/* --- modes, routing and vocabulary --------------------------------------- */

function modeOptions(selected) {
  return cfg.modes
    .map(m => `<option value="${m.id}"${m.id === selected ? ' selected' : ''}>${m.name}</option>`)
    .join('');
}

/** Rules are rebuilt wholesale on change. The list is short and the
 *  alternative — patching indices in place after a delete — is where
 *  off-by-one bugs live. */
function renderRules() {
  const host = $('rules');
  host.innerHTML = '';
  cfg.rules.forEach((r, i) => {
    const row = document.createElement('div');
    row.className = 'rule';
    row.innerHTML = `
      <select>${modeOptions(r.mode)}</select>
      <input class="exe" placeholder="Code.exe, devenv.exe" value="${(r.exe || []).join(', ')}">
      <input class="ttl" placeholder="title contains… (optional)" value="${r.title ?? ''}">
      <button class="del" title="Remove">&#10005;</button>`;
    const [sel, exe, ttl] = [row.querySelector('select'), row.querySelector('.exe'), row.querySelector('.ttl')];
    sel.onchange = () => { cfg.rules[i].mode = sel.value; save(); };
    exe.onchange = () => {
      cfg.rules[i].exe = exe.value.split(',').map(s => s.trim()).filter(Boolean);
      save('Routing saved');
    };
    ttl.onchange = () => {
      cfg.rules[i].title = ttl.value.trim() || null;
      save('Routing saved');
    };
    row.querySelector('.del').onclick = () => {
      cfg.rules.splice(i, 1);
      renderRules();
      save('Rule removed');
    };
    host.appendChild(row);
  });
}

function buildModes(s) {
  // Rewrite model. An unreachable Ollama returns nothing, so keep whatever is
  // configured as a choice rather than silently switching models underneath.
  const sel = $('llm_model');
  const names = s.llm_models.length ? s.llm_models : [cfg.llm_model];
  sel.innerHTML = names
    .map(n => `<option value="${n}"${n === cfg.llm_model ? ' selected' : ''}>${n}</option>`)
    .join('');
  $('llm-note').textContent = s.llm_models.length
    ? 'Used only by modes with the model pass switched on.'
    : `No local models found at ${cfg.llm_url}. Modes with the model pass on will insert the cleaned transcript instead.`;
  sel.onchange = () => { cfg.llm_model = sel.value; save('Rewrite model set'); };

  const def = $('default_mode');
  def.innerHTML = modeOptions(cfg.default_mode);
  def.onchange = () => { cfg.default_mode = def.value; save('Fallback mode set'); };

  const host = $('modes');
  host.innerHTML = '';
  cfg.modes.forEach((m, i) => {
    const card = document.createElement('div');
    card.className = 'mode';
    card.innerHTML = `
      <div class="hd">
        <span class="nm">${m.name}</span><span class="id">${m.id}</span>
        <span class="sw"><span>model pass</span><button class="tgl${m.llm ? ' on' : ''}"></button></span>
      </div>
      <div class="desc">${m.description}</div>`;
    const tgl = card.querySelector('.tgl');

    // Instructions only matter when the model pass is on, so the box appears
    // with it rather than sitting inert.
    const ta = document.createElement('textarea');
    ta.spellcheck = false;
    ta.value = m.instructions;
    ta.placeholder = 'Instructions for the rewrite model…';
    ta.hidden = !m.llm;
    ta.onchange = () => { cfg.modes[i].instructions = ta.value; save('Instructions saved'); };
    card.appendChild(ta);

    tgl.onclick = () => {
      cfg.modes[i].llm = !cfg.modes[i].llm;
      tgl.classList.toggle('on', cfg.modes[i].llm);
      ta.hidden = !cfg.modes[i].llm;
      save(cfg.modes[i].llm ? 'Model pass on' : 'Model pass off');
    };
    host.appendChild(card);
  });

  renderRules();
  $('add-rule').onclick = () => {
    cfg.rules.push({ mode: cfg.default_mode, exe: [], title: null });
    renderRules();
    save();
  };

  const vocab = $('vocabulary');
  vocab.value = (cfg.vocabulary || []).join('\n');
  vocab.onchange = () => {
    cfg.vocabulary = vocab.value.split('\n').map(s => s.trim()).filter(Boolean);
    save('Vocabulary saved');
  };
}

/** Rebuild the panels that depend on what's on disk. */
async function reload() {
  const keep = document.querySelector('.nav.on')?.dataset.panel;
  $('models').innerHTML = '';
  $('engine').innerHTML = '';
  $('modes').innerHTML = '';
  $('rules').innerHTML = '';
  $('microphone').innerHTML = '<option value="">System default</option>';
  await boot();
  if (keep) document.querySelector(`.nav[data-panel="${keep}"]`)?.click();
}

boot().catch(e => toast(`${e}`));
