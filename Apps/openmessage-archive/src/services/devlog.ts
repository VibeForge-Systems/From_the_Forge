// The app's only console output (M5 §8.1 rule 6). Each note is a closed
// code with a fixed sentence, plus at most one number (a duration from a
// `performance.now()` mark, or a count), so nothing the archive holds can
// ever reach the console: no message text, address, name, id or query.
// Notes are written in development builds only. The source rule
// (`tests/frontend/source-rules.spec.ts`) fails on `console.` anywhere
// else under `src/`.

/** Every note the frontend may write. */
export type DevNoteKind =
  | 'jobIdMismatch'
  | 'viewerPageMs'
  | 'viewerWindowEvicted'
  | 'featureDegraded';

const NOTES: Record<DevNoteKind, string> = {
  jobIdMismatch: 'job id mismatch between response and events; ignored',
  viewerPageMs: 'viewer page loaded (ms)',
  viewerWindowEvicted: 'viewer pages evicted (count)',
  featureDegraded: 'engine features missing, degraded paths in use (count)',
};

/** The sentence a note writes; exported for tests. */
export function devNoteText(kind: DevNoteKind): string {
  return NOTES[kind];
}

/**
 * Write a development note. `value` must be a number the caller computed
 * (milliseconds or a count), never derived from content; a non-finite value
 * is dropped.
 */
export function devNote(kind: DevNoteKind, value?: number): void {
  if (!import.meta.env.DEV) return;
  const text = `[oma] ${NOTES[kind]}`;
  if (typeof value === 'number' && Number.isFinite(value)) console.debug(text, value);
  else console.debug(text);
}
