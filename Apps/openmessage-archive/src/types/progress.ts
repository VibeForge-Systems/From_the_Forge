// Hand-written (not generated): what JobProgress.vue needs from a job.
// The backup jobs' state (stores/backups.ts) and the archive jobs' state
// (stores/archives.ts) both fit.
import type { JobKind } from './generated/JobKind';
import type { JobPhase } from './generated/JobPhase';
import type { VerifyDepthDto } from './generated/VerifyDepthDto';

export interface ProgressJob {
  kind: JobKind;
  phase: JobPhase | null;
  done: number;
  total: number | null;
  cancelling: boolean;
  /** A full verify counts `verifyingFiles` in bytes, a quick one in files. */
  depth?: VerifyDepthDto | null;
}
