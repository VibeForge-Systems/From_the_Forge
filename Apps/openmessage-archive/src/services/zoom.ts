// The zoom shortcuts (M5 §8.6, D116): Ctrl+= and Ctrl+- step through a
// ladder of `uiZoom` values, Ctrl+0 resets. Rust validates the value (100
// to 400 in steps of 10) and applies it with `set_zoom`; the webview's own
// zoom hotkeys stay off.

/** The shortcut steps, in percent: a subset of `ZOOM_STEPS` in `stores/settings.ts`, which Rust accepts. */
export const ZOOM_LADDER: readonly number[] = [100, 110, 120, 130, 150, 170, 200, 250, 300, 400];

export const ZOOM_DEFAULT = 100;

/** The next step in `direction` from `current` (which need not be on the ladder). */
export function nextZoom(current: number, direction: 1 | -1): number {
  if (direction > 0) return ZOOM_LADDER.find((z) => z > current) ?? ZOOM_LADDER[ZOOM_LADDER.length - 1]!;
  return [...ZOOM_LADDER].reverse().find((z) => z < current) ?? ZOOM_LADDER[0]!;
}
