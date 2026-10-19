import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter } from 'vue-router';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      chooseArchiveDirectory: vi.fn(),
      updateSettings: vi.fn(),
      listArchives: vi.fn(),
      getArchiveDetails: vi.fn(),
      verifyArchive: vi.fn(),
      cancelJob: vi.fn(),
      listBackups: vi.fn(),
    },
  };
});
import { ipc } from '@/services/ipc';
import ArchiveView from '@/views/ArchiveView.vue';
import { useArchives } from '@/stores/archives';
import { useSettings } from '@/stores/settings';
import type { ArchiveStatusDto } from '@/types';
import {
  ARCHIVE_ID,
  OTHER_ARCHIVE_ID,
  archiveDetails,
  archiveEntry,
  archiveListing,
  archiveSummary,
  backupList,
  lastImport,
  snapshot,
  status,
  stubMatchMedia,
  verifyCounts,
  verifySummary,
} from './helpers';

const api = vi.mocked(ipc);
let lastRouter: ReturnType<typeof createRouter> | null = null;

function mountView() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: ArchiveView },
      { path: '/settings', component: { template: '<div />' } },
      { path: '/backups', component: { template: '<div />' } },
      { path: '/archive/:archiveId/viewer', name: 'viewer', component: { template: '<div />' } },
    ],
  });
  lastRouter = router;
  return mount(ArchiveView, { global: { plugins: [router] } });
}

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
  stubMatchMedia(() => true); // wide window: both panels dock
  api.listArchives.mockResolvedValue(archiveListing([]));
  api.getArchiveDetails.mockResolvedValue(archiveDetails());
  api.listBackups.mockResolvedValue(backupList());
  api.verifyArchive.mockResolvedValue({ jobId: 'job-2' });
  api.cancelJob.mockResolvedValue(true);
});

afterEach(() => useArchives().stopPolling());

/** A ready library and the view mounted on it. */
async function mountReady() {
  const settings = useSettings();
  settings.loaded = true;
  settings.snapshot = snapshot({ archiveDirectory: '/data/archives' });
  settings.location = status('ready');
  const wrapper = mountView();
  await flushPromises();
  return wrapper;
}

function button(w: ReturnType<typeof mountView>, text: string) {
  return w.findAll('button').find((b) => b.text().includes(text));
}

describe('ArchiveView', () => {
  it('welcomes a new user with one working action and only exports marked planned', () => {
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot();
    const wrapper = mountView();

    expect(wrapper.text()).toContain('Welcome to OpenMessage Archive');
    const choose = wrapper.findAll('button').find((b) => b.text().includes('Choose archive folder'));
    expect(choose?.attributes('disabled')).toBeUndefined();
    expect(wrapper.findAll('.oma-planned').map((b) => b.text())).toEqual(['Planned · M6']);
    expect(api.listArchives).not.toHaveBeenCalled();
    const steps = wrapper.findAll('.oma-steps li').map((li) => li.text());
    expect(steps).toHaveLength(4);
    expect(steps[1]).toContain('Find your iPhone backup.');
    expect(steps[1]).not.toContain('Planned');
    expect(wrapper.find('.oma-steps a[href="/backups"]').exists()).toBe(true);
    expect(steps[2]).toContain('Import messages.');
    expect(steps[2]).not.toContain('Planned');
    expect(steps[3]).toContain('Read and search your conversations in the viewer.');
    expect(steps[3]).toContain('Planned · M6');
    expect(steps[3]).not.toContain('Planned · M5');
  });

  it('offers Create archive and Find backups in an empty library, and says archives are unencrypted', async () => {
    const wrapper = await mountReady();
    expect(api.listArchives).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain('Your archive library is ready');
    const find = wrapper.findAll('a').find((a) => a.text().includes('Find backups'));
    expect(find?.attributes('href')).toBe('/backups');
    const create = wrapper.findAll('button').filter((b) => b.text().includes('Create archive'));
    expect(create.length).toBe(2);
    for (const b of create) expect(b.attributes('disabled')).toBeUndefined();
    await create[0]?.trigger('click');
    expect(useArchives().createOpen).toBe(true);
    expect(wrapper.text()).toContain('No archives yet.');
    expect(wrapper.text()).toContain('The archive is stored unencrypted.');
    expect(wrapper.text()).toContain('Archives are stored unencrypted');
    expect(wrapper.text()).not.toContain('Planned · M4');
    expect(wrapper.text()).toContain('Backup passwords are used once and never saved.');
  });

  it('lists every archive status with its badge and selects the first archive', async () => {
    const statuses: ArchiveStatusDto[] = ['ready', 'importing', 'verifying', 'inUse', 'needsUpgrade', 'newerVersion', 'damaged', 'notAnArchive', 'refused'];
    api.listArchives.mockResolvedValue(
      archiveListing(statuses.map((status, i) => archiveEntry({ status, archiveId: `a${String(i).repeat(32)}` }))),
    );
    api.getArchiveDetails.mockResolvedValue(archiveDetails({ summary: archiveSummary({ archiveId: `a${'0'.repeat(32)}` }) }));
    const wrapper = await mountReady();
    const badges = wrapper.findAll('.oma-archive-list .badge').map((b) => b.text());
    expect(badges).toEqual(['Ready', 'Importing', 'Verifying', 'In use', 'Needs upgrade', 'Newer version', 'Damaged', 'Not an archive', 'Refused']);
    expect(useArchives().selectedId).toBe(`a${'0'.repeat(32)}`);
    expect(wrapper.find('.oma-archive-list').text()).toContain('Synthetic archive');
  });

  it('opens the viewer from Browse conversations, and only for an archive the viewer can read (5h3)', async () => {
    api.listArchives.mockResolvedValue(archiveListing([archiveEntry()]));
    api.getArchiveDetails.mockResolvedValue(archiveDetails());
    const wrapper = await mountReady();
    const browse = button(wrapper, 'Browse conversations');
    expect(browse?.attributes('disabled')).toBeUndefined();
    await browse?.trigger('click');
    await flushPromises();
    expect(lastRouter?.currentRoute.value.name).toBe('viewer');
    expect(lastRouter?.currentRoute.value.params.archiveId).toBe(ARCHIVE_ID);

    api.listArchives.mockResolvedValue(archiveListing([archiveEntry({ status: 'needsUpgrade' })]));
    const other = await mountReady();
    expect(button(other, 'Browse conversations')?.attributes('disabled')).toBeDefined();
  });

  it('opens the viewer with its search panel from Search messages (5i)', async () => {
    api.listArchives.mockResolvedValue(archiveListing([archiveEntry()]));
    api.getArchiveDetails.mockResolvedValue(archiveDetails());
    const wrapper = await mountReady();
    const search = button(wrapper, 'Search messages');
    expect(search?.attributes('disabled')).toBeUndefined();
    await search?.trigger('click');
    await flushPromises();
    expect(lastRouter?.currentRoute.value.name).toBe('viewer');
    expect(lastRouter?.currentRoute.value.params.archiveId).toBe(ARCHIVE_ID);
    // Nothing about the search enters the URL.
    expect(lastRouter?.currentRoute.value.fullPath).toBe(`/archive/${ARCHIVE_ID}/viewer`);
  });

  it('shows a damaged archive with its reason and no enabled action', async () => {
    api.listArchives.mockResolvedValue(archiveListing([archiveEntry({ status: 'damaged', damage: 'schemaMismatch' })]));
    api.getArchiveDetails.mockRejectedValue(new (await import('@/services/ipc')).IpcError('get_archive_details', 'archiveDamaged', 'Damaged.'));
    const wrapper = await mountReady();
    expect(wrapper.find('#archive-status-reason').text()).toContain('It was left as it is.');
    expect(button(wrapper, 'Import from a backup')?.attributes('disabled')).toBeDefined();
    expect(button(wrapper, 'Verify archive (quick)')?.attributes('disabled')).toBeDefined();
    expect(wrapper.text()).toContain('Damaged.');
  });

  it('offers import, continue and both verifies for a resumable archive, with the last import and verify', async () => {
    api.listArchives.mockResolvedValue(archiveListing([archiveEntry()]));
    api.getArchiveDetails.mockResolvedValue(
      archiveDetails({
        resumable: true,
        lastImport: lastImport({ state: 'stopped', stopReason: 'insufficientSpace', endedAtMs: null }),
        lastVerify: verifySummary({ outcome: 'problems', counts: verifyCounts({ blobMissing: 1, hasProblems: true }) }),
        sessions: [{ session: 1, state: 'stopped', startedAtMs: Date.UTC(2026, 9, 18), endedAtMs: null, schemaVersion: 1, resumes: 0, fullObservation: null }],
      }),
    );
    const wrapper = await mountReady();
    expect(button(wrapper, 'Import from a backup')?.attributes('disabled')).toBeUndefined();
    const cont = button(wrapper, 'Continue importing');
    expect(cont?.attributes('disabled')).toBeUndefined();
    await cont?.trigger('click');
    expect(useArchives().importDialog).toEqual({ archiveId: ARCHIVE_ID, backupId: null, continuing: true });
    expect(wrapper.text()).toContain('Stopped; can continue');
    expect(wrapper.text()).toContain('There was not enough free space.');
    expect(wrapper.text()).toContain('1 attachment file is missing.');
    expect(wrapper.text()).toContain('Counts: not recorded until the first import finishes.');
    // Nothing is planned here any more: the viewer and its search (slice 5i) open from here.
    expect(wrapper.findAll('.oma-planned')).toHaveLength(0);
    expect(button(wrapper, 'Browse conversations')?.attributes('disabled')).toBeUndefined();
    expect(button(wrapper, 'Search messages')?.attributes('disabled')).toBeUndefined();

    await button(wrapper, 'Verify archive (full)')?.trigger('click');
    await flushPromises();
    expect(api.verifyArchive).toHaveBeenCalledWith(ARCHIVE_ID, 'full', expect.any(Function));
    expect(wrapper.find('.oma-job').exists()).toBe(true);
    expect(wrapper.find('.progress').attributes('aria-label')).toBe('Verification progress');
  });

  it('shows a job it cannot hear from the details, with a working Cancel', async () => {
    api.listArchives.mockResolvedValue(archiveListing([archiveEntry({ status: 'importing' }), archiveEntry({ archiveId: OTHER_ARCHIVE_ID })]));
    api.getArchiveDetails.mockResolvedValue(
      archiveDetails({
        activeJob: { jobId: 'job-9', kind: 'import', phase: 'importingMessages', backupId: 'b1', waitingAtGate: false },
      }),
    );
    const wrapper = await mountReady();
    expect(wrapper.find('.oma-job-label').text()).toBe('Importing messages');
    expect(button(wrapper, 'Import from a backup')).toBeUndefined();
    await button(wrapper, 'Cancel')?.trigger('click');
    expect(api.cancelJob).toHaveBeenCalledWith('job-9');
    expect(wrapper.find('.oma-job-label').text()).toBe('Cancelling…');
  });

  it('shows why an unavailable location failed', () => {
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot({ archiveDirectory: '/mnt/usb/archives' });
    settings.location = status('error', '/mnt/usb/archives', [
      { id: 'exists', level: 'error', message: 'The folder could not be found.' },
    ]);
    const wrapper = mountView();
    expect(wrapper.text()).toContain('The archive location is unavailable');
    expect(wrapper.text()).toContain('The folder could not be found.');
  });

  it('docks both panels on a wide window and hides them per the saved layout', async () => {
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot();
    const wrapper = mountView();
    expect(wrapper.find('#oma-left-panel').exists()).toBe(true);
    expect(wrapper.find('#oma-right-panel').exists()).toBe(true);

    settings.snapshot = snapshot({ layout: { leftPanelOpen: false, rightPanelOpen: true } });
    await wrapper.vm.$nextTick();
    expect(wrapper.find('#oma-left-panel').exists()).toBe(false);
  });

  it('starts with overlays closed on a narrow window', () => {
    stubMatchMedia(() => false);
    const settings = useSettings();
    settings.loaded = true;
    settings.snapshot = snapshot();
    const wrapper = mountView();
    expect(wrapper.find('#oma-left-panel').exists()).toBe(false);
    expect(wrapper.find('#oma-right-panel').exists()).toBe(false);
  });
});
