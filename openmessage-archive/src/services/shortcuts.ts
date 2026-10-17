// Keyboard shortcuts are declared once here. The global handler matches
// against this table and the help dialog renders it, so the documentation
// cannot drift from the behavior.

export type ShortcutId =
  | 'toggle-left-panel'
  | 'toggle-right-panel'
  | 'go-archive'
  | 'go-settings'
  | 'go-about'
  | 'cycle-theme'
  | 'show-shortcuts';

export interface Shortcut {
  id: ShortcutId;
  description: string;
  /** Primary modifier: Ctrl on Windows and Linux, Command on macOS. */
  mod: boolean;
  shift?: boolean;
  /** Compared against KeyboardEvent.key, case-insensitively. */
  key: string;
  /** Shown in the help dialog when the key name alone is unclear. */
  label?: string;
  /** Also fire while focus is in a text field. */
  inInputs?: boolean;
}

export const SHORTCUTS: readonly Shortcut[] = [
  { id: 'toggle-left-panel', description: 'Show or hide the conversation list', mod: true, key: 'b' },
  {
    id: 'toggle-right-panel',
    description: 'Show or hide the details panel',
    mod: true,
    shift: true,
    key: 'b',
  },
  { id: 'go-archive', description: 'Go to the archive', mod: true, key: '1' },
  { id: 'go-settings', description: 'Open settings', mod: true, key: ',' },
  { id: 'go-about', description: 'Open About', mod: true, key: '2' },
  { id: 'cycle-theme', description: 'Switch theme (system → light → dark)', mod: true, shift: true, key: 'l' },
  { id: 'show-shortcuts', description: 'Show keyboard shortcuts', mod: false, key: 'F1', inInputs: true },
];

export const isMac = (): boolean =>
  typeof navigator !== 'undefined' && /mac/i.test(navigator.userAgent);

function isTextInput(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (tag !== 'INPUT') return false;
  const type = (target as HTMLInputElement).type;
  return !['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color', 'file'].includes(type);
}

export function matchShortcut(event: KeyboardEvent, mac = isMac()): Shortcut | undefined {
  if (event.isComposing || event.repeat) return undefined;
  const mod = mac ? event.metaKey : event.ctrlKey;
  const otherMod = mac ? event.ctrlKey : event.metaKey;
  if (otherMod || event.altKey) return undefined;
  const inInput = isTextInput(event.target);
  return SHORTCUTS.find(
    (s) =>
      s.mod === mod &&
      Boolean(s.shift) === event.shiftKey &&
      s.key.toLowerCase() === event.key.toLowerCase() &&
      (!inInput || s.inInputs === true),
  );
}

export function formatShortcut(s: Shortcut, mac = isMac()): string[] {
  const keys: string[] = [];
  if (s.mod) keys.push(mac ? '⌘' : 'Ctrl');
  if (s.shift) keys.push('Shift');
  keys.push(s.label ?? (s.key.length === 1 ? s.key.toUpperCase() : s.key));
  return keys;
}
