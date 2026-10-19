// The viewer's routes (M5 §8.3): message ids never enter the URL, the
// title never names a chat (D112), and leaving the viewer closes its
// session (D114). Chat ids from the URL are parsed with Rust's SafeId rule.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import type { RouteLocationNormalized } from 'vue-router';

const fake = vi.hoisted(() => ({
  calls: [] as [string, unknown][],
  handlers: {} as Record<string, (arg: unknown) => Promise<unknown>>,
}));
vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  const call =
    (name: string) =>
    (...args: unknown[]) => {
      const arg = args.length === 1 ? args[0] : args;
      fake.calls.push([name, arg]);
      const handler = fake.handlers[name];
      return handler ? handler(arg) : Promise.resolve(null);
    };
  return { ...actual, ipc: new Proxy({}, { get: (_t, name: string) => call(name) }) };
});
import { closeViewerOnLeave, isViewerRoute, routes } from '@/router';
import { parseSafeId } from '@/services/ids';
import { useViewer } from '@/stores/viewer';
import { ARCHIVE_ID, viewerSession } from './helpers';

const at = (name: string) => ({ name }) as unknown as RouteLocationNormalized;

beforeEach(() => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  fake.handlers = { openViewer: () => Promise.resolve(viewerSession({ viewer: 9 })) };
});

describe('viewer routes', () => {
  it('declare the archive and chat only, with a fixed title', () => {
    const viewer = routes.filter((r) => typeof r.name === 'string' && isViewerRoute({ name: r.name }));
    expect(viewer.map((r) => r.path)).toEqual([
      '/archive/:archiveId/viewer',
      '/archive/:archiveId/viewer/c/:chatId',
    ]);
    for (const r of viewer) {
      expect(r.meta?.title).toBe('Viewer');
      expect(r.path).not.toMatch(/message/i);
    }
  });

  it('close the session when leaving the viewer, not when moving inside it', async () => {
    const store = useViewer();
    await store.open(ARCHIVE_ID);
    closeViewerOnLeave(at('viewer-chat'), at('viewer'));
    expect(fake.calls.some(([n]) => n === 'closeViewer')).toBe(false);
    closeViewerOnLeave(at('archive'), at('viewer-chat'));
    await Promise.resolve();
    expect(fake.calls.at(-1)).toEqual(['closeViewer', 9]);
    expect(store.session).toBeNull();
  });

  it('parse chat ids from the URL with the SafeId rule', () => {
    expect(parseSafeId('42')).toBe(42);
    expect(parseSafeId('9007199254740991')).toBe(9007199254740991);
    for (const bad of ['0', '-1', '01', '1.5', '1e3', ' 7', '9007199254740992', '', 'abc', undefined]) {
      expect(parseSafeId(bad), String(bad)).toBeNull();
    }
  });
});
