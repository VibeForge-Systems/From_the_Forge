<script setup lang="ts">
import { computed } from 'vue';
import { RouterLink } from 'vue-router';
import ExportStatusChip from '@/components/ExportStatusChip.vue';
import { useSettings } from '@/stores/settings';
import { useBackups } from '@/stores/backups';
import { nextTheme } from '@/composables/useTheme';
import type { Theme } from '@/types';

const emit = defineEmits<{ 'show-shortcuts': [] }>();
const settings = useSettings();
const backups = useBackups();

const THEME_META: Record<Theme, { label: string; icon: string }> = {
  system: { label: 'System', icon: 'bi-circle-half' },
  light: { label: 'Light', icon: 'bi-sun' },
  dark: { label: 'Dark', icon: 'bi-moon-stars' },
};
const current = computed(() => THEME_META[settings.theme]);
const upcoming = computed(() => THEME_META[nextTheme(settings.theme)]);
</script>

<template>
  <nav class="navbar navbar-expand border-bottom bg-body-tertiary px-3 py-1 oma-navbar">
    <RouterLink class="navbar-brand d-flex align-items-center gap-2 fs-6 fw-semibold" to="/">
      <i class="bi bi-archive-fill text-primary" aria-hidden="true"></i>
      OpenMessage Archive
    </RouterLink>
    <ul class="navbar-nav me-auto">
      <li class="nav-item">
        <RouterLink class="nav-link" exact-active-class="active" to="/">Archive</RouterLink>
      </li>
      <li class="nav-item">
        <RouterLink class="nav-link d-flex align-items-center gap-1" exact-active-class="active" to="/backups">
          Backups
          <span
            v-if="backups.jobRunning"
            class="spinner-border spinner-border-sm text-primary oma-nav-spinner"
            role="status"
            aria-label="A backup check is running"
          ></span>
        </RouterLink>
      </li>
      <li class="nav-item">
        <RouterLink class="nav-link" exact-active-class="active" to="/settings">Settings</RouterLink>
      </li>
      <li class="nav-item">
        <RouterLink class="nav-link" exact-active-class="active" to="/about">About</RouterLink>
      </li>
      <ExportStatusChip />
    </ul>
    <div class="d-flex gap-1">
      <button
        type="button"
        class="btn btn-sm btn-outline-secondary border-0"
        :aria-label="`Theme: ${current.label}. Switch to ${upcoming.label}.`"
        :title="`Theme: ${current.label} — switch to ${upcoming.label}`"
        @click="settings.setTheme(nextTheme(settings.theme))"
      >
        <i class="bi" :class="current.icon" aria-hidden="true"></i>
      </button>
      <button
        type="button"
        class="btn btn-sm btn-outline-secondary border-0"
        aria-label="Keyboard shortcuts"
        title="Keyboard shortcuts (F1)"
        @click="emit('show-shortcuts')"
      >
        <i class="bi bi-keyboard" aria-hidden="true"></i>
      </button>
    </div>
  </nav>
</template>
