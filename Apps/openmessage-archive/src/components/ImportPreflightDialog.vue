<script setup lang="ts">
// The gate (M4 §8.2 step 7, §16). Each fact is its own sentence, never one
// "OK". Nothing has been written to the archive yet: Import writes, and
// Cancel (or two minutes without an answer) writes nothing.
import { computed, ref, watch } from 'vue';
import ModalDialog from '@/components/ModalDialog.vue';
import { useArchives } from '@/stores/archives';
import {
  FIRST_IMPORT,
  NEW_LINEAGE_LABEL,
  NO_IDENTITY_PROOF,
  keySentences,
  lineageEvidence,
  lineageLabel,
  orderSentences,
  privacySentences,
  qualifierSentences,
  recentlyDeletedSentences,
  resumeSentences,
  spaceSentences,
} from '@/services/importText';
import type { LineageInput } from '@/types';

const archives = useArchives();
const view = computed(() => archives.gate);
const p = computed(() => view.value?.preflight ?? null);

/** `'new'` or an import set's number. */
const choice = ref<'new' | number | null>(null);

watch(
  () => view.value?.jobId ?? null,
  () => {
    const s = p.value?.suggestedLineage;
    choice.value = !s ? null : s.kind === 'new' ? 'new' : s.lineageId;
  },
  { immediate: true },
);

const lineage = computed<LineageInput | null>(() => {
  if (choice.value === null) return null;
  return choice.value === 'new' ? { kind: 'new' } : { kind: 'existing', lineageId: choice.value };
});

const sections = computed(() => {
  const pf = p.value;
  if (!pf) return [];
  return [
    { id: 'space', title: 'Space', items: spaceSentences(pf) },
    { id: 'keys', title: 'Messages', items: keySentences(pf) },
    { id: 'rd', title: 'Recently Deleted', items: recentlyDeletedSentences(pf) },
    { id: 'order', title: 'Backup date', items: orderSentences(pf) },
    { id: 'qualifiers', title: 'Notes', items: qualifierSentences(pf) },
    { id: 'privacy', title: 'Privacy', items: privacySentences(pf) },
  ].filter((s) => s.items.length > 0);
});

function proceed(): void {
  if (lineage.value) void archives.answerGate(true, lineage.value);
}

function decline(): void {
  void archives.answerGate(false, null);
}
</script>

<template>
  <ModalDialog :open="view !== null" title="Confirm the import" @close="decline">
    <div v-if="p" class="oma-preflight">
      <p class="small text-body-secondary">
        Nothing has been written to the archive yet. If you do not answer within two minutes, the
        import is not started.
      </p>

      <section class="mb-3" aria-labelledby="pf-resume">
        <h3 id="pf-resume" class="h6">Continue or start over</h3>
        <p v-for="s in resumeSentences(p)" :key="s" class="small mb-1 oma-pf-sentence">{{ s }}</p>
      </section>

      <fieldset class="mb-3">
        <legend class="h6">Which phone is this backup from?</legend>
        <p class="small mb-2">{{ p.device === 'firstImport' ? FIRST_IMPORT : NO_IDENTITY_PROOF }}</p>
        <div v-for="l in p.lineages" :key="l.lineageId" class="form-check mb-2">
          <input
            :id="`pf-lineage-${l.lineageId}`"
            v-model="choice"
            class="form-check-input"
            type="radio"
            name="pf-lineage"
            :value="l.lineageId"
          />
          <label class="form-check-label small" :for="`pf-lineage-${l.lineageId}`">
            {{ lineageLabel(l) }}
            <span v-if="p.suggestedLineage.kind === 'existing' && p.suggestedLineage.lineageId === l.lineageId" class="badge text-bg-info ms-1">Suggested</span>
            <span v-for="e in lineageEvidence(l)" :key="e" class="d-block text-body-secondary">{{ e }}</span>
          </label>
        </div>
        <div class="form-check">
          <input id="pf-lineage-new" v-model="choice" class="form-check-input" type="radio" name="pf-lineage" value="new" />
          <label class="form-check-label small" for="pf-lineage-new">
            {{ NEW_LINEAGE_LABEL }}
            <span v-if="p.suggestedLineage.kind === 'new'" class="badge text-bg-info ms-1">Suggested</span>
          </label>
        </div>
      </fieldset>

      <section v-for="s in sections" :key="s.id" class="mb-3" :aria-labelledby="`pf-${s.id}`">
        <h3 :id="`pf-${s.id}`" class="h6">{{ s.title }}</h3>
        <p v-for="t in s.items" :key="t" class="small mb-1 oma-pf-sentence">{{ t }}</p>
      </section>

      <p v-if="archives.gateError" class="text-danger small mb-0" role="alert">{{ archives.gateError }}</p>
    </div>
    <template #footer>
      <button type="button" class="btn btn-outline-secondary" :disabled="archives.confirming" @click="decline">
        Cancel
      </button>
      <button type="button" class="btn btn-primary" :disabled="archives.confirming || lineage === null" @click="proceed">
        Import
      </button>
    </template>
  </ModalDialog>
</template>
