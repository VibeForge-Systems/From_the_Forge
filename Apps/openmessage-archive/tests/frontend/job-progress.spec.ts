import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import JobProgress from '@/components/JobProgress.vue';
import type { JobPhase } from '@/types';
import type { ProgressJob } from '@/types/progress';

// Every phase has a label: `Record<JobPhase, string>` in JobProgress.vue
// makes `npm run typecheck` fail otherwise. These pin the archive phases'
// wording and units.
function label(job: Partial<ProgressJob>): string {
  const w = mount(JobProgress, {
    props: { job: { kind: 'import', phase: null, done: 0, total: null, cancelling: false, ...job } },
  });
  return w.find('.oma-job-label').text();
}

describe('JobProgress for the archive jobs', () => {
  it('labels the import phases, with counts only where a total is reported', () => {
    const cases: [JobPhase, string][] = [
      ['scanningKeys', 'Checking message identifiers'],
      ['scanningRecentlyDeleted', 'Checking Recently Deleted'],
      ['estimatingAttachments', 'Estimating the space needed'],
      ['awaitingConfirmation', 'Waiting for your confirmation'],
      ['preparingArchive', 'Preparing the archive'],
      ['finalizing', 'Finishing the import'],
      ['optimizingIndex', 'Optimising the search index'],
      ['checkpointing', 'Saving the archive'],
    ];
    for (const [phase, text] of cases) expect(label({ phase, done: 3, total: null })).toBe(text);
    expect(label({ phase: 'importingMessages', done: 5, total: 40 })).toBe('Importing messages (5 of 40)');
    // P6 reports attachment rows, not bytes.
    expect(label({ phase: 'copyingAttachments', done: 2, total: 3 })).toBe('Copying attachments (2 of 3)');
    expect(label({ phase: 'stagingMessagesDb', done: 1048576, total: 2097152 })).toBe(
      'Copying the Messages database (1 MB of 2 MB)',
    );
  });

  it('counts a quick verify in files and a full one in bytes', () => {
    expect(label({ kind: 'verify', depth: 'quick', phase: 'verifyingFiles', done: 4, total: 10 })).toBe(
      'Checking attachment files (4 of 10)',
    );
    expect(label({ kind: 'verify', depth: 'full', phase: 'verifyingFiles', done: 3145728, total: 10485760 })).toBe(
      'Checking attachment files (3 MB of 10 MB)',
    );
    expect(label({ kind: 'verify', depth: 'full', phase: 'verifyingDatabase', done: 0, total: null })).toBe(
      'Checking the archive database',
    );
  });

  it('labels the export phases, counting rows only while messages are written', () => {
    const cases: [JobPhase, string][] = [
      ['preparingExport', 'Counting what to export'],
      ['finishingExport', 'Finishing the export'],
      ['removingPartial', 'Removing the unfinished export'],
    ];
    for (const [phase, text] of cases) {
      expect(label({ kind: 'export', phase, done: 0, total: null })).toBe(text);
    }
    expect(label({ kind: 'export', phase: 'writingMessages', done: 120, total: 4000 })).toBe(
      'Writing messages (120 of 4,000)',
    );
  });

  it('names the job in the progress bar', () => {
    for (const [kind, aria] of [
      ['import', 'Import progress'],
      ['verify', 'Verification progress'],
      ['validate', 'Validation progress'],
      ['extractionCheck', 'Extraction check progress'],
      ['export', 'Export progress'],
    ] as const) {
      const w = mount(JobProgress, { props: { job: { kind, phase: null, done: 0, total: null, cancelling: false } } });
      expect(w.find('.progress').attributes('aria-label')).toBe(aria);
    }
  });
});
