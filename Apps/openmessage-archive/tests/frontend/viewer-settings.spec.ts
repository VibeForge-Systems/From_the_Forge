import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';

const fake = vi.hoisted(() => ({
  updates: [] as unknown[],
  os: 'linux',
}));
vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      updateSettings: (update: unknown) => {
        fake.updates.push(update);
        return Promise.resolve(null);
      },
      getSettings: () => Promise.resolve(null),
      getAppInfo: () => Promise.resolve({ os: fake.os }),
    },
  };
});
import ViewerSettings from '@/components/ViewerSettings.vue';
import { VIEWER_DEFAULTS, ZOOM_STEPS, useSettings } from '@/stores/settings';
import { snapshot } from './helpers';

beforeEach(() => {
  setActivePinia(createPinia());
  fake.updates.length = 0;
  fake.os = 'linux';
});

function mountWith(viewer = {}) {
  const settings = useSettings();
  const snap = snapshot();
  snap.settings.viewer = { ...snap.settings.viewer, ...viewer };
  settings.snapshot = snap;
  return mount(ViewerSettings);
}

describe('viewer settings', () => {
  it('match the backend defaults', () => {
    expect(VIEWER_DEFAULTS).toEqual(snapshot().settings.viewer);
    expect(ZOOM_STEPS[0]).toBe(100);
    expect(ZOOM_STEPS.at(-1)).toBe(400);
    expect(ZOOM_STEPS).toHaveLength(31);
  });

  it('send the whole viewer object with one field changed', async () => {
    const w = mountWith({ showSeconds: true });
    await w.find('#viewer-zoom').setValue('250');
    expect(fake.updates).toEqual([{ viewer: { ...snapshot().settings.viewer, showSeconds: true, uiZoom: 250 } }]);
  });

  it('keep the viewer-only preferences disabled with the Planned badge until the viewer ships', async () => {
    const w = mountWith();
    await flushPromises();
    const fieldsets = w.findAll('fieldset[disabled]');
    expect(fieldsets.length).toBeGreaterThanOrEqual(2);
    for (const id of ['viewer-time-format', 'viewer-seconds', 'viewer-reveal-earlier', 'viewer-playback-confirm', 'viewer-blur', 'viewer-idle']) {
      const el = w.find(`#${id}`).element as HTMLInputElement;
      expect(el.matches(':disabled'), id).toBe(true);
    }
    expect(w.text()).toContain('Planned · M5');
    // Zoom applies now, to the whole window.
    expect(w.find('#viewer-zoom').element.matches(':disabled')).toBe(false);
  });

  it('offer capture protection only on Windows', async () => {
    const linux = mountWith();
    await flushPromises();
    expect(linux.find('#viewer-capture').attributes('disabled')).toBeDefined();
    fake.os = 'windows';
    const windows = mountWith();
    await flushPromises();
    expect(windows.find('#viewer-capture').attributes('disabled')).toBeUndefined();
    await windows.find('#viewer-capture').setValue(true);
    expect(fake.updates.at(-1)).toMatchObject({ viewer: { captureProtection: true } });
  });

  it('name the media risk and the three playback choices', () => {
    const w = mountWith();
    expect(w.findAll('input[name="viewer-playback"]')).toHaveLength(3);
    expect((w.find('#viewer-playback-confirm').element as HTMLInputElement).checked).toBe(true);
    expect(w.text()).toContain('could exploit a flaw');
    expect(w.text()).toContain('It is not a lock');
  });
});
