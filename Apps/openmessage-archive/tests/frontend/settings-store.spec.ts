import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      getSettings: vi.fn(),
      updateSettings: vi.fn(),
      chooseArchiveDirectory: vi.fn(),
      getArchiveLocationStatus: vi.fn(),
      clearArchiveDirectory: vi.fn(),
      openArchiveDirectory: vi.fn(),
      getAppInfo: vi.fn(),
    },
  };
});
import { IpcError, ipc } from '@/services/ipc';
import { useNotifications } from '@/stores/notifications';
import { useSettings } from '@/stores/settings';
import { snapshot, status } from './helpers';

const api = vi.mocked(ipc);

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
});

describe('settings store', () => {
  it('loads settings and the location status', async () => {
    api.getSettings.mockResolvedValue(snapshot({ theme: 'dark' }));
    api.getArchiveLocationStatus.mockResolvedValue(status('ready'));
    const store = useSettings();
    await store.load();
    expect(store.loaded).toBe(true);
    expect(store.theme).toBe('dark');
    expect(store.locationUsable).toBe(true);
  });

  it('records a load failure instead of throwing', async () => {
    api.getSettings.mockRejectedValue(new IpcError('get_settings', 'internal', 'boom'));
    const store = useSettings();
    await store.load();
    expect(store.loaded).toBe(true);
    expect(store.loadError).toBe('boom');
  });

  it('re-syncs and warns once when settings are read-only', async () => {
    const readOnly = new IpcError('update_settings', 'settingsReadOnly', 'Not saved.');
    api.updateSettings.mockRejectedValue(readOnly);
    api.getSettings.mockResolvedValue(snapshot({ theme: 'light' }, true));
    const store = useSettings();
    await store.setTheme('light');
    await store.setTheme('light');
    expect(store.theme).toBe('light');
    expect(api.getSettings).toHaveBeenCalledTimes(2);
    expect(useNotifications().toasts.filter((t) => t.level === 'warning')).toHaveLength(1);
  });

  it('does nothing when the folder picker is cancelled', async () => {
    api.chooseArchiveDirectory.mockResolvedValue(null);
    const store = useSettings();
    await store.chooseLocation();
    expect(store.location.state).toBe('unset');
    expect(useNotifications().toasts).toHaveLength(0);
    expect(store.busy).toBe(false);
  });

  it('keeps the previous location and explains why when a choice is rejected', async () => {
    api.chooseArchiveDirectory.mockResolvedValue({
      saved: false,
      settings: snapshot(),
      status: status('error', '/backup/inner', [
        { id: 'outside-backup', level: 'error', message: 'Inside an iPhone backup.' },
      ]),
    });
    const store = useSettings();
    await store.chooseLocation();
    expect(store.location.state).toBe('unset');
    const [toast] = useNotifications().toasts;
    expect(toast?.level).toBe('danger');
    expect(toast?.message).toContain('Inside an iPhone backup.');
  });

  it('adopts a saved location', async () => {
    api.chooseArchiveDirectory.mockResolvedValue({
      saved: true,
      settings: snapshot({ archiveDirectory: '/data/archives' }),
      status: status('ready'),
    });
    const store = useSettings();
    await store.chooseLocation();
    expect(store.location.state).toBe('ready');
    expect(store.settings?.archiveDirectory).toBe('/data/archives');
  });

  it('says that forgetting a location deletes nothing', async () => {
    api.clearArchiveDirectory.mockResolvedValue(snapshot());
    const store = useSettings();
    await store.clearLocation();
    expect(useNotifications().toasts[0]?.message).toMatch(/No files were deleted/);
  });
});
