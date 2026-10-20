// The Export dialog's state (M6 spec §10): the form, the draft and its
// exact counts, the zone table, the destination, the job and its result.
//
// - **Nothing is persisted**, and nothing carries over between openings:
//   `openExport` builds a fresh form every time, so both reveal opt-ins
//   (owner Q4) and "Keep original file extensions" (Q3b) start off on
//   every open, whatever an earlier export chose. The viewer's
//   `revealEarlierText` setting is never read here.
// - **Defaults** follow the owner's decisions: attachments per Q3a
//   (`defaultAttachments`, the rule `oma_export::plan::default_attachments`
//   applies), identifiers as export-local ordinals unless the user opts in
//   (Q5, a lean awaiting confirmation).
// - **The draft** comes from `preview_export`; its counts arrive on the
//   channel and are matched to the draft id (events can arrive before the
//   call resolves, so they wait until it does). Any change to the form
//   discards the draft; Review counts again. A zone other than UTC needs
//   the zone table (`services/zoneTable.ts`): a date range sends it with a
//   second preview (`needsZoneTable`), every other scope with the first
//   `choose_export_destination`.
// - **Consent** (D147): each reveal that is on is confirmed against its own
//   exact count, and `start_export` sends exactly that count; a recount
//   that finds more is `exportConfirmationStale`, and the dialog counts
//   again (`STALE_NOTICE`).
// - **Paths never reach the webview** (I4): the destination is a token and
//   a display name, rendered by `SafeText`.
// - **The privacy screen** closes the dialog and drops the draft
//   (`closeForPrivacy`); a running export continues, shown by the navbar's
//   status chip, and its end is announced by a toast.
// - **Continue in background** (slice 6k2) closes the dialog while the
//   job runs; the navbar's chip (`chipVisible`) reopens it at its progress
//   or, once it ended unseen, at its result (`reopen`).
// - **"Save a copy"** (`openSaveCopy`, slice 6k2, spec §6.6) is the same
//   dialog with no form: the attachment's facts (`copyReady`), the
//   disclosure and, for an attachment of an unsent part, the
//   earlier-content confirmation; then a save dialog, whose name Rust
//   checks (`riskyName` needs its own confirmation); then the job.
import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { IpcError, ipc } from '@/services/ipc';
import {
  COPY_SAVED,
  INFORMATIONAL_WARNINGS,
  OUTCOME_TEXT,
  cancelledSentence,
  failedNote,
  type ExportStep,
  type ScopeKind,
} from '@/services/exportText';
import {
  buildZoneTable,
  supportedZones,
  systemZoneName,
  withMargin,
  type ZoneTableResult,
} from '@/services/zoneTable';
import type {
  AttachmentModeInput,
  ClockInput,
  CopyPreviewDto,
  CsvProfileInput,
  DraftId,
  ErrorCode,
  ExportConfirmationsInput,
  ExportDestinationDto,
  ExportFormatInput,
  ExportOptionsInput,
  ExportOutcomeDto,
  ExportPreviewDto,
  ExportScopeInput,
  ExportSummaryDto,
  ExportWarningDto,
  JobEvent,
  LineEndingsInput,
  LocalDate,
  PageSizeInput,
  PreviewEvent,
  PreviewExportRequest,
  SafeId,
  ZoneChoiceInput,
  ZoneTableInputDto,
} from '@/types';
import type { ProgressJob } from '@/types/progress';
import { useNotifications } from './notifications';

/** A search set the dialog may export (from the viewer's search panel). */
export interface ExportSearchContext {
  viewer: number;
  searchId: number;
  matches: number;
  /** `null` when not every hit has been loaded, so the number is not known. */
  conversations: number | null;
  /** Only the first 10,000 matches are in the set. */
  capped: boolean;
}

/** The attachment "Save a copy" saves: one the viewer surfaced. */
export interface ExportCopyTarget {
  archiveId: string;
  viewer: number;
  attachmentId: SafeId;
}

/** Where the dialog was opened, and so which scopes it offers. */
export interface ExportOpenContext {
  archiveId: string;
  /** The open conversation, for "This conversation". */
  chatId?: SafeId | null;
  search?: ExportSearchContext | null;
  /** The scope pre-selected. */
  scope: ScopeKind;
}

/** The whole form, rebuilt on every open. */
export interface ExportForm {
  scope: ScopeKind;
  fromDate: string;
  throughDate: string;
  /** A date range in the open conversation only. */
  dateThisConversation: boolean;
  allMatches: boolean;
  formats: ExportFormatInput[];
  /** `null`: follow the default for the scope and formats (Q3a). */
  attachments: AttachmentModeInput | null;
  keepExtensions: boolean;
  previews: boolean;
  largerPreviews: boolean;
  history: boolean;
  reactions: boolean;
  events: boolean;
  revealOneTimeCodes: boolean;
  revealEarlierContent: boolean;
  hiddenCharacterMarkers: boolean;
  seconds: boolean;
  includeIdentifiers: boolean;
  clock: ClockInput;
  lineEndings: LineEndingsInput;
  csvProfile: CsvProfileInput;
  pageSize: PageSizeInput;
  zone: ZoneChoiceInput;
  namedZone: string;
}

export interface StoredExportError {
  code: ErrorCode;
  message: string;
}

export type ZoneFailure = 'timedOut' | 'tooManyTransitions' | 'unsupported';

export type CountState =
  | { kind: 'idle' }
  | { kind: 'counting'; rows: number }
  | { kind: 'zone' }
  | { kind: 'zoneFailed'; reason: ZoneFailure }
  | { kind: 'ready'; preview: ExportPreviewDto }
  | { kind: 'copyReady'; copy: CopyPreviewDto }
  | { kind: 'failed'; error: StoredExportError };

export interface ExportJobState extends ProgressJob {
  jobId: string | null;
  ended: boolean;
}

export type ExportResult =
  | {
      kind: 'finished';
      jobId: string;
      outcome: ExportOutcomeDto;
      summary: ExportSummaryDto;
      formats: ExportFormatInput[];
      /** "Save a copy" of one attachment. */
      copy: boolean;
    }
  | { kind: 'failed'; error: StoredExportError; stagingRemoved: boolean }
  | { kind: 'cancelled'; stagingRemoved: boolean };

/** Formats that carry identifiers (D141; XLSX is not available yet). */
const IDENTIFIER_FORMATS: readonly ExportFormatInput[] = ['json', 'csv', 'xlsx'];
/** Formats that embed image previews. */
const PREVIEW_FORMATS: readonly ExportFormatInput[] = ['html', 'pdf'];
/** Formats this version can write (XLSX is refused, §5.3 deferred). */
export const AVAILABLE_FORMATS: readonly ExportFormatInput[] = ['pdf', 'html', 'txt', 'csv', 'json'];

const LOCAL_DATE = /^(\d{4})-(\d{2})-(\d{2})$/;

/** A `YYYY-MM-DD` date from 1900 to 9999 that exists, as days since 1970. */
export function localDateDays(s: string): number | null {
  const m = LOCAL_DATE.exec(s);
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (y < 1900 || y > 9999 || mo < 1 || mo > 12 || d < 1) return null;
  const t = Date.UTC(y, mo - 1, d);
  const back = new Date(t);
  if (back.getUTCFullYear() !== y || back.getUTCMonth() !== mo - 1 || back.getUTCDate() !== d) return null;
  return Math.round(t / 86_400_000);
}

function daysToLocalDate(days: number): LocalDate {
  const d = new Date(days * 86_400_000);
  const pad = (n: number, w: number) => String(n).padStart(w, '0');
  return `${pad(d.getUTCFullYear(), 4)}-${pad(d.getUTCMonth() + 1, 2)}-${pad(d.getUTCDate(), 2)}`;
}

/**
 * The half-open range Rust takes (`fromLocalDate` up to `untilLocalDate`)
 * from the inclusive "From" and "Through" days the dialog shows; `null`
 * when either is not a date or the end is before the start.
 */
export function dateRangeInput(from: string, through: string): { from: LocalDate; until: LocalDate } | null {
  const a = localDateDays(from);
  const b = localDateDays(through);
  if (a === null || b === null || b < a) return null;
  const until = daysToLocalDate(b + 1);
  if (localDateDays(until) === null) return null;
  return { from: daysToLocalDate(a), until };
}

/** One conversation's export (the rule of `ScopePlan::single_conversation`). */
function singleConversation(form: ExportForm): boolean {
  return form.scope === 'conversation' || (form.scope === 'dateRange' && form.dateThisConversation);
}

/** A folder even without copied files: several files, CSV, or several conversations (D126). */
export function bundleWithoutFiles(form: ExportForm): boolean {
  return form.formats.length !== 1 || !singleConversation(form) || form.formats.includes('csv');
}

/**
 * Owner decision Q3a: original files are copied by default for bundle
 * exports with HTML or JSON and for the whole archive; a single file
 * carries names only; CSV, XLSX and TXT bundles copy files only when the
 * box is ticked. The same rule as `oma_export::plan::default_attachments`.
 */
export function defaultAttachments(form: ExportForm): AttachmentModeInput {
  const rich = form.formats.some((f) => f === 'html' || f === 'json');
  return form.scope === 'archive' || (rich && bundleWithoutFiles(form)) ? 'copyOriginals' : 'none';
}

export function freshForm(ctx: ExportOpenContext, systemZone: string | null): ExportForm {
  return {
    scope: ctx.scope,
    fromDate: '',
    throughDate: '',
    dateThisConversation: false,
    allMatches: false,
    formats: [],
    attachments: null,
    keepExtensions: false,
    previews: true,
    largerPreviews: false,
    history: true,
    reactions: true,
    events: true,
    revealOneTimeCodes: false,
    revealEarlierContent: false,
    hiddenCharacterMarkers: true,
    seconds: true,
    includeIdentifiers: false,
    clock: 'h23',
    lineEndings: 'lf',
    csvProfile: 'spreadsheet',
    pageSize: 'a4',
    zone: systemZone === null ? 'utc' : 'system',
    namedZone: '',
  };
}

/**
 * "Save a copy"'s fixed options: the file is copied and every other option
 * is off (none applies to one file without records), in UTC. Rust accepts
 * exactly this set for a copy and refuses any other.
 */
export const COPY_OPTIONS: ExportOptionsInput = {
  attachments: 'copyOriginals',
  keepExtensions: false,
  previews: false,
  largerPreviews: false,
  history: false,
  reactions: false,
  events: false,
  revealOneTimeCodes: false,
  revealEarlierContent: false,
  hiddenCharacterMarkers: false,
  seconds: false,
  includeIdentifiers: false,
  clock: 'h23',
  lineEndings: 'lf',
  zone: 'utc',
};

const DRAFT_FALLBACK = 'The export could not be counted. Close the dialog and try again.';
/** `failed` on the preview channel carries a code only. */
const PREVIEW_FAILED: Partial<Record<ErrorCode, string>> = {
  exportDraftExpired: 'This export is no longer prepared. Review the export again.',
  archiveDamaged: 'The archive is damaged and cannot be exported. Verify it on the Archive page.',
  archiveNotFound: 'The archive was not found.',
  archiveLocked: 'The archive is in use by another task. Try again when it has finished.',
  archiveNeedsUpgrade: 'The archive needs an upgrade before it can be exported.',
  archiveNewer: 'The archive was made by a newer version of the app.',
  archiveNewerRules: 'The archive was made by a newer version of the app.',
  invalidInput: 'The conversation or message to export is not in this archive. Review the export again.',
  attachmentUnavailable:
    'This attachment has no stored file that can be saved, or it is no longer shown. Open it again in the viewer and try again.',
  viewerClosed: 'The viewer was closed, so its search results can no longer be exported.',
  searchSuperseded: 'A newer search replaced these results. Export the new results instead.',
  queryTooBroad: 'The search is too broad to export. Narrow it and try again.',
  exportUnavailable: 'This export is not available in this version.',
};

function stored(error: unknown): StoredExportError {
  if (error instanceof IpcError) return { code: error.code, message: error.message };
  return { code: 'internal', message: 'Something went wrong. Please report this as a bug.' };
}

export const useExports = defineStore('exports', () => {
  const open = ref(false);
  const context = ref<ExportOpenContext | null>(null);
  /** Set for "Save a copy"; `null` for an export. */
  const copyTarget = ref<ExportCopyTarget | null>(null);
  const form = ref<ExportForm | null>(null);
  const step = ref<ExportStep>('what');
  const optionsOpen = ref(false);

  const draftId = ref<DraftId | null>(null);
  const count = ref<CountState>({ kind: 'idle' });
  const zoneTable = ref<ZoneTableInputDto | null>(null);
  /** The draft has its zone (sent with a preview or a destination). */
  let zoneAttached = false;
  /** Shown above Review after a stale confirmation or an expired draft. */
  const notice = ref<string | null>(null);

  const confirmCodes = ref(false);
  const confirmEarlier = ref(false);
  const confirmKeepExtensions = ref(false);
  /** "Save a copy" of an attachment of an unsent part (owner Q4). */
  const confirmCopyEarlier = ref(false);
  /** "Save a copy" under a name that can run programs (§6.6). */
  const confirmRiskyName = ref(false);

  const destination = ref<ExportDestinationDto | null>(null);
  const choosing = ref(false);
  const destinationError = ref<StoredExportError | null>(null);
  const confirmedWarnings = ref<ExportWarningDto[]>([]);
  const removeLeftovers = ref(false);
  const replaceConfirmed = ref(false);
  /** `start_export` consumed the draft and refused: Review must count again. */
  const needsReview = ref(false);
  const starting = ref(false);

  const job = ref<ExportJobState | null>(null);
  /** The archive the job reads (the Archive page's "Exporting" state). */
  const jobArchiveId = ref<string | null>(null);
  const result = ref<ExportResult | null>(null);
  /** The job ended while the dialog was closed, and its result is unread. */
  const resultUnseen = ref(false);
  const askCancel = ref(false);
  const showingFolder = ref(false);

  const systemZone = ref<string | null>(null);
  const zones = ref<string[] | null>(null);

  /** Bumped whenever a count is superseded, so late answers are dropped. */
  let countEpoch = 0;
  let pending: PreviewEvent[] = [];
  let zoneAbort: AbortController | null = null;
  let formatsAtStart: ExportFormatInput[] = [];
  let copyAtStart = false;

  const liveJob = computed(() => (job.value && !job.value.ended ? job.value : null));
  const isCopy = computed(() => copyTarget.value !== null);
  /** The navbar chip: a job runs, or ended unseen, while the dialog is closed. */
  const chipVisible = computed(() => !open.value && (liveJob.value !== null || resultUnseen.value));

  // --- Derived form facts ------------------------------------------------------

  const attachments = computed<AttachmentModeInput>(() => {
    const f = form.value;
    if (!f) return 'none';
    return f.attachments ?? defaultAttachments(f);
  });
  const previewsApply = computed(() => form.value?.formats.some((f) => PREVIEW_FORMATS.includes(f)) ?? false);
  const identifiersApply = computed(() => form.value?.formats.some((f) => IDENTIFIER_FORMATS.includes(f)) ?? false);
  const zoneName = computed<string | null>(() => {
    const f = form.value;
    if (!f) return null;
    if (f.zone === 'system') return systemZone.value;
    if (f.zone === 'named') return f.namedZone !== '' ? f.namedZone : null;
    return null;
  });
  const dateRange = computed(() => (form.value ? dateRangeInput(form.value.fromDate, form.value.throughDate) : null));
  const whatValid = computed(() => {
    const f = form.value;
    if (!f) return false;
    if (f.scope === 'dateRange') return dateRange.value !== null;
    if (f.scope === 'conversation') return !!context.value?.chatId;
    if (f.scope === 'searchResults') return !!context.value?.search;
    return true;
  });
  const formatValid = computed(() => (form.value?.formats.length ?? 0) > 0);
  const optionsValid = computed(() => {
    const f = form.value;
    return !!f && (f.zone !== 'named' || (f.namedZone !== '' && (zones.value?.includes(f.namedZone) ?? false)));
  });

  const preview = computed(() => (count.value.kind === 'ready' ? count.value.preview : null));
  const copyFacts = computed(() => (count.value.kind === 'copyReady' ? count.value.copy : null));
  const codesCount = computed(() => preview.value?.messagesWithFlaggedCode ?? 0);
  const earlierCount = computed(() => preview.value?.messagesWithEarlierContent ?? 0);
  const reviewConfirmed = computed(() => {
    const c = copyFacts.value;
    if (isCopy.value) return c !== null && (!c.earlierContent || confirmCopyEarlier.value);
    const f = form.value;
    if (!f || !preview.value) return false;
    if (f.revealOneTimeCodes && codesCount.value > 0 && !confirmCodes.value) return false;
    if (f.revealEarlierContent && earlierCount.value > 0 && !confirmEarlier.value) return false;
    if (f.keepExtensions && attachments.value === 'copyOriginals' && !confirmKeepExtensions.value) return false;
    return true;
  });
  const warningsToConfirm = computed(() =>
    (destination.value?.warnings ?? []).filter((w) => !INFORMATIONAL_WARNINGS.has(w)),
  );
  const canStart = computed(() => {
    const d = destination.value;
    if (!d || !draftId.value || needsReview.value || starting.value || liveJob.value) return false;
    if (!reviewConfirmed.value) return false;
    if (warningsToConfirm.value.some((w) => !confirmedWarnings.value.includes(w))) return false;
    if (d.riskyName && !confirmRiskyName.value) return false;
    return !d.targetExists || replaceConfirmed.value;
  });

  // --- The request ---------------------------------------------------------------

  function scopeInput(): ExportScopeInput | null {
    const f = form.value;
    const ctx = context.value;
    const t = copyTarget.value;
    if (t) return { kind: 'attachment', viewer: t.viewer, attachmentId: t.attachmentId };
    if (!f || !ctx) return null;
    switch (f.scope) {
      case 'conversation':
        return ctx.chatId ? { kind: 'conversation', archiveId: ctx.archiveId, chatId: ctx.chatId } : null;
      case 'searchResults':
        return ctx.search
          ? {
              kind: 'searchResults',
              viewer: ctx.search.viewer,
              searchId: ctx.search.searchId,
              allMatches: ctx.search.capped && f.allMatches,
            }
          : null;
      case 'dateRange': {
        const r = dateRange.value;
        if (!r) return null;
        const chat = f.dateThisConversation && ctx.chatId ? { chatId: ctx.chatId } : {};
        return { kind: 'dateRange', archiveId: ctx.archiveId, fromLocalDate: r.from, untilLocalDate: r.until, ...chat };
      }
      case 'archive':
        return { kind: 'archive', archiveId: ctx.archiveId };
    }
  }

  /**
   * The options exactly as Rust validates them: every field is sent, and
   * an option that does not apply to the chosen formats is sent off (Rust
   * refuses it on, never ignores it).
   */
  function optionsInput(): ExportOptionsInput | null {
    const f = form.value;
    if (!f) return null;
    const copies = attachments.value === 'copyOriginals';
    const previews = previewsApply.value && f.previews;
    return {
      attachments: attachments.value,
      keepExtensions: copies && f.keepExtensions,
      previews,
      largerPreviews: previews && f.largerPreviews,
      history: f.history,
      reactions: f.reactions,
      events: f.events,
      revealOneTimeCodes: f.revealOneTimeCodes,
      revealEarlierContent: f.revealEarlierContent,
      hiddenCharacterMarkers: f.hiddenCharacterMarkers,
      seconds: f.seconds,
      includeIdentifiers: identifiersApply.value && f.includeIdentifiers,
      clock: f.clock,
      lineEndings: f.lineEndings,
      ...(f.formats.includes('csv') ? { csvProfile: f.csvProfile } : {}),
      ...(f.formats.includes('pdf') ? { pageSize: f.pageSize } : {}),
      zone: f.zone,
    };
  }

  function request(zone?: ZoneTableInputDto): PreviewExportRequest | null {
    const scope = scopeInput();
    if (scope?.kind === 'attachment') return { scope, formats: [], options: { ...COPY_OPTIONS } };
    const options = optionsInput();
    const f = form.value;
    if (!scope || !options || !f) return null;
    const formats = AVAILABLE_FORMATS.filter((x) => f.formats.includes(x));
    return { scope, formats, options, ...(zone ? { zone } : {}) };
  }

  // --- Opening and closing ---------------------------------------------------------

  function toast(level: 'info' | 'success' | 'warning' | 'danger', message: string): void {
    useNotifications().push(level, message);
  }

  /**
   * Open the dialog with a fresh form. While an export runs, the dialog
   * shows its progress instead (one job at a time).
   */
  function openExport(ctx: ExportOpenContext): void {
    if (liveJob.value) {
      reopen();
      return;
    }
    begin(ctx, null);
    step.value = 'what';
    open.value = true;
  }

  /**
   * "Save a copy" of one surfaced attachment (§6.6): straight to the
   * attachment's facts and the disclosure, then where to save.
   */
  function openSaveCopy(target: ExportCopyTarget): void {
    if (liveJob.value) {
      reopen();
      return;
    }
    begin({ archiveId: target.archiveId, scope: 'archive' }, target);
    step.value = 'review';
    open.value = true;
    void startCount();
  }

  /** A fresh dialog: nothing carries over from an earlier opening. */
  function begin(ctx: ExportOpenContext, target: ExportCopyTarget | null): void {
    supersede();
    systemZone.value = systemZoneName();
    zones.value = supportedZones();
    context.value = ctx;
    copyTarget.value = target;
    form.value = freshForm(ctx, systemZone.value);
    optionsOpen.value = false;
    result.value = null;
    resultUnseen.value = false;
    job.value = null;
    notice.value = null;
    askCancel.value = false;
    resetReview();
  }

  /** Show the running job's progress, or its unread result (the chip). */
  function reopen(): void {
    if (liveJob.value) step.value = 'progress';
    else if (result.value) step.value = 'result';
    else return;
    resultUnseen.value = false;
    open.value = true;
  }

  /** Close the dialog and let the export run on (§10 step 6). */
  function continueInBackground(): void {
    if (!liveJob.value) return;
    askCancel.value = false;
    open.value = false;
  }

  function resetReview(): void {
    count.value = { kind: 'idle' };
    zoneTable.value = null;
    zoneAttached = false;
    confirmCodes.value = false;
    confirmEarlier.value = false;
    confirmKeepExtensions.value = false;
    confirmCopyEarlier.value = false;
    resetDestination();
  }

  function resetDestination(): void {
    destination.value = null;
    destinationError.value = null;
    confirmedWarnings.value = [];
    removeLeftovers.value = false;
    replaceConfirmed.value = false;
    confirmRiskyName.value = false;
    needsReview.value = false;
  }

  /** Stop whatever count or zone build runs, and drop the draft. */
  function supersede(): void {
    countEpoch += 1;
    pending = [];
    zoneAbort?.abort();
    zoneAbort = null;
    const id = draftId.value;
    draftId.value = null;
    if (id) void ipc.discardExportDraft(id).catch(() => undefined);
  }

  /** Escape, the close button or the backdrop: a running export asks first. */
  function requestClose(): void {
    if (liveJob.value) {
      askCancel.value = true;
      return;
    }
    close();
  }

  function close(): void {
    supersede();
    resetReview();
    open.value = false;
    askCancel.value = false;
    // A result read in the dialog needs no chip.
    if (!liveJob.value) resultUnseen.value = false;
  }

  /**
   * The privacy screen (§10): close the dialog and drop the draft. A
   * running export continues; its end is announced by a toast.
   */
  function closeForPrivacy(): void {
    if (!open.value && draftId.value === null) return;
    supersede();
    resetReview();
    open.value = false;
    askCancel.value = false;
  }

  // --- Steps -----------------------------------------------------------------------

  function goTo(next: ExportStep): void {
    if (step.value === 'review' || step.value === 'destination') {
      if (next !== 'review' && next !== 'destination') {
        supersede();
        resetReview();
      }
    }
    step.value = next;
    // Back from the destination keeps the counted draft (an export's
    // counts, or a copy's facts).
    const counted = count.value.kind === 'ready' || count.value.kind === 'copyReady';
    if (next === 'review' && (draftId.value === null || !counted)) void startCount();
  }

  // --- Counting ----------------------------------------------------------------------

  async function startCount(zone?: ZoneTableInputDto): Promise<void> {
    supersede();
    resetReview();
    const epoch = countEpoch;
    const req = request(zone);
    if (!req) {
      count.value = { kind: 'failed', error: { code: 'invalidInput', message: DRAFT_FALLBACK } };
      return;
    }
    if (zone) {
      zoneTable.value = zone;
      zoneAttached = true;
    }
    count.value = { kind: 'counting', rows: 0 };
    try {
      const r = await ipc.previewExport(req, (e) => onPreviewEvent(epoch, e));
      if (epoch !== countEpoch) {
        void ipc.discardExportDraft(r.draftId).catch(() => undefined);
        return;
      }
      draftId.value = r.draftId;
      const waiting = pending;
      pending = [];
      for (const e of waiting) if (e.draftId === r.draftId) void handlePreview(epoch, e);
    } catch (error) {
      if (epoch === countEpoch) count.value = { kind: 'failed', error: stored(error) };
    }
  }

  function onPreviewEvent(epoch: number, e: PreviewEvent): void {
    if (epoch !== countEpoch) return;
    if (draftId.value === null) {
      pending.push(e);
      return;
    }
    if (e.draftId !== draftId.value) return;
    void handlePreview(epoch, e);
  }

  async function handlePreview(epoch: number, e: PreviewEvent): Promise<void> {
    switch (e.kind) {
      case 'progress':
        if (count.value.kind === 'counting') count.value = { kind: 'counting', rows: e.rows };
        return;
      case 'failed':
        count.value = {
          kind: 'failed',
          error: { code: e.code, message: PREVIEW_FAILED[e.code] ?? DRAFT_FALLBACK },
        };
        return;
      case 'needsZoneTable': {
        // A date range: its local days need the table before counting.
        const built = await buildTable(epoch, e.windowUtcMs);
        if (built === null || epoch !== countEpoch) return;
        if (built.kind !== 'ok') {
          count.value = { kind: 'zoneFailed', reason: built.kind };
          return;
        }
        await startCount(built.table);
        return;
      }
      case 'copyReady':
        count.value = { kind: 'copyReady', copy: e.copy };
        return;
      case 'ready': {
        const window = e.preview.zoneWindowUtcMs;
        if (window && !zoneAttached) {
          const built = await buildTable(epoch, window);
          if (built === null || epoch !== countEpoch) return;
          if (built.kind !== 'ok') {
            count.value = { kind: 'zoneFailed', reason: built.kind };
            return;
          }
          zoneTable.value = built.table;
        }
        count.value = { kind: 'ready', preview: e.preview };
        return;
      }
    }
  }

  /** Build the zone table for `window`; `null` when superseded or stopped. */
  async function buildTable(
    epoch: number,
    window: readonly [number, number],
  ): Promise<Exclude<ZoneTableResult, { kind: 'cancelled' }> | null> {
    const name = zoneName.value;
    if (name === null) return { kind: 'unsupported' };
    count.value = { kind: 'zone' };
    zoneAbort?.abort();
    const abort = new AbortController();
    zoneAbort = abort;
    const r = await buildZoneTable(name, withMargin(window), { signal: abort.signal });
    if (zoneAbort === abort) zoneAbort = null;
    if (r.kind === 'cancelled' || epoch !== countEpoch) return null;
    return r;
  }

  /** "Use UTC instead" (§10 step 4): count again in UTC. */
  function useUtc(): void {
    if (!form.value) return;
    form.value.zone = 'utc';
    void startCount();
  }

  /** Count again (after a stale confirmation or an expired draft). */
  function reviewAgain(message: string | null = null): void {
    notice.value = message;
    step.value = 'review';
    void startCount();
  }

  // --- Destination -------------------------------------------------------------------

  async function chooseDestination(): Promise<void> {
    const id = draftId.value;
    if (!id || !(preview.value || copyFacts.value) || !reviewConfirmed.value || choosing.value) return;
    choosing.value = true;
    destinationError.value = null;
    const zone = zoneTable.value && !zoneAttached ? { zone: zoneTable.value } : {};
    try {
      const d = await ipc.chooseExportDestination({ draftId: id, ...zone });
      zoneAttached = true;
      if (draftId.value !== id) return;
      if (d) {
        resetDestination();
        destination.value = d;
        step.value = 'destination';
      }
    } catch (error) {
      const e = stored(error);
      if (e.code !== 'invalidInput' && e.code !== 'exportDraftExpired') zoneAttached = true;
      if (draftId.value !== id) return;
      if (e.code === 'exportDraftExpired' || e.code === 'exportConfirmationStale') {
        reviewAgain(e.message);
        return;
      }
      destinationError.value = e;
    } finally {
      choosing.value = false;
    }
  }

  function setWarningConfirmed(w: ExportWarningDto, on: boolean): void {
    const rest = confirmedWarnings.value.filter((x) => x !== w);
    confirmedWarnings.value = on ? [...rest, w] : rest;
  }

  // --- The job ------------------------------------------------------------------------

  function confirmations(): ExportConfirmationsInput {
    const d = destination.value;
    const c = copyFacts.value;
    if (isCopy.value) {
      return {
        warnings: warningsToConfirm.value.filter((w) => confirmedWarnings.value.includes(w)),
        replace: (d?.targetExists ?? false) && replaceConfirmed.value,
        // The one attachment, confirmed (owner Q4).
        reveal: c?.earlierContent ? { earlierContent: 1 } : null,
        keepRiskyExtension: (d?.riskyName ?? false) && confirmRiskyName.value,
        removeLeftovers: (d?.leftovers ?? 0) > 0 && removeLeftovers.value,
      };
    }
    const f = form.value;
    const reveal =
      f && (f.revealOneTimeCodes || f.revealEarlierContent)
        ? {
            ...(f.revealOneTimeCodes ? { oneTimeCodes: codesCount.value } : {}),
            ...(f.revealEarlierContent ? { earlierContent: earlierCount.value } : {}),
          }
        : null;
    return {
      warnings: warningsToConfirm.value.filter((w) => confirmedWarnings.value.includes(w)),
      replace: (destination.value?.targetExists ?? false) && replaceConfirmed.value,
      reveal,
      keepRiskyExtension: optionsInput()?.keepExtensions ?? false,
      removeLeftovers: (destination.value?.leftovers ?? 0) > 0 && removeLeftovers.value,
    };
  }

  async function start(): Promise<void> {
    const id = draftId.value;
    const d = destination.value;
    if (!canStart.value || !id || !d || !form.value) return;
    starting.value = true;
    const c = confirmations();
    copyAtStart = isCopy.value;
    formatsAtStart = copyAtStart ? [] : AVAILABLE_FORMATS.filter((x) => form.value?.formats.includes(x));
    jobArchiveId.value = context.value?.archiveId ?? null;
    // `start_export` consumes the draft, whatever its answer.
    draftId.value = null;
    countEpoch += 1;
    job.value = {
      kind: 'export',
      jobId: null,
      phase: null,
      done: 0,
      total: null,
      cancelling: false,
      ended: false,
    };
    // The reactive object, so the dialog sees every change made to it.
    const current = job.value;
    result.value = null;
    try {
      const r = await ipc.startExport({ draftId: id, destinationToken: d.destinationToken, confirmations: c }, (e) =>
        onJobEvent(current, e),
      );
      if (current.jobId === null && !current.ended) adopt(current, r.jobId);
      if (!current.ended) step.value = 'progress';
    } catch (error) {
      if (job.value === current) job.value = null;
      const e = stored(error);
      if (e.code === 'exportConfirmationStale' || e.code === 'exportDraftExpired') {
        reviewAgain(e.message);
      } else {
        needsReview.value = true;
        destinationError.value = e;
      }
    } finally {
      starting.value = false;
    }
  }

  function adopt(current: ExportJobState, jobId: string): void {
    if (current.jobId !== null) return;
    current.jobId = jobId;
    if (current.cancelling) void sendCancel(jobId);
  }

  async function sendCancel(jobId: string): Promise<void> {
    try {
      await ipc.cancelJob(jobId);
    } catch (error) {
      toast('danger', stored(error).message);
    }
  }

  async function cancelExport(): Promise<void> {
    askCancel.value = false;
    const current = liveJob.value;
    if (!current || current.cancelling) return;
    current.cancelling = true;
    if (current.jobId !== null) await sendCancel(current.jobId);
  }

  function onJobEvent(current: ExportJobState, event: JobEvent): void {
    if (current.ended || job.value !== current) return;
    if (current.jobId !== null && current.jobId !== event.jobId) return;
    adopt(current, event.jobId);
    switch (event.kind) {
      case 'progress':
        current.phase = event.phase;
        current.done = event.done;
        current.total = event.total;
        step.value = 'progress';
        return;
      case 'exportFinished':
        result.value = {
          kind: 'finished',
          jobId: event.jobId,
          outcome: event.outcome,
          summary: event.summary,
          formats: formatsAtStart,
          copy: copyAtStart,
        };
        if (!open.value) {
          toast(
            event.outcome === 'completed' ? 'success' : 'warning',
            copyAtStart ? COPY_SAVED : OUTCOME_TEXT[event.outcome],
          );
        }
        break;
      case 'failed':
        result.value = {
          kind: 'failed',
          error: { code: event.error.code, message: event.error.message },
          stagingRemoved: event.stagingRemoved,
        };
        if (!open.value) toast('danger', `${event.error.message} ${failedNote(event.stagingRemoved)}`);
        break;
      case 'cancelled':
        result.value = { kind: 'cancelled', stagingRemoved: event.stagingRemoved };
        if (!open.value) toast(event.stagingRemoved ? 'info' : 'warning', cancelledSentence(event.stagingRemoved));
        break;
      // Other jobs' events never arrive on an export's channel.
      default:
        return;
    }
    current.ended = true;
    askCancel.value = false;
    step.value = 'result';
    if (!open.value) resultUnseen.value = true;
  }

  async function showInFolder(): Promise<void> {
    const r = result.value;
    if (r?.kind !== 'finished' || showingFolder.value) return;
    showingFolder.value = true;
    try {
      await ipc.showExportInFolder(r.jobId);
    } catch (error) {
      toast('danger', stored(error).message);
    } finally {
      showingFolder.value = false;
    }
  }

  return {
    open,
    context,
    copyTarget,
    isCopy,
    copyFacts,
    confirmCopyEarlier,
    confirmRiskyName,
    jobArchiveId,
    resultUnseen,
    chipVisible,
    openSaveCopy,
    reopen,
    continueInBackground,
    form,
    step,
    optionsOpen,
    draftId,
    count,
    zoneTable,
    notice,
    confirmCodes,
    confirmEarlier,
    confirmKeepExtensions,
    destination,
    choosing,
    destinationError,
    confirmedWarnings,
    removeLeftovers,
    replaceConfirmed,
    needsReview,
    starting,
    job,
    result,
    askCancel,
    systemZone,
    zones,
    liveJob,
    attachments,
    previewsApply,
    identifiersApply,
    zoneName,
    dateRange,
    whatValid,
    formatValid,
    optionsValid,
    preview,
    codesCount,
    earlierCount,
    reviewConfirmed,
    warningsToConfirm,
    canStart,
    request,
    confirmations,
    openExport,
    requestClose,
    close,
    closeForPrivacy,
    goTo,
    startCount,
    useUtc,
    reviewAgain,
    chooseDestination,
    setWarningConfirmed,
    start,
    cancelExport,
    showInFolder,
  };
});
