/* Settings window.
 *
 * Every control writes straight through to the config file; there is no Save
 * button and no dirty state to get out of sync. The engine picks changes up on
 * the next dictation.
 */

const invoke = window.__TAURI__?.core?.invoke;
const currentWindow = window.__TAURI__?.window?.getCurrentWindow?.();

const $ = id => document.getElementById(id);
let cfg = null;
let saveTimer = null;

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
      if (!en.available) {
        $('engine-hint').textContent = `${en.name}: ${en.note} Not built into this version.`;
        toast(`${en.name} is not available yet`);
        return;
      }
      cfg.engine = en.id;
      $('engine-hint').textContent = en.note;
      render();
      save('Engine set');
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
    const b = document.createElement('button');
    b.className = 'model';
    b.dataset.file = m.file;
    b.dataset.engine = m.engine;
    const usable = engineOf(m.engine)?.available && m.installed;
    const recommended = m.file === s.recommendation.model;
    const tag = !engineOf(m.engine)?.available ? 'ENGINE NOT BUILT'
              : m.installed ? 'INSTALLED' : 'NOT DOWNLOADED';
    b.innerHTML = `
      <div class="top">
        <span class="nm">${m.name}</span>
        <span class="tag">${tag}</span>
        ${m.streaming ? '<span class="stream">STREAMING</span>' : ''}
        ${recommended ? '<span class="rec">RECOMMENDED</span>' : ''}
      </div>
      <div class="bar"><div style="width:${Math.round(m.size_mb / widest * 100)}%"></div></div>
      <div class="note${usable ? '' : ' miss'}">
        ${m.size_mb} MB · needs ~${(m.needs_mb / 1024).toFixed(1)} GB · ${m.license} · ${m.note}
        ${m.installed || m.engine !== 'whisper.cpp' ? '' : '<br>Place the file in the models folder to use it.'}
      </div>`;
    b.onclick = () => {
      if (!engineOf(m.engine)?.available) {
        toast(`Needs the ${engineOf(m.engine)?.name} engine, which isn't built yet`);
        return;
      }
      if (!m.installed) { toast('That model is not downloaded yet'); return; }
      cfg.model = m.file;
      render();
      save('Model set — takes effect on next load');
    };
    wrap.appendChild(b);
  });

  $('hk-capture').onclick = () => (arming ? disarmCapture() : armCapture());

  document.querySelectorAll('.vis').forEach(b => {
    b.onclick = () => { cfg.visual = b.dataset.v; render(); save('Overlay style set'); };
  });

  $('logpath').textContent = s.log_path;
  $('cfgpath').textContent = s.config_path;

  render();
}

boot().catch(e => toast(`${e}`));
