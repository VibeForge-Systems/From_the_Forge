<script setup lang="ts">
// One conversation in the list (M5 §8.4): its label, service, "N items
// recorded", the last dated date, the Rust-built preview (masked,
// retraction withheld, sentences for events) when the `listPreviews`
// setting is on, and badges. Content goes through SafeText only.
import { computed } from 'vue';
import type { ConversationRowDto } from '@/types';
import ConversationLabel from './ConversationLabel.vue';
import SafeText from './SafeText.vue';
import { formatInstant } from '@/services/time';
import { previewSentence } from '@/services/messageView';
import { LIST_TEXT, SERVICE_TEXT, fill } from '@/services/viewerText';
import { useSettings } from '@/stores/settings';

const props = defineProps<{ row: ConversationRowDto }>();
const settings = useSettings();

const items = computed(() => {
  const s = props.row.stats;
  if (!s) return LIST_TEXT.noStats;
  return fill(LIST_TEXT.itemsRecorded, { n: s.itemsRecorded });
});

const lastDated = computed(() =>
  props.row.lastDatedMs === null
    ? LIST_TEXT.noDated
    : fill(LIST_TEXT.lastDated, { date: formatInstant(props.row.lastDatedMs, settings.viewer, 'date') }),
);

const undated = computed(() => {
  const u = props.row.undated;
  if (u.n === 0) return null;
  return fill(u.capped ? LIST_TEXT.undatedCapped : LIST_TEXT.undated, { n: u.n });
});

const preview = computed(() => (settings.viewer.listPreviews ? props.row.preview : null));

</script>

<template>
  <div class="oma-conv-row">
    <div class="oma-conv-title">
      <ConversationLabel :label="row.label" :chat-id="row.id" quiet />
    </div>
    <div class="oma-conv-meta">
      <span v-if="row.service">{{ SERVICE_TEXT[row.service] }}</span>
      <span v-if="row.group">{{ LIST_TEXT.group }}</span>
      <span>{{ items }}<template v-if="row.statsStale"> ({{ LIST_TEXT.statsStale }})</template></span>
      <span>{{ lastDated }}</span>
      <span v-if="undated">{{ undated }}</span>
      <span v-if="row.empty" class="oma-missing">{{ LIST_TEXT.empty }}</span>
    </div>
    <div v-if="preview" class="oma-conv-preview oma-contained">
      <span v-if="preview.direction === 'you'" class="oma-preview-who">{{ LIST_TEXT.previewYou }}{{ ' ' }}</span
      ><SafeText v-if="preview.body.kind === 'text'" :pieces="preview.body.pieces" quiet />
      <span v-else class="oma-missing">{{ previewSentence(preview.body, preview.item) }}</span>
    </div>
  </div>
</template>
