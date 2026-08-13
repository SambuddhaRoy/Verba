/* Accuracy and speed bars, shared by the settings and onboarding windows.
 *
 * Two separate bars, never one blended score: accuracy and speed pull against
 * each other, and that trade-off is the entire reason someone is comparing
 * models. Averaging it away would leave them picking blind.
 *
 * Speed is the hardware-adjusted figure the backend computed for this machine.
 * A large model is quick on a GPU and slow spilling to CPU; showing its ideal
 * rating to someone who cannot reach it would be a lie by omission, so when the
 * two differ the bar shows what they will actually get and says why.
 */
/* Push the Windows accent and light/dark theme onto the document.
 *
 * Shared by the settings and onboarding windows, and called both at load and
 * whenever the engine reports a change — the accent follows the wallpaper when
 * Windows is set to pick one automatically, so it moves without the user
 * touching a colour setting at all.
 */
function applySystemTheme(accent) {
  if (!accent) return;
  const r = document.documentElement;
  r.style.setProperty('--accent', accent.base);
  // The base accent can be dark enough to be unreadable on a dark surface, and
  // light enough to vanish on a light one, so each theme takes the variant
  // Windows itself uses for accent text there.
  r.style.setProperty('--accent-light',
    accent.theme === 'light' ? accent.dark1 : accent.light2);
  r.style.setProperty('--accent-dim',
    accent.theme === 'light' ? accent.light2 : accent.dark1);
  r.style.setProperty('--accent-rgb', accent.rgb);
  r.dataset.theme = accent.theme === 'light' ? 'light' : 'dark';
}

function ratingBars(m) {
  const here = m.speed_here ?? m.speed;
  // A couple of points of slack: rounding alone should not print a caveat.
  const throttled = here < m.speed - 2;
  const rows = [
    ['Accuracy', m.accuracy, 'acc', ''],
    ['Speed', here, 'spd',
      m.fits === false ? 'needs more memory than this machine has'
        : throttled ? `on this machine — ${m.speed} with GPU offload`
        : ''],
  ];

  const clamp = v => Math.max(0, Math.min(100, Number(v) || 0));
  return `<div class="rates">${rows.map(([label, v, cls, hint]) => `
    <div class="rate">
      <span class="rl">${label}</span>
      <div class="rt"><i class="${cls}" style="width:${clamp(v)}%"></i></div>
      <span class="rv">${clamp(v)}</span>
      ${hint ? `<span class="rh">${hint}</span>` : ''}
    </div>`).join('')}</div>`;
}
