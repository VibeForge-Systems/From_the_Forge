import { describe, expect, it } from 'vitest';
import { SHORTCUTS, formatShortcut, matchShortcut } from '@/services/shortcuts';

function key(init: KeyboardEventInit, target?: HTMLElement): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { bubbles: true, ...init });
  if (target) {
    document.body.appendChild(target);
    target.dispatchEvent(event);
    target.remove();
  }
  return event;
}

describe('matchShortcut', () => {
  it('matches Ctrl+B and Ctrl+Shift+B as different shortcuts', () => {
    expect(matchShortcut(key({ key: 'b', ctrlKey: true }), false)?.id).toBe('toggle-left-panel');
    expect(matchShortcut(key({ key: 'B', ctrlKey: true, shiftKey: true }), false)?.id).toBe(
      'toggle-right-panel',
    );
  });

  it('uses Command instead of Ctrl on macOS', () => {
    expect(matchShortcut(key({ key: 'b', metaKey: true }), true)?.id).toBe('toggle-left-panel');
    expect(matchShortcut(key({ key: 'b', ctrlKey: true }), true)).toBeUndefined();
  });

  it('ignores Alt combinations and auto-repeat', () => {
    expect(matchShortcut(key({ key: 'b', ctrlKey: true, altKey: true }), false)).toBeUndefined();
    expect(matchShortcut(key({ key: 'b', ctrlKey: true, repeat: true }), false)).toBeUndefined();
  });

  it('does not steal keys from text fields, except shortcuts marked for them', () => {
    const input = document.createElement('input');
    expect(matchShortcut(key({ key: 'b', ctrlKey: true }, input), false)).toBeUndefined();
    expect(matchShortcut(key({ key: 'F1' }, document.createElement('input')), false)?.id).toBe(
      'show-shortcuts',
    );
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    expect(matchShortcut(key({ key: 'b', ctrlKey: true }, checkbox), false)?.id).toBe(
      'toggle-left-panel',
    );
  });

  it('has no two shortcuts bound to the same keys', () => {
    const seen = new Set(SHORTCUTS.map((s) => `${s.mod}-${s.shift ?? false}-${s.key.toLowerCase()}`));
    expect(seen.size).toBe(SHORTCUTS.length);
  });
});

describe('formatShortcut', () => {
  it('names the platform modifier', () => {
    const s = SHORTCUTS.find((x) => x.id === 'toggle-right-panel')!;
    expect(formatShortcut(s, false)).toEqual(['Ctrl', 'Shift', 'B']);
    expect(formatShortcut(s, true)).toEqual(['⌘', 'Shift', 'B']);
  });
});
