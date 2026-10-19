<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, toRef, watch } from 'vue';
import { useRouter } from 'vue-router';
import AppNavbar from '@/components/AppNavbar.vue';
import ArchiveDialogs from '@/components/ArchiveDialogs.vue';
import ShortcutHelpDialog from '@/components/ShortcutHelpDialog.vue';
import ToastHost from '@/components/ToastHost.vue';
import { nextTheme, useTheme } from '@/composables/useTheme';
import { isMessageShortcut, matchShortcut, type GlobalShortcutId } from '@/services/shortcuts';
import { ZOOM_DEFAULT, nextZoom } from '@/services/zoom';
import { useLayout } from '@/stores/layout';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const settings = useSettings();
const layout = useLayout();
const viewer = useViewer();
const router = useRouter();
const showShortcuts = ref(false);
const noticeDismissed = ref(false);

useTheme(toRef(settings, 'theme'));

// The viewer's motion setting (M5 §10): "Always reduce motion" applies the
// same reset as the system's reduced-motion preference.
watch(
  () => settings.viewer.motion,
  (motion) => document.documentElement.classList.toggle('oma-reduce-motion', motion === 'reduce'),
  { immediate: true },
);

const hasPanels = () =>
  ['archive', 'backups', 'viewer', 'viewer-chat'].includes(String(router.currentRoute.value.name));

const routeName = () => String(router.currentRoute.value.name);
/** The viewer's shortcuts act only on the viewer page, with a session. */
const inViewer = () => ['viewer', 'viewer-chat'].includes(routeName()) && viewer.session !== null;
/** ... and those about a conversation, only with one open. */
const inChat = () => routeName() === 'viewer-chat' && viewer.session !== null && viewer.openChatId !== null;

/** The in-app zoom (D116): Rust validates and applies `uiZoom`. */
function zoomTo(uiZoom: number): unknown {
  return settings.snapshot && uiZoom !== settings.viewer.uiZoom ? settings.setViewer({ uiZoom }) : undefined;
}

// The message shortcuts (Enter, R) are matched by the thread, never here.
const actions: Record<GlobalShortcutId, () => unknown> = {
  'toggle-left-panel': () => hasPanels() && layout.toggleLeft(),
  'toggle-right-panel': () => hasPanels() && layout.toggleRight(),
  'go-archive': () => router.push({ name: 'archive' }),
  'go-backups': () => router.push({ name: 'backups' }),
  'go-settings': () => router.push({ name: 'settings' }),
  'go-about': () => router.push({ name: 'about' }),
  'cycle-theme': () => settings.setTheme(nextTheme(settings.theme)),
  'show-shortcuts': () => (showShortcuts.value = true),
  'viewer-search-conversation': () => inViewer() && viewer.openSearch(inChat() ? viewer.openChatId : null),
  'viewer-search-all': () => inViewer() && viewer.openSearch(null),
  'viewer-jump-date': () => inChat() && (viewer.dateJumpOpen = true),
  'viewer-first': () => inChat() && viewer.requestThread('first'),
  'viewer-last': () => inChat() && viewer.requestThread('last'),
  'zoom-in': () => zoomTo(nextZoom(settings.viewer.uiZoom, 1)),
  'zoom-out': () => zoomTo(nextZoom(settings.viewer.uiZoom, -1)),
  'zoom-reset': () => zoomTo(ZOOM_DEFAULT),
};

function onKeydown(event: KeyboardEvent): void {
  const shortcut = matchShortcut(event);
  if (!shortcut || isMessageShortcut(shortcut.id)) return;
  event.preventDefault();
  void actions[shortcut.id]();
}

onMounted(() => {
  window.addEventListener('keydown', onKeydown);
  void settings.load();
});
onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown));
</script>

<template>
  <a class="visually-hidden-focusable oma-skip-link" href="#main-content">Skip to main content</a>
  <div class="oma-app">
    <AppNavbar @show-shortcuts="showShortcuts = true" />
    <div
      v-if="settings.snapshot?.persistence.notice && !noticeDismissed"
      class="alert alert-warning rounded-0 border-0 border-bottom mb-0 py-2 d-flex align-items-center gap-2 small"
      role="status"
    >
      <i class="bi bi-exclamation-triangle-fill" aria-hidden="true"></i>
      <span class="me-auto">{{ settings.snapshot.persistence.notice }}</span>
      <button
        type="button"
        class="btn-close"
        aria-label="Dismiss settings notice"
        @click="noticeDismissed = true"
      ></button>
    </div>
    <main id="main-content" class="oma-main" tabindex="-1">
      <RouterView />
    </main>
  </div>
  <ToastHost />
  <ShortcutHelpDialog :open="showShortcuts" @close="showShortcuts = false" />
  <ArchiveDialogs />
</template>
