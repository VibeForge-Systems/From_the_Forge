<script setup lang="ts">
// One attachment of a message (M5 §5.9, §8.4), slice 5h3: a thumbnail, a
// Play card or a file card, decided by `services/attachmentView.ts` from
// Rust's card state and the sniffed content, never from the declared type.
//
// - It asks `inspect_attachment` when it first comes into view (the store
//   runs at most four at once) and caches the answer for the session.
// - A thumbnail's `src` is set only while the card is in view, and only
//   from `mediaUrl` with the token Rust minted for that inspection. A
//   failed load is retried while in view after 0.5 s, 1 s and 2 s, then
//   shows "Preview could not be loaded" with Retry.
// - The image has `alt=""`; its accessible name comes from the caption
//   (`aria-labelledby`), rendered by SafeText ("Image, <file name>", or
//   the recorded emoji description). Nothing is opened, executed or saved.
// - Play asks Rust to check the file (structure and full hash), then the
//   confirmation under `confirm`, then plays in the media viewer. Without
//   a usable engine type, or with playback off, there is no Play button.
// - Media boxes are fixed-size placeholders, so a late image never
//   reflows the feed.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import type { AttachmentDto } from '@/types';
import SafeText from './SafeText.vue';
import { cardView, sizeLine } from '@/services/attachmentView';
import { mediaUrl, retryDelayMs } from '@/services/media';
import { ATTACHMENT_TEXT, SNIFFED_KIND_TEXT, fill } from '@/services/viewerText';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{
  attachment: AttachmentDto;
  /** Its number among the message's attachments, from 1. */
  index: number;
  /** An app-made prefix for element ids (`oma-m-<id>` or `oma-r-<id>`). */
  idBase: string;
}>();

const viewer = useViewer();
const settings = useSettings();

const root = ref<HTMLElement | null>(null);
const visible = ref(false);
const failures = ref(0);
const failed = ref(false);
const waitingRetry = ref(false);
let timer: ReturnType<typeof setTimeout> | null = null;
let observer: IntersectionObserver | null = null;

const inspection = computed(() => viewer.inspections.get(props.attachment.id) ?? null);
const view = computed(() => cardView(inspection.value, settings.viewer.mediaPlayback));
const base = computed(() => viewer.session?.mediaBase ?? null);
const captionId = computed(() => `${props.idBase}-a${props.index}`);
const isChecking = computed(() => viewer.checking.has(props.attachment.id));

const sticker = computed(() => props.attachment.sticker === true);
const emoji = computed(() => {
  const e = inspection.value?.facts.emojiDescription;
  return e && e.kind === 'text' ? e : null;
});
const named = computed(() => props.attachment.name.kind === 'text');
const size = computed(() => sizeLine(props.attachment.storedBytes, props.attachment.declaredBytes));
const kindText = computed(() => {
  const v = view.value;
  if (v.kind === 'image' || v.kind === 'playable') return SNIFFED_KIND_TEXT[v.sniffed];
  if (v.kind === 'file' && v.sniffed !== null) return SNIFFED_KIND_TEXT[v.sniffed];
  return null;
});

const boxClass = computed(() =>
  sticker.value ? 'oma-attachment oma-attachment-sticker' : 'oma-attachment',
);

/** The thumbnail is requested only while in view, between retries. */
const loadThumb = computed(
  () => view.value.kind === 'image' && base.value !== null && visible.value && !failed.value && !waitingRetry.value,
);
const thumbToken = computed(() => (view.value.kind === 'image' ? view.value.token : null));

function inspectIfNeeded(): void {
  if (visible.value && inspection.value === null) void viewer.inspect(props.attachment.id);
}

function setVisible(now: boolean): void {
  visible.value = now;
  inspectIfNeeded();
}

onMounted(() => {
  const el = root.value;
  if (el && typeof IntersectionObserver === 'function') {
    observer = new IntersectionObserver((entries) => {
      for (const e of entries) setVisible(e.isIntersecting);
    });
    observer.observe(el);
  } else {
    // Without IntersectionObserver a mounted card counts as in view: the
    // feed mounts only the rows near the viewport.
    setVisible(true);
  }
});

onBeforeUnmount(() => {
  observer?.disconnect();
  if (timer !== null) clearTimeout(timer);
});

// A new session forgets inspections: ask again while in view.
watch(inspection, (now) => {
  if (now === null) inspectIfNeeded();
});

function onError(): void {
  const delay = retryDelayMs(failures.value);
  failures.value += 1;
  if (delay === null) {
    failed.value = true;
    return;
  }
  waitingRetry.value = true;
  timer = setTimeout(() => {
    timer = null;
    waitingRetry.value = false;
  }, delay);
}

function onLoad(): void {
  failures.value = 0;
}

function retry(): void {
  failures.value = 0;
  failed.value = false;
}
</script>

<template>
  <figure ref="root" :class="boxClass">
    <div v-if="view.kind === 'pending'" class="oma-media-box oma-media-placeholder" aria-hidden="true"></div>

    <template v-else-if="view.kind === 'image'">
      <button
        type="button"
        class="oma-media-btn"
        :aria-labelledby="captionId"
        @click="viewer.openImage(attachment)"
      >
        <img
          v-if="loadThumb && base && thumbToken"
          :key="failures"
          :src="mediaUrl(base, thumbToken, 'thumb') ?? undefined"
          alt=""
          class="oma-media-box"
          decoding="async"
          draggable="false"
          @error="onError"
          @load="onLoad"
        />
        <span v-else class="oma-media-box oma-media-placeholder" aria-hidden="true"></span>
      </button>
      <p v-if="failed" class="oma-body-note" role="status">
        {{ ATTACHMENT_TEXT.previewFailed }}
        <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="retry">
          {{ ATTACHMENT_TEXT.retry }}
        </button>
      </p>
    </template>

    <div v-else class="oma-file-card">
      <p v-if="kindText" class="oma-file-kind">{{ kindText }}</p>
      <p v-if="kindText && attachment.declaredType.kind === 'text'" class="oma-body-note">
        {{ ATTACHMENT_TEXT.typeRecorded }} <SafeText :value="attachment.declaredType" mode="identifier" quiet />;
        {{ ATTACHMENT_TEXT.content }} {{ kindText }}
      </p>
      <template v-if="view.kind === 'playable'">
        <p v-if="!view.structureChecked" class="oma-body-note">{{ ATTACHMENT_TEXT.structureNotChecked }}</p>
        <p v-if="view.blocked" class="oma-body-note">{{ view.blocked }}</p>
        <p v-else class="mb-0">
          <button
            type="button"
            class="btn btn-sm btn-outline-secondary"
            :disabled="isChecking"
            :aria-describedby="captionId"
            @click="viewer.requestPlay(attachment)"
          >
            <i class="bi bi-play-fill" aria-hidden="true"></i> {{ ATTACHMENT_TEXT.play }}
          </button>
          <span v-if="isChecking" class="oma-body-note ms-2" role="status">{{ ATTACHMENT_TEXT.checking }}</span>
        </p>
      </template>
      <p v-for="(line, i) in view.kind === 'file' || view.kind === 'unavailable' ? view.lines : []" :key="i" class="oma-body-note">
        {{ line }}
      </p>
    </div>

    <figcaption :id="captionId" class="oma-attachment-caption">
      <span class="visually-hidden">{{ fill(ATTACHMENT_TEXT.numbered, { n: index }) }}{{ ' ' }}</span>
      <template v-if="view.kind === 'image'">{{ sticker ? ATTACHMENT_TEXT.sticker : ATTACHMENT_TEXT.imageOf }}{{ ' ' }}</template>
      <template v-else-if="sticker">{{ ATTACHMENT_TEXT.sticker }}{{ ' ' }}</template>
      <template v-if="emoji">{{ ATTACHMENT_TEXT.emoji }}{{ ' ' }}<SafeText :value="emoji" quiet /></template>
      <SafeText v-else-if="named" :value="attachment.name" mode="identifier" />
      <span v-else class="oma-missing">{{ ATTACHMENT_TEXT.noName }}</span>
      <template v-if="view.kind === 'image' && view.animated">{{ ' ' }}<span class="oma-badge-inline">{{ ATTACHMENT_TEXT.animated }}</span></template>
      <span class="oma-attachment-size">{{ size }}</span>
    </figcaption>
  </figure>
</template>
