<script setup lang="ts">
import { computed } from 'vue';
import type { BackupState } from '@/types';

const props = defineProps<{
  state: BackupState;
  /** The last report's verdict, when there is one. */
  lastReportImportable?: boolean | null;
}>();

const STATE: Record<BackupState, { text: string; cls: string }> = {
  ready: { text: 'Ready to check', cls: 'text-bg-success' },
  needsPassword: { text: 'Encrypted', cls: 'text-bg-info' },
  incomplete: { text: 'Incomplete', cls: 'text-bg-warning' },
  unsupported: { text: 'Unsupported', cls: 'text-bg-secondary' },
  unreadable: { text: 'Unreadable', cls: 'text-bg-danger' },
  notInspected: { text: 'Not inspected', cls: 'text-bg-light border' },
};

// A report speaks only for a backup that can still be read. When the backup
// has since become incomplete, unsupported or unreadable, that state wins.
const REPORT_APPLIES: ReadonlySet<BackupState> = new Set(['ready', 'needsPassword']);

const presentation = computed(() => {
  if (!REPORT_APPLIES.has(props.state)) return STATE[props.state];
  if (props.lastReportImportable === true) return { text: 'Checked', cls: 'text-bg-success' };
  if (props.lastReportImportable === false) return { text: 'Problems found', cls: 'text-bg-danger' };
  return STATE[props.state];
});
</script>

<template>
  <span class="badge" :class="presentation.cls">{{ presentation.text }}</span>
</template>
