import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      listBackups: vi.fn(),
      getBackupDetails: vi.fn(),
      cancelJob: vi.fn(),
      listArchives: vi.fn(),
      createArchive: vi.fn(),
      getArchiveDetails: vi.fn(),
      startImport: vi.fn(),
      confirmImport: vi.fn(),
      verifyArchive: vi.fn(),
    },
  };
});
import { ipc } from '@/services/ipc';
import ArchiveDialogs from '@/components/ArchiveDialogs.vue';
import ImportSummary from '@/components/ImportSummary.vue';
import VerifySummary from '@/components/VerifySummary.vue';
import { useArchives } from '@/stores/archives';
import { useBackups } from '@/stores/backups';
import { PROVISIONAL, RD_WARNING, STAGING_NOTE, UNENCRYPTED_NOTE } from '@/services/importText';
import type { ImportPreflightDto } from '@/types';
import {
  ARCHIVE_ID,
  archiveDetails,
  archiveListing,
  backupList,
  lastImport,
  preflight,
  summary,
  verifyCounts,
  verifySummary,
} from './helpers';

const api = vi.mocked(ipc);
let wrapper: ReturnType<typeof mount> | null = null;

async function mountDialogs() {
  wrapper = mount(ArchiveDialogs, { attachTo: document.body });
  await flushPromises();
  return wrapper;
}

function button(text: string) {
  return wrapper?.findAll('button').find((b) => b.text().trim() === text);
}

async function showGate(p: ImportPreflightDto) {
  api.getArchiveDetails.mockResolvedValue(
    archiveDetails({
      activeJob: { jobId: 'job-1', kind: 'import', phase: 'awaitingConfirmation', backupId: 'b1', waitingAtGate: true },
      preflight: p,
    }),
  );
  await useArchives().select(ARCHIVE_ID);
  await flushPromises();
}

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
  api.listBackups.mockResolvedValue(
    backupList([
      summary(),
      summary({ id: 'enc', state: 'needsPassword', encrypted: true, deviceName: { value: 'Second Test Phone', source: 'infoPlist' } }),
      summary({ id: 'bad', state: 'incomplete' }),
    ]),
  );
  api.listArchives.mockResolvedValue(archiveListing());
  api.getArchiveDetails.mockResolvedValue(archiveDetails());
  api.startImport.mockResolvedValue({ jobId: 'job-1' });
  api.confirmImport.mockResolvedValue(null);
});

afterEach(() => {
  useArchives().stopPolling();
  wrapper?.unmount();
  wrapper = null;
});

describe('CreateArchiveDialog', () => {
  it('notes that the archive is unencrypted and refuses an over-long name before sending it', async () => {
    await mountDialogs();
    useArchives().openCreate();
    await flushPromises();
    expect(wrapper?.text()).toContain(UNENCRYPTED_NOTE);
    await wrapper?.find('#archive-label').setValue('x'.repeat(201));
    expect(wrapper?.find('#archive-label-error').text()).toContain('up to 200 bytes');
    expect(button('Create archive')?.attributes('disabled')).toBeDefined();
    await wrapper?.find('#archive-label').setValue('Family phone');
    expect(button('Create archive')?.attributes('disabled')).toBeUndefined();
  });
});

describe('ImportDialog', () => {
  async function openImport(opts: Parameters<ReturnType<typeof useArchives>['openImport']>[0] = {}) {
    await useBackups().refresh();
    await useArchives().refresh();
    await mountDialogs();
    useArchives().openImport(opts);
    await flushPromises();
  }

  it('defaults to copying attachments and leaving Recently Deleted out, with the RD warning naming attachments', async () => {
    await openImport({ backupId: 'b1' });
    const copy = wrapper?.find<HTMLInputElement>('#import-copy-attachments');
    const rd = wrapper?.find<HTMLInputElement>('#import-include-rd');
    expect(copy?.element.checked).toBe(true);
    expect(rd?.element.checked).toBe(false);
    expect(wrapper?.find('#import-rd-note').text()).toBe(RD_WARNING);
    expect(RD_WARNING).toContain('and their attachments, were deleted on the phone');
    await rd?.setValue(true);
    expect(wrapper?.find('#import-rd-note').classes()).toContain('alert-warning');
  });

  it('pre-selects the backup and the only archive, and starts with the chosen options', async () => {
    await openImport({ backupId: 'b1' });
    expect(wrapper?.find<HTMLInputElement>('#import-backup-b1').element.checked).toBe(true);
    expect(wrapper?.find<HTMLSelectElement>('#import-archive').element.value).toBe(ARCHIVE_ID);
    expect(wrapper?.find<HTMLInputElement>('#import-backup-bad').element.disabled).toBe(true);
    await wrapper?.find('#import-copy-attachments').setValue(false);
    await wrapper?.find('form#import-form').trigger('submit');
    await flushPromises();
    expect(api.startImport).toHaveBeenCalledWith(
      'b1',
      ARCHIVE_ID,
      null,
      { copyAttachments: false, includeRecentlyDeleted: false },
      expect.any(Function),
    );
    expect(useArchives().importDialog).toBeNull();
  });

  it('asks for the password of an encrypted backup with an import button label', async () => {
    await openImport({ backupId: 'enc' });
    expect(button('Unlock and prepare import…')).toBeDefined();
    await wrapper?.find('form#import-form').trigger('submit');
    await flushPromises();
    expect(api.startImport).not.toHaveBeenCalled();
    expect(wrapper?.text()).toContain('Second Test Phone');
    expect(button('Unlock and prepare import')).toBeDefined();
  });

  it('marks the backups that match an unfinished import and pre-selects one on Continue importing', async () => {
    api.getArchiveDetails.mockResolvedValue(archiveDetails({ resumable: true, matchingBackupIds: ['enc'] }));
    await useArchives().select(ARCHIVE_ID);
    await openImport({ archiveId: ARCHIVE_ID, continuing: true });
    expect(wrapper?.text()).toContain('Continue importing');
    expect(wrapper?.find<HTMLInputElement>('#import-backup-enc').element.checked).toBe(true);
    expect(wrapper?.text()).toContain("Looks like the unfinished import's backup");
  });

  it('starts Continue importing from the unfinished import\'s options, and a new import from the defaults', async () => {
    api.getArchiveDetails.mockResolvedValue(
      archiveDetails({
        resumable: true,
        matchingBackupIds: ['b1'],
        resumeOptions: { includeRecentlyDeleted: true, copyAttachments: false },
      }),
    );
    await useArchives().select(ARCHIVE_ID);
    await openImport({ archiveId: ARCHIVE_ID, continuing: true });
    expect(wrapper?.find<HTMLInputElement>('#import-copy-attachments').element.checked).toBe(false);
    expect(wrapper?.find<HTMLInputElement>('#import-include-rd').element.checked).toBe(true);
    await wrapper?.find('form#import-form').trigger('submit');
    await flushPromises();
    expect(api.startImport).toHaveBeenCalledWith(
      'b1',
      ARCHIVE_ID,
      null,
      { copyAttachments: false, includeRecentlyDeleted: true },
      expect.any(Function),
    );
    // Not continuing: the defaults, whatever the unfinished import used.
    useArchives().openImport({ archiveId: ARCHIVE_ID, backupId: 'b1' });
    await flushPromises();
    expect(wrapper?.find<HTMLInputElement>('#import-copy-attachments').element.checked).toBe(true);
    expect(wrapper?.find<HTMLInputElement>('#import-include-rd').element.checked).toBe(false);
  });

  it('offers to create an archive first when there is none', async () => {
    api.listArchives.mockResolvedValue(archiveListing([]));
    await openImport({ backupId: 'b1' });
    expect(wrapper?.text()).toContain('There is no archive to import into yet.');
    expect(button('Prepare import')?.attributes('disabled')).toBeDefined();
    await button('Create archive first…')?.trigger('click');
    expect(useArchives().createOpen).toBe(true);
    expect(useArchives().createThenImport).toEqual({ backupId: 'b1' });
  });
});

describe('ImportPreflightDialog', () => {
  it('shows each fact as its own sentence, the privacy notes, and starts a first import as a new set', async () => {
    await mountDialogs();
    await showGate(preflight());
    const text = wrapper?.text() ?? '';
    expect(text).toContain('Confirm the import');
    expect(text).toContain('This starts a new import.');
    expect(text).toContain('This is the first import into this archive.');
    expect(text).toContain('This import needs about 2 GB of free space (an estimate).');
    expect(text).toContain(UNENCRYPTED_NOTE);
    expect(text).toContain(STAGING_NOTE);
    expect(text).not.toMatch(/\bOK\b/);
    expect(wrapper?.findAll('.oma-pf-sentence').length).toBeGreaterThan(5);
    expect(wrapper?.find<HTMLInputElement>('#pf-lineage-new').element.checked).toBe(true);
    await button('Import')?.trigger('click');
    await flushPromises();
    expect(api.confirmImport).toHaveBeenCalledWith('job-1', true, { kind: 'new' });
  });

  it('pre-selects the suggested import set, states its evidence, and continues or starts over', async () => {
    await mountDialogs();
    await showGate(
      preflight({
        device: 'compared',
        resume: { kind: 'continue', session: 3, lineageId: 2, fromPass: 'messages' },
        lineages: [
          { lineageId: 1, sessions: 1, fullSessions: 1, lastBackupMs: null, modelDiffers: true, nameDiffers: true, deviceMatches: false },
          { lineageId: 2, sessions: 2, fullSessions: 1, lastBackupMs: Date.UTC(2026, 8, 1), modelDiffers: false, nameDiffers: false, deviceMatches: true },
        ],
        suggestedLineage: { kind: 'existing', lineageId: 2 },
        order: { dated: true, source: 'infoPlistLastBackupDate', outOfRange: false, sortsBeforeLatest: true, sourceDiffers: false },
        qualifiers: ['iosVersionUntested'],
        provisional: true,
      }),
    );
    const text = wrapper?.text() ?? '';
    expect(text).toContain('Continuing import session 3 from messages, if you choose import set 2 below.');
    expect(text).toContain('A newer backup of the same phone as import set 1 (1 import, last backup date not recorded)');
    expect(text).toContain('The phone model differs.');
    expect(text).toContain('A different phone or source');
    expect(text).toContain('This backup is older than the newest one in this archive.');
    expect(text).toContain(PROVISIONAL);
    expect(wrapper?.find<HTMLInputElement>('#pf-lineage-2').element.checked).toBe(true);
    await wrapper?.find('#pf-lineage-1').setValue(true);
    await button('Import')?.trigger('click');
    await flushPromises();
    expect(api.confirmImport).toHaveBeenCalledWith('job-1', true, { kind: 'existing', lineageId: 1 });
  });

  it('declines on Cancel, which writes nothing', async () => {
    await mountDialogs();
    await showGate(preflight());
    const gate = wrapper?.findAll('dialog').find((d) => d.text().includes('Confirm the import'));
    await gate?.findAll('button').find((b) => b.text().trim() === 'Cancel')?.trigger('click');
    await flushPromises();
    expect(api.confirmImport).toHaveBeenCalledWith('job-1', false, null);
  });

  it('is not shown for an answered gate, even if a stale poll still says it waits', async () => {
    await mountDialogs();
    await showGate(preflight());
    await button('Import')?.trigger('click');
    await flushPromises();
    await useArchives().loadDetails(ARCHIVE_ID);
    await flushPromises();
    expect(useArchives().gate).toBeNull();
  });
});

describe('ImportSummary and VerifySummary', () => {
  it('says "stopped; can continue", never "failed", with its reason and the staging line', () => {
    const w = mount(ImportSummary, {
      props: {
        last: lastImport({ session: 3, state: 'stopped', stopReason: 'sourceChanged', endedAtMs: null, provisional: true }),
        result: {
          jobId: 'job-1',
          outcome: 'stopped',
          stopReason: 'sourceChanged',
          notImportable: null,
          gate: null,
          session: 3,
          notice: null,
          stagingRemoved: true,
          plaintextRemoved: true,
        },
      },
    });
    const text = w.text();
    expect(text).toContain('Stopped; can continue');
    expect(text).toContain('The backup changed while it was being imported.');
    expect(text).not.toContain('Failed');
    expect(text).toContain('The temporary copy of the Messages database was deleted: yes.');
    expect(text).toContain(PROVISIONAL);
    expect(text).toContain('not recorded');
    expect(text).toContain('Read from the backup');
  });

  it('shows "not recorded" for missing counts and no staging line for another session', () => {
    const w = mount(ImportSummary, {
      props: {
        last: lastImport({ counts: null }),
        result: {
          jobId: 'j',
          outcome: 'completed',
          stopReason: null,
          notImportable: null,
          gate: null,
          session: 99,
          notice: null,
          stagingRemoved: false,
          plaintextRemoved: null,
        },
      },
    });
    expect(w.text()).toContain('The counts of this import: not recorded.');
    expect(w.text()).not.toContain('was deleted');
    expect(w.findAll('table')).toHaveLength(0);
  });

  it('writes one sentence per finding kind, and none for a clean archive', () => {
    const clean = mount(VerifySummary, { props: { summary: verifySummary() } });
    expect(clean.text()).toContain('No problems were found.');
    expect(clean.findAll('.oma-verify-findings li')).toHaveLength(0);
    const bad = mount(VerifySummary, {
      props: {
        summary: verifySummary({
          depth: 'full',
          outcome: 'problems',
          counts: verifyCounts({ blobMissing: 2, foreignKeyViolations: 1, hasProblems: true }),
        }),
      },
    });
    const items = bad.findAll('.oma-verify-findings li').map((li) => li.text());
    expect(items).toEqual(['1 link between records points at a missing record.', '2 attachment files are missing.']);
    expect(bad.text()).toContain('Full check');
    const crashed = mount(VerifySummary, { props: { summary: verifySummary({ outcome: null, counts: null, endedAtMs: null }) } });
    expect(crashed.text()).toContain('The check did not finish');
    expect(crashed.text()).toContain('not recorded');
  });
});
