<script setup lang="ts">
// The viewer page (M5 §8.3): `archive/:archiveId/viewer` and
// `…/viewer/c/:chatId`. It opens a viewer session for the archive, shows
// the conversation list on the left and the open conversation in the
// centre. Message ids never enter the URL. Leaving the viewer routes
// closes the session (the guard in `router/index.ts`, D114), and the
// window title stays "Viewer", never a chat name (D112).
//
// Until slice 5h3 nothing links here: the Archive page keeps its Planned
// badge, because bubbles, attachments and details are not shown yet.
import { computed, watch } from 'vue';
import { RouterLink, useRouter } from 'vue-router';
import type { SafeId } from '@/types';
import PanelLayout from '@/components/PanelLayout.vue';
import EmptyState from '@/components/EmptyState.vue';
import ConversationList from '@/components/viewer/ConversationList.vue';
import ThreadView from '@/components/viewer/ThreadView.vue';
import { parseSafeId } from '@/services/ids';
import { THREAD_TEXT, fill } from '@/services/viewerText';
import { useLayout } from '@/stores/layout';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{ archiveId: string; chatId?: string }>();
const viewer = useViewer();
const layout = useLayout();
const router = useRouter();

const chat = computed(() => (props.chatId === undefined ? null : parseSafeId(props.chatId)));
const stats = computed(() => viewer.session?.stats ?? null);

watch(
  () => props.archiveId,
  async (id) => {
    if (viewer.archiveId === id && viewer.session) return;
    await viewer.open(id);
  },
  { immediate: true },
);

function openChat(id: SafeId): void {
  void router.push({ name: 'viewer-chat', params: { archiveId: props.archiveId, chatId: String(id) } });
}

function closeViewer(): void {
  void router.push({ name: 'archive' });
}
</script>

<template>
  <PanelLayout left-label="Conversations" right-label="Archive in the viewer">
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
  </PanelLayout>
</template>
