// Keyboard shortcuts are declared once here. The global handler matches
// against this table and the help dialog renders it, so the documentation
// cannot drift from the behavior.
//
// Most shortcuts are global (`App.vue` dispatches them). The viewer's
// message shortcuts (M5 §8.6, slice 5i) are scoped to a focused message:
// `ThreadView` matches them on its articles only, never the global
// handler, so a single key such as R or Enter is active only while a
// message has focus (WCAG 2.1.4) and never steals Enter from a button.

export type MessageShortcutId = 'viewer-details' | 'viewer-reply-thread';

export type ShortcutId =
  | 'toggle-left-panel'
  | 'toggle-right-panel'
  | 'go-archive'
  | 'go-backups'
  | 'go-settings'
  | 'go-about'
  | 'cycle-theme'
  | 'show-shortcuts'
  | 'viewer-search-conversation'
  | 'viewer-search-all'
  | 'viewer-jump-date'
  | 'viewer-first'
  | 'viewer-last'
  | 'zoom-in'
  | 'zoom-out'
  | 'zoom-reset'
  | MessageShortcutId;

export type GlobalShortcutId = Exclude<ShortcutId, MessageShortcutId>;

/** Where a shortcut is matched: everywhere, or on a focused message. */
export type ShortcutScope = 'global' | 'message';

export interface Shortcut {
  id: ShortcutId;
  description: string;
  /** Primary modifier: Ctrl on Windows and Linux, Command on macOS. */
  mod: boolean;
  shift?: boolean;
  /** Match whether or not Shift is held (keys that need Shift on some layouts). */
  anyShift?: boolean;
  /** Compared against KeyboardEvent.key, case-insensitively. */
  key: string;
  /** Other `KeyboardEvent.key` values for the same shortcut. */
  aliases?: readonly string[];
  /** Shown in the help dialog when the key name alone is unclear. */
  label?: string;
  /** Also fire while focus is in a text field. */
  inInputs?: boolean;
  /** `message`: only on a focused message, matched by the thread. */
  scope?: ShortcutScope;
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
  { id: 'go-backups', description: 'Go to backups', mod: true, key: '3' },
  { id: 'go-settings', description: 'Open settings', mod: true, key: ',' },
  { id: 'go-about', description: 'Open About', mod: true, key: '2' },
  { id: 'cycle-theme', description: 'Switch theme (system → light → dark)', mod: true, shift: true, key: 'l' },
  { id: 'show-shortcuts', description: 'Show keyboard shortcuts', mod: false, key: 'F1', inInputs: true },
  {
    id: 'viewer-search-conversation',
    description: 'Viewer: search this conversation',
    mod: true,
    key: 'f',
  },
  { id: 'viewer-search-all', description: 'Viewer: search all conversations', mod: true, shift: true, key: 'f' },
  { id: 'viewer-jump-date', description: 'Viewer: jump to a date', mod: true, key: 'g' },
  { id: 'viewer-first', description: 'Viewer: jump to the first item', mod: true, key: 'Home' },
  { id: 'viewer-last', description: 'Viewer: jump to the newest item', mod: true, key: 'End' },
  {
    id: 'viewer-details',
    description: 'Viewer: show the focused message’s details',
    mod: false,
    key: 'Enter',
    scope: 'message',
  },
  {
    id: 'viewer-reply-thread',
    description: 'Viewer: show the replies to the focused message',
    mod: false,
    key: 'r',
    scope: 'message',
  },
  { id: 'zoom-in', description: 'Zoom in', mod: true, key: '=', aliases: ['+'], anyShift: true, label: '+' },
  { id: 'zoom-out', description: 'Zoom out', mod: true, key: '-', label: '−' },
  { id: 'zoom-reset', description: 'Reset zoom', mod: true, key: '0' },
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

function keyMatches(s: Shortcut, key: string): boolean {
  const k = key.toLowerCase();
  return s.key.toLowerCase() === k || (s.aliases ?? []).some((a) => a.toLowerCase() === k);
}

export function matchShortcut(
  event: KeyboardEvent,
  mac = isMac(),
  scope: ShortcutScope = 'global',
): Shortcut | undefined {
  if (event.isComposing || event.repeat) return undefined;
  const mod = mac ? event.metaKey : event.ctrlKey;
  const otherMod = mac ? event.ctrlKey : event.metaKey;
  if (otherMod || event.altKey) return undefined;
  const inInput = isTextInput(event.target);
  return SHORTCUTS.find(
    (s) =>
      (s.scope ?? 'global') === scope &&
      s.mod === mod &&
      (s.anyShift === true || Boolean(s.shift) === event.shiftKey) &&
      keyMatches(s, event.key) &&
      (!inInput || s.inInputs === true),
  );
}

export function isMessageShortcut(id: ShortcutId): id is MessageShortcutId {
  return id === 'viewer-details' || id === 'viewer-reply-thread';
}

export function formatShortcut(s: Shortcut, mac = isMac()): string[] {
  const keys: string[] = [];
  if (s.mod) keys.push(mac ? '⌘' : 'Ctrl');
  if (s.shift) keys.push('Shift');
  keys.push(s.label ?? (s.key.length === 1 ? s.key.toUpperCase() : s.key));
  return keys;
}
