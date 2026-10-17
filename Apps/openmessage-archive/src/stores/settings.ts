import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { IpcError, ipc } from '@/services/ipc';
import type {
  ArchiveLocationStatus,
  LayoutPreferences,
  SettingsSnapshot,
  SettingsUpdate,
  Theme,
} from '@/types';
import { useNotifications } from './notifications';

export const useSettings = defineStore('settings', () => {
  const snapshot = ref<SettingsSnapshot | null>(null);
  const location = ref<ArchiveLocationStatus>({ path: null, state: 'unset', checks: [] });
  const loaded = ref(false);
  const loadError = ref<string | null>(null);
  const busy = ref(false);
  let warnedReadOnly = false;

  const settings = computed(() => snapshot.value?.settings ?? null);
  const theme = computed<Theme>(() => settings.value?.theme ?? 'system');
  const layout = computed<LayoutPreferences>(
    () => settings.value?.layout ?? { leftPanelOpen: true, rightPanelOpen: true },
  );
  const locationUsable = computed(
    () => location.value.state === 'ready' || location.value.state === 'warning',
  );

  function report(error: unknown): void {
    const notifications = useNotifications();
    const message = error instanceof Error ? error.message : String(error);
    notifications.push('danger', message);
  }

  async function load(): Promise<void> {
    try {
      snapshot.value = await ipc.getSettings();
      location.value = await ipc.getArchiveLocationStatus();
      loadError.value = null;
    } catch (error) {
      loadError.value = error instanceof Error ? error.message : String(error);
    } finally {
      loaded.value = true;
    }
  }

  /**
   * Persist a preference change. When settings are read-only this session,
   * the backend still applies the change in memory and rejects with
   * `settingsReadOnly`; re-read the snapshot so the UI matches, and say so
   * once rather than on every click.
   */
  async function update(change: SettingsUpdate): Promise<void> {
    try {
      snapshot.value = await ipc.updateSettings(change);
    } catch (error) {
      if (error instanceof IpcError && error.code === 'settingsReadOnly') {
        snapshot.value = await ipc.getSettings();
        if (!warnedReadOnly) {
          warnedReadOnly = true;
          useNotifications().push('warning', error.message);
        }
        return;
      }
      report(error);
    }
  }

  const setTheme = (next: Theme) => update({ theme: next });
  const setLayout = (next: LayoutPreferences) => update({ layout: next });

  async function chooseLocation(): Promise<void> {
    busy.value = true;
    try {
      const choice = await ipc.chooseArchiveDirectory();
      if (choice === null) return;
      snapshot.value = choice.settings;
      if (choice.saved) {
        location.value = choice.status;
        useNotifications().push(
          choice.status.state === 'warning' ? 'warning' : 'success',
          choice.status.state === 'warning'
            ? 'Archive location saved, with a warning. Review it in Settings.'
            : 'Archive location saved.',
        );
      } else {
        const reason =
          choice.status.checks.find((c) => c.level === 'error')?.message ??
          'The folder did not pass the location checks.';
        useNotifications().push('danger', `That folder cannot be used. ${reason}`);
      }
    } catch (error) {
      report(error);
    } finally {
      busy.value = false;
    }
  }

  async function refreshLocation(): Promise<void> {
    busy.value = true;
    try {
      location.value = await ipc.getArchiveLocationStatus();
    } catch (error) {
      report(error);
    } finally {
      busy.value = false;
    }
  }

  async function clearLocation(): Promise<void> {
    busy.value = true;
    try {
      snapshot.value = await ipc.clearArchiveDirectory();
      location.value = { path: null, state: 'unset', checks: [] };
      useNotifications().push('info', 'Archive location forgotten. No files were deleted.');
    } catch (error) {
      report(error);
    } finally {
      busy.value = false;
    }
  }

  async function openLocation(): Promise<void> {
    try {
      await ipc.openArchiveDirectory();
    } catch (error) {
      report(error);
    }
  }

  return {
    snapshot,
    settings,
    theme,
    layout,
    location,
    locationUsable,
    loaded,
    loadError,
    busy,
    load,
    setTheme,
    setLayout,
    chooseLocation,
    refreshLocation,
    clearLocation,
    openLocation,
  };
});
