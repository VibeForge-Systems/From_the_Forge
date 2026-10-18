import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter } from 'vue-router';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      listBackups: vi.fn(),
      addBackupFolder: vi.fn(),
      forgetBackupFolder: vi.fn(),
      getBackupDetails: vi.fn(),
      openBackup: vi.fn(),
      cancelJob: vi.fn(),
      updateSettings: vi.fn(),
    },
  };
});
import { ipc } from '@/services/ipc';
import BackupsView from '@/views/BackupsView.vue';
import { useBackups } from '@/stores/backups';
import { useSettings } from '@/stores/settings';
import type { BackupDetails, BackupList, BackupState } from '@/types';
import { backupList, details, location, report, snapshot, stubMatchMedia, summary } from './helpers';

const api = vi.mocked(ipc);

async function mountWith(list: BackupList, d?: BackupDetails) {
  api.listBackups.mockResolvedValue(list);
  if (d) api.getBackupDetails.mockResolvedValue(d);
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: { template: '<div />' } },
      { path: '/backups', component: BackupsView },
      { path: '/settings', component: { template: '<div />' } },
    ],
  });
  const wrapper = mount(BackupsView, { global: { plugins: [router] } });
  await flushPromises();
  if (d) {
    await useBackups().select(d.summary.id);
    await flushPromises();
  }
  return wrapper;
}

function button(w: Awaited<ReturnType<typeof mountWith>>, text: string) {
  return w.findAll('button').find((b) => b.text().includes(text));
}

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
  stubMatchMedia(() => true);
  const settings = useSettings();
  settings.loaded = true;
  settings.snapshot = snapshot({ archiveDirectory: '/data/archives' });
});

describe('BackupsView', () => {
  it('shows the Linux empty state when there are no default locations', async () => {
    const w = await mountWith(backupList([], []));
    expect(w.text()).toContain('Linux has no standard backup location');
    expect(button(w, 'Add backup folder')?.attributes('disabled')).toBeUndefined();
  });

  it('lists the scanned standard locations when they hold no backups', async () => {
    const w = await mountWith(
      backupList([], [location({ kind: 'appleDevices', path: 'C:\\Users\\a\\Apple\\MobileSync\\Backup', backupCount: 0 })]),
    );
    expect(w.text()).toContain('No iPhone backup was found in the standard locations');
    expect(w.text()).toContain('MobileSync');
  });

  it('lists iMazing\'s default location among the scanned standard locations', async () => {
    const w = await mountWith(
      // Only the iMazing location: it alone counts as a standard location.
      backupList([], [
        location({ kind: 'iMazing', path: 'C:\\Users\\a\\AppData\\Roaming\\iMazing\\Backups', backupCount: 0 }),
      ]),
    );
    expect(w.text()).toContain('No iPhone backup was found in the standard locations');
    expect(w.text()).toContain('iMazing\\Backups');
    expect(w.text()).not.toContain('Linux has no standard backup location');
  });

  it('asks the user to select a backup when none is selected', async () => {
    const w = await mountWith(backupList());
    expect(w.text()).toContain('Select a backup to see its details.');
  });

  it('offers Validate for a ready backup, with Import messages disabled and planned', async () => {
    const w = await mountWith(backupList(), details());
    const validate = button(w, 'Validate');
    expect(validate?.attributes('disabled')).toBeUndefined();
    const imp = button(w, 'Import messages');
    expect(imp?.attributes('disabled')).toBeDefined();
    expect(w.find('#import-planned').text()).toContain('Planned · M3');
    expect(w.text()).toContain('The backup itself is never changed.');
    expect(w.text()).toContain('from Info.plist');
  });

  it('offers Unlock and validate for an encrypted backup', async () => {
    const s = summary({ state: 'needsPassword', encrypted: true });
    const w = await mountWith(backupList([s]), details(s));
    expect(button(w, 'Unlock and validate…')?.attributes('disabled')).toBeUndefined();
    expect(w.text()).toContain('Encrypted');
    expect(button(w, 'Import messages')?.attributes('disabled')).toBeDefined();
  });

  it.each(['incomplete', 'unsupported', 'unreadable'] as BackupState[])(
    'disables Validate for a %s backup and points at the reason',
    async (state) => {
      const s = summary({ state, stateReason: `Reason for ${state}.` });
      const w = await mountWith(backupList([s]), details(s));
      const validate = button(w, 'Validate');
      expect(validate?.attributes('disabled')).toBeDefined();
      expect(validate?.attributes('aria-describedby')).toBe('backup-state-reason');
      expect(w.find('#backup-state-reason').text()).toBe(`Reason for ${state}.`);
      expect(button(w, 'Refresh')?.attributes('disabled')).toBeUndefined();
      expect(button(w, 'Import messages')?.attributes('disabled')).toBeDefined();
    },
  );

  it.each([false, true])('shows the sync warning before validation (encrypted: %s)', async (encrypted) => {
    const s = summary({ state: encrypted ? 'needsPassword' : 'ready', encrypted });
    const w = await mountWith(backupList([s]), details(s, { archiveSyncProvider: 'OneDrive' }));
    expect(w.text()).toContain('Your archive folder appears to be synchronized by OneDrive');
    expect(button(w, encrypted ? 'Unlock and validate' : 'Validate')?.attributes('disabled')).toBeUndefined();
  });

  it('requires an archive folder before validating', async () => {
    const w = await mountWith(backupList(), details(summary(), { archiveConfigured: false }));
    expect(w.text()).toContain('Choose an archive folder first.');
    expect(w.find('a[href="/settings"]').exists()).toBe(true);
    const validate = button(w, 'Validate');
    expect(validate?.attributes('disabled')).toBeDefined();
    expect(validate?.attributes('aria-describedby')).toBe('backup-no-archive');
  });

  it('renders "Not recorded" for missing values and the folder path', async () => {
    const s = summary({ iosVersion: null, lastBackupDate: null });
    const w = await mountWith(backupList([s]), details(s));
    expect(w.text()).toContain('Not recorded');
    expect(w.find('code').text()).toContain('SYNTHETIC0-0000000000000000');
    expect(w.text()).toContain("The folder name is usually your device's identifier.");
  });

  it('shows the leftover-files warning when the last cleanup was incomplete', async () => {
    const w = await mountWith(backupList(), details(summary(), { lastCleanup: { stagingRemoved: false } }));
    expect(w.text()).toContain('Some temporary files could not be removed yet.');
  });

  it('shows the report with Validate again', async () => {
    const w = await mountWith(backupList(), details(summary(), { lastReport: report(false) }));
    expect(w.text()).toContain('This backup cannot be imported. See the failed checks below.');
    expect(w.text()).toContain('missing from the backup');
    expect(button(w, 'Validate again')?.attributes('disabled')).toBeUndefined();
  });

  it('shows progress and Cancel while this backup is checked', async () => {
    const w = await mountWith(backupList(), details());
    const store = useBackups();
    store.job = {
      jobId: 'job-1',
      backupId: 'b1',
      phase: 'checkingFiles',
      done: 3,
      total: 8,
      cancelling: false,
      ended: false,
    };
    await flushPromises();
    expect(w.text()).toContain('Checking file records (3 of 8)');
    // The live region announces the phase only, never the running count.
    const live = w.find('[aria-live="polite"]');
    expect(live.text()).toBe('Checking file records');
    const bar = w.find('[role="progressbar"]');
    expect(bar.attributes('aria-label')).toBe('Validation progress');
    expect(bar.attributes('aria-valuetext')).toBe('Checking file records (3 of 8)');
    expect(button(w, 'Cancel')?.attributes('disabled')).toBeUndefined();
  });

  it('names the device of the backup the password prompt is for, not the selected one', async () => {
    const a = summary({ id: 'a', state: 'needsPassword', encrypted: true, deviceName: { value: 'Synthetic Phone A', source: 'infoPlist' } });
    const b = summary({ id: 'b', deviceName: { value: 'Synthetic Phone B', source: 'infoPlist' } });
    api.getBackupDetails.mockImplementation(async (id) => details(id === 'a' ? a : b));
    const w = await mountWith(backupList([a, b]));
    const store = useBackups();
    await store.select('a');
    await store.select('b');
    store.passwordPrompt = { backupId: 'a', error: 'That password did not unlock the backup.' };
    await flushPromises();
    const help = w.find('#backup-password-help').text();
    expect(help).toContain('Synthetic Phone A');
    expect(help).not.toContain('Synthetic Phone B');
  });

  it('shows the current blocked state rather than an earlier report verdict', async () => {
    const s = summary({ state: 'incomplete', stateReason: 'Reason.', lastReportImportable: true });
    const w = await mountWith(backupList([s]), details(s));
    const badges = w.findAll('.badge').map((b) => b.text());
    expect(badges).toContain('Incomplete');
    expect(badges).not.toContain('Checked');
  });

  it('disables Validate while another backup is checked', async () => {
    const w = await mountWith(backupList(), details());
    useBackups().job = {
      jobId: 'job-1',
      backupId: 'other',
      phase: null,
      done: 0,
      total: null,
      cancelling: false,
      ended: false,
    };
    await flushPromises();
    expect(w.text()).toContain('Another backup is being checked.');
    expect(button(w, 'Validate')?.attributes('disabled')).toBeDefined();
  });

  it('shows a failure with its message', async () => {
    const w = await mountWith(backupList(), details());
    useBackups().lastError = { b1: { code: 'io', message: 'The backup could not be read (permission denied).' } };
    await flushPromises();
    expect(w.find('.alert-danger').text()).toContain('permission denied');
  });

  it('never renders an enabled Import messages button in any state', async () => {
    for (const state of ['ready', 'needsPassword', 'incomplete', 'unsupported', 'unreadable', 'notInspected'] as BackupState[]) {
      setActivePinia(createPinia());
      const settings = useSettings();
      settings.loaded = true;
      settings.snapshot = snapshot();
      const s = summary({ state });
      const w = await mountWith(backupList([s]), details(s));
      expect(button(w, 'Import messages')?.attributes('disabled')).toBeDefined();
      expect(w.find('#import-planned .oma-planned').exists()).toBe(true);
      w.unmount();
    }
  });
});
