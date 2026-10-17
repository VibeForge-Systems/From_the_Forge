<script setup lang="ts">
import LocationStateBadge from '@/components/LocationStateBadge.vue';
import LocationStatusList from '@/components/LocationStatusList.vue';
import { useSettings } from '@/stores/settings';
import type { Theme } from '@/types';

const settings = useSettings();

const THEMES: { value: Theme; label: string; icon: string }[] = [
  { value: 'system', label: 'System', icon: 'bi-circle-half' },
  { value: 'light', label: 'Light', icon: 'bi-sun' },
  { value: 'dark', label: 'Dark', icon: 'bi-moon-stars' },
];
</script>

<template>
  <div class="oma-page">
    <h1 class="h4 mb-4">Settings</h1>

    <section class="card mb-4" aria-labelledby="settings-location">
      <div class="card-header d-flex align-items-center gap-2">
        <h2 id="settings-location" class="h6 mb-0 me-auto">Archive location</h2>
        <LocationStateBadge :state="settings.location.state" />
      </div>
      <div class="card-body">
        <p class="text-body-secondary small">
          The folder where OpenMessage Archive keeps its archives. Each archive gets its own
          subfolder. It must be outside any iPhone backup, and should be on a local drive.
        </p>
        <p v-if="settings.location.path" class="font-monospace text-break mb-3">
          {{ settings.location.path }}
        </p>
        <p v-else class="fst-italic text-body-secondary mb-3">No folder chosen yet.</p>
        <LocationStatusList
          v-if="settings.location.checks.length"
          :checks="settings.location.checks"
          class="mb-3 small"
        />
        <div class="d-flex flex-wrap gap-2">
          <button
            type="button"
            class="btn btn-primary btn-sm"
            :disabled="settings.busy"
            @click="settings.chooseLocation()"
          >
            <i class="bi bi-folder2-open me-1" aria-hidden="true"></i>
            {{ settings.location.path ? 'Change folder…' : 'Choose folder…' }}
          </button>
          <button
            type="button"
            class="btn btn-outline-secondary btn-sm"
            :disabled="settings.busy || settings.location.state === 'unset'"
            @click="settings.refreshLocation()"
          >
            Check again
          </button>
          <button
            type="button"
            class="btn btn-outline-secondary btn-sm"
            :disabled="!settings.locationUsable"
            @click="settings.openLocation()"
          >
            Show in file manager
          </button>
          <button
            type="button"
            class="btn btn-outline-secondary btn-sm"
            :disabled="settings.busy || settings.location.state === 'unset'"
            aria-describedby="forget-hint"
            @click="settings.clearLocation()"
          >
            Forget location
          </button>
        </div>
        <p id="forget-hint" class="form-text mb-0">
          Forgetting the location only clears this setting. Nothing on disk is deleted.
        </p>
      </div>
    </section>

    <section class="card mb-4" aria-labelledby="settings-appearance">
      <div class="card-header">
        <h2 id="settings-appearance" class="h6 mb-0">Appearance</h2>
      </div>
      <div class="card-body">
        <fieldset>
          <legend class="form-label small">Theme</legend>
          <div class="btn-group" role="group">
            <template v-for="t in THEMES" :key="t.value">
              <input
                :id="`theme-${t.value}`"
                type="radio"
                class="btn-check"
                name="theme"
                :value="t.value"
                :checked="settings.theme === t.value"
                @change="settings.setTheme(t.value)"
              />
              <label class="btn btn-outline-primary btn-sm" :for="`theme-${t.value}`">
                <i class="bi me-1" :class="t.icon" aria-hidden="true"></i>{{ t.label }}
              </label>
            </template>
          </div>
        </fieldset>
      </div>
    </section>

    <section class="card mb-4" aria-labelledby="settings-storage">
      <div class="card-header d-flex align-items-center gap-2">
        <h2 id="settings-storage" class="h6 mb-0 me-auto">Settings file</h2>
        <span v-if="settings.snapshot?.persistence.readOnly" class="badge text-bg-warning">
          Read-only this session
        </span>
      </div>
      <div class="card-body">
        <div
          v-if="settings.snapshot?.persistence.notice"
          class="alert alert-warning small"
          role="status"
        >
          {{ settings.snapshot.persistence.notice }}
        </div>
        <p class="small text-body-secondary mb-1">
          Preferences are stored in a plain JSON file. It never contains message content,
          contacts, or passwords.
        </p>
        <p class="font-monospace small text-break mb-0">{{ settings.snapshot?.persistence.file }}</p>
      </div>
    </section>

    <section class="card" aria-labelledby="settings-privacy">
      <div class="card-header">
        <h2 id="settings-privacy" class="h6 mb-0">Privacy</h2>
      </div>
      <div class="card-body small">
        <ul class="mb-0">
          <li>OpenMessage Archive makes no network requests and has no telemetry.</li>
          <li>Archives are stored only in the folder above, in formats documented in the project.</li>
          <li>Backup passwords will never be written to disk in plain text.</li>
        </ul>
      </div>
    </section>
  </div>
</template>
