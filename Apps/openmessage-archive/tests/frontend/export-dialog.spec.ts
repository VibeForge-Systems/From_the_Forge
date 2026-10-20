// The Export dialog and its store (M6 spec §10, §14.6, slice 6k1): steps
// and focus, the options reset on every open, XLSX shown as unavailable,
// the owner's attachment defaults (Q3a), the request Rust validates,
// "Counting…" with Cancel, the reveal confirmations against exact counts
// (Q4), the zone table and "Use UTC instead", the excluded-hits sentence,
// the destination (warnings, leftovers, the replace prompt, no path), the
// stale-confirmation path, progress and cancel, the result, and the
// privacy screen.
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
import {
  DISCLOSURE,
  EVENTS_NOTE,
  FORMAT_UNAVAILABLE,
  KEEP_EXTENSIONS_CONFIRM,
  REPLACE_CONFIRMED,
  STALE_NOTICE,
  formatList,
  resultNotes,
  resultSentence,
  reviewLines,
} from '@/services/exportText';
import {
  bundleWithoutFiles,
  dateRangeInput,
  defaultAttachments,
  freshForm,
  localDateDays,
  useExports,
  type ExportOpenContext,
} from '@/stores/exports';
import { useNotifications } from '@/stores/notifications';
import { useViewer } from '@/stores/viewer';
import type {
  DestinationToken,
  DraftId,
  ExportDestinationDto,
  ExportPreviewDto,
  ExportSummaryDto,
  JobEvent,
  PreviewEvent,
  PreviewExportRequest,
  SafeId,
  UntrustedText,
} from '@/types';
import { ARCHIVE_ID } from './helpers';

const api = vi.mocked(ipc);
const CHAT = 7 as SafeId;
const DRAFT = 'd'.repeat(32) as DraftId;
const DRAFT2 = 'e'.repeat(32) as DraftId;
const TOKEN = 'f'.repeat(32) as DestinationToken;

let wrapper: ReturnType<typeof mount> | null = null;
/** The channel callbacks of each previewExport call, in order. */
let previewChannels: ((e: PreviewEvent) => void)[] = [];
let jobChannel: ((e: JobEvent) => void) | null = null;
let draftIds: DraftId[] = [];

function preview(overrides: Partial<ExportPreviewDto> = {}): ExportPreviewDto {
  return {
    packaging: 'singleFile',
    conversations: 1,
    timelineRows: 120,
    distinctMessages: 118,
    foldedReactionRows: 2,
    undatedExcluded: 0,
    unassigned: 0,
    attachments: 5,
    attachmentsWithFile: 4,
    attachmentsWithheldUnsent: 0,
    distinctFiles: 4,
    fileBytes: 3 * 1024 * 1024,
    largestFileBytes: 1024 * 1024,
    messagesWithFlaggedCode: 0,
    messagesWithEarlierContent: 0,
    estimatedBytes: 4 * 1024 * 1024,
    spanUtcMs: [Date.UTC(2024, 0, 1), Date.UTC(2024, 5, 1)],
    zoneWindowUtcMs: null,
    search: null,
    eventsLeftOut: 0,
    clamped: false,
    ...overrides,
  };
}

function destination(overrides: Partial<ExportDestinationDto> = {}): ExportDestinationDto {
  return {
    destinationToken: TOKEN,
    packaging: 'singleFile',
    displayName: 'Messages export 2026-10-20 18-30.html' as UntrustedText,
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
    items: 3901,
    foldedReactionRows: 0,
    conversations: 2,
    filesWritten: 300,
    attachmentsCopied: 292,
    attachmentsBlocked: 3,
    attachmentsExcluded: { withheldUnsent: 0, noStoredFile: 12, checksumMismatch: 0, tooLargeForVolume: 0, writeFailed: 0 },
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
    bundle: true,
    size: 'under1Gb',
    clamped: false,
    ...overrides,
  };
}

const CONVERSATION: ExportOpenContext = { archiveId: ARCHIVE_ID, chatId: CHAT, scope: 'conversation' };

async function mountDialog(ctx: ExportOpenContext = CONVERSATION) {
  wrapper = mount(ExportDialog, { attachTo: document.body });
  useExports().openExport(ctx);
  await flushPromises();
  return wrapper;
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

async function check(selector: string, on = true) {
  const el = wrapper!.find<HTMLInputElement>(selector);
  expect(el.exists(), selector).toBe(true);
  await el.setValue(on);
  await flushPromises();
}

/** What → Format (with these formats) → Options → Review. */
async function toReview(formats: string[] = ['html'], setOptions?: () => Promise<void>) {
  await click('Next');
  for (const f of formats) await check(`#export-format-${f}`);
  await click('Next');
  if (setOptions) {
    await click('Change…');
    await setOptions();
  }
  await click('Next');
}

function emitPreview(e: PreviewEvent, call = previewChannels.length - 1) {
  previewChannels[call]!(e);
}

async function ready(p: Partial<ExportPreviewDto> = {}, draftId: DraftId = DRAFT) {
  emitPreview({ kind: 'ready', draftId, preview: preview(p) });
  await flushPromises();
}

function lastPreviewRequest(): PreviewExportRequest {
  const calls = api.previewExport.mock.calls;
  return calls[calls.length - 1]![0];
}

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
  previewChannels = [];
  jobChannel = null;
  draftIds = [DRAFT, DRAFT2];
  api.previewExport.mockImplementation((_req, onEvent) => {
    previewChannels.push(onEvent);
    return Promise.resolve({ draftId: draftIds.shift() ?? DRAFT2 });
  });
  api.chooseExportDestination.mockResolvedValue(destination());
  api.startExport.mockImplementation((_req, onEvent) => {
    jobChannel = onEvent;
    return Promise.resolve({ jobId: 'job-1' });
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
  wrapper = null;
  document.body.innerHTML = '';
});

describe('the rules behind the defaults', () => {
  const ctx = CONVERSATION;
  const form = (patch: Partial<ReturnType<typeof freshForm>>) => ({ ...freshForm(ctx, 'Europe/Berlin'), ...patch });

  it('copies attachments by default exactly as owner decision Q3a says', () => {
    // One conversation in one format is a single file: names only.
    expect(defaultAttachments(form({ formats: ['html'] }))).toBe('none');
    expect(defaultAttachments(form({ formats: ['json'] }))).toBe('none');
    // A bundle with HTML or JSON copies files.
    expect(defaultAttachments(form({ formats: ['html', 'txt'] }))).toBe('copyOriginals');
    expect(defaultAttachments(form({ scope: 'dateRange', formats: ['json'] }))).toBe('copyOriginals');
    // CSV and TXT bundles copy only when ticked.
    expect(defaultAttachments(form({ formats: ['csv'] }))).toBe('none');
    expect(defaultAttachments(form({ formats: ['txt', 'csv'] }))).toBe('none');
    expect(defaultAttachments(form({ scope: 'dateRange', formats: ['txt'] }))).toBe('none');
    // The whole archive always copies by default.
    expect(defaultAttachments(form({ scope: 'archive', formats: ['txt'] }))).toBe('copyOriginals');
    // A date range in one conversation is a single conversation.
    expect(bundleWithoutFiles(form({ scope: 'dateRange', dateThisConversation: true, formats: ['pdf'] }))).toBe(false);
    expect(bundleWithoutFiles(form({ scope: 'searchResults', formats: ['pdf'] }))).toBe(true);
  });

  it('starts every form with both reveals, kept extensions and identifiers off', () => {
    const f = freshForm(ctx, 'Europe/Berlin');
    expect(f.revealOneTimeCodes).toBe(false);
    expect(f.revealEarlierContent).toBe(false);
    expect(f.keepExtensions).toBe(false);
    expect(f.includeIdentifiers).toBe(false);
    expect(f.zone).toBe('system');
    expect(freshForm(ctx, null).zone).toBe('utc');
  });

  it('turns the inclusive "Through" day into the half-open range Rust takes', () => {
    expect(dateRangeInput('2024-02-28', '2024-02-29')).toEqual({ from: '2024-02-28', until: '2024-03-01' });
    expect(dateRangeInput('2024-12-31', '2024-12-31')).toEqual({ from: '2024-12-31', until: '2025-01-01' });
    expect(dateRangeInput('2024-03-02', '2024-03-01')).toBeNull();
    expect(dateRangeInput('1899-12-31', '1900-01-02')).toBeNull();
    expect(dateRangeInput('2023-02-29', '2023-03-01')).toBeNull();
    expect(dateRangeInput('9999-01-01', '9999-12-31')).toBeNull();
    expect(localDateDays('1970-01-02')).toBe(1);
    expect(localDateDays('1900-01-01')).toBe(-25_567);
  });
});

describe('the words', () => {
  it('summarise a finished export as the spec words it', () => {
    expect(resultSentence(summary(), ['html', 'json'])).toBe(
      'Exported 3,901 items from 2 conversations as HTML and JSON. 292 attachment files copied; 12 not included (reasons in manifest.json).',
    );
    expect(formatList(['json', 'pdf', 'txt'])).toBe('PDF, TXT and JSON');
    const notes = resultNotes(summary());
    expect(notes).toContain('12 attachments have no file in the archive.');
    expect(notes).toContain('3 copied files have ".blocked" added to their name.');
    expect(notes[notes.length - 1]).toBe('A folder, under 1 GB.');
  });

  it('describe a preview with counts and sizes only', () => {
    const lines = reviewLines(preview({ undatedExcluded: 2, unassigned: 1, attachmentsWithheldUnsent: 1 }), true);
    expect(lines[0]).toBe('120 items (118 messages) from 1 conversation.');
    expect(lines).toContain('2 undated items are left out of the date range.');
    expect(lines).toContain('5 attachments, 4 files to copy (3 MB); 1 not available.');
    expect(reviewLines(preview(), false)).toContain('5 attachments, names only; 1 not available.');
  });
});

describe('ExportDialog', () => {
  it('walks the steps, each a fieldset with a legend, moving focus to its first control', async () => {
    await mountDialog();
    expect(wrapper!.find('legend').text()).toBe('What to export');
    expect(document.activeElement?.id).toBe('export-scope-conversation');
    await click('Next');
    expect(wrapper!.find('legend').text()).toBe('Formats');
    expect(document.activeElement?.id).toBe('export-format-pdf');
    expect(button('Next')?.attributes('disabled')).toBeDefined();
    await check('#export-format-html');
    await click('Next');
    expect(wrapper!.find('legend').text()).toBe('Options');
    await click('Back');
    expect(wrapper!.find('legend').text()).toBe('Formats');
  });

  it('shows XLSX as unavailable with its reason, and never sends it', async () => {
    await mountDialog();
    await click('Next');
    const xlsx = wrapper!.find<HTMLInputElement>('#export-format-xlsx');
    expect(xlsx.attributes('disabled')).toBeDefined();
    expect(wrapper!.find('#export-format-xlsx-note').text()).toBe(FORMAT_UNAVAILABLE.xlsx);
    await xlsx.trigger('change');
    expect(useExports().form?.formats).toEqual([]);
  });

  it('resets both reveals and kept extensions every time it opens', async () => {
    await mountDialog();
    const x = useExports();
    x.form!.revealOneTimeCodes = true;
    x.form!.revealEarlierContent = true;
    x.form!.keepExtensions = true;
    x.form!.includeIdentifiers = true;
    x.close();
    x.openExport(CONVERSATION);
    await flushPromises();
    expect(x.form?.revealOneTimeCodes).toBe(false);
    expect(x.form?.revealEarlierContent).toBe(false);
    expect(x.form?.keepExtensions).toBe(false);
    expect(x.form?.includeIdentifiers).toBe(false);
  });

  it('sends the options exactly as Rust validates them', async () => {
    await mountDialog();
    await toReview(['csv', 'html', 'pdf'], async () => {
      await check('#export-identifiers');
      await check('#export-att-copy');
    });
    const req = lastPreviewRequest();
    expect(req.scope).toEqual({ kind: 'conversation', archiveId: ARCHIVE_ID, chatId: CHAT });
    expect(req.formats).toEqual(['pdf', 'html', 'csv']);
    expect(req.options).toMatchObject({
      attachments: 'copyOriginals',
      keepExtensions: false,
      previews: true,
      events: true,
      revealOneTimeCodes: false,
      revealEarlierContent: false,
      includeIdentifiers: true,
      csvProfile: 'spreadsheet',
      pageSize: 'a4',
    });
    expect(req.zone).toBeUndefined();
  });

  it('sends options that do not apply as off, and events as chosen', async () => {
    await mountDialog();
    await toReview(['txt'], async () => {
      expect(wrapper!.find('#export-events-note').text()).toBe(EVENTS_NOTE);
      expect(wrapper!.find('#export-events').attributes('disabled')).toBeUndefined();
      expect(wrapper!.find<HTMLInputElement>('#export-events').element.checked).toBe(true);
      await check('#export-events', false);
      expect(wrapper!.find('#export-identifiers').attributes('disabled')).toBeDefined();
      expect(wrapper!.find('#export-previews').attributes('disabled')).toBeDefined();
      expect(wrapper!.find('#export-keep-extensions').attributes('disabled')).toBeDefined();
    });
    const o = lastPreviewRequest().options;
    expect(o.events).toBe(false);
    expect(o.previews).toBe(false);
    expect(o.largerPreviews).toBe(false);
    expect(o.includeIdentifiers).toBe(false);
    expect(o.keepExtensions).toBe(false);
    expect('csvProfile' in o).toBe(false);
    expect('pageSize' in o).toBe(false);
  });

  it('shows "Counting…" with Cancel, which discards the draft', async () => {
    await mountDialog();
    await toReview();
    expect(wrapper!.text()).toContain('Counting…');
    emitPreview({ kind: 'progress', draftId: DRAFT, rows: 8192 });
    await flushPromises();
    expect(wrapper!.text()).toContain('8,192 rows so far');
    await click('Cancel');
    expect(api.discardExportDraft).toHaveBeenCalledWith(DRAFT);
    expect(wrapper!.find('legend').text()).toBe('Options');
    // A late answer of the discarded count is ignored.
    await ready();
    expect(useExports().count.kind).toBe('idle');
  });

  it('keeps events that arrive before the draft id', async () => {
    let resolve: (v: { draftId: DraftId }) => void = () => undefined;
    api.previewExport.mockImplementationOnce((_req, onEvent) => {
      previewChannels.push(onEvent);
      return new Promise((r) => (resolve = r));
    });
    await mountDialog();
    await toReview();
    emitPreview({ kind: 'ready', draftId: DRAFT, preview: preview({ zoneWindowUtcMs: null }) });
    await flushPromises();
    expect(useExports().count.kind).toBe('counting');
    resolve({ draftId: DRAFT });
    await flushPromises();
    expect(useExports().count.kind).toBe('ready');
    expect(wrapper!.text()).toContain(DISCLOSURE);
  });

  it('requires each reveal confirmed against its exact count, and sends that count', async () => {
    await mountDialog();
    await toReview(['html'], async () => {
      await check('#export-reveal-codes');
      await check('#export-reveal-earlier');
      await check('#export-zone-utc');
    });
    await ready({ messagesWithFlaggedCode: 7, messagesWithEarlierContent: 1 });
    expect(wrapper!.find('label[for="export-confirm-codes"]').text()).toContain('one-time codes from 7 messages');
    expect(wrapper!.find('label[for="export-confirm-earlier"]').text()).toContain('content from 1 message that');
    const choose = () => button('Choose where to save…');
    expect(choose()?.attributes('disabled')).toBeDefined();
    await check('#export-confirm-codes');
    expect(choose()?.attributes('disabled')).toBeDefined();
    await check('#export-confirm-earlier');
    expect(choose()?.attributes('disabled')).toBeUndefined();
    await click('Choose where to save…');
    await click('Start export');
    expect(api.startExport.mock.calls[0]![0].confirmations.reveal).toEqual({ oneTimeCodes: 7, earlierContent: 1 });
  });

  it('requires "Keep original file extensions" confirmed in Review', async () => {
    await mountDialog();
    await toReview(['html'], async () => {
      await check('#export-att-copy');
      await check('#export-keep-extensions');
      await check('#export-zone-utc');
    });
    await ready();
    expect(wrapper!.find('label[for="export-confirm-extensions"]').text()).toBe(KEEP_EXTENSIONS_CONFIRM);
    expect(button('Choose where to save…')?.attributes('disabled')).toBeDefined();
    await check('#export-confirm-extensions');
    await click('Choose where to save…');
    await click('Start export');
    const c = api.startExport.mock.calls[0]![0].confirmations;
    expect(c.keepRiskyExtension).toBe(true);
    expect(c.reveal).toBeNull();
  });

  it('builds the zone table for the window Rust names and sends it once, with the first destination', async () => {
    await mountDialog();
    const x = useExports();
    x.form!.zone = 'named';
    x.form!.namedZone = 'Europe/Berlin';
    x.zones = ['Europe/Berlin'];
    await toReview();
    const window: [number, number] = [Date.UTC(2024, 0, 1), Date.UTC(2025, 0, 1)];
    await ready({ zoneWindowUtcMs: window });
    await vi.waitFor(() => expect(x.count.kind).toBe('ready'));
    await flushPromises();
    expect(wrapper!.find('#export-zone-sentence').text()).toBe(
      'Times will be shown in Europe/Berlin. The zone name suggests where you are.',
    );
    await click('Choose where to save…');
    const first = api.chooseExportDestination.mock.calls[0]![0];
    expect(first.zone?.name).toBe('Europe/Berlin');
    expect(first.zone?.windowUtcMs[0]).toBe(window[0]);
    expect(first.zone?.windowUtcMs[1]).toBeGreaterThan(window[1]);
    expect(first.zone?.transitions).toEqual([
      [window[0], 3600],
      [Date.UTC(2024, 2, 31, 1), 7200],
      [Date.UTC(2024, 9, 27, 1), 3600],
    ]);
    await click('Choose another name');
    expect(api.chooseExportDestination.mock.calls[1]![0]).toEqual({ draftId: DRAFT });
  });

  it('offers "Use UTC instead", which counts again in UTC', async () => {
    await mountDialog();
    const x = useExports();
    x.form!.zone = 'named';
    x.form!.namedZone = 'Europe/Berlin';
    x.zones = ['Europe/Berlin'];
    await toReview();
    await ready({ zoneWindowUtcMs: [Date.UTC(2024, 0, 1), Date.UTC(2024, 1, 1)] });
    await vi.waitFor(() => expect(x.count.kind).toBe('ready'));
    await click('Use UTC instead');
    expect(api.discardExportDraft).toHaveBeenCalledWith(DRAFT);
    expect(lastPreviewRequest().options.zone).toBe('utc');
    await ready({}, DRAFT2);
    expect(wrapper!.find('#export-zone-sentence').text()).toBe('Times will be shown in UTC.');
    expect(button('Use UTC instead')).toBeUndefined();
  });

  it('sends a date range with its table in a second preview when Rust asks', async () => {
    await mountDialog({ archiveId: ARCHIVE_ID, scope: 'dateRange' });
    const x = useExports();
    x.form!.zone = 'named';
    x.form!.namedZone = 'Europe/Berlin';
    x.zones = ['Europe/Berlin'];
    await wrapper!.find('#export-date-from').setValue('2024-03-01');
    await wrapper!.find('#export-date-through').setValue('2024-03-31');
    await toReview(['json']);
    expect(lastPreviewRequest().scope).toEqual({
      kind: 'dateRange',
      archiveId: ARCHIVE_ID,
      fromLocalDate: '2024-03-01',
      untilLocalDate: '2024-04-01',
    });
    emitPreview({
      kind: 'needsZoneTable',
      draftId: DRAFT,
      spanUtcMs: null,
      windowUtcMs: [Date.UTC(2024, 1, 29), Date.UTC(2024, 3, 2)],
    });
    await vi.waitFor(() => expect(api.previewExport).toHaveBeenCalledTimes(2));
    const second = lastPreviewRequest();
    expect(second.zone?.transitions).toEqual([
      [Date.UTC(2024, 1, 29), 3600],
      [Date.UTC(2024, 2, 31, 1), 7200],
    ]);
    await flushPromises();
    await ready({ zoneWindowUtcMs: null }, DRAFT2);
    await click('Choose where to save…');
    // The draft already has its table.
    expect(api.chooseExportDestination.mock.calls[0]![0]).toEqual({ draftId: DRAFT2 });
  });

  it('says how many search matches are left out for matching only hidden text', async () => {
    await mountDialog({
      archiveId: ARCHIVE_ID,
      scope: 'searchResults',
      search: { viewer: 3, searchId: 9, matches: 10_000, conversations: 4, capped: true },
    });
    expect(wrapper!.text()).toContain('These search results: 10,000 matches in 4 conversations');
    await check('#export-all-matches');
    await toReview(['json']);
    expect(lastPreviewRequest().scope).toEqual({ kind: 'searchResults', viewer: 3, searchId: 9, allMatches: true });
    await ready({
      search: {
        kept: 9_990,
        excluded: { oneTimeCodes: 4, earlierContent: 3, both: 1, notWritten: 2 },
        capped: false,
        newestKept: true,
        coverageComplete: true,
        allMatches: true,
      },
    });
    expect(wrapper!.text()).toContain(
      '8 matches are left out because they matched only text this export does not include.',
    );
    expect(wrapper!.text()).toContain('9,990 matches will be exported.');
  });

  it('asks before replacing an existing file, and shows the name, never a path', async () => {
    api.chooseExportDestination.mockResolvedValue(destination({ targetExists: true }));
    await mountDialog();
    await toReview(['html'], async () => check('#export-zone-utc'));
    await ready();
    await click('Choose where to save…');
    expect(wrapper!.find('legend').text()).toBe('Where to save');
    expect(wrapper!.text()).toContain('Replace “Messages export 2026-10-20 18-30.html”? This cannot be undone.');
    expect(button('Start export')?.attributes('disabled')).toBeDefined();
    await click('Replace');
    expect(wrapper!.text()).toContain(REPLACE_CONFIRMED);
    await click('Start export');
    expect(api.startExport.mock.calls[0]![0]).toEqual({
      draftId: DRAFT,
      destinationToken: TOKEN,
      confirmations: { warnings: [], replace: true, reveal: null, keepRiskyExtension: false, removeLeftovers: false },
    });
    // Nothing the webview holds is a path.
    expect(JSON.stringify(useExports().$state)).not.toMatch(/"(path|destination|fileName)"\s*:\s*"/);
  });

  it('renders hidden characters in the chosen name as visible tokens', async () => {
    api.chooseExportDestination.mockResolvedValue(
      destination({ displayName: 'notes‮lmth.txt' as UntrustedText }),
    );
    await mountDialog();
    await toReview(['txt'], async () => check('#export-zone-utc'));
    await ready();
    await click('Choose where to save…');
    expect(wrapper!.text()).toContain('notes⟨U+202E⟩lmth.txt');
  });

  it('needs every warning confirmed, shows the FAT note, and offers to remove leftovers', async () => {
    api.chooseExportDestination.mockResolvedValue(
      destination({
        packaging: 'bundle',
        warnings: ['syncFolder', 'fatVolume', 'mayNotFit'],
        syncProvider: 'oneDrive',
        leftovers: 2,
        enough: 'unknown',
        freeSpace: null,
      }),
    );
    await mountDialog();
    await toReview(['html', 'json'], async () => check('#export-zone-utc'));
    await ready({ packaging: 'bundle' });
    await click('Choose where to save…');
    expect(wrapper!.find('label[for="export-warning-syncFolder"]').text()).toContain('uploaded by OneDrive');
    expect(wrapper!.find('#export-warning-fatVolume').exists()).toBe(false);
    expect(wrapper!.text()).toContain('Files over 4 GiB cannot be written');
    expect(wrapper!.text()).toContain('Free space is not known.');
    expect(button('Start export')?.attributes('disabled')).toBeDefined();
    await check('#export-warning-syncFolder');
    expect(button('Start export')?.attributes('disabled')).toBeDefined();
    await check('#export-warning-mayNotFit');
    await check('#export-leftovers');
    await click('Start export');
    const c = api.startExport.mock.calls[0]![0].confirmations;
    expect(c.warnings.sort()).toEqual(['mayNotFit', 'syncFolder']);
    expect(c.removeLeftovers).toBe(true);
  });

  it('counts again when the confirmation went stale', async () => {
    api.startExport.mockRejectedValueOnce(
      new IpcError('start_export', 'exportConfirmationStale', 'The archive changed or more messages qualify than you confirmed; review the export again.'),
    );
    await mountDialog();
    await toReview(['html'], async () => check('#export-zone-utc'));
    await ready();
    await click('Choose where to save…');
    await click('Start export');
    expect(wrapper!.find('legend').text()).toBe('Review');
    expect(api.previewExport).toHaveBeenCalledTimes(2);
    await ready({}, DRAFT2);
    expect(wrapper!.text()).toContain('The archive changed or more messages qualify than you confirmed');
  });

  it('shows progress in a live region, asks on Escape, and cancels', async () => {
    await mountDialog();
    await toReview(['html'], async () => check('#export-zone-utc'));
    await ready();
    await click('Choose where to save…');
    await click('Start export');
    jobChannel!({ kind: 'progress', jobId: 'job-1', phase: 'writingMessages', done: 40, total: 120 });
    await flushPromises();
    expect(wrapper!.find('legend').text()).toBe('Exporting');
    expect(wrapper!.find('[aria-live="polite"]').text()).toBe('Writing messages');
    expect(wrapper!.text()).toContain('Writing messages (40 of 120)');
    // Escape asks first; the dialog stays open.
    wrapper!.find('dialog').element.dispatchEvent(new Event('cancel', { cancelable: true }));
    await flushPromises();
    expect(useExports().open).toBe(true);
    expect(wrapper!.text()).toContain('The export is still running. Cancel it?');
    await click('Keep exporting');
    expect(api.cancelJob).not.toHaveBeenCalled();
    await click('Cancel export');
    expect(api.cancelJob).toHaveBeenCalledWith('job-1');
    jobChannel!({ kind: 'cancelled', jobId: 'job-1', stagingRemoved: true });
    await flushPromises();
    expect(wrapper!.text()).toContain('Export cancelled. The unfinished export was removed.');
  });

  it('shows the result with Show in folder and Done, and no Open file', async () => {
    await mountDialog();
    await toReview(['html', 'json'], async () => check('#export-zone-utc'));
    await ready({ packaging: 'bundle' });
    await click('Choose where to save…');
    await click('Start export');
    jobChannel!({ kind: 'exportFinished', jobId: 'job-1', outcome: 'completedWithExclusions', summary: summary(), stagingRemoved: true });
    await flushPromises();
    expect(wrapper!.find('legend').text()).toBe('Export finished');
    expect(wrapper!.text()).toContain('Exported 3,901 items from 2 conversations as HTML and JSON.');
    expect(button('Open file')).toBeUndefined();
    await click('Show in folder');
    expect(api.showExportInFolder).toHaveBeenCalledWith('job-1');
    await click('Done');
    expect(useExports().open).toBe(false);
  });

  it('offers to review again when the job found more to reveal than confirmed', async () => {
    await mountDialog();
    await toReview(['html'], async () => check('#export-zone-utc'));
    await ready();
    await click('Choose where to save…');
    await click('Start export');
    jobChannel!({
      kind: 'failed',
      jobId: 'job-1',
      error: { code: 'exportConfirmationStale', message: 'The archive changed.' },
      stagingRemoved: true,
    });
    await flushPromises();
    expect(wrapper!.text()).toContain('The archive changed. Nothing was saved.');
    await click('Review again');
    expect(api.previewExport).toHaveBeenCalledTimes(2);
    await ready({}, DRAFT2);
    expect(wrapper!.text()).toContain(STALE_NOTICE);
  });

  it('is closed by the privacy screen, which drops the draft; a running export continues', async () => {
    await mountDialog();
    await toReview(['html'], async () => check('#export-zone-utc'));
    await ready();
    useViewer().closeDialogs();
    await flushPromises();
    expect(useExports().open).toBe(false);
    expect(api.discardExportDraft).toHaveBeenCalledWith(DRAFT);

    // A running export: the dialog closes, the job continues, its end is a toast.
    const x = useExports();
    x.openExport(CONVERSATION);
    await flushPromises();
    await toReview(['html'], async () => check('#export-zone-utc'));
    await ready({}, DRAFT2);
    await click('Choose where to save…');
    await click('Start export');
    useViewer().closeDialogs();
    await flushPromises();
    expect(x.open).toBe(false);
    expect(api.cancelJob).not.toHaveBeenCalled();
    jobChannel!({ kind: 'exportFinished', jobId: 'job-1', outcome: 'completed', summary: summary(), stagingRemoved: true });
    await flushPromises();
    expect(useNotifications().toasts.map((t) => t.message)).toContain('Export finished.');
  });
});
