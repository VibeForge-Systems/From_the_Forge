<script setup lang="ts">
// The newest verify run (M4 §11, §16): one sentence per finding kind, with
// counts. The detailed report page is deferred (§21.2).
import { computed } from 'vue';
import type { VerifySummaryDto } from '@/types';
import { NOT_RECORDED, VERIFY_DEPTH, VERIFY_OUTCOME, formatMs, verifyScope, verifySentences } from '@/services/importText';

const props = defineProps<{ summary: VerifySummaryDto }>();

const v = computed(() => props.summary);
const findings = computed(() => (v.value.counts ? verifySentences(v.value.counts) : []));
const outcome = computed(() =>
  v.value.outcome === null
    ? 'The check did not finish: the app may have closed while it ran.'
    : VERIFY_OUTCOME[v.value.outcome],
);
const alertClass = computed(() => {
  switch (v.value.outcome) {
    case 'ok':
      return 'alert-success';
    case 'problems':
      return 'alert-danger';
    default:
      return 'alert-warning';
  }
});
</script>

<template>
  <section class="card mb-3 oma-verify-summary" aria-labelledby="verify-summary-title">
    <div class="card-header">
      <h2 id="verify-summary-title" class="h6 mb-0">
        Last check: {{ VERIFY_DEPTH[summary.depth] }}, {{ formatMs(summary.startedAtMs) }}
      </h2>
    </div>
    <div class="card-body">
      <div class="alert small mb-2" :class="alertClass" role="status">{{ outcome }}</div>
      <p v-if="!summary.counts" class="small mb-0">The results of this check: {{ NOT_RECORDED }}.</p>
      <template v-else>
        <ul v-if="findings.length" class="small mb-2 oma-verify-findings">
          <li v-for="f in findings" :key="f">{{ f }}</li>
        </ul>
        <p class="small text-body-secondary mb-0">{{ verifyScope(summary.counts, summary.depth) }}</p>
      </template>
    </div>
  </section>
</template>
