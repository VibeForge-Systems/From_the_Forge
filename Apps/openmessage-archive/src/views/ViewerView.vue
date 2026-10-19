<script setup lang="ts">
// The viewer page (M5 §8.3): `archive/:archiveId/viewer` and
// `…/viewer/c/:chatId`. It opens a viewer session for the archive, shows
// the conversation list on the left and the open conversation in the
// centre. Message ids never enter the URL. Leaving the viewer routes
// closes the session (the guard in `router/index.ts`, D114), and the
// window title stays "Viewer", never a chat name (D112).
//
// Since slice 5h3 the Archive page links here ("Browse conversations").
// The right panel shows a message's details ("As recorded") or its reply
// thread when one is asked for, and the archive's facts otherwise. The
// media viewer and the play confirmation are mounted once, here. Since
// slice 5i the right panel also shows the search panel ("Search", Ctrl+F
// for the open conversation, Ctrl+Shift+F for all); opening a result goes
// through `openChat` with the message remembered in the store.
//
// Privacy (slice 5j): while the privacy screen is up nothing of the viewer
// is mounted here (`v-if`), and the curtain (App.vue) covers the window.
// "Hide screen" (or Ctrl+Shift+H) raises it. While this page is mounted the
// page guards hold (print, save, reload, the engine's context menu and
// drag-out; `services/guards.ts`) and the message menu is mounted once.
import { computed, onBeforeUnmount, onMounted, watch } from 'vue';
import { RouterLink, useRouter } from 'vue-router';
import type { SafeId } from '@/types';
import PanelLayout from '@/components/PanelLayout.vue';
import EmptyState from '@/components/EmptyState.vue';
import ConversationList from '@/components/viewer/ConversationList.vue';
import ThreadView from '@/components/viewer/ThreadView.vue';
import MessageDetails from '@/components/viewer/MessageDetails.vue';
import ReplyThread from '@/components/viewer/ReplyThread.vue';
import MediaViewer from '@/components/viewer/MediaViewer.vue';
import PlayConfirmDialog from '@/components/viewer/PlayConfirmDialog.vue';
import SearchPanel from '@/components/viewer/SearchPanel.vue';
import MessageMenu from '@/components/viewer/MessageMenu.vue';
import { installViewerGuards } from '@/services/guards';
import { parseSafeId } from '@/services/ids';
import { DETAILS_TEXT, PRIVACY_TEXT, SEARCH_TEXT, THREAD_TEXT, fill } from '@/services/viewerText';
import { useLayout } from '@/stores/layout';
import { usePrivacy } from '@/stores/privacy';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{ archiveId: string; chatId?: string }>();
const viewer = useViewer();
const layout = useLayout();
const router = useRouter();
const privacy = usePrivacy();

let removeGuards: (() => void) | null = null;
onMounted(() => {
  removeGuards = installViewerGuards();
  privacy.attach();
});
onBeforeUnmount(() => {
  removeGuards?.();
  privacy.detach();
});

const chat = computed(() => (props.chatId === undefined ? null : parseSafeId(props.chatId)));
const stats = computed(() => viewer.session?.stats ?? null);
const panel = computed(() => viewer.panel);
const handles = computed(() => viewer.openWindow?.handles ?? new Map());
const rightLabel = computed(() =>
  panel.value?.kind === 'details'
    ? DETAILS_TEXT.title
    : panel.value?.kind === 'replies'
      ? DETAILS_TEXT.replies
      : panel.value?.kind === 'search'
        ? SEARCH_TEXT.title
        : THREAD_TEXT.infoHeading,
);

// Asking for details or replies shows the right panel. Closing it while
// it is an overlay closes the overlay too, so focus can return to the
// message instead of staying under the backdrop.
watch(panel, (now) => {
  if (now !== null) void layout.openRight();
  else if (layout.rightOverlay) layout.closeOverlays();
});

watch(
  () => props.archiveId,
  async (id) => {
    if (viewer.archiveId === id && viewer.session) return;
    await viewer.open(id);
  },
  { immediate: true },
);

// Choosing a conversation (the list overlay) or opening a search result
// (the search overlay) moves the user into the thread: an overlay closes
// so it never covers the conversation or the focused message (WCAG
// 2.4.11). Docked panels stay.
function openChat(id: SafeId): void {
  layout.closeOverlays();
  void router.push({ name: 'viewer-chat', params: { archiveId: props.archiveId, chatId: String(id) } });
}

function closeViewer(): void {
  void router.push({ name: 'archive' });
}
</script>

<template>
  <template v-if="!privacy.hidden">
    <PanelLayout left-label="Conversations" :right-label="rightLabel">
      <template #left>
        <ConversationList v-if="viewer.session" :key="viewer.viewer ?? 0" :selected="chat" @open="openChat" />
      </template>

      <div class="oma-thread-toolbar">
        <button
          type="button"
          class="btn btn-sm btn-outline-secondary border-0"
          :aria-pressed="layout.leftVisible"
          aria-controls="oma-left-panel"
          title="Show or hide the conversation list"
          @click="layout.toggleLeft()"
        >
          <i class="bi bi-layout-sidebar" aria-hidden="true"></i>
          <span class="visually-hidden">Toggle conversation list</span>
        </button>
        <span class="flex-grow-1"></span>
        <button
          v-if="viewer.session"
          type="button"
          class="btn btn-sm btn-outline-secondary"
          title="Hide the conversations (Ctrl+Shift+H). This is not a lock."
          @click="privacy.hide('manual')"
        >
          <i class="bi bi-eye-slash me-1" aria-hidden="true"></i>{{ PRIVACY_TEXT.hide }}
        </button>
        <button
          v-if="viewer.session"
          type="button"
          class="btn btn-sm btn-outline-secondary"
          @click="viewer.openSearch(null)"
        >
          <i class="bi bi-search me-1" aria-hidden="true"></i>{{ THREAD_TEXT.searchAll }}
        </button>
        <button type="button" class="btn btn-sm btn-outline-secondary" @click="closeViewer">
          {{ THREAD_TEXT.closeViewer }}
        </button>
        <button
          type="button"
          class="btn btn-sm btn-outline-secondary border-0"
          :aria-pressed="layout.rightVisible"
          aria-controls="oma-right-panel"
          title="Show or hide archive information"
          @click="layout.toggleRight()"
        >
          <i class="bi bi-layout-sidebar-reverse" aria-hidden="true"></i>
          <span class="visually-hidden">Toggle archive information</span>
        </button>
      </div>

      <div class="oma-viewer-body">
        <div v-if="viewer.opening" class="d-flex justify-content-center p-5" role="status">
          <div class="spinner-border text-secondary" aria-hidden="true"></div>
          <span class="visually-hidden">{{ THREAD_TEXT.opening }}</span>
        </div>
        <div v-else-if="!viewer.session" class="alert alert-warning m-4" role="alert">
          <p class="mb-2">{{ viewer.error ?? THREAD_TEXT.sessionClosed }}</p>
          <RouterLink :to="{ name: 'archive' }" class="btn btn-sm btn-outline-secondary">Back to Archive</RouterLink>
        </div>
        <ThreadView v-else-if="chat !== null" :key="`${viewer.viewer}:${chat}`" :chat-id="chat" @open-chat="openChat" />
        <EmptyState v-else icon="chat-left-text" :title="THREAD_TEXT.chooseConversation">
          <p>{{ THREAD_TEXT.chooseHint }}</p>
        </EmptyState>
      </div>

      <template #right>
        <MessageDetails
          v-if="viewer.session && panel?.kind === 'details'"
          :message-id="panel.message"
          :reactions="panel.reactions"
          :handles="handles"
          @close="viewer.closePanel()"
        />
        <ReplyThread
          v-else-if="viewer.session && panel?.kind === 'replies'"
          :message-id="panel.message"
          @close="viewer.closePanel()"
        />
        <SearchPanel
          v-else-if="viewer.session && panel?.kind === 'search'"
          @close="viewer.closePanel()"
          @open-chat="openChat"
        />
        <template v-else>
          <div class="oma-panel-header">
            <h2 class="oma-panel-title">{{ THREAD_TEXT.infoHeading }}</h2>
          </div>
          <div v-if="viewer.session" class="oma-panel-body px-3 small">
            <ul v-if="stats" class="list-unstyled">
              <li>{{ fill(THREAD_TEXT.infoChats, { n: stats.chats }) }}</li>
              <li>{{ fill(THREAD_TEXT.infoMessages, { n: stats.messages }) }}</li>
              <li>{{ fill(THREAD_TEXT.infoAttachments, { n: stats.attachments }) }}</li>
            </ul>
            <p v-else>{{ THREAD_TEXT.infoNoStats }}</p>
            <p v-if="viewer.session.provisional">{{ THREAD_TEXT.provisional }}</p>
            <p v-if="(viewer.session.unsearchable ?? 0) > 0">
              {{ fill(THREAD_TEXT.unsearchable, { n: viewer.session.unsearchable ?? 0 }) }}
            </p>
          </div>
        </template>
      </template>
    </PanelLayout>
    <MediaViewer />
    <PlayConfirmDialog />
    <MessageMenu />
  </template>
</template>
