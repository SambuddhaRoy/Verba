# Drive the real Verba.exe end to end and watch where the words land.
#
#   powershell -NoProfile -File tools/e2e.ps1 [-Engine whisper.cpp -Model ggml-small.en-q5_1.bin]
#
# A speech recording stands in for the microphone (VERBA_FAKE_MIC), `Verba.exe
# --press` holds the hotkey, and a WinForms text box takes the output, so what
# is measured is what a user would see: when the first word appears, whether it
# keeps growing while the key is held, and what the text is at the end.
#
# It stops any running Verba, swaps in a test config, and restores both after.
# Run it on a desktop nobody is typing on: the text box needs the focus.
param(
  [string]$Exe = "$PSScriptRoot\..\dist\Verba.exe",
  [string]$Engine = 'whisper.cpp',
  [string]$Model = 'ggml-small.en-q5_1.bin',
  [int[]]$Only,        # run just these scenarios (1 live, 2 unload, 3 microphone, 4 live off)
  [switch]$Trace,      # print the log lines that explain what each scenario did
  [string]$Say = "Thanks for sending the deck over. I read the pricing section this morning and it mostly holds up. Let's talk on Thursday about the timeline."
)

$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
$utf8 = [System.Text.UTF8Encoding]::new($false)
$cfgPath = Join-Path $env:APPDATA 'Verba\config.json'
$logPath = Join-Path $env:LOCALAPPDATA 'Verba\verba.log'
$work = Join-Path ([IO.Path]::GetTempPath()) 'verba-e2e'
New-Item -ItemType Directory -Force $work | Out-Null

Add-Type -AssemblyName System.Speech, System.Windows.Forms, System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @'
using System; using System.Collections.Generic; using System.Diagnostics; using System.Drawing;
using System.Runtime.InteropServices; using System.Threading; using System.Windows.Forms;
public class Target : IDisposable {
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  Form f; TextBox tb; Thread th;
  public Stopwatch Clock = Stopwatch.StartNew();
  public List<long[]> Samples = new List<long[]>();   // {ms, text length}
  public int Packets, PacketsWithModifiers;
  public Target() {
    var ready = new ManualResetEventSlim();
    th = new Thread(() => {
      f = new Form { Width = 520, Height = 260, StartPosition = FormStartPosition.Manual, Location = new Point(80, 80), TopMost = true, Text = "Verba e2e target" };
      tb = new TextBox { Multiline = true, Dock = DockStyle.Fill, Font = new Font("Segoe UI", 12) };
      f.Controls.Add(tb);
      // Typed characters arrive as VK_PACKET. If any arrives with Ctrl or Alt
      // down, an app with shortcuts would have run them as shortcuts.
      tb.KeyDown += (s, e) => { if (e.KeyCode == (Keys)0xE7) { Packets++; if (e.Control || e.Alt) PacketsWithModifiers++; } };
      var timer = new System.Windows.Forms.Timer { Interval = 80 };
      timer.Tick += (s, e) => { lock (Samples) Samples.Add(new long[] { Clock.ElapsedMilliseconds, tb.TextLength }); };
      f.Shown += (s, e) => { Focus(); timer.Start(); ready.Set(); };
      Application.Run(f);
    });
    th.SetApartmentState(ApartmentState.STA); th.Start(); ready.Wait();
  }
  // Windows refuses SetForegroundWindow from a background thread unless an Alt
  // key went down first.
  public void Focus() {
    for (int i = 0; i < 20 && GetForegroundWindow() != f.Handle; i++) {
      f.Invoke((Action)(() => { keybd_event(0xA4, 0, 0, UIntPtr.Zero); SetForegroundWindow(f.Handle); keybd_event(0xA4, 0, 2, UIntPtr.Zero); f.Activate(); tb.Focus(); }));
      Thread.Sleep(100);
    }
  }
  public bool Focused { get { return GetForegroundWindow() == f.Handle; } }
  public string Text { get { string r = null; f.Invoke((Action)(() => r = tb.Text)); return r; } }
  public long[][] Snap() { lock (Samples) return Samples.ToArray(); }
  public void Reset() { f.Invoke((Action)(() => tb.Clear())); lock (Samples) Samples.Clear(); Packets = 0; PacketsWithModifiers = 0; Clock.Restart(); }
  public void Dispose() { f.Invoke((Action)(() => f.Close())); th.Join(); }
}
'@

$script:logStart = 0
$script:failed = 0
$script:total = 0
function Check($name, $ok, $detail) {
  $script:total++
  if ($ok) { Write-Host "  PASS $name" -ForegroundColor DarkGray }
  else { $script:failed++; Write-Host "  FAIL $name -- $detail" -ForegroundColor Red }
}

# --- config and process plumbing ---------------------------------------------

function Set-Key([string]$text, [string]$key, [string]$json) {
  $re = '("' + $key + '"\s*:\s*)(?:"[^"]*"|\w+)'
  $rx = [regex]::new($re)
  if ($rx.IsMatch($text)) { return $rx.Replace($text, [System.Text.RegularExpressions.MatchEvaluator]{ param($m) $m.Groups[1].Value + $json }, 1) }
  return $text.Insert($text.IndexOf('{') + 1, "`n  `"$key`": $json,")
}
function Start-Verba($live, $ejectSecs, $env2) {
  $t = [IO.File]::ReadAllText($cfgPath, $utf8).TrimStart([char]0xFEFF)
  $t = Set-Key $t 'live_typing' $live
  $t = Set-Key $t 'model_idle_eject_secs' $ejectSecs
  $t = Set-Key $t 'engine' ('"' + $Engine + '"')
  $t = Set-Key $t 'model' ('"' + $Model + '"')
  $t = Set-Key $t 'onboarded' 'true'
  $t = Set-Key $t 'preload_model' 'true'
  [IO.File]::WriteAllText($cfgPath, $t, $utf8)
  $env:VERBA_FAKE_MIC = $wav
  if ($env2) { $env:VERBA_FAKE_MIC_DIES_AFTER = $env2 } else { Remove-Item Env:VERBA_FAKE_MIC_DIES_AFTER -EA SilentlyContinue }
  # The log is appended to, so note where this run starts.
  $script:logStart = if (Test-Path $logPath) { (Get-Item $logPath).Length } else { 0 }
  $p = Start-Process $Exe -PassThru
  # Wait for the preloaded model rather than guessing: it loads after "ready".
  for ($i = 0; $i -lt 900 -and -not (Read-Log).Contains('loaded in'); $i++) { Start-Sleep -Milliseconds 100 }
  return $p
}
function Read-Log {
  try {
    $fs = [IO.File]::Open($logPath, 'Open', 'Read', 'ReadWrite')
    try {
      $null = $fs.Seek([Math]::Min($script:logStart, $fs.Length), 'Begin')
      return [IO.StreamReader]::new($fs, $utf8).ReadToEnd()
    } finally { $fs.Dispose() }
  } catch { return '' }
}
function Trace-Log {
  if (-not $Trace) { return }
  (Read-Log) -split "`n" | Where-Object { $_.Trim() } |
    ForEach-Object { Write-Host "    | $($_.TrimEnd())" -ForegroundColor DarkYellow }
}
function Want($n) { return (-not $Only) -or ($Only -contains $n) }
function Words($s) { return @(($s.ToLower() -replace "[^a-z0-9' ]", ' ').Split(' ', [StringSplitOptions]::RemoveEmptyEntries)) }
# Word error rate: edit distance over words, divided by the reference length.
function Wer($ref, $hyp) {
  $r = Words $ref; $h = Words $hyp
  $d = New-Object 'int[,]' ($r.Count + 1), ($h.Count + 1)
  for ($i = 0; $i -le $r.Count; $i++) { $d[$i, 0] = $i }
  for ($j = 0; $j -le $h.Count; $j++) { $d[0, $j] = $j }
  for ($i = 1; $i -le $r.Count; $i++) { for ($j = 1; $j -le $h.Count; $j++) {
    $im = $i - 1; $jm = $j - 1
    $c = if ($r[$im] -eq $h[$jm]) { 0 } else { 1 }
    $up = $d[$im, $j] + 1; $left = $d[$i, $jm] + 1; $diag = $d[$im, $jm] + $c
    $d[$i, $j] = [Math]::Min([Math]::Min($up, $left), $diag)
  } }
  return $d[$r.Count, $h.Count] / [Math]::Max(1, $r.Count)
}

# Hold the hotkey for `holdMs` and return what the text box did.
#
# This runs on a desktop someone may be using. If another window takes focus
# mid-dictation, live typing stops on purpose rather than type into the wrong
# document, and the run proves nothing. That case is retried, and only that
# case: the log has to say so.
function Dictate($target, [int]$holdMs, [int]$settleMs = 6000, [int]$attempt = 1) {
  $target.Reset(); $target.Focus()
  if (-not $target.Focused) { throw 'the test text box could not take focus' }
  $from = (Read-Log).Length
  $null = Start-Process $Exe -ArgumentList '--press', $holdMs -PassThru -WindowStyle Hidden
  Start-Sleep -Milliseconds ($holdMs + $settleMs)
  if ((Read-Log).Substring($from) -match 'live typing stopped' -and $attempt -lt 3) {
    Write-Host "    focus was taken mid-dictation, trying again ($attempt)" -ForegroundColor Yellow
    Start-Sleep -Seconds 2
    return Dictate $target $holdMs $settleMs ($attempt + 1)
  }
  $samples = @($target.Snap())
  $atRelease = ($samples | Where-Object { $_[0] -le $holdMs } | Select-Object -Last 1)
  return [pscustomobject]@{
    Text = $target.Text
    LenAtRelease = if ($atRelease) { $atRelease[1] } else { 0 }
    FirstTyped = ($samples | Where-Object { $_[1] -gt 0 } | Select-Object -First 1 | ForEach-Object { $_[0] })
    Steps = @($samples | Where-Object { $_[0] -le $holdMs } | ForEach-Object { $_[1] } | Select-Object -Unique).Count - 1
    Packets = $target.Packets
    Bad = $target.PacketsWithModifiers
  }
}

# --- the recording ------------------------------------------------------------

$wav = Join-Path $work 'speech.wav'
$syn = New-Object System.Speech.Synthesis.SpeechSynthesizer
$syn.SetOutputToWaveFile($wav, (New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, 'Sixteen', 'Mono')))
$syn.Speak($Say); $syn.Dispose()
$speechMs = [int](([IO.FileInfo]$wav).Length / 32)    # 16 kHz x 2 bytes
$holdMs = $speechMs + 1200
Write-Host "speech: $([math]::Round($speechMs / 1000, 1))s, engine $Engine / $Model"

$mine = Get-Process Verba -EA SilentlyContinue | Where-Object { $_.Path -eq $Exe }
$mine | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 600
$cfgBackup = [IO.File]::ReadAllBytes($cfgPath)
$target = $null; $verba = $null
try {
  $target = New-Object Target

  # 1. Live typing: words arrive while the key is still down.
  if (Want 1) {
  Write-Host "live typing"
  $verba = Start-Verba 'true' 600 $null
  $r = Dictate $target $holdMs
  $log = Read-Log
  Trace-Log
  Check 'the first word appears while the key is held' ($null -ne $r.FirstTyped -and $r.FirstTyped -lt $holdMs - 2500) "first word at $($r.FirstTyped)ms, key released at ${holdMs}ms"
  Check 'the text keeps growing during the hold' ($r.Steps -ge 3) "$($r.Steps) growth steps"
  Check 'most of the text is in before the key comes up' ($r.LenAtRelease -gt 0.4 * $r.Text.Trim().Length) "$($r.LenAtRelease) of $($r.Text.Trim().Length) chars"
  Check 'the final text matches what was said' ((Wer $Say $r.Text) -le 0.25) "WER $([math]::Round((Wer $Say $r.Text), 2)): [$($r.Text.Trim())]"
  Check 'typed characters never arrive as Ctrl or Alt shortcuts' ($r.Packets -gt 0 -and $r.Bad -eq 0) "$($r.Bad) of $($r.Packets) arrived with a modifier down"
  Check 'the hotkey watchdog took the injected press' ($log.Contains('hook missed a press') -and $log.Contains('hook reinstalled (ok)')) 'no recovery lines in the log'
  Check 'live mode logged its summary' ($log -match 'live: \d+ chars typed') 'no "live:" line'
  Write-Host "    first word at $($r.FirstTyped)ms of $holdMs, $($r.Steps) steps, $($r.LenAtRelease)/$($r.Text.Trim().Length) chars at release" -ForegroundColor DarkCyan
  }

  # 2. The model unloads when idle and must come back for the next dictation.
  if (Want 2) {
  Write-Host "idle unload and reload"
  if ($verba) { $verba | Stop-Process -Force; Start-Sleep -Milliseconds 600 }
  $verba = Start-Verba 'true' 5 $null
  $null = Dictate $target $holdMs 4000
  Start-Sleep -Seconds 12
  Check 'the model unloaded while idle' ((Read-Log) -match 'model unloaded') 'no "model unloaded" line'
  $r = Dictate $target $holdMs
  Check 'a dictation after the unload still types the text' ((Wer $Say $r.Text) -le 0.25) "WER $([math]::Round((Wer $Say $r.Text), 2)): [$($r.Text.Trim())]"
  Check 'the model was loaded again' (([regex]::Matches((Read-Log), 'loading ')).Count -ge 2) 'one load only'
  Trace-Log
  }

  # 3. A dead microphone stream is noticed and reopened.
  if (Want 3) {
  Write-Host "dead microphone"
  if ($verba) { $verba | Stop-Process -Force; Start-Sleep -Milliseconds 600 }
  $verba = Start-Verba 'true' 600 5
  Start-Sleep -Seconds 14
  $log = Read-Log
  Check 'a dead microphone stream is reopened' ($log -match 'not delivering audio, reopening it') 'no reopen line'
  Check 'the microphone was opened again' (([regex]::Matches($log, 'mic: fake')).Count -ge 2) 'opened once'
  $r = Dictate $target $holdMs
  Check 'dictation works after the microphone was reopened' ((Wer $Say $r.Text) -le 0.25) "WER $([math]::Round((Wer $Say $r.Text), 2)): [$($r.Text.Trim())]"
  Trace-Log
  }

  # 4. Live typing off: nothing until the key comes up, then the whole text.
  if (Want 4) {
  Write-Host "live typing off"
  if ($verba) { $verba | Stop-Process -Force; Start-Sleep -Milliseconds 600 }
  $verba = Start-Verba 'false' 600 $null
  $r = Dictate $target $holdMs
  Check 'nothing is typed while the key is held' ($r.LenAtRelease -eq 0) "$($r.LenAtRelease) chars before release"
  Check 'the whole text is inserted after release' ((Wer $Say $r.Text) -le 0.25) "WER $([math]::Round((Wer $Say $r.Text), 2)): [$($r.Text.Trim())]"
  Trace-Log
  }
}
finally {
  if ($verba) { $verba | Stop-Process -Force -EA SilentlyContinue }
  if ($target) { $target.Dispose() }
  [IO.File]::WriteAllBytes($cfgPath, $cfgBackup)
  Remove-Item Env:VERBA_FAKE_MIC -EA SilentlyContinue
  Remove-Item Env:VERBA_FAKE_MIC_DIES_AFTER -EA SilentlyContinue
  if ($mine) { Start-Process $Exe | Out-Null }
}

Write-Host ""
if ($script:failed -gt 0) { Write-Host "e2e: $($script:failed) of $($script:total) failed" -ForegroundColor Red; exit 1 }
Write-Host "e2e: $($script:total) passed" -ForegroundColor Green
