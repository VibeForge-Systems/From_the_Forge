<script setup lang="ts">
// The reply thread (M5 §3.6), slice 5h3, in the viewer's right panel:
// the replies recorded with one message as their originator, across
// conversations, read with `get_reply_thread` (built and masked in Rust,
// oldest first, capped). It takes focus on its heading when it opens, and
// the store gives focus back to the opener on Close. Each reply is a ThreadItem with its own id prefix
// (`oma-r-`), so its ids never collide with the thread's; the quote of the
// originator is left out, because the panel is about that originator.
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import type { HandleDto, SafeId, ThreadPageDto } from '@/types';
import ThreadItem from './ThreadItem.vue';
import { DETAILS_TEXT, VIEWER_STATE_TEXT } from '@/services/viewerText';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{ messageId: SafeId }>();
const emit = defineEmits<{ close: [] }>();
const viewer = useViewer();

const page = ref<ThreadPageDto | null>(null);
const heading = ref<HTMLElement | null>(null);

function focusHeading(): void {
  void nextTick(() => heading.value?.focus({ preventScroll: true }));
}
onMounted(focusHeading);
watch(() => viewer.panelFocus, focusHeading);
const loading = ref(false);
const error = ref<string | null>(null);
let seq = 0;

async function load(): Promise<void> {
  const mine = (seq += 1);
  loading.value = true;
  error.value = null;
  const got = await viewer.loadReplies(props.messageId);
  if (mine !== seq) return;
  loading.value = false;
  if (got === null) return;
  if ('error' in got) error.value = got.error;
  else page.value = got.page;
}

watch(
  () => props.messageId,
  () => {
    page.value = null;
    void load();
  },
  { immediate: true },
);

const handles = computed(() => new Map<SafeId, HandleDto>((page.value?.handles ?? []).map((h) => [h.id, h])));
const more = computed(() => !!page.value && (page.value.hasNewer || page.value.hasOlder || page.value.cutForSize));
</script>

<template>
  <div class="oma-replies">
    <div class="oma-panel-header">
      <h2 id="oma-replies-title" ref="heading" class="oma-panel-title" tabindex="-1">{{ DETAILS_TEXT.replies }}</h2>
      <button type="button" class="btn-close" :aria-label="DETAILS_TEXT.close" @click="emit('close')"></button>
    </div>
    <div class="oma-panel-body px-3 small" role="region" aria-labelledby="oma-replies-title">
      <p class="oma-body-note">{{ DETAILS_TEXT.repliesNote }}</p>
      <p v-if="loading && !page" role="status">{{ DETAILS_TEXT.loading }}</p>
      <div v-if="error" class="alert alert-warning py-2" role="alert">
        {{ DETAILS_TEXT.failed }} {{ error }}
        <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="load">{{ DETAILS_TEXT.retry }}</button>
      </div>
      <template v-if="page">
        <p v-if="page.items.length === 0">{{ DETAILS_TEXT.repliesNone }}</p>
        <div class="oma-reply-list">
          <ThreadItem
            v-for="m in page.items"
            :key="m.id"
            :message="m"
            :handles="handles"
            :day-label="null"
            :show-sender="true"
            id-prefix="oma-r-"
            in-panel
          />
        </div>
        <p v-if="more" class="oma-body-note">{{ DETAILS_TEXT.repliesMore }}</p>
        <p v-if="page.extrasIncomplete" class="oma-body-note">{{ VIEWER_STATE_TEXT.extrasIncomplete }}</p>
        <p v-if="page.skippedRows > 0" class="oma-body-note">{{ VIEWER_STATE_TEXT.skippedRows }}</p>
      </template>
    </div>
  </div>
</template>
