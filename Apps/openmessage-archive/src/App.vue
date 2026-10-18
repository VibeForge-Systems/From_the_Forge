<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, toRef } from 'vue';
import { useRouter } from 'vue-router';
import AppNavbar from '@/components/AppNavbar.vue';
import ArchiveDialogs from '@/components/ArchiveDialogs.vue';
import ShortcutHelpDialog from '@/components/ShortcutHelpDialog.vue';
import ToastHost from '@/components/ToastHost.vue';
import { nextTheme, useTheme } from '@/composables/useTheme';
import { matchShortcut, type ShortcutId } from '@/services/shortcuts';
import { useLayout } from '@/stores/layout';
import { useSettings } from '@/stores/settings';

const settings = useSettings();
const layout = useLayout();
const router = useRouter();
const showShortcuts = ref(false);
const noticeDismissed = ref(false);

useTheme(toRef(settings, 'theme'));

const hasPanels = () => ['archive', 'backups'].includes(String(router.currentRoute.value.name));

const actions: Record<ShortcutId, () => unknown> = {
  'toggle-left-panel': () => hasPanels() && layout.toggleLeft(),
  'toggle-right-panel': () => hasPanels() && layout.toggleRight(),
  'go-archive': () => router.push({ name: 'archive' }),
  'go-backups': () => router.push({ name: 'backups' }),
  'go-settings': () => router.push({ name: 'settings' }),
  'go-about': () => router.push({ name: 'about' }),
  'cycle-theme': () => settings.setTheme(nextTheme(settings.theme)),
  'show-shortcuts': () => (showShortcuts.value = true),
};

function onKeydown(event: KeyboardEvent): void {
  const shortcut = matchShortcut(event);
  if (!shortcut) return;
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
