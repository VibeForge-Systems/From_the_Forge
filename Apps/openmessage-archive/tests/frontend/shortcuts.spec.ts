import { describe, expect, it } from 'vitest';
import { SHORTCUTS, formatShortcut, isMessageShortcut, matchShortcut } from '@/services/shortcuts';
import { ZOOM_LADDER, nextZoom } from '@/services/zoom';
import { ZOOM_STEPS } from '@/stores/settings';

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

  it('declares the viewer shortcuts of M5 §8.6 (slice 5i)', () => {
    const k = (init: KeyboardEventInit) => matchShortcut(key(init), false)?.id;
    expect(k({ key: 'f', ctrlKey: true })).toBe('viewer-search-conversation');
    expect(k({ key: 'F', ctrlKey: true, shiftKey: true })).toBe('viewer-search-all');
    expect(k({ key: 'g', ctrlKey: true })).toBe('viewer-jump-date');
    expect(k({ key: 'Home', ctrlKey: true })).toBe('viewer-first');
    expect(k({ key: 'End', ctrlKey: true })).toBe('viewer-last');
    expect(k({ key: '=', ctrlKey: true })).toBe('zoom-in');
    expect(k({ key: '+', ctrlKey: true, shiftKey: true })).toBe('zoom-in');
    expect(k({ key: '-', ctrlKey: true })).toBe('zoom-out');
    expect(k({ key: '0', ctrlKey: true })).toBe('zoom-reset');
    // Home and End without Ctrl stay the page's own.
    expect(k({ key: 'Home' })).toBeUndefined();
  });

  it('matches Enter and R only in the message scope, never globally (WCAG 2.1.4)', () => {
    expect(matchShortcut(key({ key: 'Enter' }), false)).toBeUndefined();
    expect(matchShortcut(key({ key: 'r' }), false)).toBeUndefined();
    expect(matchShortcut(key({ key: 'Enter' }), false, 'message')?.id).toBe('viewer-details');
    expect(matchShortcut(key({ key: 'R' }), false, 'message')?.id).toBe('viewer-reply-thread');
    // Ctrl+F is not a message shortcut.
    expect(matchShortcut(key({ key: 'f', ctrlKey: true }), false, 'message')).toBeUndefined();
    expect(SHORTCUTS.filter((s) => s.scope === 'message').every((s) => isMessageShortcut(s.id))).toBe(true);
    expect(SHORTCUTS.filter((s) => s.scope !== 'message').some((s) => isMessageShortcut(s.id))).toBe(false);
  });

  it('leaves the viewer shortcuts alone in text fields, so Ctrl+Home still moves the caret', () => {
    expect(matchShortcut(key({ key: 'Home', ctrlKey: true }, document.createElement('input')), false)).toBeUndefined();
    expect(matchShortcut(key({ key: 'f', ctrlKey: true }, document.createElement('textarea')), false)).toBeUndefined();
  });
});

describe('formatShortcut', () => {
  it('names the platform modifier', () => {
    const s = SHORTCUTS.find((x) => x.id === 'toggle-right-panel')!;
    expect(formatShortcut(s, false)).toEqual(['Ctrl', 'Shift', 'B']);
    expect(formatShortcut(s, true)).toEqual(['⌘', 'Shift', 'B']);
  });
});

describe('zoom steps', () => {
  it('steps through values Rust accepts, clamped at both ends', () => {
    for (const z of ZOOM_LADDER) expect(ZOOM_STEPS).toContain(z);
    expect(nextZoom(100, 1)).toBe(110);
    expect(nextZoom(140, 1)).toBe(150);
    expect(nextZoom(140, -1)).toBe(130);
    expect(nextZoom(400, 1)).toBe(400);
    expect(nextZoom(100, -1)).toBe(100);
  });
});
