// The Export dialog's slice 6k2 (M6 spec §6.6, §10, §14.6): "Save a copy"
// of one attachment (its facts and the disclosure, the earlier-content
// confirmation for an attachment of an unsent part, the risky-name
// confirmation, the fixed request, the result), events left out (the
// option sent, the words of the preview and the result), and "Continue in
// background" with the navbar's status chip (the dialog closes, the chip
// shows and announces the state, reopens the progress, then the unread
// result, and goes once it is read).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      previewExport: vi.fn(),
      chooseExportDestination: vi.fn(),
      startExport: vi.fn(),
      discardExportDraft: vi.fn(),
      showExportInFolder: vi.fn(),
      cancelJob: vi.fn(),
    },
  };
});
import { IpcError, ipc } from '@/services/ipc';
import ExportDialog from '@/components/ExportDialog.vue';
import ExportStatusChip from '@/components/ExportStatusChip.vue';
import {
  CHIP_TEXT,
  COPY_CHIP_TEXT,
  COPY_DISCLOSURE,
  COPY_EARLIER_CONFIRM,
  COPY_SAVED,
  resultNotes,
  reviewLines,
} from '@/services/exportText';
import { COPY_OPTIONS, useExports, type ExportCopyTarget } from '@/stores/exports';
import { useNotifications } from '@/stores/notifications';
import type {
  DestinationToken,
  DraftId,
  ExportDestinationDto,
  ExportPreviewDto,
  ExportSummaryDto,
  JobEvent,
  PreviewEvent,
  SafeId,
  UntrustedText,
} from '@/types';
import { ARCHIVE_ID } from './helpers';

const api = vi.mocked(ipc);
const DRAFT = 'a'.repeat(32) as DraftId;
const TOKEN = 'b'.repeat(32) as DestinationToken;
const TARGET: ExportCopyTarget = { archiveId: ARCHIVE_ID, viewer: 4, attachmentId: 12 as SafeId };

let wrapper: ReturnType<typeof mount> | null = null;
let chip: ReturnType<typeof mount> | null = null;
let previewChannel: ((e: PreviewEvent) => void) | null = null;
let jobChannel: ((e: JobEvent) => void) | null = null;

function destination(overrides: Partial<ExportDestinationDto> = {}): ExportDestinationDto {
  return {
    destinationToken: TOKEN,
    packaging: 'singleFile',
    displayName: 'IMG_0001.jpg' as UntrustedText,
    targetExists: false,
    warnings: [],
    syncProvider: null,
    freeSpace: 'under100Gb',
    enough: 'yes',
    leftovers: 0,
    riskyName: false,
    ...overrides,
  };
}

function summary(overrides: Partial<ExportSummaryDto> = {}): ExportSummaryDto {
  return {
    items: 0,
    foldedReactionRows: 0,
    conversations: 0,
    filesWritten: 1,
    attachmentsCopied: 1,
    attachmentsBlocked: 0,
    attachmentsExcluded: { withheldUnsent: 0, noStoredFile: 0, checksumMismatch: 0, tooLargeForVolume: 0, writeFailed: 0 },
    oneTimeCodesMasked: 0,
    earlierTextWithheld: 0,
    editTextWithheld: 0,
    alternativeTextWithheld: 0,
    hiddenCharactersMarked: 0,
    controlsEscaped: 0,
    neutralisedCells: 0,
    timesOutsideZoneWindow: 0,
    undatedExcluded: 0,
    eventsNotIncluded: 0,
    motwMissing: false,
    searchHitsExcluded: null,
    bundle: false,
    size: 'under10Mb',
    clamped: false,
    ...overrides,
  };
}

function preview(overrides: Partial<ExportPreviewDto> = {}): ExportPreviewDto {
  return {
    packaging: 'bundle',
    conversations: 3,
    timelineRows: 900,
    distinctMessages: 880,
    foldedReactionRows: 20,
    undatedExcluded: 0,
    unassigned: 0,
    attachments: 0,
    attachmentsWithFile: 0,
    attachmentsWithheldUnsent: 0,
    distinctFiles: 0,
    fileBytes: 0,
    largestFileBytes: 0,
    messagesWithFlaggedCode: 0,
    messagesWithEarlierContent: 0,
    estimatedBytes: 1024 * 1024,
    spanUtcMs: null,
    zoneWindowUtcMs: null,
    search: null,
    eventsLeftOut: 0,
    clamped: false,
    ...overrides,
  };
}

function button(text: string) {
  return wrapper?.findAll('button').find((b) => b.text().trim() === text);
}

async function click(text: string) {
  const b = button(text);
  expect(b, `button "${text}"`).toBeTruthy();
  await b!.trigger('click');
  await flushPromises();
}

async function openCopy(): Promise<void> {
  wrapper = mount(ExportDialog, { attachTo: document.body });
  useExports().openSaveCopy(TARGET);
  await flushPromises();
}

async function copyReady(earlierContent = false): Promise<void> {
  previewChannel!({ kind: 'copyReady', draftId: DRAFT, copy: { earlierContent, fileBytes: 3 * 1024 * 1024 } });
  await flushPromises();
}

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
  previewChannel = null;
  jobChannel = null;
  api.previewExport.mockImplementation((_req, onEvent) => {
    previewChannel = onEvent;
    return Promise.resolve({ draftId: DRAFT });
  });
  api.chooseExportDestination.mockResolvedValue(destination());
  api.startExport.mockImplementation((_req, onEvent) => {
    jobChannel = onEvent;
    return Promise.resolve({ jobId: 'job-7' });
  });
  api.discardExportDraft.mockResolvedValue(null);
  api.showExportInFolder.mockResolvedValue(null);
  api.cancelJob.mockResolvedValue(true);
  HTMLDialogElement.prototype.showModal = function showModal(this: HTMLDialogElement) {
    this.setAttribute('open', '');
  };
  HTMLDialogElement.prototype.close = function close(this: HTMLDialogElement) {
    if (!this.hasAttribute('open')) return;
    this.removeAttribute('open');
    this.dispatchEvent(new Event('close'));
  };
});

afterEach(() => {
  wrapper?.unmount();
  chip?.unmount();
  wrapper = null;
  chip = null;
  document.body.innerHTML = '';
});

describe('Save a copy', () => {
  it('asks Rust for the attachment by its id with the fixed copy options, and shows the disclosure', async () => {
    await openCopy();
    expect(api.previewExport.mock.calls[0]![0]).toEqual({
      scope: { kind: 'attachment', viewer: 4, attachmentId: 12 },
      formats: [],
      options: COPY_OPTIONS,
    });
    expect(wrapper!.find('h2').text()).toBe('Save a copy');
    expect(wrapper!.find('legend').text()).toBe('Save a copy of this attachment');
    expect(wrapper!.text()).toContain('Reading the attachment…');
    // Nothing comes before Review for a copy.
    expect(button('Back')).toBeUndefined();
    await copyReady();
    expect(wrapper!.text()).toContain('One file, 3 MB.');
    expect(wrapper!.text()).toContain(COPY_DISCLOSURE);
    expect(wrapper!.find('#export-confirm-copy-earlier').exists()).toBe(false);
    expect(button('Choose where to save…')?.attributes('disabled')).toBeUndefined();
  });

  it('Cancel while reading closes the dialog and drops the draft', async () => {
    await openCopy();
    await click('Cancel');
    expect(useExports().open).toBe(false);
    expect(api.discardExportDraft).toHaveBeenCalledWith(DRAFT);
  });

  it('needs the earlier-content confirmation for an attachment of an unsent part, and sends it for the one attachment', async () => {
    await openCopy();
    await copyReady(true);
    expect(wrapper!.find('label[for="export-confirm-copy-earlier"]').text()).toBe(COPY_EARLIER_CONFIRM);
    expect(button('Choose where to save…')?.attributes('disabled')).toBeDefined();
    await wrapper!.find('#export-confirm-copy-earlier').setValue(true);
    await click('Choose where to save…');
    expect(wrapper!.find('legend').text()).toBe('Where to save the copy');
    expect(wrapper!.text()).toContain('The copy will be saved as IMG_0001.jpg.');
    // Back keeps the facts: no second preview.
    await click('Back');
    expect(wrapper!.find('legend').text()).toBe('Save a copy of this attachment');
    expect(api.previewExport).toHaveBeenCalledTimes(1);
    await click('Choose where to save…');
    await click('Save copy');
    expect(api.startExport.mock.calls[0]![0].confirmations).toEqual({
      warnings: [],
      replace: false,
      reveal: { earlierContent: 1 },
      keepRiskyExtension: false,
      removeLeftovers: false,
    });
  });

  it('needs its own confirmation for a name that can run programs, naming the file', async () => {
    api.chooseExportDestination.mockResolvedValue(
      destination({ displayName: 'setup.exe' as UntrustedText, riskyName: true }),
    );
    await openCopy();
    await copyReady();
    await click('Choose where to save…');
    expect(wrapper!.find('label[for="export-confirm-risky"]').text()).toBe(
      'This file type can run programs. Save it as “setup.exe” anyway',
    );
    expect(button('Save copy')?.attributes('disabled')).toBeDefined();
    await wrapper!.find('#export-confirm-risky').setValue(true);
    await click('Save copy');
    const c = api.startExport.mock.calls[0]![0].confirmations;
    expect(c.keepRiskyExtension).toBe(true);
    expect(c.reveal).toBeNull();
  });

  it('says the copy was saved, with Show in folder, and resets everything on the next open', async () => {
    await openCopy();
    await copyReady();
    await click('Choose where to save…');
    await click('Save copy');
    jobChannel!({ kind: 'progress', jobId: 'job-7', phase: 'copyingAttachments', done: 0, total: 1 });
    await flushPromises();
    expect(wrapper!.find('legend').text()).toBe('Saving the copy');
    expect(button('Cancel saving')).toBeTruthy();
    jobChannel!({ kind: 'exportFinished', jobId: 'job-7', outcome: 'completed', summary: summary(), stagingRemoved: true });
    await flushPromises();
    expect(wrapper!.find('legend').text()).toBe('Copy saved');
    expect(wrapper!.text()).toContain(COPY_SAVED);
    expect(wrapper!.text()).not.toContain('Exported 0 items');
    await click('Show in folder');
    expect(api.showExportInFolder).toHaveBeenCalledWith('job-7');
    await click('Done');
    const x = useExports();
    x.openExport({ archiveId: ARCHIVE_ID, scope: 'archive' });
    await flushPromises();
    expect(x.copyTarget).toBeNull();
    expect(x.confirmCopyEarlier).toBe(false);
    expect(x.confirmRiskyName).toBe(false);
    expect(wrapper!.find('h2').text()).toBe('Export');
  });
});

describe('events left out', () => {
  it('words the preview and the result', () => {
    expect(reviewLines(preview({ eventsLeftOut: 4 }), false)).toContain(
      '4 group or system events are left out, with any reactions to them.',
    );
    expect(reviewLines(preview(), false).join(' ')).not.toContain('event');
    expect(resultNotes(summary({ eventsNotIncluded: 1 }))).toContain(
      '1 group or system event was left out, with any reactions to them.',
    );
    expect(resultNotes(summary({ motwMissing: true })).join(' ')).toContain('Mark of the Web');
  });
});

describe('Continue in background and the status chip', () => {
  async function runningExport(): Promise<void> {
    wrapper = mount(ExportDialog, { attachTo: document.body });
    chip = mount(ExportStatusChip, { attachTo: document.body });
    const x = useExports();
    x.openExport({ archiveId: ARCHIVE_ID, scope: 'archive' });
    await flushPromises();
    await click('Next');
    await wrapper!.find('#export-format-json').setValue(true);
    await click('Next');
    x.form!.zone = 'utc';
    await click('Next');
    previewChannel!({ kind: 'ready', draftId: DRAFT, preview: preview() });
    await flushPromises();
    await click('Choose where to save…');
    await click('Start export');
    jobChannel!({ kind: 'progress', jobId: 'job-7', phase: 'writingMessages', done: 10, total: 900 });
    await flushPromises();
  }

  it('closes the dialog, shows the chip, and reopens the progress from it', async () => {
    await runningExport();
    const x = useExports();
    expect(chip!.find('button').exists()).toBe(false);
    expect(button('Continue in background')?.attributes('aria-describedby')).toBe('export-background-note');
    await click('Continue in background');
    expect(x.open).toBe(false);
    expect(api.cancelJob).not.toHaveBeenCalled();
    await flushPromises();
    const b = chip!.find('button');
    expect(b.text()).toBe(CHIP_TEXT.running);
    expect(b.find('[role="status"]').attributes('aria-live')).toBe('polite');
    expect(chip!.find('#oma-export-chip-hint').text()).toBe(CHIP_TEXT.show);
    await b.trigger('click');
    await flushPromises();
    expect(x.open).toBe(true);
    expect(x.step).toBe('progress');
    expect(chip!.find('button').exists()).toBe(false);
  });

  it('offers "Continue in background" when Escape asks before cancelling', async () => {
    await runningExport();
    wrapper!.find('dialog').element.dispatchEvent(new Event('cancel', { cancelable: true }));
    await flushPromises();
    expect(wrapper!.text()).toContain('The export is still running. Cancel it?');
    const buttons = wrapper!.findAll('.alert-warning button').map((b) => b.text());
    expect(buttons).toEqual(['Cancel export', 'Keep exporting', 'Continue in background']);
    await wrapper!.findAll('.alert-warning button')[2]!.trigger('click');
    await flushPromises();
    expect(useExports().open).toBe(false);
    expect(useExports().askCancel).toBe(false);
  });

  it('announces an end while closed with a toast, keeps the chip until the result is read, then removes it', async () => {
    await runningExport();
    const x = useExports();
    await click('Continue in background');
    jobChannel!({ kind: 'exportFinished', jobId: 'job-7', outcome: 'completed', summary: summary({ items: 900, conversations: 3, bundle: true }), stagingRemoved: true });
    await flushPromises();
    expect(useNotifications().toasts.map((t) => t.message)).toContain('Export finished.');
    expect(x.liveJob).toBeNull();
    expect(chip!.find('button').text()).toBe(CHIP_TEXT.finished);
    expect(chip!.find('#oma-export-chip-hint').text()).toBe(CHIP_TEXT.showResult);
    await chip!.find('button').trigger('click');
    await flushPromises();
    expect(x.open).toBe(true);
    expect(x.step).toBe('result');
    expect(wrapper!.find('legend').text()).toBe('Export finished');
    await click('Done');
    expect(x.chipVisible).toBe(false);
    expect(chip!.find('button').exists()).toBe(false);
  });

  it('shows a failed or cancelled export on the chip', async () => {
    await runningExport();
    const x = useExports();
    await click('Continue in background');
    jobChannel!({ kind: 'cancelled', jobId: 'job-7', stagingRemoved: true });
    await flushPromises();
    expect(chip!.find('button').text()).toBe(CHIP_TEXT.cancelled);
    expect(x.resultUnseen).toBe(true);
  });
});

describe('Save a copy: review fixes', () => {
  it('explains why it is back at the facts when the draft expired', async () => {
    api.chooseExportDestination.mockRejectedValueOnce(
      new IpcError('choose_export_destination', 'exportDraftExpired', 'The review expired; it was taken again.'),
    );
    await openCopy();
    await copyReady();
    await click('Choose where to save…');
    await copyReady();
    expect(wrapper!.find('legend').text()).toBe('Save a copy of this attachment');
    expect(wrapper!.find('[role="status"].alert-info').text()).toBe('The review expired; it was taken again.');
  });

  it('words a failed or cancelled copy as a copy, in the legend, the chip and the toast', async () => {
    wrapper = mount(ExportDialog, { attachTo: document.body });
    chip = mount(ExportStatusChip, { attachTo: document.body });
    useExports().openSaveCopy(TARGET);
    await flushPromises();
    await copyReady();
    await click('Choose where to save…');
    await click('Save copy');
    jobChannel!({ kind: 'progress', jobId: 'job-7', phase: 'copyingAttachments', done: 0, total: 1 });
    await flushPromises();
    await click('Continue in background');
    expect(chip!.find('button').text()).toBe(COPY_CHIP_TEXT.running);
    jobChannel!({ kind: 'cancelled', jobId: 'job-7', stagingRemoved: true });
    await flushPromises();
    expect(chip!.find('button').text()).toBe(COPY_CHIP_TEXT.cancelled);
    expect(useNotifications().toasts.map((t) => t.message)).toContain(
      'Saving the copy was cancelled. The unfinished copy was removed.',
    );
    await chip!.find('button').trigger('click');
    await flushPromises();
    expect(wrapper!.find('legend').text()).toBe('Copy cancelled');
    expect(wrapper!.text()).not.toContain('Export cancelled');
  });
});
