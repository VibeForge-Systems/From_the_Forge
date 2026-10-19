<script setup lang="ts">
// The media viewer (M5 §5.5, §5.6, §8.4), slice 5h3: a native `<dialog>`
// (ModalDialog) that shows an image's `display` rendition, or plays audio
// or video after Rust's checks (and, under `confirm`, the user's
// confirmation). Mounted once, in the viewer page.
//
// - The image is the app's own re-encoded preview, labelled "Preview,
//   re-encoded, at most 2048 px; original W×H; N stored (M
//   declared)". The original size is asked from Rust once the preview has
//   loaded (it is known only after the decode helper made one).
// - `<audio>`/`<video>` exist only once playback was enabled: native
//   controls, `preload="metadata"`, never autoplay, and no download,
//   picture-in-picture or remote playback. While anything plays, the
//   media session's title is the neutral "OpenMessage Archive", never a
//   file name. Closing the dialog unmounts the element, which stops it.
// - It shows the recorded type beside the sniffed content and both sizes.
//   There is no download, open or share action (§5.10).
import { computed, ref, watch } from 'vue';
import ModalDialog from '@/components/ModalDialog.vue';
import SafeText from './SafeText.vue';
import { mediaElementFor, renditionLabel, sizeLine } from '@/services/attachmentView';
import { NEUTRAL_MEDIA_TITLE, mediaUrl, preloadFor } from '@/services/media';
import { ATTACHMENT_TEXT, SNIFFED_KIND_TEXT } from '@/services/viewerText';
import { useViewer } from '@/stores/viewer';

const viewer = useViewer();

const open = computed(() => viewer.media);
const attachment = computed(() => open.value?.attachment ?? null);
const inspection = computed(() =>
  attachment.value ? (viewer.inspections.get(attachment.value.id) ?? null) : null,
);
const base = computed(() => viewer.session?.mediaBase ?? null);
const token = computed(() => inspection.value?.token ?? null);
const sniffed = computed(() =>
  inspection.value?.content.kind === 'sniffed' ? inspection.value.content.sniffed : null,
);
const image = computed(() => open.value?.mode === 'image');
/** Playback was enabled for this file: Rust allowed it, and the user asked. */
const playing = computed(
  () => open.value?.mode === 'play' && inspection.value?.playback?.kind === 'allowed' && sniffed.value !== null,
);
const element = computed(() => (sniffed.value ? mediaElementFor(sniffed.value) : 'video'));
const title = computed(() => (image.value ? ATTACHMENT_TEXT.viewerTitle : ATTACHMENT_TEXT.playerTitle));

const failed = ref(false);
const loaded = ref(false);
const askedSize = ref(false);

watch(attachment, () => {
  failed.value = false;
  loaded.value = false;
  askedSize.value = false;
  clearMediaSession();
});

const label = computed(() => {
  const a = attachment.value;
  if (!a) return '';
  if (image.value) return renditionLabel(inspection.value?.originalSize ?? null, a.storedBytes, a.declaredBytes);
  return sizeLine(a.storedBytes, a.declaredBytes);
});

function onImageLoad(): void {
  loaded.value = true;
  const a = attachment.value;
  if (a && !askedSize.value && !inspection.value?.originalSize) {
    askedSize.value = true;
    void viewer.inspect(a.id, false, true);
  }
}

function onError(): void {
  failed.value = true;
}

function retry(): void {
  failed.value = false;
}

/** A neutral "now playing" entry, where the engine has a media session. */
function onPlay(): void {
  const session = typeof navigator !== 'undefined' ? navigator.mediaSession : undefined;
  if (session && typeof MediaMetadata === 'function') {
    session.metadata = new MediaMetadata({ title: NEUTRAL_MEDIA_TITLE });
  }
}

function clearMediaSession(): void {
  const session = typeof navigator !== 'undefined' ? navigator.mediaSession : undefined;
  if (session) session.metadata = null;
}

function close(): void {
  clearMediaSession();
  viewer.closeMedia();
}
</script>

<template>
  <ModalDialog :open="open !== null" :title="title" :return-to="open?.returnTo ?? null" @close="close">
    <figure v-if="attachment" class="oma-media-viewer">
      <template v-if="image && base && token && !failed">
        <img
          :src="mediaUrl(base, token, 'display') ?? undefined"
          alt=""
          aria-labelledby="oma-media-caption"
          class="oma-media-full"
          draggable="false"
          @load="onImageLoad"
          @error="onError"
        />
        <p v-if="!loaded" class="oma-body-note" role="status">{{ ATTACHMENT_TEXT.loadingPreview }}</p>
      </template>
      <p v-else-if="image && failed" class="oma-body-note" role="status">
        {{ ATTACHMENT_TEXT.previewFailed }}
        <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="retry">
          {{ ATTACHMENT_TEXT.retry }}
        </button>
      </p>
      <template v-else-if="playing && base && token && !failed">
        <video
          v-if="element === 'video'"
          :src="mediaUrl(base, token, 'stream') ?? undefined"
          class="oma-media-full"
          controls
          :preload="preloadFor(true)"
          controlslist="nodownload noremoteplayback"
          disablepictureinpicture
          disableremoteplayback
          playsinline
          draggable="false"
          aria-labelledby="oma-media-caption"
          @play="onPlay"
          @error="onError"
        ></video>
        <audio
          v-else
          :src="mediaUrl(base, token, 'stream') ?? undefined"
          controls
          :preload="preloadFor(true)"
          controlslist="nodownload noremoteplayback"
          disableremoteplayback
          aria-labelledby="oma-media-caption"
          @play="onPlay"
          @error="onError"
        ></audio>
      </template>
      <p v-else-if="!image && failed" class="oma-body-note" role="alert">{{ ATTACHMENT_TEXT.playFailed }}</p>

      <figcaption id="oma-media-caption">
        <SafeText v-if="attachment.name.kind === 'text'" :value="attachment.name" mode="identifier" />
        <span v-else class="oma-missing">{{ ATTACHMENT_TEXT.noName }}</span>
      </figcaption>
    </figure>
    <template v-if="attachment">
      <p class="oma-body-note">{{ label }}</p>
      <p v-if="sniffed" class="oma-body-note">
        {{ ATTACHMENT_TEXT.typeRecorded }}
        <SafeText v-if="attachment.declaredType.kind === 'text'" :value="attachment.declaredType" mode="identifier" quiet />
        <SafeText v-else :value="attachment.declaredType" quiet />;
        {{ ATTACHMENT_TEXT.content }} {{ SNIFFED_KIND_TEXT[sniffed] }}
      </p>
    </template>
  </ModalDialog>
</template>
