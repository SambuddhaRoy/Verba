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

function render() {
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

  // Engine. faster-whisper is listed but not yet implemented; say so plainly
  // rather than letting someone select a dead option.
  document.querySelectorAll('#engine button').forEach(b => {
    if (b.dataset.v === 'faster-whisper' && !s.engines.includes('faster-whisper')) {
      b.disabled = true;
    }
    b.onclick = () => { cfg.engine = b.dataset.v; render(); save('Engine set'); };
  });
  $('engine-hint').textContent = s.engines.includes('faster-whisper')
    ? 'faster-whisper needs an NVIDIA GPU and a Python runtime.'
    : 'faster-whisper is not installed in this build. whisper.cpp reaches any GPU through Vulkan and needs no Python.';

  // Models.
  const wrap = $('models');
  s.models.forEach(m => {
    const b = document.createElement('button');
    b.className = 'model';
    b.dataset.file = m.file;
    const recommended = m.file === s.recommendation.model;
    const widest = Math.max(...s.models.map(x => x.size_mb));
    b.innerHTML = `
      <div class="top">
        <span class="nm">${m.name}</span>
        <span class="tag">${m.installed ? 'INSTALLED' : 'NOT DOWNLOADED'}</span>
        ${recommended ? '<span class="rec">RECOMMENDED</span>' : ''}
      </div>
      <div class="bar"><div style="width:${Math.round(m.size_mb / widest * 100)}%"></div></div>
      <div class="note${m.installed ? '' : ' miss'}">
        ${m.size_mb} MB · needs ~${(m.needs_mb / 1024).toFixed(1)} GB · ${m.note}
        ${m.installed ? '' : '<br>Place the file in the models folder to use it.'}
      </div>`;
    b.onclick = () => {
      if (!m.installed) { toast('That model is not downloaded yet'); return; }
      cfg.model = m.file;
      render();
      save('Model set — takes effect on next load');
    };
    wrap.appendChild(b);
  });

  document.querySelectorAll('.vis').forEach(b => {
    b.onclick = () => { cfg.visual = b.dataset.v; render(); save('Overlay style set'); };
  });

  $('logpath').textContent = s.log_path;
  $('cfgpath').textContent = s.config_path;

  render();
}

boot().catch(e => toast(`${e}`));
