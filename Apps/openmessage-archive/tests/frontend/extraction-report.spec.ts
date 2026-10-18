import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import ExtractionReport from '@/components/ExtractionReport.vue';
import {
  FAILURE_LABEL,
  NOT_RECORDED,
  SOURCE_CHANGED_CAVEAT,
  qualifierSentence,
} from '@/services/extractionText';
import type { ExtractionReportDto, FailureKindDto, QualifierDto } from '@/types';
import { extractionReport } from './helpers';

function render(overrides: Partial<ExtractionReportDto> = {}) {
  setActivePinia(createPinia());
  return mount(ExtractionReport, { props: { report: extractionReport(overrides) } });
}

/** The value cell of the row whose header is exactly `label`. */
function cell(w: ReturnType<typeof render>, label: string): string | undefined {
  const row = w.findAll('tr').find((tr) => tr.find('th').text() === label);
  return row?.find('td').text();
}

const ALL_QUALIFIERS: QualifierDto[] = [
  { kind: 'iosVersionUntested' },
  { kind: 'unknownTables', count: 1 },
  { kind: 'unknownColumns', count: 3 },
  { kind: 'wrongCaseColumns', count: 2 },
  { kind: 'walSkipped' },
  { kind: 'walStateUnknown' },
  { kind: 'walFramesInvalid' },
  { kind: 'sourceChanged' },
  { kind: 'aggregatesIncomplete' },
  { kind: 'valuesWithheld' },
  { kind: 'nonUtf8Encoding' },
  { kind: 'sizeMismatch' },
  { kind: 'other' },
];

describe('ExtractionReport', () => {
  it('renders every must-ship section of a readable report', () => {
    const w = render();
    const text = w.text();
    expect(text).toContain('Messages can be read from this backup, with these limits:');
    expect(w.find('.oma-extraction-counts').exists()).toBe(true);
    expect(w.find('.oma-extraction-dates').exists()).toBe(true);
    expect(w.find('.oma-extraction-attachments').exists()).toBe(true);
    expect(w.find('.oma-extraction-database').exists()).toBe(true);
    expect(w.find('.oma-extraction-problems').exists()).toBe(true);
    expect(cell(w, 'Message rows')).toBe('40');
    expect(cell(w, 'Reactions, stickers and votes (associations)')).toBe('5');
    expect(cell(w, 'Non-message rows (item_type ≠ 0)')).toBe('2');
    expect(w.text()).not.toContain('Group actions');
    expect(cell(w, 'In Recently Deleted')).toBe('1');
    expect(cell(w, 'Other rows')).toBe('32');
    expect(cell(w, 'Chat rows')).toBe('4');
    expect(cell(w, 'Addresses (handle rows: address and service pairs)')).toBe('6');
    expect(cell(w, 'Attachment rows')).toBe('3');
    expect(cell(w, 'Messages in no chat')).toBe('1');
    expect(cell(w, 'Earliest message')).toContain('2020');
    expect(cell(w, 'Latest message')).toContain('2026');
    expect(cell(w, 'Left out: before 2007')).toBe('0');
    expect(cell(w, 'Left out: unusable dates')).toBe('1');
    expect(cell(w, 'Opened')).toBe('Read-only');
    expect(cell(w, 'Integrity')).toBe("Passed SQLite's integrity check");
    expect(cell(w, 'Expected columns not present')).toBe('2');
    expect(w.findAll('.oma-missing-columns li').map((li) => li.text())).toEqual([
      'message.dateRetracted',
      'message.scheduleType',
    ]);
    expect(text).toContain('Detected features (derived from the columns present, not from an iOS version)');
    expect(cell(w, FAILURE_LABEL.tsTruncated)).toBe('1');
    expect(text).toContain('The temporary copy of the Messages database was deleted straight after reading.');
    expect(text).toContain('All temporary files were removed at the end of the check.');
    expect(text).toContain('Nothing from this check was saved. Importing into an archive is a separate step.');
  });

  it('says just "can be read" only when no qualifier applies', () => {
    const w = render({ outcome: 'readable', qualifiers: [] });
    expect(w.find('.oma-extraction-outcome').text()).toBe('Messages can be read from this backup.');
    expect(w.find('.oma-qualifiers').exists()).toBe(false);
  });

  it('renders null counters as "not recorded", never as 0', () => {
    const base = extractionReport();
    const w = render({
      counts: { ...base.counts!, associationRows: null, recentlyDeletedRows: null, attachmentRows: null },
      dates: { ...base.dates!, excludedScheduled: null },
    });
    for (const label of [
      'Reactions, stickers and votes (associations)',
      'In Recently Deleted',
      'Attachment rows',
      'Left out: scheduled',
    ]) {
      expect(cell(w, label)).toBe(NOT_RECORDED);
    }
    expect(NOT_RECORDED).toBe('not recorded by this database');
  });

  it('marks clamped counters', () => {
    const w = render({ saturated: true });
    expect(cell(w, 'Message rows')).toBe('40 or more');
  });

  it('gives each qualifier its own sentence', () => {
    const w = render({ qualifiers: ALL_QUALIFIERS });
    const items = w.findAll('.oma-qualifiers li').map((li) => li.text());
    expect(items).toHaveLength(ALL_QUALIFIERS.length);
    for (const q of ALL_QUALIFIERS) expect(items).toContain(qualifierSentence(q));
    expect(new Set(items).size).toBe(items.length);
    expect(items).toContain('This iOS version has not been tested.');
    expect(items).toContain('3 columns this version does not understand.');
    expect(items).toContain('1 table this version does not understand.');
    expect(items).toContain(
      'The backup changed during the check; these counts may come from an inconsistent copy.',
    );
    expect(items).toContain('Some values were too large or unreadable and were not counted.');
    expect(items).toContain('Some summary counts could not be computed.');
    for (const kind of ['walSkipped', 'walStateUnknown', 'walFramesInvalid'] as const) {
      expect(qualifierSentence({ kind })).toContain('The newest messages may be missing.');
    }
    // The database section repeats the WAL sentence whenever it applies.
    expect(w.find('.oma-extraction-database').text()).toContain('The newest messages may be missing.');
    expect(render().find('.oma-extraction-database').text()).not.toContain('newest messages');
  });

  it('says "not in this backup" only for the sourced key', () => {
    const w = render();
    const attachments = w.find('.oma-extraction-attachments');
    const rows = attachments.findAll('tr').map((tr) => tr.find('th').text());
    const matching = rows.filter((r) => r.toLowerCase().includes('not in this backup'));
    expect(matching).toEqual(['Not in this backup']);
    expect(cell(w, 'Not in this backup')).toBe('1');
    expect(cell(w, 'Not found at an assumed location')).toBe('0');
    expect(cell(w, 'Not looked up')).toBe('0');
    expect(cell(w, 'File missing')).toBe('0');
    expect(cell(w, 'The backup has no key for this file')).toBe('0');
  });

  it('breaks "Found" down into every probe outcome, so the rows add up', () => {
    const base = extractionReport();
    const a = {
      ...base.attachments!,
      found: 36,
      filePresent: 1,
      fileMissing: 2,
      fileSizeMismatch: 3,
      fileNotReadable: 4,
      noKey: 5,
      keyDoesNotUnwrap: 6,
      classKeyUnavailable: 7,
      probeOther: 8,
      notProbed: 0,
      ambiguous: 9,
      invalidEntry: 10,
    };
    const w = render({ attachments: a });
    const rows = w.findAll('.oma-extraction-attachments tr');
    const found = rows.findIndex((tr) => tr.find('th').text() === 'Found in the file index');
    const sub: number[] = [];
    for (const tr of rows.slice(found + 1)) {
      if (!tr.find('th').classes().includes('ps-3')) break;
      sub.push(Number(tr.find('td').text()));
    }
    expect(sub).toHaveLength(9);
    expect(sub.reduce((x, y) => x + y, 0)).toBe(a.found);
    expect(cell(w, "File size differs from the backup's record")).toBe('3');
    expect(cell(w, 'The key class this file needs is unavailable')).toBe('7');
    expect(cell(w, 'Listed more than once in the file index')).toBe('9');
    expect(cell(w, 'Invalid file-index entry')).toBe('10');
  });

  it('says the WAL header shows WAL mode, not that a -wal was expected', () => {
    const s = qualifierSentence({ kind: 'walStateUnknown' });
    expect(s).not.toContain('expects');
    expect(s).toContain('WAL mode');
    expect(s).toContain('whether changes were pending is unknown');
  });

  it('warns that the backup changed under every outcome that ran', () => {
    const notRead = { counts: null, dates: null, attachments: null, problems: null, qualifiers: [] };
    for (const outcome of ['damaged', 'unsupported', 'incomplete'] as const) {
      const w = render({
        ...notRead,
        outcome,
        damageReason: outcome === 'damaged' ? { kind: 'integrityProblems' } : null,
        sourceChanged: true,
      });
      expect(w.find('.oma-source-changed').text()).toBe(SOURCE_CHANGED_CAVEAT);
      expect(render({ ...notRead, outcome, sourceChanged: false }).find('.oma-source-changed').exists()).toBe(
        false,
      );
    }
    // A readable result says it once, as its qualifier.
    const w = render({ sourceChanged: true, qualifiers: [{ kind: 'sourceChanged' }] });
    expect(w.find('.oma-source-changed').exists()).toBe(false);
    expect(w.find('.oma-qualifiers').text()).toContain('The backup changed during the check');
    // A skipped check read nothing and changed nothing.
    expect(
      render({ ...notRead, outcome: 'skippedNotImportable', sourceChanged: true, schema: null, database: null })
        .find('.oma-source-changed')
        .exists(),
    ).toBe(false);
  });

  it('shows a copy whose size differs from the backup record, for any outcome', () => {
    const database = { ...extractionReport().database!, sizeMatches: false };
    const w = render({
      outcome: 'damaged',
      damageReason: { kind: 'integrityProblems' },
      qualifiers: [],
      counts: null,
      dates: null,
      attachments: null,
      problems: null,
      database,
    });
    expect(cell(w, 'Size of the copy')).toBe('Differs from the size the backup records');
    expect(cell(render(), 'Size of the copy')).toBeUndefined();
  });

  it('never calls a key that does not unwrap "damaged"', () => {
    const w = render();
    const attachments = w.find('.oma-extraction-attachments').text();
    expect(cell(w, "This file's key could not be unwrapped")).toBe('1');
    expect(attachments.toLowerCase()).not.toContain('damaged');
    expect(FAILURE_LABEL.attachmentKeyProblem.toLowerCase()).not.toContain('damaged');
  });

  it('has a distinct label for every failure kind, and renders each', () => {
    const kinds = Object.keys(FAILURE_LABEL) as FailureKindDto[];
    const labels = Object.values(FAILURE_LABEL);
    expect(kinds.length).toBe(46);
    for (const l of labels) expect(l.length).toBeGreaterThan(0);
    expect(new Set(labels).size).toBe(labels.length);
    const w = render({ problems: kinds.map((kind, i) => ({ kind, count: i + 1 })) });
    for (const [i, kind] of kinds.entries()) {
      expect(cell(w, FAILURE_LABEL[kind])).toBe(String(i + 1));
    }
  });

  it('says when no problem was counted', () => {
    expect(render({ problems: [] }).find('.oma-extraction-problems').text()).toContain(
      'No problems were counted.',
    );
  });

  it('explains an unsupported layout in our own words', () => {
    const w = render({
      outcome: 'unsupported',
      qualifiers: [],
      unsupportedReasons: [
        { kind: 'missingTable', table: 'chatHandleJoin' },
        { kind: 'notOrdinaryTable', table: 'message' },
        { kind: 'missingRequiredColumn', column: 'message.isFromMe' },
        { kind: 'rowidShape', table: 'attachment' },
        { kind: 'virtualTablePresent' },
        { kind: 'schemaExceedsLimits' },
      ],
      counts: null,
      dates: null,
      attachments: null,
      problems: null,
    });
    const text = w.find('.oma-extraction-outcome').text();
    expect(text).toContain('This Messages database has a layout this version does not support:');
    expect(text).toContain('The chat_handle_join table is missing.');
    expect(text).toContain('A required column is missing: message.isFromMe.');
    expect(text).toContain('It contains a virtual table, which this version does not read.');
    expect(w.findAll('.oma-unsupported li')).toHaveLength(6);
    expect(w.find('.oma-extraction-counts').exists()).toBe(false);
    expect(w.text()).toContain('Messages, chats and attachments were not read.');
    expect(w.find('.oma-extraction-database').exists()).toBe(true);
  });

  it('reports a damaged database', () => {
    const w = render({
      outcome: 'damaged',
      qualifiers: [],
      damageReason: { kind: 'integrityProblems' },
      counts: null,
      dates: null,
      attachments: null,
      problems: null,
      database: { ...extractionReport().database!, integrity: 'problems' },
    });
    expect(w.find('.oma-extraction-outcome').text()).toBe(
      'This Messages database is damaged (its integrity check failed). Nothing was read from it.',
    );
    expect(cell(w, 'Integrity')).toBe("Failed SQLite's integrity check");
    expect(w.find('.oma-extraction-outcome').classes()).toContain('alert-danger');
  });

  it('reports an integrity check that did not finish', () => {
    const w = render({
      outcome: 'incomplete',
      qualifiers: [],
      incompleteReason: 'integrityCheckOverBudget',
      counts: null,
      dates: null,
      attachments: null,
      problems: null,
    });
    expect(w.find('.oma-extraction-outcome').text()).toBe(
      'The integrity check did not finish within its limit.',
    );
  });

  it('reports a check skipped because validation found errors', () => {
    const w = render({
      outcome: 'skippedNotImportable',
      qualifiers: [],
      schema: null,
      database: null,
      counts: null,
      dates: null,
      attachments: null,
      problems: null,
      checkedAt: null,
    });
    expect(w.find('.oma-extraction-outcome').text()).toBe('Not run: validation found errors.');
    expect(w.find('.oma-copy-status').exists()).toBe(false);
    expect(w.find('.oma-extraction-database').exists()).toBe(false);
    expect(w.text()).toContain('Nothing from this check was saved.');
  });

  it('says when the copy could not be deleted straight away', () => {
    const w = render({ plaintextRemovedAfterRowPass: false, stagingRemoved: false });
    const text = w.find('.oma-copy-status').text();
    expect(text).toContain('could not be deleted straight after reading');
    expect(text).toContain('They will be removed the next time OpenMessage Archive starts.');
  });

  it('renders text only: markup in a value is never interpreted', () => {
    // Kinds are closed literals, so the only free-form values are dates; a
    // hostile value would be shown as text, not parsed.
    const w = render({ checkedAt: '<img src=x onerror=alert(1)>' });
    expect(w.find('img').exists()).toBe(false);
  });
});
