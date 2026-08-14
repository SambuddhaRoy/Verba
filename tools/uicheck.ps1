# Render the three Verba windows in headless Edge and assert on the result.
#
#   powershell -NoProfile -File tools/uicheck.ps1
#
# The frontend is a third of this codebase and had one self-check, so every UI
# regression has been caught by a person looking at a screenshot. This runs the
# same checks without one.
#
# Each window is loaded with window.__TAURI__ stubbed and a real --state
# payload, so the assertions run against the shape the backend actually emits
# rather than a fixture that drifts. Results come back through --dump-dom.
param(
  [string]$State,          # a --state payload; produced from dist\Verba.exe if omitted
  [string]$Exe = "$PSScriptRoot\..\dist\Verba.exe",
  [switch]$KeepPages       # leave the generated pages behind for inspection
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$work = Join-Path ([System.IO.Path]::GetTempPath()) "verba-uicheck"
New-Item -ItemType Directory -Force -Path $work | Out-Null

function Read-Utf8($path) {
  [System.Text.UTF8Encoding]::new($false).GetString([System.IO.File]::ReadAllBytes($path))
}
function Write-Utf8($path, $text) {
  [System.IO.File]::WriteAllText($path, $text, [System.Text.UTF8Encoding]::new($false))
}

# Chrome's helper processes inherit the redirected stdout handle and can still
# hold it briefly after the parent exits, so a plain read races them.
function Read-Utf8Shared($path) {
  for ($i = 0; $i -lt 40; $i++) {
    try {
      $fs = [System.IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
      try {
        return (New-Object System.IO.StreamReader($fs, [System.Text.UTF8Encoding]::new($false))).ReadToEnd()
      } finally { $fs.Dispose() }
    } catch {
      Start-Sleep -Milliseconds 50
    }
  }
  return ''
}

# --- the state payload ------------------------------------------------------

if (-not $State) {
  if (-not (Test-Path $Exe)) { Write-Error "no state given and $Exe does not exist"; exit 1 }
  # --state logs rather than printing: this is a windows-subsystem binary, so
  # stdout goes nowhere. Read the log's bytes, not the console, or non-ASCII
  # comes back mangled.
  $log = Join-Path $env:LOCALAPPDATA 'Verba\verba.log'
  Remove-Item $log -Force -ErrorAction SilentlyContinue
  # Bounded, because an unattended runner will otherwise sit on a hung child
  # until the job's own timeout. Start-Process rather than the call operator so
  # there is a handle to wait on and kill.
  $proc = Start-Process $Exe -ArgumentList '--state' -PassThru -WindowStyle Hidden
  if (-not $proc.WaitForExit(60000)) {
    try { $proc.Kill() } catch {}
    Write-Error "$Exe --state did not exit within 60s"
    exit 1
  }
  if (-not (Test-Path $log)) { Write-Error "--state produced no log"; exit 1 }
  $text = Read-Utf8 $log
  $State = Join-Path $work 'state.json'
  Write-Utf8 $State $text.Substring($text.IndexOf('{'))
}
$statejson = Read-Utf8 $State
Write-Host "state: $State ($([math]::Round($statejson.Length/1KB,1)) KB)"

# --- headless browser -------------------------------------------------------

$browser = @(
  "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe",
  "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
  "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
  "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { Write-Error 'no Edge or Chrome found'; exit 1 }
Write-Host "browser: $browser"

# --- build a page per window ------------------------------------------------

$checks = Read-Utf8 (Join-Path $PSScriptRoot 'uicheck.js')

# Windows are keyed by the html file and the script they load.
$windows = @(
  @{ name = 'settings'; html = 'settings.html'; js = @('ratings.js', 'settings.js') },
  @{ name = 'onboard';  html = 'onboard.html';  js = @('ratings.js', 'onboard.js') },
  @{ name = 'overlay';  html = 'overlay.html';  js = @('wave.js') }
)

$stub = @"
<script>
// Enough of the Tauri surface for a window to boot. Commands that only matter
// for their side effects return null; the ones the UI renders from return
// plausible data so the assertions have something to check.
window.__state = __STATE__;
window.__errors = [];
addEventListener('error', e => window.__errors.push(String(e.message)));
addEventListener('unhandledrejection', e => window.__errors.push('reject: ' + e.reason));
window.__TAURI__ = {
  core: {
    invoke: async (cmd) => {
      if (cmd === 'get_state') return JSON.parse(JSON.stringify(window.__state));
      if (cmd === 'list_packs') return [
        { id:'code', name:'Code and programming', description:'d', terms:['a'], hints:[], transforms:['x => y'], user:false },
        { id:'medical', name:'Medical', description:'d', terms:['a'], hints:[], transforms:[], user:false },
        { id:'legal', name:'Legal', description:'d', terms:['a'], hints:[], transforms:[], user:false },
      ];
      if (cmd === 'network_log') return window.__net || [];
      if (cmd === 'clear_network_log') { window.__net = []; return null; }
      if (cmd === 'learned_corrections') return [];
      if (cmd === 'last_dictation') return null;
      if (cmd === 'check_update') return null;
      return null;
    },
  },
  event: { listen: async (n, cb) => { (window.__subs = window.__subs || {})[n] = cb; return () => {}; } },
  window: { getCurrentWindow: () => ({ close(){}, minimize(){}, hide(){} }) },
};
</script>
"@

$pages = @()
foreach ($w in $windows) {
  $html = Read-Utf8 (Join-Path $root "src\$($w.html)")

  # Inline the stylesheets: the page is loaded from a temp directory, and a
  # cached or unresolved sheet would make every layout assertion meaningless.
  foreach ($css in @('settings', 'onboard', 'overlay')) {
    $p = Join-Path $root "src\$css.css"
    if (Test-Path $p) {
      $html = $html -replace "<link rel=`"stylesheet`" href=`"$css\.css`">", ('<style>' + (Read-Utf8 $p) + '</style>')
    }
  }

  # Inline the scripts, with the stub ahead of them so the window sees a Tauri
  # API at load, and the checks after so they run against a booted window.
  $inline = $stub.Replace('__STATE__', $statejson)
  foreach ($js in $w.js) {
    $inline += "<script>" + (Read-Utf8 (Join-Path $root "src\$js")) + "</script>"
  }
  $inline += "<script>$checks</script>"
  $inline += "<script>runChecks('$($w.name)', window.__state);</script>"

  foreach ($js in $w.js) {
    $html = $html -replace "<script src=`"$([regex]::Escape($js))`"></script>", ''
  }
  $html = $html -replace '</body>', ($inline + '</body>')

  $out = Join-Path $work "$($w.name).html"
  Write-Utf8 $out $html
  $pages += @{ name = $w.name; path = $out }
}

# --- run --------------------------------------------------------------------

$failed = 0
$total = 0
foreach ($p in $pages) {
  $dump = Join-Path $work "$($p.name).dom"
  # --virtual-time-budget lets the page's async boot and the checks complete
  # before the DOM is dumped; without it the dump races the first await.
  # Not $args: that is an automatic variable, and shadowing it is how a
  # native call silently receives nothing.
  $browserArgs = @(
    '--headless=new', '--disable-gpu', '--no-sandbox', '--hide-scrollbars',
    '--virtual-time-budget=15000', '--window-size=1100,760',
    '--dump-dom', ('file:///' + ($p.path -replace '\\', '/'))
  )
  # Windows PowerShell turns a native program's stderr into a terminating error
  # under ErrorActionPreference='Stop', and headless Edge writes chatter there
  # on a server SKU ("LLM: Not supported on non Desktop SKU"). That aborted the
  # whole check on CI while passing locally, where Edge stays quiet. Stderr is
  # kept for diagnosis rather than discarded.
  # Also bounded. Headless Chrome can sit forever on a runner if a page never
  # settles, and a hung browser is indistinguishable from a slow one until the
  # job times out an hour later.
  $prev = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $out = Join-Path $work "$($p.name).out"
  $b = Start-Process $browser -ArgumentList $browserArgs -PassThru -NoNewWindow `
       -RedirectStandardOutput $out -RedirectStandardError (Join-Path $work "$($p.name).err")
  if (-not $b.WaitForExit(90000)) {
    try { $b.Kill() } catch {}
    Write-Host "FAIL $($p.name): the browser did not exit within 90s" -ForegroundColor Red
    $ErrorActionPreference = $prev
    $failed++
    continue
  }
  $ErrorActionPreference = $prev
  $dom = if (Test-Path $out) { Read-Utf8Shared $out } else { '' }
  Write-Utf8 $dump $dom

  # Pull the block out by its container first. Splitting the whole dump on
  # newlines loses the first result, because --dump-dom puts it on the same
  # line as the opening tag.
  $block = ''
  if ($dom -match '(?s)id="uicheck">(.*?)</pre>') { $block = $matches[1] }
  $lines = ($block -split "`n") | Where-Object { $_ -match '^(PASS|FAIL|DONE) ' }
  if (-not ($lines | Where-Object { $_ -match '^DONE ' })) {
    Write-Host "FAIL $($p.name): checks did not run (see $dump)" -ForegroundColor Red
    $failed++
    continue
  }
  foreach ($l in $lines) {
    if ($l -match '^DONE ') { continue }
    $total++
    if ($l -match '^FAIL ') {
      Write-Host "  $($l.Trim())" -ForegroundColor Red
      $failed++
    } else {
      Write-Host "  $($l.Trim())" -ForegroundColor DarkGray
    }
  }
}

if (-not $KeepPages) { Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue }

Write-Host ""
if ($failed -gt 0) {
  Write-Host "ui check: $failed of $total failed" -ForegroundColor Red
  exit 1
}
Write-Host "ui check: $total passed" -ForegroundColor Green
