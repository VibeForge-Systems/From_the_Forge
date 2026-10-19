<script setup lang="ts">
// The Viewer section of Settings (M5 §7.6). Zoom applies to the whole
// window now, and capture protection on Windows; both are applied by Rust
// on every change. The other preferences only affect the viewer, so they
// were disabled, with the viewer's Planned badge, until the viewer shipped
// (roadmap `viewer`, available since slice 5h3); the privacy-screen
// preferences were disabled until the privacy screen shipped (roadmap
// `privacy-screen`, slice 5j). No control looks functional before it is.
// Capture protection exists only on Windows (tao has no Linux
// implementation), so it is not shown elsewhere (M5 §9.2).
import { computed, onMounted, ref } from 'vue';
import PlannedBadge from '@/components/PlannedBadge.vue';
import { ipc } from '@/services/ipc';
import { capability } from '@/services/roadmap';
import {
  MEDIA_PLAYBACK_TEXT,
  MOTION_TEXT,
  TIME_FORMAT_TEXT,
  TIME_ZONE_TEXT,
} from '@/services/viewerText';
import { IDLE_HIDE_CHOICES, ZOOM_STEPS, useSettings } from '@/stores/settings';
import type { MediaPlayback, MotionChoice, TimeFormat, TimeZoneChoice } from '@/types';

const settings = useSettings();
const prefs = computed(() => settings.viewer);

/** The viewer is not available yet: its preferences cannot take effect. */
const viewerReady = computed(() => capability('viewer').status === 'available');
/** The privacy screen (blur, idle hide) arrives in slice 5j (roadmap `privacy-screen`). */
const privacyReady = capability('privacy-screen').status === 'available';

const os = ref<string | null>(null);
onMounted(async () => {
  try {
    os.value = (await ipc.getAppInfo()).os;
  } catch {
    os.value = null;
  }
});
const isWindows = computed(() => os.value === 'windows');

const TIME_FORMATS = Object.keys(TIME_FORMAT_TEXT) as TimeFormat[];
const TIME_ZONES = Object.keys(TIME_ZONE_TEXT) as TimeZoneChoice[];
const MOTIONS = Object.keys(MOTION_TEXT) as MotionChoice[];
const PLAYBACK = Object.keys(MEDIA_PLAYBACK_TEXT) as MediaPlayback[];

function onSelect(event: Event): string {
  return (event.target as HTMLSelectElement).value;
}
function onCheck(event: Event): boolean {
  return (event.target as HTMLInputElement).checked;
}
const idleLabel = (m: number) => (m === 0 ? 'Off' : m === 1 ? 'After 1 minute' : `After ${m} minutes`);
</script>

<template>
  <section class="card mb-4" aria-labelledby="settings-viewer">
    <div class="card-header d-flex align-items-center gap-2">
      <h2 id="settings-viewer" class="h6 mb-0 me-auto">Viewer</h2>
    </div>
    <div class="card-body">
      <div class="row g-3 mb-3">
        <div class="col-sm-6">
          <label for="viewer-zoom" class="form-label small">Zoom</label>
          <select
            id="viewer-zoom"
            class="form-select form-select-sm"
            :value="String(prefs.uiZoom)"
            aria-describedby="viewer-zoom-hint"
            @change="settings.setViewer({ uiZoom: Number(onSelect($event)) })"
          >
            <option v-for="z in ZOOM_STEPS" :key="z" :value="String(z)">{{ z }} %</option>
          </select>
          <p id="viewer-zoom-hint" class="form-text mb-0">
            Enlarges the whole window, up to 400 %.
          </p>
        </div>
        <div v-if="isWindows" class="col-sm-6">
          <div class="form-check mt-sm-4">
            <input
              id="viewer-capture"
              type="checkbox"
              class="form-check-input"
              :checked="prefs.captureProtection"
              aria-describedby="viewer-capture-hint"
              @change="settings.setViewer({ captureProtection: onCheck($event) })"
            />
            <label class="form-check-label small" for="viewer-capture">
              Hide this window from screen capture
            </label>
          </div>
          <p id="viewer-capture-hint" class="form-text mb-0">
            Asks Windows to leave this window out of screenshots and screen capture. Microsoft
            says this is not a security feature. It also blanks the window in screen sharing and
            remote help. Its effect on screenshots, screen sharing and Windows Recall has not been
            tested yet.
          </p>
        </div>
      </div>

      <fieldset :disabled="!viewerReady" aria-describedby="viewer-pending-hint">
        <legend class="small fw-semibold d-flex align-items-center gap-2 mb-2">
          Conversations
          <PlannedBadge v-if="!viewerReady" capability="viewer" />
        </legend>
        <p v-if="!viewerReady" id="viewer-pending-hint" class="form-text mt-0">
          These take effect when the conversation viewer is available.
        </p>
        <div class="row g-3">
          <div class="col-sm-4">
            <label for="viewer-time-format" class="form-label small">Time format</label>
            <select
              id="viewer-time-format"
              class="form-select form-select-sm"
              :value="prefs.timeFormat"
              @change="settings.setViewer({ timeFormat: onSelect($event) as TimeFormat })"
            >
              <option v-for="f in TIME_FORMATS" :key="f" :value="f">{{ TIME_FORMAT_TEXT[f] }}</option>
            </select>
          </div>
          <div class="col-sm-4">
            <label for="viewer-time-zone" class="form-label small">Time zone</label>
            <select
              id="viewer-time-zone"
              class="form-select form-select-sm"
              :value="prefs.timeZone"
              aria-describedby="viewer-time-zone-hint"
              @change="settings.setViewer({ timeZone: onSelect($event) as TimeZoneChoice })"
            >
              <option v-for="z in TIME_ZONES" :key="z" :value="z">{{ TIME_ZONE_TEXT[z] }}</option>
            </select>
            <p id="viewer-time-zone-hint" class="form-text mb-0">
              No time zone is recorded in an archive.
            </p>
          </div>
          <div class="col-sm-4">
            <label for="viewer-motion" class="form-label small">Motion</label>
            <select
              id="viewer-motion"
              class="form-select form-select-sm"
              :value="prefs.motion"
              @change="settings.setViewer({ motion: onSelect($event) as MotionChoice })"
            >
              <option v-for="m in MOTIONS" :key="m" :value="m">{{ MOTION_TEXT[m] }}</option>
            </select>
          </div>
        </div>
        <div class="form-check mt-3">
          <input
            id="viewer-seconds"
            type="checkbox"
            class="form-check-input"
            :checked="prefs.showSeconds"
            @change="settings.setViewer({ showSeconds: onCheck($event) })"
          />
          <label class="form-check-label small" for="viewer-seconds">Show seconds</label>
        </div>
        <div class="form-check">
          <input
            id="viewer-previews"
            type="checkbox"
            class="form-check-input"
            :checked="prefs.listPreviews"
            @change="settings.setViewer({ listPreviews: onCheck($event) })"
          />
          <label class="form-check-label small" for="viewer-previews">
            Show the last message in the conversation list
          </label>
        </div>
        <div class="form-check">
          <input
            id="viewer-hidden-chars"
            type="checkbox"
            class="form-check-input"
            :checked="prefs.showHiddenCharacters"
            aria-describedby="viewer-hidden-chars-hint"
            @change="settings.setViewer({ showHiddenCharacters: onCheck($event) })"
          />
          <label class="form-check-label small" for="viewer-hidden-chars">
            Show invisible and direction-control characters in messages
          </label>
          <p id="viewer-hidden-chars-hint" class="form-text mt-0 mb-0">
            Names and addresses always show them.
          </p>
        </div>
        <div class="form-check">
          <input
            id="viewer-reveal-earlier"
            type="checkbox"
            class="form-check-input"
            :checked="prefs.revealEarlierText"
            aria-describedby="viewer-reveal-earlier-hint"
            @change="settings.setViewer({ revealEarlierText: onCheck($event) })"
          />
          <label class="form-check-label small" for="viewer-reveal-earlier">
            Show earlier text of unsent and edited messages without asking
          </label>
          <p id="viewer-reveal-earlier-hint" class="form-text mt-0 mb-0">
            Off by default: text a sender withdrew stays hidden until you choose to show it.
          </p>
        </div>

        <fieldset class="mt-3" aria-describedby="viewer-playback-hint">
          <legend class="form-label small mb-1">Audio and video</legend>
          <div v-for="p in PLAYBACK" :key="p" class="form-check">
            <input
              :id="`viewer-playback-${p}`"
              type="radio"
              class="form-check-input"
              name="viewer-playback"
              :value="p"
              :checked="prefs.mediaPlayback === p"
              @change="settings.setViewer({ mediaPlayback: p })"
            />
            <label class="form-check-label small" :for="`viewer-playback-${p}`">
              {{ MEDIA_PLAYBACK_TEXT[p] }}
            </label>
          </div>
          <p id="viewer-playback-hint" class="form-text mb-0">
            Audio and video are played by your system's media codecs from the original file,
            after a check of the file's structure. A file crafted by its sender could exploit a
            flaw in those codecs; the app cannot prevent that. "Always play without asking" in
            the confirmation switches this to "Play when I click Play, without asking".
          </p>
        </fieldset>
      </fieldset>

      <fieldset :disabled="!viewerReady || !privacyReady" class="mt-3">
        <legend class="small fw-semibold d-flex align-items-center gap-2 mb-2">
          Privacy screen
          <PlannedBadge v-if="!viewerReady" capability="viewer" />
          <PlannedBadge v-else-if="!privacyReady" capability="privacy-screen" />
        </legend>
        <div class="form-check">
          <input
            id="viewer-blur"
            type="checkbox"
            class="form-check-input"
            :checked="prefs.blurOnFocusLoss"
            @change="settings.setViewer({ blurOnFocusLoss: onCheck($event) })"
          />
          <label class="form-check-label small" for="viewer-blur">
            Hide conversations when the window loses focus
          </label>
        </div>
        <div class="mt-2">
          <label for="viewer-idle" class="form-label small">Close the viewer when idle</label>
          <select
            id="viewer-idle"
            class="form-select form-select-sm w-auto"
            :value="String(prefs.idleHideMinutes)"
            @change="settings.setViewer({ idleHideMinutes: Number(onSelect($event)) })"
          >
            <option v-for="m in IDLE_HIDE_CHOICES" :key="m" :value="String(m)">
              {{ idleLabel(m) }}
            </option>
          </select>
        </div>
        <p class="form-text mb-0">
          The privacy screen (Ctrl+Shift+H, or "Hide screen" in the viewer) hides the
          conversations. It is not a lock: anyone at this computer can show them again, and the
          archive is not encrypted. Window previews may capture a frame before the screen is
          hidden. Closing the viewer when idle loses nothing; "Show" opens it again.
        </p>
      </fieldset>
    </div>
  </section>
</template>
